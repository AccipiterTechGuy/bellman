//! Golden tests for the occurrence engine.
//!
//! Covers: DST gap + fold (Europe/Helsinki, America/New_York, Asia/Kathmandu),
//! Feb-29 yearly clamp, day-31 monthly clamp, year boundary, interval/DST
//! independence, preview next-5 for every variant, exclusion dates, skip-next.

use super::*;
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::{America::New_York, Asia::Kathmandu, Europe::Helsinki, Tz};

fn hms(h: u32, m: u32, s: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, s).unwrap()
}

fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn local(tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Tz> {
    // Prefer single mapping; for test fixtures we pick unambiguous civil times
    // except when deliberately testing gap/fold.
    tz.with_ymd_and_hms(y, mo, d, h, mi, s)
        .single()
        .unwrap_or_else(|| {
            // Fall back through resolve for gap fixtures that need first-valid.
            resolve_or_panic(tz, y, mo, d, h, mi, s)
        })
}

fn resolve_or_panic(tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Tz> {
    civil::resolve_local(
        tz,
        ymd(y, mo, d),
        hms(h, mi, s),
        DstGapPolicy::FirstValidAfterGap,
        DstFoldPolicy::FirstOccurrence,
    )
    .unwrap_or_else(|| panic!("cannot resolve {y}-{mo}-{d} {h}:{mi}:{s} in {tz}"))
}

#[allow(clippy::too_many_arguments)]
fn assert_fire(got: DateTime<Tz>, tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) {
    let got = got.with_timezone(&tz);
    assert_eq!(got.year(), y, "year");
    assert_eq!(got.month(), mo, "month");
    assert_eq!(got.day(), d, "day");
    assert_eq!(got.hour(), h, "hour");
    assert_eq!(got.minute(), mi, "minute");
    assert_eq!(got.second(), s, "second");
}

/// Local-minus-UTC offset in seconds (works for chrono-tz `TzOffset`).
fn offset_secs(dt: DateTime<Tz>) -> i64 {
    dt.naive_local().and_utc().timestamp() - dt.timestamp()
}

// ---------------------------------------------------------------------------
// DST spring-forward gap — first valid after gap
// ---------------------------------------------------------------------------

/// Europe/Helsinki 2026-03-29: clocks jump 03:00 → 04:00. 03:30 does not exist.
#[test]
fn dst_gap_helsinki_daily_at_0330() {
    let mut occ = Occurrence::new(OccurrenceKind::Daily { at: hms(3, 30, 0) }, "Europe/Helsinki")
        .unwrap();
    // Just before the transition day.
    let after = local(Helsinki, 2026, 3, 28, 12, 0, 0);
    let fire = occ.next_fire(after).expect("fire");
    // First valid after the gap on the 29th is 04:00:00.
    assert_fire(fire, Helsinki, 2026, 3, 29, 4, 0, 0);
}

/// America/New_York 2026-03-08: clocks jump 02:00 → 03:00. 02:30 does not exist.
#[test]
fn dst_gap_new_york_daily_at_0230() {
    let mut occ =
        Occurrence::new(OccurrenceKind::Daily { at: hms(2, 30, 0) }, "America/New_York").unwrap();
    let after = local(New_York, 2026, 3, 7, 12, 0, 0);
    let fire = occ.next_fire(after).expect("fire");
    assert_fire(fire, New_York, 2026, 3, 8, 3, 0, 0);
}

/// Asia/Kathmandu has no DST but is a +05:45 half-hour zone — wall-clock math
/// must still land on the correct offset.
#[test]
fn half_hour_zone_kathmandu_daily() {
    let mut occ =
        Occurrence::new(OccurrenceKind::Daily { at: hms(9, 0, 0) }, "Asia/Kathmandu").unwrap();
    let after = local(Kathmandu, 2026, 6, 1, 8, 0, 0);
    let fire = occ.next_fire(after).expect("fire");
    assert_fire(fire, Kathmandu, 2026, 6, 1, 9, 0, 0);
    // Offset must be +05:45.
    assert_eq!(offset_secs(fire), 5 * 3600 + 45 * 60);
}

// ---------------------------------------------------------------------------
// DST fall-back fold — first occurrence only
// ---------------------------------------------------------------------------

/// Europe/Helsinki 2026-10-25: clocks fall 04:00 → 03:00. 03:30 occurs twice;
/// we fire once at the first (earliest / still-summer) occurrence.
#[test]
fn dst_fold_helsinki_daily_at_0330() {
    let mut occ = Occurrence::new(OccurrenceKind::Daily { at: hms(3, 30, 0) }, "Europe/Helsinki")
        .unwrap();
    let after = local(Helsinki, 2026, 10, 24, 12, 0, 0);
    let fire = occ.next_fire(after).expect("fire");
    assert_eq!(fire.date_naive(), ymd(2026, 10, 25));
    assert_eq!(fire.time(), hms(3, 30, 0));
    // First occurrence still has the summer offset (UTC+3 = 10800).
    assert_eq!(offset_secs(fire), 3 * 3600);

    // Next day is unambiguous — only one fire for the fold day.
    let next = occ.next_fire(fire).expect("next");
    assert_fire(next, Helsinki, 2026, 10, 26, 3, 30, 0);
}

/// America/New_York 2026-11-01: clocks fall 02:00 → 01:00. 01:30 occurs twice.
#[test]
fn dst_fold_new_york_daily_at_0130() {
    let mut occ =
        Occurrence::new(OccurrenceKind::Daily { at: hms(1, 30, 0) }, "America/New_York").unwrap();
    let after = local(New_York, 2026, 10, 31, 12, 0, 0);
    let fire = occ.next_fire(after).expect("fire");
    assert_eq!(fire.date_naive(), ymd(2026, 11, 1));
    assert_eq!(fire.time(), hms(1, 30, 0));
    // First occurrence still has EDT (UTC-4).
    assert_eq!(offset_secs(fire), -4 * 3600);
}

// ---------------------------------------------------------------------------
// Invalid month-day clamp
// ---------------------------------------------------------------------------

#[test]
fn monthly_day_31_clamps_to_last_day() {
    let mut occ = Occurrence::new(
        OccurrenceKind::Monthly {
            day: 31,
            at: hms(9, 0, 0),
        },
        "UTC",
    )
    .unwrap();
    // After Jan 31 → next is Feb 28 (2026 not leap) at 09:00.
    let after = Utc.with_ymd_and_hms(2026, 1, 31, 10, 0, 0).unwrap();
    let fire = occ
        .next_fire(after.with_timezone(&Tz::UTC))
        .expect("fire");
    assert_fire(fire, Tz::UTC, 2026, 2, 28, 9, 0, 0);

    // After Feb 28 → March 31.
    let fire2 = occ.next_fire(fire).expect("fire2");
    assert_fire(fire2, Tz::UTC, 2026, 3, 31, 9, 0, 0);

    // After March 31 → April 30.
    let fire3 = occ.next_fire(fire2).expect("fire3");
    assert_fire(fire3, Tz::UTC, 2026, 4, 30, 9, 0, 0);
}

#[test]
fn yearly_feb29_clamps_in_non_leap() {
    let mut occ = Occurrence::new(
        OccurrenceKind::Yearly {
            month: 2,
            day: 29,
            at: hms(12, 0, 0),
        },
        "UTC",
    )
    .unwrap();
    // From 2025-01-01: next is 2025-02-28 (non-leap clamp).
    let after = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    let fire = occ
        .next_fire(after.with_timezone(&Tz::UTC))
        .expect("fire");
    assert_fire(fire, Tz::UTC, 2025, 2, 28, 12, 0, 0);

    // From just after that: 2026-02-28 (also non-leap).
    let fire2 = occ.next_fire(fire).expect("fire2");
    assert_fire(fire2, Tz::UTC, 2026, 2, 28, 12, 0, 0);

    // 2028 is leap — keep Feb 29. Walk past 2027.
    let after_2027 = Utc.with_ymd_and_hms(2027, 3, 1, 0, 0, 0).unwrap();
    let fire_leap = occ
        .next_fire(after_2027.with_timezone(&Tz::UTC))
        .expect("leap");
    assert_fire(fire_leap, Tz::UTC, 2028, 2, 29, 12, 0, 0);
}

// ---------------------------------------------------------------------------
// Year boundary
// ---------------------------------------------------------------------------

#[test]
fn year_boundary_daily_dec31_to_jan1() {
    let mut occ = Occurrence::new(OccurrenceKind::Daily { at: hms(0, 0, 0) }, "UTC").unwrap();
    let after = Utc.with_ymd_and_hms(2026, 12, 31, 0, 0, 0).unwrap();
    let fire = occ
        .next_fire(after.with_timezone(&Tz::UTC))
        .expect("fire");
    assert_fire(fire, Tz::UTC, 2027, 1, 1, 0, 0, 0);
}

#[test]
fn year_boundary_yearly() {
    let mut occ = Occurrence::new(
        OccurrenceKind::Yearly {
            month: 12,
            day: 31,
            at: hms(23, 59, 0),
        },
        "UTC",
    )
    .unwrap();
    let after = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 0).unwrap();
    let fire = occ
        .next_fire(after.with_timezone(&Tz::UTC))
        .expect("fire");
    assert_fire(fire, Tz::UTC, 2027, 12, 31, 23, 59, 0);
}

// ---------------------------------------------------------------------------
// Interval unaffected by DST
// ---------------------------------------------------------------------------

/// Regression: occurrence index must not narrow to i32. A 1-second interval
/// from a 1970 anchor has > i32::MAX periods by 2040; next_fire must still
/// return the instant strictly after `after` (not wrap into the past).
#[test]
fn interval_index_beyond_i32_max_stays_strictly_after() {
    let anchor = Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap();
    let mut occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 1,
            anchor,
        },
        "UTC",
    )
    .unwrap();
    let after = Utc.with_ymd_and_hms(2040, 1, 1, 0, 0, 0).unwrap();
    let after_tz = after.with_timezone(&Tz::UTC);
    // Sanity: periods from anchor exceed i32::MAX.
    let periods = after.signed_duration_since(anchor).num_seconds();
    assert!(periods > i64::from(i32::MAX));

    let fire = occ.next_fire(after_tz).expect("fire");
    assert!(
        fire > after_tz,
        "next_fire must be strictly after; after={after_tz} got={fire}"
    );
    assert_eq!(fire.with_timezone(&Utc), after + chrono::Duration::seconds(1));
}

#[test]
fn interval_unaffected_by_dst_spring_forward() {
    // Anchor just before Helsinki spring-forward; period 1 hour.
    // Wall-clock would "lose" an hour; elapsed-time must keep exact 3600 s steps.
    let anchor = Helsinki
        .with_ymd_and_hms(2026, 3, 29, 1, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let mut occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 3600,
            anchor,
        },
        "Europe/Helsinki",
    )
    .unwrap();

    let after = anchor.with_timezone(&Helsinki) - chrono::Duration::seconds(1);
    let mut prev = after;
    let mut gaps = Vec::new();
    for _ in 0..5 {
        let fire = occ.next_fire(prev).expect("fire");
        let gap = fire
            .with_timezone(&Utc)
            .signed_duration_since(prev.with_timezone(&Utc));
        // First gap is ~1 s (from after = anchor-1s to first fire = anchor).
        // Subsequent gaps must be exactly 3600 s.
        gaps.push(gap.num_seconds());
        prev = fire;
    }
    assert_eq!(gaps[0], 1);
    for g in &gaps[1..] {
        assert_eq!(*g, 3600, "interval must not stretch/shrink across DST");
    }
}

// ---------------------------------------------------------------------------
// Preview next-5 for every variant
// ---------------------------------------------------------------------------

#[test]
fn preview_next5_once() {
    let at = NaiveDateTime::new(ymd(2026, 8, 1), hms(14, 30, 0));
    let occ = Occurrence::new(OccurrenceKind::Once { at }, "UTC").unwrap();
    let after = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
    let preview = occ.preview(after.with_timezone(&Tz::UTC), 5);
    assert_eq!(preview.len(), 1);
    assert_fire(preview[0], Tz::UTC, 2026, 8, 1, 14, 30, 0);
}

#[test]
fn preview_next5_interval() {
    let anchor = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 90,
            anchor,
        },
        "UTC",
    )
    .unwrap();
    let after = anchor.with_timezone(&Tz::UTC) - chrono::Duration::seconds(1);
    let preview = occ.preview(after, 5);
    assert_eq!(preview.len(), 5);
    for (i, p) in preview.iter().enumerate() {
        let expected = anchor + chrono::Duration::seconds(90 * i as i64);
        assert_eq!(p.with_timezone(&Utc), expected);
    }
}

#[test]
fn preview_next5_daily() {
    let occ = Occurrence::new(OccurrenceKind::Daily { at: hms(7, 30, 0) }, "UTC").unwrap();
    let after = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let preview = occ.preview(after.with_timezone(&Tz::UTC), 5);
    assert_eq!(preview.len(), 5);
    for (i, p) in preview.iter().enumerate() {
        assert_fire(*p, Tz::UTC, 2026, 6, 1 + i as u32, 7, 30, 0);
    }
}

#[test]
fn preview_next5_weekly() {
    let days = Weekdays::from_slice(&[chrono::Weekday::Mon, chrono::Weekday::Fri]);
    let occ = Occurrence::new(
        OccurrenceKind::Weekly {
            days,
            at: hms(7, 30, 0),
        },
        "UTC",
    )
    .unwrap();
    // 2026-06-01 is a Monday.
    let after = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let preview = occ.preview(after.with_timezone(&Tz::UTC), 5);
    assert_eq!(preview.len(), 5);
    let expected_days = [1u32, 5, 8, 12, 15]; // Mon, Fri, Mon, Fri, Mon in June 2026
    for (p, d) in preview.iter().zip(expected_days) {
        assert_fire(*p, Tz::UTC, 2026, 6, d, 7, 30, 0);
    }
}

#[test]
fn preview_next5_monthly() {
    let occ = Occurrence::new(
        OccurrenceKind::Monthly {
            day: 15,
            at: hms(9, 0, 0),
        },
        "UTC",
    )
    .unwrap();
    let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let preview = occ.preview(after.with_timezone(&Tz::UTC), 5);
    assert_eq!(preview.len(), 5);
    for (i, p) in preview.iter().enumerate() {
        assert_fire(*p, Tz::UTC, 2026, 1 + i as u32, 15, 9, 0, 0);
    }
}

#[test]
fn preview_next5_yearly() {
    let occ = Occurrence::new(
        OccurrenceKind::Yearly {
            month: 7,
            day: 4,
            at: hms(12, 0, 0),
        },
        "UTC",
    )
    .unwrap();
    let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let preview = occ.preview(after.with_timezone(&Tz::UTC), 5);
    assert_eq!(preview.len(), 5);
    for (i, p) in preview.iter().enumerate() {
        assert_fire(*p, Tz::UTC, 2026 + i as i32, 7, 4, 12, 0, 0);
    }
}

#[test]
fn preview_next5_cron() {
    // Every day at 06:00:00 (6-field with seconds).
    let occ = Occurrence::new(
        OccurrenceKind::Cron {
            expr: "0 0 6 * * *".into(),
        },
        "UTC",
    )
    .unwrap();
    let after = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
    let preview = occ.preview(after.with_timezone(&Tz::UTC), 5);
    assert_eq!(preview.len(), 5);
    for (i, p) in preview.iter().enumerate() {
        assert_fire(*p, Tz::UTC, 2026, 3, 1 + i as u32, 6, 0, 0);
    }
}

// ---------------------------------------------------------------------------
// Exclusion dates
// ---------------------------------------------------------------------------

/// Supervisor finding: a 1-second interval with a full excluded day must not
/// exhaust the candidate budget — next fire is the first tick on the next day.
#[test]
fn exclusion_full_day_on_one_second_interval() {
    let anchor = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let mut occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 1,
            anchor,
        },
        "UTC",
    )
    .unwrap();
    occ.exclude_date(ymd(2026, 6, 1));

    let after = anchor - chrono::Duration::seconds(1);
    let fire = occ
        .next_fire(after.with_timezone(&Tz::UTC))
        .expect("fire after excluded day");
    assert_fire(fire, Tz::UTC, 2026, 6, 2, 0, 0, 0);
}

/// Supervisor finding: skip_next on a Once must consume the skip even when the
/// only candidate is skipped and next_fire returns None.
#[test]
fn skip_next_on_once_consumes_pending_when_exhausted() {
    let at = NaiveDateTime::new(ymd(2026, 8, 1), hms(14, 30, 0));
    let mut occ = Occurrence::new(OccurrenceKind::Once { at }, "UTC").unwrap();
    let after = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();

    occ.skip_next();
    assert_eq!(occ.pending_skips(), 1);
    assert!(occ.next_fire(after.with_timezone(&Tz::UTC)).is_none());
    assert_eq!(
        occ.pending_skips(),
        0,
        "skip_next must consume exactly one occurrence even when nothing remains"
    );
}

#[test]
fn exclusion_skipped_by_next_fire_and_preview() {
    let mut occ = Occurrence::new(OccurrenceKind::Daily { at: hms(10, 0, 0) }, "UTC").unwrap();
    occ.exclude_date(ymd(2026, 6, 2));
    occ.exclude_date(ymd(2026, 6, 4));

    let after = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let mut working = occ.clone();
    let f1 = working
        .next_fire(after.with_timezone(&Tz::UTC))
        .expect("f1");
    assert_fire(f1, Tz::UTC, 2026, 6, 1, 10, 0, 0);
    let f2 = working.next_fire(f1).expect("f2");
    // 6/2 excluded → 6/3
    assert_fire(f2, Tz::UTC, 2026, 6, 3, 10, 0, 0);
    let f3 = working.next_fire(f2).expect("f3");
    // 6/4 excluded → 6/5
    assert_fire(f3, Tz::UTC, 2026, 6, 5, 10, 0, 0);

    let preview = occ.preview(after.with_timezone(&Tz::UTC), 4);
    assert_eq!(preview.len(), 4);
    assert_fire(preview[0], Tz::UTC, 2026, 6, 1, 10, 0, 0);
    assert_fire(preview[1], Tz::UTC, 2026, 6, 3, 10, 0, 0);
    assert_fire(preview[2], Tz::UTC, 2026, 6, 5, 10, 0, 0);
    assert_fire(preview[3], Tz::UTC, 2026, 6, 6, 10, 0, 0);
}

// ---------------------------------------------------------------------------
// Skip-next
// ---------------------------------------------------------------------------

#[test]
fn skip_next_skips_exactly_one_occurrence() {
    let mut occ = Occurrence::new(OccurrenceKind::Daily { at: hms(8, 0, 0) }, "UTC").unwrap();
    let after = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

    // Without skip: June 1.
    let peek = occ
        .peek_next_fire(after.with_timezone(&Tz::UTC))
        .expect("peek");
    assert_fire(peek, Tz::UTC, 2026, 6, 1, 8, 0, 0);

    occ.skip_next();
    let fire = occ
        .next_fire(after.with_timezone(&Tz::UTC))
        .expect("fire");
    // Skipped June 1 → June 2.
    assert_fire(fire, Tz::UTC, 2026, 6, 2, 8, 0, 0);
    // pending_skips consumed.
    assert_eq!(occ.pending_skips(), 0);

    // Subsequent call is normal next (June 3).
    let fire2 = occ.next_fire(fire).expect("fire2");
    assert_fire(fire2, Tz::UTC, 2026, 6, 3, 8, 0, 0);
}

// ---------------------------------------------------------------------------
// Validity window + max_runs
// ---------------------------------------------------------------------------

#[test]
fn validity_window_and_max_runs() {
    let mut occ = Occurrence::new(OccurrenceKind::Daily { at: hms(12, 0, 0) }, "UTC")
        .unwrap()
        .with_valid_from(Utc.with_ymd_and_hms(2026, 6, 3, 0, 0, 0).unwrap())
        .with_valid_until(Utc.with_ymd_and_hms(2026, 6, 6, 0, 0, 0).unwrap())
        .with_max_runs(2);

    let after = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let f1 = occ
        .next_fire(after.with_timezone(&Tz::UTC))
        .expect("f1");
    assert_fire(f1, Tz::UTC, 2026, 6, 3, 12, 0, 0);
    occ.record_run();

    let f2 = occ.next_fire(f1).expect("f2");
    assert_fire(f2, Tz::UTC, 2026, 6, 4, 12, 0, 0);
    occ.record_run();

    // max_runs=2 exhausted.
    assert!(occ.next_fire(f2).is_none());
}

#[test]
fn once_in_the_past_returns_none() {
    let at = NaiveDateTime::new(ymd(2020, 1, 1), hms(0, 0, 0));
    let mut occ = Occurrence::new(OccurrenceKind::Once { at }, "UTC").unwrap();
    let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    assert!(occ.next_fire(after.with_timezone(&Tz::UTC)).is_none());
}

#[test]
fn iter_after_matches_preview() {
    let occ = Occurrence::new(OccurrenceKind::Daily { at: hms(1, 2, 3) }, "UTC").unwrap();
    let after = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
    let via_preview = occ.preview(after.with_timezone(&Tz::UTC), 5);
    let via_iter: Vec<_> = occ.iter_after(after.with_timezone(&Tz::UTC)).take(5).collect();
    assert_eq!(via_preview, via_iter);
}
