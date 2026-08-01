//! Next-run computation via bellman-core's occurrence engine (croner + chrono-tz).

use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::visible::cron::parse::{CronSchedule, SpecialSchedule};
use chrono::{DateTime, Local, Utc};
use chrono_tz::Tz;
use std::str::FromStr;

/// Compute next fire for a classic 5-field cron expression in `tz`.
///
/// Reuses [`Occurrence`] so DST gap/fold policies match the rest of Bellman.
pub fn next_cron_run(
    expr_5: &str,
    tz_name: &str,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, String> {
    let tz: Tz = Tz::from_str(tz_name).map_err(|e| format!("bad tz '{tz_name}': {e}"))?;
    // croner accepts 5-field (minutes…) with seconds optional — feed as-is.
    let occ = Occurrence::new(
        OccurrenceKind::Cron {
            expr: expr_5.to_string(),
        },
        tz_name,
    )?;
    let after_local = after.with_timezone(&tz);
    Ok(occ
        .peek_next_fire(after_local)
        .map(|t| t.with_timezone(&Utc)))
}

/// Next run from a parsed [`CronSchedule`].
pub fn next_from_schedule(
    schedule: &CronSchedule,
    tz_name: &str,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, String> {
    match schedule {
        CronSchedule::Special(SpecialSchedule::Reboot) => Ok(None),
        CronSchedule::Special(s) => {
            let expr = s.as_five_field();
            next_cron_run(&expr, tz_name, after)
        }
        CronSchedule::Fields {
            minute,
            hour,
            dom,
            month,
            dow,
        } => {
            let expr = format!("{minute} {hour} {dom} {month} {dow}");
            next_cron_run(&expr, tz_name, after)
        }
    }
}

/// System local IANA timezone name (best-effort).
pub fn system_tz_name() -> String {
    // Prefer TZ env, then /etc/localtime link, then chrono Local offset label.
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() && Tz::from_str(&tz).is_ok() {
            return tz;
        }
    }
    if let Ok(link) = std::fs::read_link("/etc/localtime") {
        let s = link.to_string_lossy();
        if let Some(idx) = s.find("zoneinfo/") {
            let name = &s[idx + "zoneinfo/".len()..];
            if Tz::from_str(name).is_ok() {
                return name.to_string();
            }
        }
    }
    if let Ok(contents) = std::fs::read_to_string("/etc/timezone") {
        let name = contents.trim();
        if Tz::from_str(name).is_ok() {
            return name.to_string();
        }
    }
    // Last resort: use fixed offset name if Local works; else UTC.
    let _ = Local::now();
    "UTC".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    #[test]
    fn step_seven_minutes() {
        let after = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        let next = next_cron_run("*/7 * * * *", "UTC", after)
            .unwrap()
            .expect("next");
        // Strictly after 10:00 → 10:07
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 1, 15, 10, 7, 0).unwrap());
    }

    #[test]
    fn mon_fri_morning() {
        // 2026-01-15 is Thursday
        let after = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let next = next_cron_run("0 6 * * MON-FRI", "UTC", after)
            .unwrap()
            .expect("next");
        // Next weekday 06:00 after Thursday noon → Friday 06:00
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 1, 16, 6, 0, 0).unwrap());
    }

    #[test]
    fn dst_spring_forward_helsinki() {
        // Europe/Helsinki springs forward 2026-03-29 03:00 → 04:00.
        // A job at 02:30 should not land in the missing hour.
        let after = Utc.with_ymd_and_hms(2026, 3, 28, 12, 0, 0).unwrap();
        let next = next_cron_run("30 2 * * *", "Europe/Helsinki", after)
            .unwrap()
            .expect("next");
        let local = next.with_timezone(&Tz::from_str("Europe/Helsinki").unwrap());
        assert_eq!(local.date_naive().to_string(), "2026-03-29");
        // 02:30 EET is before the 03:00→04:00 gap — a real wall time.
        assert_eq!(local.hour(), 2);
        assert_eq!(local.minute(), 30);
        assert!(next > after);
    }
}
