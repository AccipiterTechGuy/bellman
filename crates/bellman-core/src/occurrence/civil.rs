//! Resolve a wall-clock (date + time) in an IANA zone to a single `DateTime<Tz>`,
//! applying DST gap/fold policies.

use super::policy::{DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy};
use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Tz;

/// Clamp an invalid day-of-month to the last valid day of that month when policy
/// is `Clamp`. Returns `None` only if the year/month themselves are invalid.
pub fn clamp_month_day(
    year: i32,
    month: u32,
    day: u32,
    policy: InvalidMonthDayPolicy,
) -> Option<NaiveDate> {
    let last = super::kind::OccurrenceKind::days_in_month(year, month);
    let use_day = match policy {
        InvalidMonthDayPolicy::Clamp => day.min(last),
    };
    NaiveDate::from_ymd_opt(year, month, use_day)
}

/// Map a civil local datetime to a unique instant under the given DST policies.
///
/// - Gap (spring-forward): first valid local second at-or-after the requested time.
/// - Fold (fall-back): earliest of the two ambiguous instants.
pub fn resolve_local(
    tz: Tz,
    date: NaiveDate,
    time: NaiveTime,
    gap: DstGapPolicy,
    fold: DstFoldPolicy,
) -> Option<DateTime<Tz>> {
    let naive = NaiveDateTime::new(date, time);
    resolve_naive(tz, naive, gap, fold)
}

fn resolve_naive(
    tz: Tz,
    naive: NaiveDateTime,
    gap: DstGapPolicy,
    fold: DstFoldPolicy,
) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt),
        LocalResult::Ambiguous(earliest, _latest) => match fold {
            DstFoldPolicy::FirstOccurrence => Some(earliest),
        },
        LocalResult::None => match gap {
            DstGapPolicy::FirstValidAfterGap => first_valid_after_gap(tz, naive),
        },
    }
}

/// Walk forward from `naive` until a single (or fold) local mapping exists.
///
/// Real DST gaps are at most one hour; we allow up to three hours as a safety
/// bound so pathological / historical tzdata quirks still resolve.
fn first_valid_after_gap(tz: Tz, naive: NaiveDateTime) -> Option<DateTime<Tz>> {
    // Coarse then fine: jump by minutes, then settle to the first second.
    let mut probe = naive;
    for _ in 0..180 {
        probe += Duration::minutes(1);
        match tz.from_local_datetime(&probe) {
            LocalResult::Single(dt) => {
                // Back up to the first valid second in this minute window.
                return Some(first_second_in_window(tz, probe - Duration::minutes(1), dt));
            }
            LocalResult::Ambiguous(earliest, _) => {
                return Some(earliest);
            }
            LocalResult::None => {}
        }
    }
    // Second-by-second fallback (covers sub-minute gaps if any).
    let mut probe = naive;
    for _ in 0..3 * 3600 {
        probe += Duration::seconds(1);
        match tz.from_local_datetime(&probe) {
            LocalResult::Single(dt) => return Some(dt),
            LocalResult::Ambiguous(earliest, _) => return Some(earliest),
            LocalResult::None => {}
        }
    }
    None
}

/// Given that `after_gap` is a known-valid instant whose local time is ~1 minute
/// past the gap, find the earliest valid second strictly after `before_or_at_gap`.
fn first_second_in_window(
    tz: Tz,
    before_or_at_gap: NaiveDateTime,
    after_gap: DateTime<Tz>,
) -> DateTime<Tz> {
    let mut probe = before_or_at_gap;
    let end = after_gap.naive_local() + Duration::seconds(1);
    while probe <= end {
        probe += Duration::seconds(1);
        match tz.from_local_datetime(&probe) {
            LocalResult::Single(dt) => return dt,
            LocalResult::Ambiguous(earliest, _) => return earliest,
            LocalResult::None => {}
        }
    }
    after_gap
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Europe::Helsinki;

    #[test]
    fn clamp_day_31_in_april() {
        let d = clamp_month_day(2026, 4, 31, InvalidMonthDayPolicy::Clamp).unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 4, 30).unwrap());
    }

    #[test]
    fn clamp_feb_29_non_leap() {
        let d = clamp_month_day(2025, 2, 29, InvalidMonthDayPolicy::Clamp).unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2025, 2, 28).unwrap());
    }

    #[test]
    fn clamp_feb_29_leap_keeps() {
        let d = clamp_month_day(2024, 2, 29, InvalidMonthDayPolicy::Clamp).unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2024, 2, 29).unwrap());
    }

    #[test]
    fn helsinki_spring_gap_first_valid() {
        // EU: last Sunday of March — 2026-03-29, clocks 03:00 → 04:00 (EET→EEST).
        // 03:30 does not exist; first valid after is 04:00:00.
        let date = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        let time = NaiveTime::from_hms_opt(3, 30, 0).unwrap();
        let dt = resolve_local(
            Helsinki,
            date,
            time,
            DstGapPolicy::FirstValidAfterGap,
            DstFoldPolicy::FirstOccurrence,
        )
        .unwrap();
        assert_eq!(dt.time(), NaiveTime::from_hms_opt(4, 0, 0).unwrap());
        assert_eq!(dt.date_naive(), date);
    }
}
