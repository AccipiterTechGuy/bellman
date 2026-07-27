//! Build store timers from a slot payload.

use super::envelope::SlotPayload;
use crate::occurrence::{parse_weekdays, Occurrence, OccurrenceKind, Weekdays};
use crate::store::{Action, MisfirePolicy, NewTimer, OverlapPolicy, RetryPolicy, TimerPatch};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde_json::Value;

/// Convert a slot payload into a [`NewTimer`] for `add`.
pub fn new_timer_from_payload(payload: &SlotPayload) -> Result<NewTimer, String> {
    let name = payload
        .timer_name
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "payload.timer_name is required for add".to_string())?;
    let occurrence = occurrence_from_payload(payload)?;
    let mut new = NewTimer::new(name, occurrence);
    if let Some(id) = payload.resolved_timer_id() {
        new.id = Some(id);
    }
    new.action = action_from_payload(payload)?;
    if let Some(m) = &payload.misfire_policy {
        new.misfire = misfire_from_value(m)?;
    }
    Ok(new)
}

/// Build a partial patch for `modify`.
pub fn patch_from_payload(payload: &SlotPayload) -> Result<TimerPatch, String> {
    let mut patch = TimerPatch::default();
    if let Some(name) = &payload.timer_name {
        if !name.is_empty() {
            patch.name = Some(name.clone());
        }
    }
    if payload.occurrence.is_some()
        || payload.time.is_some()
        || payload.every_secs.is_some()
        || payload.cron.is_some()
    {
        patch.occurrence = Some(occurrence_from_payload(payload)?);
    }
    if payload.action.is_some() || payload.launch_command.is_some() {
        patch.action = Some(action_from_payload(payload)?);
    }
    if let Some(m) = &payload.misfire_policy {
        patch.misfire = Some(misfire_from_value(m)?);
    }
    Ok(patch)
}

fn occurrence_from_payload(payload: &SlotPayload) -> Result<Occurrence, String> {
    let tz = payload.tz.clone().unwrap_or_else(|| "UTC".to_string());

    // Prefer full OccurrenceKind JSON under `occurrence`.
    if let Some(occ_v) = &payload.occurrence {
        // Allow either the tagged kind or a full Occurrence object.
        if let Ok(kind) = serde_json::from_value::<OccurrenceKind>(occ_v.clone()) {
            return Occurrence::new(kind, tz);
        }
        if let Ok(occ) = serde_json::from_value::<Occurrence>(occ_v.clone()) {
            return Ok(occ);
        }
        // Simplified: { "kind": "daily", "time": "08:00:00", ... }
        if let Some(obj) = occ_v.as_object() {
            let kind_s = obj
                .get("kind")
                .or_else(|| obj.get("occ"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "occurrence.kind is required".to_string())?;
            return build_kind(
                kind_s,
                &SimplifiedOcc {
                    time: obj
                        .get("time")
                        .or_else(|| obj.get("at"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or_else(|| payload.time.clone()),
                    every_secs: obj
                        .get("every_secs")
                        .and_then(|v| v.as_u64())
                        .or(payload.every_secs),
                    days: obj.get("days").cloned().or_else(|| payload.days.clone()),
                    day: obj
                        .get("day")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as u8)
                        .or(payload.day),
                    month: obj
                        .get("month")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as u8)
                        .or(payload.month),
                    cron: obj
                        .get("cron")
                        .or_else(|| obj.get("expr"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or_else(|| payload.cron.clone()),
                },
                &tz,
            );
        }
        return Err(format!("could not parse occurrence: {occ_v}"));
    }

    // Top-level convenience: occurrence kind inferred from fields.
    // Default to daily when only `time` is present; interval when every_secs.
    if let Some(every) = payload.every_secs {
        return build_kind(
            "interval",
            &SimplifiedOcc {
                time: payload.time.clone(),
                every_secs: Some(every),
                days: payload.days.clone(),
                day: payload.day,
                month: payload.month,
                cron: payload.cron.clone(),
            },
            &tz,
        );
    }
    if payload.cron.is_some() {
        return build_kind(
            "cron",
            &SimplifiedOcc {
                time: payload.time.clone(),
                every_secs: None,
                days: payload.days.clone(),
                day: payload.day,
                month: payload.month,
                cron: payload.cron.clone(),
            },
            &tz,
        );
    }
    if payload.time.is_some() {
        return build_kind(
            "daily",
            &SimplifiedOcc {
                time: payload.time.clone(),
                every_secs: None,
                days: payload.days.clone(),
                day: payload.day,
                month: payload.month,
                cron: None,
            },
            &tz,
        );
    }
    Err("payload must include occurrence (or time / every_secs / cron)".into())
}

struct SimplifiedOcc {
    time: Option<String>,
    every_secs: Option<u64>,
    days: Option<Value>,
    day: Option<u8>,
    month: Option<u8>,
    cron: Option<String>,
}

fn build_kind(kind: &str, s: &SimplifiedOcc, tz: &str) -> Result<Occurrence, String> {
    let kind = match kind.to_ascii_lowercase().as_str() {
        "once" => {
            let t = s
                .time
                .as_deref()
                .ok_or_else(|| "once requires time".to_string())?;
            OccurrenceKind::Once {
                at: parse_once_at(t)?,
            }
        }
        "interval" => {
            let every = s
                .every_secs
                .ok_or_else(|| "interval requires every_secs".to_string())?;
            OccurrenceKind::Interval {
                every_secs: every,
                anchor: Utc::now(),
            }
        }
        "daily" => {
            let t = s
                .time
                .as_deref()
                .ok_or_else(|| "daily requires time".to_string())?;
            OccurrenceKind::Daily {
                at: parse_clock(t)?,
            }
        }
        "weekly" => {
            let t = s
                .time
                .as_deref()
                .ok_or_else(|| "weekly requires time".to_string())?;
            let days = parse_days_value(s.days.as_ref())?;
            OccurrenceKind::Weekly {
                days,
                at: parse_clock(t)?,
            }
        }
        "monthly" => {
            let t = s
                .time
                .as_deref()
                .ok_or_else(|| "monthly requires time".to_string())?;
            let day = s.day.ok_or_else(|| "monthly requires day".to_string())?;
            OccurrenceKind::Monthly {
                day,
                at: parse_clock(t)?,
            }
        }
        "yearly" => {
            let t = s
                .time
                .as_deref()
                .ok_or_else(|| "yearly requires time".to_string())?;
            let month = s.month.ok_or_else(|| "yearly requires month".to_string())?;
            let day = s.day.ok_or_else(|| "yearly requires day".to_string())?;
            OccurrenceKind::Yearly {
                month,
                day,
                at: parse_clock(t)?,
            }
        }
        "cron" => {
            let expr = s
                .cron
                .clone()
                .ok_or_else(|| "cron requires cron expression".to_string())?;
            OccurrenceKind::Cron { expr }
        }
        other => {
            return Err(format!(
                "unknown occurrence kind '{other}' (once|interval|daily|weekly|monthly|yearly|cron)"
            ));
        }
    };
    Occurrence::new(kind, tz)
}

fn parse_days_value(v: Option<&Value>) -> Result<Weekdays, String> {
    let v = v.ok_or_else(|| "weekly requires days".to_string())?;
    if let Some(arr) = v.as_array() {
        let names: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
        return parse_weekdays(&names);
    }
    if let Some(s) = v.as_str() {
        let names: Vec<&str> = s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        return parse_weekdays(&names);
    }
    Err("days must be a string or array of weekday names".into())
}

fn parse_clock(s: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(s, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
        .map_err(|e| format!("invalid clock time '{s}': {e}"))
}

fn parse_once_at(s: &str) -> Result<NaiveDateTime, String> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| {
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|e| format!("invalid once datetime '{s}': {e}"))?;
            Ok(d.and_hms_opt(0, 0, 0).unwrap())
        })
        .map_err(|e: String| e)
}

fn action_from_payload(payload: &SlotPayload) -> Result<Action, String> {
    if let Some(a) = &payload.action {
        return serde_json::from_value(a.clone()).map_err(|e| format!("invalid action: {e}"));
    }
    if let Some(cmd) = &payload.launch_command {
        return Ok(Action::Launch {
            command: cmd.clone(),
            args: payload.args.clone().unwrap_or_default(),
            workdir: payload.workdir.clone(),
        });
    }
    Ok(Action::None)
}

fn misfire_from_value(v: &Value) -> Result<MisfirePolicy, String> {
    if let Ok(p) = serde_json::from_value::<MisfirePolicy>(v.clone()) {
        return Ok(p);
    }
    if let Some(s) = v.as_str() {
        return match s.to_ascii_lowercase().as_str() {
            "skip" => Ok(MisfirePolicy::Skip),
            "coalesce" => Ok(MisfirePolicy::default_calendar()),
            other => Err(format!("unknown misfire_policy '{other}'")),
        };
    }
    Err(format!("invalid misfire_policy: {v}"))
}

/// Keep unused imports honest when only used via serde defaults paths.
#[allow(dead_code)]
fn _policy_defaults() -> (OverlapPolicy, RetryPolicy) {
    (OverlapPolicy::default(), RetryPolicy::default())
}
