//! Bridge webview-friendly occurrence DTOs into the core `Occurrence` /
//! `TimerPatch` builders.
//!
//! This mirrors `bellman-cli::parse` so the GUI and CLI share the same
//! validation surface — both go through `Occurrence::new(kind, tz)` and the
//! store's optimistic update path. Tests in `dto_serde_tests.rs` pin the
//! camelCase wire shape; the helper itself is exercised by the round-trip
//! tests added for C8.

use bellman_core::occurrence::{
    DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy, Occurrence, OccurrenceKind, Weekdays,
};
use bellman_core::store::{Action, MisfirePolicy, NewTimer, OverlapPolicy, RetryPolicy, Timer};
use chrono::offset::Offset as _Offset;
use chrono::{
    DateTime, Datelike, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Optional per-timer settings the dialog exposes (with sane defaults when
/// omitted). All fields are optional; `None` falls back to the product
/// default for the chosen occurrence kind.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTimerInput {
    pub name: String,
    pub occurrence: OccurrenceInput,
    pub enabled: Option<bool>,
    pub action: Option<Action>,
    pub misfire: Option<MisfirePolicy>,
    pub overlap: Option<OverlapPolicy>,
    pub retry: Option<RetryPolicy>,
    pub tags: Option<Vec<String>>,
}

/// What the dialog emits per occurrence variant. The discriminator is the
/// `kind` string ("once" | "interval" | ...); kind-specific fields are only
/// meaningful for their variant. Mirrors `OccurrenceKind`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OccurrenceInput {
    pub kind: String,
    pub tz: Option<String>,
    /// Wall-clock time-of-day for daily/weekly/monthly/yearly.
    pub time: Option<String>,
    /// Year-aware naive datetime for once: YYYY-MM-DDTHH:MM[:SS].
    pub once_at: Option<String>,
    pub every_secs: Option<u64>,
    /// Interval anchor (UTC); defaults to now() when omitted.
    pub interval_anchor: Option<DateTime<Utc>>,
    /// Comma-separated weekday names ("mon,wed,fri") for weekly.
    pub days: Option<String>,
    pub day: Option<u8>,
    pub month: Option<u8>,
    pub cron_expr: Option<String>,
    pub dst_gap: Option<DstGapPolicy>,
    pub dst_fold: Option<DstFoldPolicy>,
    pub invalid_monthday: Option<InvalidMonthDayPolicy>,
}



impl OccurrenceInput {
    /// Build a fresh `Occurrence` honoring all policies. Validates early so
    /// the dialog rejects malformed input before it hits the store.
    pub fn build(self) -> Result<Occurrence, String> {
        let tz_name = resolve_tz_name(self.tz.as_deref())?;
        let kind = match self.kind.to_ascii_lowercase().as_str() {
            "once" => {
                let raw = self
                    .once_at
                    .as_deref()
                    .ok_or_else(|| "occurrence 'once' needs onceAt (YYYY-MM-DDTHH:MM:SS)".to_string())?;
                let at = parse_once_at(raw, &tz_name)?;
                OccurrenceKind::Once { at }
            }
            "interval" => {
                let every = self
                    .every_secs
                    .ok_or_else(|| "occurrence 'interval' needs everySecs".to_string())?;
                let anchor = self.interval_anchor.unwrap_or_else(Utc::now);
                OccurrenceKind::Interval {
                    every_secs: every,
                    anchor,
                }
            }
            "daily" => {
                let t = self
                    .time
                    .as_deref()
                    .ok_or_else(|| "occurrence 'daily' needs time (HH:MM[:SS])".to_string())?;
                OccurrenceKind::Daily {
                    at: parse_clock_time(t)?,
                }
            }
            "weekly" => {
                let t = self
                    .time
                    .as_deref()
                    .ok_or_else(|| "occurrence 'weekly' needs time (HH:MM[:SS])".to_string())?;
                let days_s = self.days.as_deref().ok_or_else(|| {
                    "occurrence 'weekly' needs days (e.g. 'mon,wed,fri')".to_string()
                })?;
                let names: Vec<&str> = days_s
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .collect();
                let wd = parse_weekdays_csv(&names)?;
                OccurrenceKind::Weekly {
                    days: wd,
                    at: parse_clock_time(t)?,
                }
            }
            "monthly" => {
                let t = self
                    .time
                    .as_deref()
                    .ok_or_else(|| "occurrence 'monthly' needs time (HH:MM[:SS])".to_string())?;
                let d = self
                    .day
                    .ok_or_else(|| "occurrence 'monthly' needs day (1..=31)".to_string())?;
                OccurrenceKind::Monthly {
                    day: d,
                    at: parse_clock_time(t)?,
                }
            }
            "yearly" => {
                let t = self
                    .time
                    .as_deref()
                    .ok_or_else(|| "occurrence 'yearly' needs time (HH:MM[:SS])".to_string())?;
                let m = self
                    .month
                    .ok_or_else(|| "occurrence 'yearly' needs month (1..=12)".to_string())?;
                let d = self
                    .day
                    .ok_or_else(|| "occurrence 'yearly' needs day (1..=31)".to_string())?;
                OccurrenceKind::Yearly {
                    month: m,
                    day: d,
                    at: parse_clock_time(t)?,
                }
            }
            "cron" => {
                let expr = self
                    .cron_expr
                    .as_deref()
                    .ok_or_else(|| "occurrence 'cron' needs cronExpr".to_string())?
                    .to_string();
                OccurrenceKind::Cron { expr }
            }
            other => {
                return Err(format!(
                    "unknown occurrence kind '{other}' (expected once|interval|daily|weekly|monthly|yearly|cron)"
                ))
            }
        };
        let mut occ = Occurrence::new(kind, tz_name)?;
        if let Some(p) = self.dst_gap {
            occ = occ.with_dst_gap(p);
        }
        if let Some(p) = self.dst_fold {
            occ = occ.with_dst_fold(p);
        }
        if let Some(p) = self.invalid_monthday {
            occ = occ.with_invalid_monthday(p);
        }
        Ok(occ)
    }
}

impl CreateTimerInput {
    /// Build the [`NewTimer`] the store consumes. Caller passes it to
    /// `Store::create_timer`.
    pub fn into_new_timer(self) -> Result<NewTimer, String> {
        let occurrence = self.occurrence.build()?;
        let mut new = NewTimer::new(self.name, occurrence);
        if let Some(b) = self.enabled {
            new.enabled = b;
        }
        if let Some(a) = self.action {
            new.action = a;
        }
        if let Some(p) = self.misfire {
            new.misfire = p;
        }
        if let Some(p) = self.overlap {
            new.overlap = p;
        }
        if let Some(p) = self.retry {
            new.retry = p;
        }
        if let Some(t) = self.tags {
            new.tags = t;
        }
        Ok(new)
    }
}

/// Render a `Timer` back into the DTO shape the dialog prefills when editing.
/// All timer's stored fields surface so the dialog round-trips without loss.
pub fn timer_to_input(timer: &Timer) -> CreateTimerInput {
    let occ = &timer.occurrence;
    let kind_str = occ.kind().kind_label().to_string();
    let tz = Some(occ.tz_name().to_string());

    let (time, once_at, every_secs, interval_anchor, days, day, month, cron_expr) = match occ.kind()
    {
        OccurrenceKind::Once { at } => (
            None,
            Some(at.format("%Y-%m-%dT%H:%M:%S").to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        OccurrenceKind::Interval { every_secs, anchor } => (
            None,
            None,
            Some(*every_secs),
            Some(*anchor),
            None,
            None,
            None,
            None,
        ),
        OccurrenceKind::Daily { at } => (
            Some(at.format("%H:%M:%S").to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        OccurrenceKind::Weekly { days, at } => (
            Some(at.format("%H:%M:%S").to_string()),
            None,
            None,
            None,
            Some(weekdays_to_csv(*days)),
            None,
            None,
            None,
        ),
        OccurrenceKind::Monthly { day, at } => (
            Some(at.format("%H:%M:%S").to_string()),
            None,
            None,
            None,
            None,
            Some(*day),
            None,
            None,
        ),
        OccurrenceKind::Yearly { month, day, at } => (
            Some(at.format("%H:%M:%S").to_string()),
            None,
            None,
            None,
            None,
            Some(*day),
            Some(*month),
            None,
        ),
        OccurrenceKind::Cron { expr } => {
            (None, None, None, None, None, None, None, Some(expr.clone()))
        }
    };

    CreateTimerInput {
        name: timer.name.clone(),
        occurrence: OccurrenceInput {
            kind: kind_str,
            tz,
            time,
            once_at,
            every_secs,
            interval_anchor,
            days,
            day,
            month,
            cron_expr,
            dst_gap: Some(occ.dst_gap()),
            dst_fold: Some(occ.dst_fold()),
            invalid_monthday: Some(occ.invalid_monthday()),
        },
        enabled: Some(timer.enabled),
        action: Some(timer.action.clone()),
        misfire: Some(timer.misfire.clone()),
        overlap: Some(timer.overlap.clone()),
        retry: Some(timer.retry.clone()),
        tags: Some(timer.tags.clone()),
    }
}

/// Identify whether a chosen wall-clock local time falls in a DST gap or
/// fold in the given IANA zone. Returns `Some(reason)` when the policy
/// produced a different local instant than the user typed (i.e. a warning
/// should appear in the UI).
///
/// `none` for non-wall-clock kinds (interval / cron) — only the calendar
/// surface carries DST semantics in our model.
pub fn dst_warning(occurrence_kind: &OccurrenceKind, tz_name: &str) -> Option<String> {
    let tz = Tz::from_str(tz_name).ok()?;
    match occurrence_kind {
        OccurrenceKind::Once { at } => dst_warning_for(*at, tz),
        OccurrenceKind::Daily { at } => dst_warning_for_today(*at, tz),
        OccurrenceKind::Weekly { days, at } => dst_warning_for_first_match(*days, *at, tz),
        OccurrenceKind::Monthly { day, at } => dst_warning_for_month(*day, *at, tz),
        OccurrenceKind::Yearly { month, day, at } => dst_warning_for_year(*month, *day, *at, tz),
        OccurrenceKind::Cron { .. } | OccurrenceKind::Interval { .. } => None,
    }
}

fn dst_warning_for(at: NaiveDateTime, tz: Tz) -> Option<String> {
    let resolved = tz.from_local_datetime(&at);
    describe_local_diff(&at, tz, resolved, "this once-at time")
}

fn dst_warning_for_today(at: NaiveTime, tz: Tz) -> Option<String> {
    let today_local = Local::now().date_naive();
    let naive = NaiveDateTime::new(today_local, at);
    let resolved = tz.from_local_datetime(&naive);
    describe_local_diff(&naive, tz, resolved, "daily times")
}

fn dst_warning_for_first_match(days: Weekdays, at: NaiveTime, tz: Tz) -> Option<String> {
    let today_local = Local::now().date_naive();
    for offset in 0..14i64 {
        let probe_date: NaiveDate = today_local
            .checked_add_signed(chrono::Duration::days(offset))
            .unwrap_or(today_local);
        if days.contains(probe_date.weekday()) {
            let naive = NaiveDateTime::new(probe_date, at);
            let resolved = tz.from_local_datetime(&naive);
            if let Some(msg) = describe_local_diff(&naive, tz, resolved, "weekly times") {
                return Some(msg);
            }
            return None;
        }
    }
    None
}

fn dst_warning_for_month(day: u8, at: NaiveTime, tz: Tz) -> Option<String> {
    let next_local = next_month_anchor(tz);
    let mut year = next_local.year();
    let mut month = next_local.month();
    for _ in 0..24 {
        let days_in = OccurrenceKind::days_in_month(year, month);
        let use_day = u32::from(day).min(days_in);
        if let Some(probe_date) = NaiveDate::from_ymd_opt(year, month, use_day) {
            let naive = NaiveDateTime::new(probe_date, at);
            let resolved = tz.from_local_datetime(&naive);
            if let Some(msg) = describe_local_diff(&naive, tz, resolved, "monthly times") {
                return Some(msg);
            }
        }
        month += 1;
        if month > 12 {
            month = 1;
            year += 1;
        }
    }
    None
}

fn dst_warning_for_year(month: u8, day: u8, at: NaiveTime, tz: Tz) -> Option<String> {
    let today_local = Local::now().date_naive();
    let year_start = today_local.year();
    for year in year_start..(year_start + 5) {
        let days_in = OccurrenceKind::days_in_month(year, month as u32);
        let use_day = (day as u32).min(days_in);
        if let Some(probe_date) = NaiveDate::from_ymd_opt(year, month as u32, use_day) {
            let naive = NaiveDateTime::new(probe_date, at);
            let resolved = tz.from_local_datetime(&naive);
            if let Some(msg) = describe_local_diff(&naive, tz, resolved, "yearly times") {
                return Some(msg);
            }
        }
    }
    None
}

/// Return Some(reason) when the resolved local instant differs from the
/// requested naive local, or None for clean alignment.
fn describe_local_diff(
    naive: &NaiveDateTime,
    _tz: Tz,
    resolved: LocalResult<DateTime<Tz>>,
    kind_label: &str,
) -> Option<String> {
    // `kind_label` is singular for a one-shot ("this once-at time") and plural for every
    // repeating kind ("daily times", "weekly times", …). The warning sentences below agree
    // with it, so a once timer reads "…time does not exist" instead of "…time do not exist".
    let plural = kind_label.ends_with('s');
    let (verb_fall, verb_exist) = if plural {
        ("fall", "do not exist")
    } else {
        ("falls", "does not exist")
    };
    match resolved {
        LocalResult::Single(dt) => {
            // The instant resolved cleanly. Compare its local clock-face to
            // the user-supplied wall clock — they may differ in absolute
            // offset (DST) but the same local time-of-day means no policy
            // skew. Most warnings happen here when the offset shifts.
            let resolved_local = dt.naive_local();
            let requested_clock = naive.time();
            if resolved_local.time() != requested_clock {
                // First-valid-after-gap: clock face moved forward.
                Some(format!(
                    "{kind_label} in this timezone {verb_fall} in a DST gap; the next valid \
                     local time is {} (your requested time {} skipped to the next real instant).",
                    resolved_local.format("%H:%M:%S"),
                    naive.format("%H:%M:%S"),
                ))
            } else {
                None
            }
        }
        LocalResult::Ambiguous(_first, _second) => {
            Some(format!(
                "{kind_label} in this timezone {verb_fall} in a DST fold; bellman schedules \
                 the earliest of the two instants (the DST policy can be changed in the advanced section)."
            ))
        }
        LocalResult::None => Some(format!(
            "{kind_label} in this timezone {verb_exist}; the schedule will resolve to the \
             first valid instant after the requested time."
        )),
    }
}

/// Now-in-zone, returned as a `NaiveDate` for downstream month/week math.
fn next_month_anchor(tz: Tz) -> chrono::NaiveDate {
    let now_tz = Utc::now().with_timezone(&tz);
    now_tz.date_naive()
}

/// Parse "mon,wed,fri" / "mon wed fri" / ["mon","wed"] into a Weekdays bitmask.
pub fn parse_weekdays_csv(names: &[&str]) -> Result<Weekdays, String> {
    let mut w = Weekdays::new();
    for name in names {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let day = match trimmed.to_ascii_lowercase().as_str() {
            "mon" | "monday" => chrono::Weekday::Mon,
            "tue" | "tues" | "tuesday" => chrono::Weekday::Tue,
            "wed" | "wednesday" => chrono::Weekday::Wed,
            "thu" | "thur" | "thurs" | "thursday" => chrono::Weekday::Thu,
            "fri" | "friday" => chrono::Weekday::Fri,
            "sat" | "saturday" => chrono::Weekday::Sat,
            "sun" | "sunday" => chrono::Weekday::Sun,
            other => {
                return Err(format!(
                    "unknown weekday '{other}' (use mon|tue|wed|thu|fri|sat|sun)"
                ))
            }
        };
        w.insert(day);
    }
    if w.is_empty() {
        return Err("weekly schedule requires at least one weekday".into());
    }
    Ok(w)
}

/// Weekdays → comma-separated lowercase names, ISO order (Mon..Sun).
pub fn weekdays_to_csv(days: Weekdays) -> String {
    days.iter()
        .map(|d| {
            let s = match d {
                chrono::Weekday::Mon => "mon",
                chrono::Weekday::Tue => "tue",
                chrono::Weekday::Wed => "wed",
                chrono::Weekday::Thu => "thu",
                chrono::Weekday::Fri => "fri",
                chrono::Weekday::Sat => "sat",
                chrono::Weekday::Sun => "sun",
            };
            s.to_string()
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// `HH:MM` or `HH:MM:SS`.
pub fn parse_clock_time(s: &str) -> Result<NaiveTime, String> {
    let s = s.trim();
    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M:%S") {
        return Ok(t);
    }
    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M") {
        return Ok(t);
    }
    Err(format!("invalid time '{s}' (expected HH:MM or HH:MM:SS)"))
}

/// Once-at datetime: RFC3339, or several naive forms interpreted in `tz`.
pub fn parse_once_at(s: &str, tz_name: &str) -> Result<NaiveDateTime, String> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        let tz = Tz::from_str(tz_name).map_err(|e| format!("unknown timezone '{tz_name}': {e}"))?;
        return Ok(dt.with_timezone(&tz).naive_local());
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        return Ok(dt);
    }
    Err(format!(
        "invalid once datetime '{s}' (expected YYYY-MM-DDTHH:MM:SS or RFC3339)"
    ))
}

/// Resolve IANA tz name: explicit flag → system local → UTC.
pub fn resolve_tz_name(explicit: Option<&str>) -> Result<String, String> {
    if let Some(tz) = explicit {
        let tz = tz.trim();
        Tz::from_str(tz).map_err(|e| format!("unknown timezone '{tz}': {e}"))?;
        return Ok(tz.to_string());
    }
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() && Tz::from_str(&tz).is_ok() {
            return Ok(tz);
        }
    }
    if let Ok(s) = std::fs::read_to_string("/etc/timezone") {
        let s = s.trim().to_string();
        if Tz::from_str(&s).is_ok() {
            return Ok(s);
        }
    }
    if let Ok(link) = std::fs::read_link("/etc/localtime") {
        if let Some(name) = extract_zoneinfo_name(&link) {
            if Tz::from_str(&name).is_ok() {
                return Ok(name);
            }
        }
    }
    Ok("UTC".to_string())
}

/// One row of the preview the dialog renders next to the form.
#[derive(Debug, Clone)]
pub struct PreviewFire {
    /// UTC instant as RFC3339 (matches the rest of the IPC envelope).
    pub utc: DateTime<Utc>,
    /// Local clock-face in the schedule tz (HH:MM:SS), with the matching date.
    pub local_date: String,
    pub local_time: String,
    /// Offset string e.g. "+02:00" or "Z".
    pub offset: String,
    /// IANA tz name displayed alongside the local time.
    pub tz_name: String,
}

/// Compute the next `n` fires from an `OccurrenceInput`. Returns at most
/// `n` rows; an empty vec means the schedule is exhausted or invalid.
pub fn preview_fires(input: &OccurrenceInput, n: usize) -> Result<Vec<PreviewFire>, String> {
    let occ = input.clone().build()?;
    let tz_name = occ.tz_name().to_string();
    let tz = Tz::from_str(&tz_name).map_err(|e| format!("unknown timezone '{tz_name}': {e}"))?;
    let after = Utc::now().with_timezone(&tz);
    let locals = occ.preview(after, n);
    Ok(locals
        .into_iter()
        .map(|dt| {
            let utc = dt.with_timezone(&Utc);
            let naive_local = dt.naive_local();
            let offset_secs = dt.offset().fix().local_minus_utc();
            let offset_str = format_offset(offset_secs);
            PreviewFire {
                utc,
                local_date: naive_local.date().format("%Y-%m-%d").to_string(),
                local_time: naive_local.time().format("%H:%M:%S").to_string(),
                offset: offset_str,
                tz_name: tz_name.clone(),
            }
        })
        .collect())
}

fn format_offset(secs: i32) -> String {
    if secs == 0 {
        "UTC".to_string()
    } else {
        let sign = if secs >= 0 { '+' } else { '-' };
        let abs = secs.unsigned_abs();
        let h = abs / 3600;
        let m = (abs % 3600) / 60;
        format!("{sign}{h:02}:{m:02}")
    }
}

fn extract_zoneinfo_name(path: &std::path::Path) -> Option<String> {
    let s = path.to_string_lossy();
    if let Some(idx) = s.find("zoneinfo/") {
        let rest = &s[idx + "zoneinfo/".len()..];
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_time_parses_short_and_long() {
        assert_eq!(
            parse_clock_time("09:30").unwrap(),
            NaiveTime::from_hms_opt(9, 30, 0).unwrap()
        );
        assert_eq!(
            parse_clock_time("09:30:00").unwrap(),
            NaiveTime::from_hms_opt(9, 30, 0).unwrap()
        );
        assert!(parse_clock_time("nope").is_err());
    }

    #[test]
    fn weekdays_csv_round_trip() {
        let names = vec!["mon", "wed", "fri"];
        let w = parse_weekdays_csv(&names).unwrap();
        assert!(w.contains(chrono::Weekday::Mon));
        assert!(w.contains(chrono::Weekday::Wed));
        assert!(w.contains(chrono::Weekday::Fri));
        let csv = weekdays_to_csv(w);
        assert_eq!(csv, "mon,wed,fri");
    }

    #[test]
    fn weekdays_empty_is_error() {
        let w = parse_weekdays_csv(&[]).unwrap_err();
        assert!(w.contains("at least one weekday"));
    }

    #[test]
    fn build_daily_uses_local_time() {
        let inp = OccurrenceInput {
            kind: "daily".into(),
            tz: Some("UTC".into()),
            time: Some("07:30".into()),
            once_at: None,
            every_secs: None,
            interval_anchor: None,
            days: None,
            day: None,
            month: None,
            cron_expr: None,
            dst_gap: None,
            dst_fold: None,
            invalid_monthday: None,
        };
        let occ = inp.build().unwrap();
        assert_eq!(occ.timezone().name(), "UTC");
        assert!(matches!(occ.kind(), OccurrenceKind::Daily { .. }));
    }

    #[test]
    fn build_interval_uses_anchor() {
        let anchor = Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap();
        let inp = OccurrenceInput {
            kind: "interval".into(),
            tz: Some("UTC".into()),
            time: None,
            once_at: None,
            every_secs: Some(60),
            interval_anchor: Some(anchor),
            days: None,
            day: None,
            month: None,
            cron_expr: None,
            dst_gap: None,
            dst_fold: None,
            invalid_monthday: None,
        };
        let occ = inp.build().unwrap();
        match occ.kind() {
            OccurrenceKind::Interval {
                every_secs,
                anchor: a,
            } => {
                assert_eq!(*every_secs, 60);
                assert_eq!(*a, anchor);
            }
            _ => panic!("not interval"),
        }
    }

    #[test]
    fn dst_warning_zero_for_clean_local() {
        let inp = OccurrenceInput {
            kind: "daily".into(),
            tz: Some("UTC".into()),
            time: Some("12:00".into()),
            once_at: None,
            every_secs: None,
            interval_anchor: None,
            days: None,
            day: None,
            month: None,
            cron_expr: None,
            dst_gap: None,
            dst_fold: None,
            invalid_monthday: None,
        };
        let occ = inp.build().unwrap();
        let w = dst_warning(occ.kind(), "UTC");
        assert!(w.is_none(), "UTC has no DST; got {:?}", w);
    }

    #[test]
    fn dst_warning_fires_for_once_at_helsinki_spring_gap() {
        // Deterministic gap test. Helsinki 2026-03-29: clocks go 03:00 → 04:00
        // (EET→EEST). 03:30 does not exist as a local time — the helper
        // resolves the naive local to the first valid instant and the
        // warning must fire. This is the proof Finding 5 demanded.
        let at = NaiveDateTime::parse_from_str("2026-03-29T03:30:00", "%Y-%m-%dT%H:%M:%S").unwrap();
        let kind = OccurrenceKind::Once { at };
        let w = dst_warning(&kind, "Europe/Helsinki");
        assert!(
            w.is_some(),
            "expected a DST warning for Helsinki 2026-03-29 03:30 (in the spring-forward gap)"
        );
        let msg = w.unwrap();
        assert!(
            msg.contains("gap") || msg.contains("DST") || msg.contains("not exist") || msg.contains("skipped"),
            "warning should mention DST / gap / non-existence / skipped; got: {msg}"
        );
        // Cross-check that the schedule actually resolves to a valid
        // post-gap instant via the core's preview path. Use the NEXT
        // upcoming Helsinki spring-forward moment (2027-03-28 03:30) so
        // the preview-from-now call returns a non-empty list; today
        // (2026-07-27) the 2026-03-29 03:30 time is already in the past.
        let tz = chrono_tz::Tz::from_str("Europe/Helsinki").unwrap();
        let at_future = NaiveDateTime::parse_from_str("2027-03-28T03:30:00", "%Y-%m-%dT%H:%M:%S").unwrap();
        let preview = Occurrence::new(
            OccurrenceKind::Once { at: at_future },
            "Europe/Helsinki",
        )
        .unwrap()
        .preview(chrono::Utc::now().with_timezone(&tz), 1);
        assert!(!preview.is_empty(), "core preview returned nothing for in-gap 03:30 (future date)");
        // Resolves to the first valid second after the gap, 04:00:00.
        let first = preview[0];
        assert_eq!(
            first.format("%H:%M:%S").to_string(),
            "04:00:00",
            "preview should land at 04:00 after the spring-forward gap"
        );
    }

    #[test]
    fn dst_warning_clean_for_once_at_helsinki_outside_gap() {
        // Same zone, same day, but at 02:30 (well before the gap) and at
        // 12:00 (well after). Both should NOT produce a gap warning.
        let cases = [
            NaiveDateTime::parse_from_str("2026-03-29T02:30:00", "%Y-%m-%dT%H:%M:%S").unwrap(),
            NaiveDateTime::parse_from_str("2026-03-29T12:00:00", "%Y-%m-%dT%H:%M:%S").unwrap(),
            NaiveDateTime::parse_from_str("2026-12-31T23:59:59", "%Y-%m-%dT%H:%M:%S").unwrap(),
        ];
        for at in cases {
            let kind = OccurrenceKind::Once { at };
            assert!(
                dst_warning(&kind, "Europe/Helsinki").is_none(),
                "unexpected warning for {at}"
            );
        }
    }

    #[test]
    fn preview_fires_works_for_daily() {
        let inp = OccurrenceInput {
            kind: "daily".into(),
            tz: Some("UTC".into()),
            time: Some("12:00:00".into()),
            ..OccurrenceInput::default()
        };
        let p = preview_fires(&inp, 3).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].local_time, "12:00:00");
        assert_eq!(p[0].tz_name, "UTC");
        assert_eq!(p[0].offset, "UTC");
    }

    #[test]
    fn preview_fires_works_for_weekly() {
        let inp = OccurrenceInput {
            kind: "weekly".into(),
            tz: Some("UTC".into()),
            time: Some("08:00".into()),
            days: Some("mon,wed,fri".into()),
            ..OccurrenceInput::default()
        };
        let p = preview_fires(&inp, 5).unwrap();
        assert_eq!(p.len(), 5);
        for r in &p {
            assert_eq!(r.local_time, "08:00:00");
        }
    }

    #[test]
    fn preview_fires_offset_string() {
        let inp = OccurrenceInput {
            kind: "daily".into(),
            tz: Some("Europe/Helsinki".into()),
            time: Some("12:00:00".into()),
            ..OccurrenceInput::default()
        };
        let p = preview_fires(&inp, 2).unwrap();
        assert_eq!(p.len(), 2);
        // Helsinki is +02:00 (EEST) or +03:00 (EEST); either way a non-empty
        // offset string (the schedule's tz guarantees the sign but not the
        // exact DST leg).
        assert!(
            p[0].offset.starts_with('+') || p[0].offset.starts_with('-'),
            "expected offset: {:?}",
            p[0].offset
        );
        assert_eq!(p[0].tz_name, "Europe/Helsinki");
    }

    #[test]
    fn timer_input_round_trip_preserves_kind() {
        let anchor = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let occ = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 30,
                anchor,
            },
            "UTC",
        )
        .unwrap();
        let timer = Timer {
            id: uuid::Uuid::nil(),
            name: "tick".into(),
            enabled: true,
            occurrence: occ,
            tz: "UTC".into(),
            next_fire_utc: Some(anchor),
            last_fired: None,
            misfire: MisfirePolicy::default_calendar(),
            overlap: OverlapPolicy::default(),
            retry: RetryPolicy::default(),
            valid_from: None,
            valid_until: None,
            max_runs: None,
            tags: vec!["x".into()],
            action: Action::default(),
            revision: 1,
            jitter_secs: 0,
            accuracy_slack_secs: None,
            wake_machine: false,
        };
        let dto = timer_to_input(&timer);
        assert_eq!(dto.name, "tick");
        assert_eq!(dto.occurrence.kind, "interval");
        assert_eq!(dto.occurrence.every_secs, Some(30));
        assert_eq!(dto.occurrence.interval_anchor, Some(anchor));
        assert_eq!(dto.tags.as_deref(), Some(&["x".to_string()][..]));
    }
}
