//! Resolve month / range / relative phrases into concrete dates.
//!
//! **Boundary:** Bellman accepts structured flags plus a small fixed set of
//! relative phrases (`today`, `tomorrow`, `this month`, `next month`,
//! `next <weekday>`, bare month name, `YYYY-MM`). Anything richer is the
//! calling agent's job — this is intentionally not a natural-language engine.

use super::types::{MONTH_ABBREV, MONTH_NAMES};
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use chrono_tz::Tz;
use std::str::FromStr;

/// Resolve `--month` value into (year, month).
///
/// Accepts:
/// - `YYYY-MM` / `YYYY-M`
/// - `this` / `this month`
/// - `next` / `next month`
/// - bare English month name (`august`, `Aug`) — calendar year of `now` in `tz`
pub fn resolve_month(spec: &str, now_local: NaiveDate) -> Result<(i32, u32), String> {
    let s = spec.trim();
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "this" | "this month" | "current" => Ok((now_local.year(), now_local.month())),
        "next" | "next month" => {
            let (y, m) = if now_local.month() == 12 {
                (now_local.year() + 1, 1)
            } else {
                (now_local.year(), now_local.month() + 1)
            };
            Ok((y, m))
        }
        _ => {
            if let Some((y, m)) = parse_year_month(s) {
                return Ok((y, m));
            }
            if let Some(m) = parse_month_name(s) {
                return Ok((now_local.year(), m));
            }
            Err(format!(
                "unrecognized month '{spec}' (expected YYYY-MM, this|next, or a month name)"
            ))
        }
    }
}

fn parse_year_month(s: &str) -> Option<(i32, u32)> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 2 {
        return None;
    }
    let y: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    if !(1..=12).contains(&m) {
        return None;
    }
    // Validate y/m form a real month.
    NaiveDate::from_ymd_opt(y, m, 1)?;
    Some((y, m))
}

fn parse_month_name(s: &str) -> Option<u32> {
    let lower = s.trim().to_ascii_lowercase();
    for (i, name) in MONTH_NAMES.iter().enumerate() {
        if lower == name.to_ascii_lowercase() {
            return Some((i + 1) as u32);
        }
    }
    for (i, name) in MONTH_ABBREV.iter().enumerate() {
        if lower == name.to_ascii_lowercase() {
            return Some((i + 1) as u32);
        }
    }
    None
}

/// First and last day of a calendar month.
pub fn month_bounds(year: i32, month: u32) -> Result<(NaiveDate, NaiveDate), String> {
    let start = NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| format!("invalid month {year}-{month:02}"))?;
    let end = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .ok_or_else(|| format!("invalid next month after {year}-{month:02}"))?
    .pred_opt()
    .ok_or_else(|| "date underflow".to_string())?;
    Ok((start, end))
}

/// Parse `YYYY-MM-DD`.
pub fn parse_date(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|e| format!("invalid date '{s}' (expected YYYY-MM-DD): {e}"))
}

/// Fixed relative phrases for `agenda` / day selection.
///
/// Supported:
/// - `today`, `tomorrow`
/// - `next <weekday>` (`next tuesday`, `next mon`, …)
/// - bare `YYYY-MM-DD`
/// - bare weekday name → upcoming occurrence (today if matching, else next)
pub fn resolve_day_phrase(phrase: &str, now_local: NaiveDate) -> Result<NaiveDate, String> {
    let s = phrase.trim();
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "today" => Ok(now_local),
        "tomorrow" => Ok(now_local + Duration::days(1)),
        _ => {
            if let Ok(d) = parse_date(s) {
                return Ok(d);
            }
            if let Some(rest) = lower.strip_prefix("next ") {
                let wd = parse_weekday(rest.trim())?;
                return Ok(next_weekday(now_local, wd, /*strict_next=*/ true));
            }
            if let Ok(wd) = parse_weekday(&lower) {
                return Ok(next_weekday(now_local, wd, /*strict_next=*/ false));
            }
            Err(format!(
                "unrecognized day phrase '{phrase}' \
                 (expected today|tomorrow|next <weekday>|YYYY-MM-DD)"
            ))
        }
    }
}

fn parse_weekday(s: &str) -> Result<Weekday, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Ok(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Ok(Weekday::Tue),
        "wed" | "wednesday" => Ok(Weekday::Wed),
        "thu" | "thur" | "thurs" | "thursday" => Ok(Weekday::Thu),
        "fri" | "friday" => Ok(Weekday::Fri),
        "sat" | "saturday" => Ok(Weekday::Sat),
        "sun" | "sunday" => Ok(Weekday::Sun),
        other => Err(format!("unknown weekday '{other}'")),
    }
}

/// Next occurrence of `wd` on or after `from` (or strictly after when `strict_next`).
fn next_weekday(from: NaiveDate, wd: Weekday, strict_next: bool) -> NaiveDate {
    let mut d = from;
    if strict_next {
        d += Duration::days(1);
    }
    for _ in 0..8 {
        if d.weekday() == wd {
            return d;
        }
        d += Duration::days(1);
    }
    from // unreachable
}

/// Parse IANA timezone; fall back error with a clear message.
pub fn parse_tz(name: &str) -> Result<Tz, String> {
    Tz::from_str(name).map_err(|e| format!("unknown timezone '{name}': {e}"))
}

/// System-local IANA name (shared logic with visible::next_run).
pub fn system_tz_name() -> String {
    crate::visible::next_run::system_tz_name()
}

/// Local civil date of `now_utc` in `tz`.
pub fn local_date(now_utc: chrono::DateTime<chrono::Utc>, tz: Tz) -> NaiveDate {
    now_utc.with_timezone(&tz).date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn year_month_and_relative() {
        let now = d(2026, 7, 29);
        assert_eq!(resolve_month("2026-08", now).unwrap(), (2026, 8));
        assert_eq!(resolve_month("this", now).unwrap(), (2026, 7));
        assert_eq!(resolve_month("next", now).unwrap(), (2026, 8));
        assert_eq!(resolve_month("August", now).unwrap(), (2026, 8));
        assert_eq!(resolve_month("feb", now).unwrap(), (2026, 2));
    }

    #[test]
    fn next_month_from_december() {
        let now = d(2026, 12, 15);
        assert_eq!(resolve_month("next month", now).unwrap(), (2027, 1));
    }

    #[test]
    fn month_bounds_leap_and_31() {
        let (a, b) = month_bounds(2028, 2).unwrap();
        assert_eq!(a, d(2028, 2, 1));
        assert_eq!(b, d(2028, 2, 29));
        let (a, b) = month_bounds(2026, 8).unwrap();
        assert_eq!(a, d(2026, 8, 1));
        assert_eq!(b, d(2026, 8, 31));
    }

    #[test]
    fn day_phrases() {
        // 2026-07-29 is Wednesday
        let now = d(2026, 7, 29);
        assert_eq!(resolve_day_phrase("today", now).unwrap(), now);
        assert_eq!(
            resolve_day_phrase("tomorrow", now).unwrap(),
            d(2026, 7, 30)
        );
        assert_eq!(
            resolve_day_phrase("next tuesday", now).unwrap(),
            d(2026, 8, 4)
        );
        assert_eq!(
            resolve_day_phrase("tuesday", now).unwrap(),
            d(2026, 8, 4)
        );
        // On a Tuesday, bare "tuesday" means today; "next tuesday" means +7.
        let tue = d(2026, 8, 4);
        assert_eq!(resolve_day_phrase("tuesday", tue).unwrap(), tue);
        assert_eq!(
            resolve_day_phrase("next tuesday", tue).unwrap(),
            d(2026, 8, 11)
        );
    }
}
