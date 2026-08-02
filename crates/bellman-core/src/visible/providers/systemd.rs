//! systemd timer discovery (system + user).

use crate::visible::explain::explain_systemd;
use crate::visible::id::task_id;
use crate::visible::types::{DiscoveredTask, LastResult, SourceKind};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use std::process::Command;

/// Discover system and user systemd timers.
pub fn discover_systemd() -> (Vec<DiscoveredTask>, Vec<String>) {
    let mut tasks = Vec::new();
    let mut warnings = Vec::new();

    match list_timers(false) {
        Ok(t) => tasks.extend(t),
        Err(e) => warnings.push(format!("system timers: {e}")),
    }
    match list_timers(true) {
        Ok(t) => tasks.extend(t),
        Err(e) => warnings.push(format!("user timers: {e}")),
    }
    (tasks, warnings)
}

fn list_timers(user: bool) -> Result<Vec<DiscoveredTask>, String> {
    let mut cmd = Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.args(["list-timers", "--all", "--no-pager", "--no-legend"]);
    let out = cmd.output().map_err(|e| format!("spawn systemctl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "systemctl list-timers failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut tasks = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(task) = parse_timer_line(line, user) {
            tasks.push(task);
        }
    }

    for task in &mut tasks {
        enrich_timer(task, user);
    }
    Ok(tasks)
}

/// Extract unit name from `source` (`systemd:{scope}:{unit}`).
fn unit_from_source(source: &str) -> &str {
    source.rsplit(':').next().unwrap_or(source)
}

/// Parse a `systemctl list-timers --all --no-legend` line.
fn parse_timer_line(line: &str, user: bool) -> Option<DiscoveredTask> {
    let kind = if user {
        SourceKind::SystemdUser
    } else {
        SourceKind::SystemdSystem
    };
    let parts: Vec<&str> = line.split_whitespace().collect();
    let unit = parts.iter().copied().find(|p| p.ends_with(".timer"))?;
    let activates = parts
        .iter()
        .copied()
        .find(|p| p.ends_with(".service"))
        .unwrap_or("");

    let next_run = parse_next_from_line(line);
    let last_run = parse_last_from_line(line);

    let scope = if user { "user" } else { "system" };
    let source = format!("systemd:{scope}:{unit}");
    let id = task_id(kind, &[&source]);

    // schedule_expr / explanation filled in enrich_timer from OnCalendar etc.
    Some(DiscoveredTask {
        id,
        source_kind: kind,
        source: source.clone(),
        owner: if user {
            std::env::var("USER").unwrap_or_else(|_| "user".into())
        } else {
            "root".into()
        },
        command: if activates.is_empty() {
            unit.to_string()
        } else {
            format!("{unit} → {activates}")
        },
        stdin_payload: None,
        schedule_expr: unit.to_string(), // placeholder until enrich
        human_explanation: format!("systemd timer {unit}"),
        next_run,
        last_run,
        last_result: if last_run.is_none() {
            LastResult::Never
        } else {
            LastResult::Unknown
        },
        enabled: true,
        writable: false,
        write_block_reason: Some(if user {
            "v1 is display-only for systemd user timers (no enable/disable via Bellman yet)".into()
        } else {
            "v1 is read-only for system systemd units; Bellman will not call sudo".into()
        }),
        timezone: None,
        line_no: None,
        raw_line: Some(line.to_string()),
        disabled_original: None,
        platform_note: None,
    })
}

fn parse_next_from_line(line: &str) -> Option<DateTime<Utc>> {
    if line.trim_start().starts_with("n/a") {
        return None;
    }
    parse_systemctl_timestamp(line)
}

fn parse_last_from_line(line: &str) -> Option<DateTime<Utc>> {
    let mut found = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 19 <= bytes.len() {
        if bytes[i].is_ascii_digit()
            && bytes.get(i + 4) == Some(&b'-')
            && bytes.get(i + 10) == Some(&b' ')
        {
            let slice = &line[i..];
            if let Some(dt) = parse_ymd_hms_prefix(slice) {
                found.push(dt);
                i += 19;
                continue;
            }
        }
        i += 1;
    }
    let next_missing = line.trim_start().starts_with("n/a");
    if found.len() >= 2 {
        Some(found[1])
    } else if next_missing && found.len() == 1 {
        Some(found[0])
    } else {
        None
    }
}

fn parse_systemctl_timestamp(line: &str) -> Option<DateTime<Utc>> {
    let s = line.trim_start();
    let s = if s.len() > 4 && s.as_bytes()[3] == b' ' && s.as_bytes()[0].is_ascii_alphabetic() {
        &s[4..]
    } else {
        s
    };
    parse_ymd_hms_prefix(s)
}

fn parse_ymd_hms_prefix(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim_start();
    if s.len() < 19 {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(&s[..19], "%Y-%m-%d %H:%M:%S").ok()?;
    let local = Local.from_local_datetime(&naive).single()?;
    Some(local.with_timezone(&Utc))
}

/// Whether UnitFileState means the timer is armed / participating.
pub fn unit_file_state_enabled(state: &str) -> bool {
    matches!(
        state,
        "enabled"
            | "enabled-runtime"
            | "static"
            | "linked"
            | "linked-runtime"
            | "transient" // systemd-run --on-* armed timers
            | "generated"
    )
}

/// Derive last_result honestly from systemd properties.
///
/// Empty `ExecMainStartTimestamp` means the unit has **never** run — do not
/// treat Result=success / ExecMainStatus=0 (systemd defaults) as evidence.
pub fn last_result_from_props(
    start_timestamp: &str,
    result: &str,
    exec_main_status: Option<i32>,
    list_timers_last: Option<DateTime<Utc>>,
) -> LastResult {
    let started = !start_timestamp.trim().is_empty()
        && !start_timestamp.eq_ignore_ascii_case("n/a")
        && start_timestamp != "-";
    if !started {
        // list-timers LAST is secondary; if service never started, prefer Never
        // even when list-timers showed something odd.
        if list_timers_last.is_none() {
            return LastResult::Never;
        }
        // Had a LAST column but no start timestamp — still no real exit evidence.
        return LastResult::Unknown;
    }
    match exec_main_status {
        Some(code) if result == "success" || code == 0 => LastResult::Ok { exit_code: code },
        Some(code) if !result.is_empty() && result != "success" => {
            LastResult::Failed { exit_code: code }
        }
        Some(code) => LastResult::Ok { exit_code: code },
        None => LastResult::Unknown,
    }
}

/// Parse `TimersCalendar` / `TimersMonotonic` property blobs into a schedule expr.
///
/// Monotonic keys appear as `OnActiveUSec=50min` (human-friendly duration) in
/// modern systemd, not only the `OnActiveSec=` form.
pub fn parse_timer_schedule(calendar: &str, monotonic: &str) -> (String, String) {
    let mut parts: Vec<String> = Vec::new();
    // TimersCalendar={ OnCalendar=*-*-* 06:00:00 ; next_elapse=... }
    for token in calendar.split(['{', '}', ';', '\n']) {
        let t = token.trim();
        if let Some(v) = t.strip_prefix("OnCalendar=") {
            let v = v.trim();
            if !v.is_empty() {
                parts.push(format!("OnCalendar={v}"));
            }
        }
    }
    for token in monotonic.split(['{', '}', ';', '\n']) {
        let t = token.trim();
        // Accept both *Sec= and *USec= (systemd show uses USec for wall form).
        for key in [
            "OnActiveUSec=",
            "OnActiveSec=",
            "OnBootUSec=",
            "OnBootSec=",
            "OnStartupUSec=",
            "OnStartupSec=",
            "OnUnitActiveUSec=",
            "OnUnitActiveSec=",
            "OnUnitInactiveUSec=",
            "OnUnitInactiveSec=",
        ] {
            if let Some(v) = t.strip_prefix(key) {
                let v = v.trim();
                if !v.is_empty() && v != "0" {
                    // Normalize USec label to Sec for a stable short expression.
                    let label = key
                        .trim_end_matches('=')
                        .trim_end_matches("USec")
                        .trim_end_matches("Sec");
                    parts.push(format!("{label}Sec={v}"));
                }
            }
        }
    }
    if parts.is_empty() {
        (
            String::new(),
            "systemd timer (no OnCalendar/On*Sec found)".into(),
        )
    } else {
        let expr = parts.join("; ");
        let human = explain_systemd(&expr);
        (expr, human)
    }
}

fn enrich_timer(task: &mut DiscoveredTask, user: bool) {
    let unit = unit_from_source(&task.source).to_string();

    // Unit file / active state + calendar schedule
    let mut cmd = Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.args([
        "show",
        &unit,
        "-p",
        "ActiveState",
        "-p",
        "UnitFileState",
        "-p",
        "Triggers",
        "-p",
        "TimersCalendar",
        "-p",
        "TimersMonotonic",
    ]);
    let mut active_state = String::new();
    if let Ok(out) = cmd.output() {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut calendar = String::new();
            let mut monotonic = String::new();
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("ActiveState=") {
                    active_state = v.to_string();
                }
                if let Some(v) = line.strip_prefix("UnitFileState=") {
                    task.enabled = unit_file_state_enabled(v);
                    if v == "disabled" || v == "masked" {
                        task.enabled = false;
                    }
                }
                if let Some(v) = line.strip_prefix("Triggers=") {
                    if !v.is_empty() {
                        task.command = format!("{unit} → {v}");
                    }
                }
                if let Some(v) = line.strip_prefix("TimersCalendar=") {
                    calendar = v.to_string();
                }
                if let Some(v) = line.strip_prefix("TimersMonotonic=") {
                    monotonic = v.to_string();
                }
            }
            // Armed transient / runtime timers with a next fire are enabled.
            if active_state == "active" && task.next_run.is_some() {
                task.enabled = true;
            }
            let (expr, human) = parse_timer_schedule(&calendar, &monotonic);
            if !expr.is_empty() {
                task.schedule_expr = expr;
                task.human_explanation = human;
            } else {
                task.schedule_expr = unit.clone();
                task.human_explanation = format!(
                    "systemd timer {unit}{}",
                    if active_state.is_empty() {
                        String::new()
                    } else {
                        format!(" (ActiveState={active_state})")
                    }
                );
            }
        }
    }

    // Last result from the triggered service — honest about never-run units.
    let service = task
        .command
        .split('→')
        .nth(1)
        .map(str::trim)
        .filter(|s| s.ends_with(".service"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| unit.replace(".timer", ".service"));

    let mut cmd = Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.args([
        "show",
        &service,
        "-p",
        "ExecMainStatus",
        "-p",
        "Result",
        "-p",
        "ExecMainStartTimestamp",
    ]);
    if let Ok(out) = cmd.output() {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut status: Option<i32> = None;
            let mut result = String::new();
            let mut start_ts = String::new();
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("ExecMainStatus=") {
                    status = v.parse().ok();
                }
                if let Some(v) = line.strip_prefix("Result=") {
                    result = v.to_string();
                }
                if let Some(v) = line.strip_prefix("ExecMainStartTimestamp=") {
                    start_ts = v.to_string();
                }
            }
            task.last_result = last_result_from_props(&start_ts, &result, status, task.last_run);
        }
    }
}

/// Fetch journal logs for a timer's service.
pub fn timer_logs(task: &DiscoveredTask, lines: usize) -> Result<String, String> {
    let user = matches!(task.source_kind, SourceKind::SystemdUser);
    let unit = unit_from_source(&task.source);
    let service = task
        .command
        .split('→')
        .nth(1)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(unit);
    let mut cmd = Command::new("journalctl");
    if user {
        cmd.arg("--user");
    }
    cmd.args(["-u", service, "-n", &lines.to_string(), "--no-pager"]);
    let out = cmd.output().map_err(|e| format!("journalctl: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sample_line() {
        let line = "Wed 2026-07-29 15:38:10 EEST  233ms Wed 2026-07-29 15:37:55 EEST      14s ago oom-protect.timer            oom-protect.service";
        let t = parse_timer_line(line, false).expect("parse");
        assert!(t.source.contains("oom-protect.timer"));
        assert!(t.next_run.is_some(), "next_run");
        assert!(t.last_run.is_some(), "last_run");
    }

    #[test]
    fn last_run_when_next_is_na() {
        let line = "n/a                         n/a Wed 2026-07-29 15:37:55 EEST      14s ago foo.timer                    foo.service";
        let t = parse_timer_line(line, false).expect("parse");
        assert!(t.next_run.is_none(), "next should be None for n/a");
        let last = t
            .last_run
            .expect("last_run must be extracted when NEXT is n/a");
        assert_eq!(last.to_rfc3339(), "2026-07-29T12:37:55+00:00");
    }

    #[test]
    fn never_run_service_is_never_not_ok() {
        // Supervisor REPRO: defaults Result=success + ExecMainStatus=0 with empty start.
        let r = last_result_from_props("", "success", Some(0), None);
        assert!(matches!(r, LastResult::Never), "expected Never, got {r:?}");
        let r2 = last_result_from_props("n/a", "success", Some(0), None);
        assert!(matches!(r2, LastResult::Never), "{r2:?}");
    }

    #[test]
    fn real_success_after_start() {
        let r = last_result_from_props(
            "Wed 2026-07-29 15:00:00 EEST",
            "success",
            Some(0),
            Some(Utc::now()),
        );
        assert!(matches!(r, LastResult::Ok { exit_code: 0 }), "{r:?}");
    }

    #[test]
    fn transient_state_is_enabled() {
        assert!(unit_file_state_enabled("transient"));
        assert!(unit_file_state_enabled("enabled"));
        assert!(!unit_file_state_enabled("disabled"));
        assert!(!unit_file_state_enabled("masked"));
    }

    #[test]
    fn parse_oncalendar_schedule() {
        let cal = "{ OnCalendar=*-*-* 6:00:00 ; next_elapse=Thu 2026-07-30 06:00:00 EEST }";
        let (expr, human) = parse_timer_schedule(cal, "");
        assert!(expr.contains("OnCalendar="), "{expr}");
        assert!(
            expr.contains("6:00:00") || expr.contains("06:00:00"),
            "{expr}"
        );
        assert!(
            !human.contains(".timer"),
            "should not just echo unit name: {human}"
        );
        assert!(
            human.to_lowercase().contains("calendar") || human.contains("OnCalendar"),
            "{human}"
        );
    }

    #[test]
    fn parse_on_active_sec() {
        let mono = "{ OnActiveUSec=50min ; next_elapse=n/a }";
        let (expr, human) = parse_timer_schedule("", mono);
        assert!(expr.contains("OnActiveSec=50min"), "{expr}");
        assert!(!human.is_empty());
    }
}
