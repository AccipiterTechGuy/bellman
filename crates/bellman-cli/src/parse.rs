//! Parse CLI strings into occurrence kinds / wall-clock values.

use bellman_core::occurrence::{parse_weekdays, Occurrence, OccurrenceKind};
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};
use chrono_tz::Tz;
use std::str::FromStr;

/// Inputs for building a new occurrence from CLI flags.
#[derive(Debug, Clone)]
pub struct BuildOccurrence {
    pub kind: String,
    pub time: Option<String>,
    pub tz: Option<String>,
    pub every_secs: Option<u64>,
    pub days: Option<String>,
    pub day: Option<u8>,
    pub month: Option<u8>,
    pub cron: Option<String>,
}

/// Inputs for patching an existing occurrence from `edit` flags.
#[derive(Debug, Clone, Default)]
pub struct PatchOccurrence {
    pub time: Option<String>,
    pub every_secs: Option<u64>,
    pub cron: Option<String>,
    pub days: Option<String>,
    pub day: Option<u8>,
    pub month: Option<u8>,
}

/// Build an [`Occurrence`] from `add` flags.
pub fn build_occurrence(args: &BuildOccurrence) -> Result<Occurrence, String> {
    let tz_name = resolve_tz_name(args.tz.as_deref())?;
    let occ_kind = match args.kind.to_ascii_lowercase().as_str() {
        "once" => {
            let t = args
                .time
                .as_deref()
                .ok_or_else(|| "--time is required for occurrence once".to_string())?;
            let at = parse_once_at(t, &tz_name)?;
            OccurrenceKind::Once { at }
        }
        "interval" => {
            let every = args.every_secs.ok_or_else(|| {
                "--every-secs is required for occurrence interval".to_string()
            })?;
            // Anchor at now (UTC). First fire is every_secs after create's next_fire
            // computation floor (last_fired/now).
            OccurrenceKind::Interval {
                every_secs: every,
                anchor: Utc::now(),
            }
        }
        "daily" => {
            let t = args
                .time
                .as_deref()
                .ok_or_else(|| "--time is required for occurrence daily".to_string())?;
            OccurrenceKind::Daily {
                at: parse_clock_time(t)?,
            }
        }
        "weekly" => {
            let t = args
                .time
                .as_deref()
                .ok_or_else(|| "--time is required for occurrence weekly".to_string())?;
            let days_s = args.days.as_deref().ok_or_else(|| {
                "--days is required for occurrence weekly (e.g. mon,wed)".to_string()
            })?;
            let names: Vec<&str> = days_s
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .collect();
            let wd = parse_weekdays(&names)?;
            OccurrenceKind::Weekly {
                days: wd,
                at: parse_clock_time(t)?,
            }
        }
        "monthly" => {
            let t = args
                .time
                .as_deref()
                .ok_or_else(|| "--time is required for occurrence monthly".to_string())?;
            let d = args
                .day
                .ok_or_else(|| "--day is required for occurrence monthly".to_string())?;
            OccurrenceKind::Monthly {
                day: d,
                at: parse_clock_time(t)?,
            }
        }
        "yearly" => {
            let t = args
                .time
                .as_deref()
                .ok_or_else(|| "--time is required for occurrence yearly".to_string())?;
            let m = args
                .month
                .ok_or_else(|| "--month is required for occurrence yearly".to_string())?;
            let d = args
                .day
                .ok_or_else(|| "--day is required for occurrence yearly".to_string())?;
            OccurrenceKind::Yearly {
                month: m,
                day: d,
                at: parse_clock_time(t)?,
            }
        }
        "cron" => {
            let expr = args
                .cron
                .as_deref()
                .ok_or_else(|| "--cron is required for occurrence cron".to_string())?
                .to_string();
            OccurrenceKind::Cron { expr }
        }
        other => {
            return Err(format!(
                "unknown occurrence kind '{other}' (expected once|interval|daily|weekly|monthly|yearly|cron)"
            ));
        }
    };
    Occurrence::new(occ_kind, tz_name)
}

/// Patch an existing occurrence's time / kind-specific fields from `edit` flags.
pub fn patch_occurrence(
    current: &Occurrence,
    args: &PatchOccurrence,
) -> Result<Option<Occurrence>, String> {
    if args.time.is_none()
        && args.every_secs.is_none()
        && args.cron.is_none()
        && args.days.is_none()
        && args.day.is_none()
        && args.month.is_none()
    {
        return Ok(None);
    }

    let tz = current.tz_name().to_string();
    let kind = match current.kind().clone() {
        OccurrenceKind::Once { at } => {
            let at = if let Some(t) = args.time.as_deref() {
                parse_once_at(t, &tz)?
            } else {
                at
            };
            OccurrenceKind::Once { at }
        }
        OccurrenceKind::Interval {
            every_secs: cur_every,
            anchor,
        } => {
            let every = args.every_secs.unwrap_or(cur_every);
            OccurrenceKind::Interval {
                every_secs: every,
                anchor,
            }
        }
        OccurrenceKind::Daily { at } => {
            let at = if let Some(t) = args.time.as_deref() {
                parse_clock_time(t)?
            } else {
                at
            };
            OccurrenceKind::Daily { at }
        }
        OccurrenceKind::Weekly {
            days: cur_days,
            at,
        } => {
            let at = if let Some(t) = args.time.as_deref() {
                parse_clock_time(t)?
            } else {
                at
            };
            let days = if let Some(d) = args.days.as_deref() {
                let names: Vec<&str> = d
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .collect();
                parse_weekdays(&names)?
            } else {
                cur_days
            };
            OccurrenceKind::Weekly { days, at }
        }
        OccurrenceKind::Monthly {
            day: cur_day,
            at,
        } => {
            let at = if let Some(t) = args.time.as_deref() {
                parse_clock_time(t)?
            } else {
                at
            };
            OccurrenceKind::Monthly {
                day: args.day.unwrap_or(cur_day),
                at,
            }
        }
        OccurrenceKind::Yearly {
            month: cur_month,
            day: cur_day,
            at,
        } => {
            let at = if let Some(t) = args.time.as_deref() {
                parse_clock_time(t)?
            } else {
                at
            };
            OccurrenceKind::Yearly {
                month: args.month.unwrap_or(cur_month),
                day: args.day.unwrap_or(cur_day),
                at,
            }
        }
        OccurrenceKind::Cron { expr } => {
            let expr = args
                .cron
                .as_deref()
                .map_or(expr, std::string::ToString::to_string);
            OccurrenceKind::Cron { expr }
        }
    };

    // Preserve policies / limits from the live schedule by cloning then
    // replacing only the kind via a fresh Occurrence and re-applying limits.
    // Occurrence fields are private; reconstruct with same tz + policies via
    // builders. We re-apply validity / max_runs / runs_done / exclusions /
    // pending_skips through public APIs where available.
    let mut next = Occurrence::new(kind, tz)?;
    if let Some(from) = current.valid_from() {
        next = next.with_valid_from(from);
    }
    if let Some(until) = current.valid_until() {
        next = next.with_valid_until(until);
    }
    if let Some(max) = current.max_runs() {
        next = next.with_max_runs(max);
    }
    next = next.with_runs_done(current.runs_done());
    for d in current.exclusions() {
        next.exclude_date(*d);
    }
    // pending_skips has no bulk setter; re-apply via skip_next.
    for _ in 0..current.pending_skips() {
        next.skip_next();
    }
    // DST / invalid-monthday policies: defaults match; if the live schedule used
    // non-defaults they are lost on edit. Acceptable for v1 CLI surface.
    Ok(Some(next))
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
    Err(format!(
        "invalid time '{s}' (expected HH:MM or HH:MM:SS)"
    ))
}

/// Once-at datetime: RFC3339, or `YYYY-MM-DDTHH:MM[:SS]` interpreted in `tz`.
pub fn parse_once_at(s: &str, tz_name: &str) -> Result<NaiveDateTime, String> {
    let s = s.trim();
    // Full RFC3339 → convert to the schedule tz's local naive.
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

pub fn parse_enabled(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" => Ok(true),
        "0" | "false" | "no" | "off" | "disabled" => Ok(false),
        other => Err(format!(
            "invalid --enabled '{other}' (expected true|false)"
        )),
    }
}

/// Resolve IANA tz name: explicit flag → system local → UTC.
pub fn resolve_tz_name(explicit: Option<&str>) -> Result<String, String> {
    if let Some(tz) = explicit {
        let tz = tz.trim();
        Tz::from_str(tz).map_err(|e| format!("unknown timezone '{tz}': {e}"))?;
        return Ok(tz.to_string());
    }
    // Prefer TZ env, then chrono Local offset name if it is IANA, else UTC.
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() && Tz::from_str(&tz).is_ok() {
            return Ok(tz);
        }
    }
    // chrono Local has no stable IANA name; try /etc/timezone then UTC.
    if let Ok(s) = std::fs::read_to_string("/etc/timezone") {
        let s = s.trim().to_string();
        if Tz::from_str(&s).is_ok() {
            return Ok(s);
        }
    }
    // Linux systemd: /etc/localtime is often a symlink into zoneinfo.
    if let Ok(link) = std::fs::read_link("/etc/localtime") {
        if let Some(name) = extract_zoneinfo_name(&link) {
            if Tz::from_str(&name).is_ok() {
                return Ok(name);
            }
        }
    }
    // Last resort: keep wall-clock consistent with Local offset by using a fixed
    // offset is not IANA — fall back to UTC. Documented in CLI.md.
    let _ = Local::now().offset().local_minus_utc();
    Ok("UTC".to_string())
}

fn extract_zoneinfo_name(path: &std::path::Path) -> Option<String> {
    let s = path.to_string_lossy();
    // …/zoneinfo/Europe/Helsinki
    if let Some(idx) = s.find("zoneinfo/") {
        let rest = &s[idx + "zoneinfo/".len()..];
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    None
}

/// Format a NaiveTime as HH:MM:SS for human output.
#[allow(dead_code)]
pub fn fmt_time(t: NaiveTime) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Format a NaiveDate for human output.
#[allow(dead_code)]
pub fn fmt_date(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}
