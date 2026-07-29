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
    // JSON is ideal when available (systemd ≥ 248-ish); fall back to parseable plain.
    cmd.args([
        "list-timers",
        "--all",
        "--no-pager",
        "--no-legend",
    ]);
    let out = cmd
        .output()
        .map_err(|e| format!("spawn systemctl: {e}"))?;
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

    // Enrich with ExecMainStatus / unit details where possible
    for task in &mut tasks {
        enrich_timer(task, user);
    }
    Ok(tasks)
}

/// Parse a `systemctl list-timers --all --no-legend` line.
///
/// Columns (when both NEXT and LEFT present):
/// NEXT LEFT LAST PASSED UNIT ACTIVATES
///
/// Times may be `n/a`. Unit is the first `*.timer` token.
fn parse_timer_line(line: &str, user: bool) -> Option<DiscoveredTask> {
    let kind = if user {
        SourceKind::SystemdUser
    } else {
        SourceKind::SystemdSystem
    };
    // Find *.timer token
    let parts: Vec<&str> = line.split_whitespace().collect();
    let unit = parts.iter().copied().find(|p| p.ends_with(".timer"))?;
    let activates = parts
        .iter()
        .copied()
        .find(|p| p.ends_with(".service"))
        .unwrap_or("");

    // NEXT is first timestamp-ish field(s). systemctl prints:
    // "Wed 2026-07-29 15:38:10 EEST" or "n/a"
    let next_run = parse_next_from_line(line);
    let last_run = parse_last_from_line(line);

    let scope = if user { "user" } else { "system" };
    let source = format!("systemd:{scope}:{unit}");
    let id = task_id(kind, &[&source]);

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
        schedule_expr: unit.to_string(),
        human_explanation: explain_systemd(unit),
        next_run,
        last_run,
        last_result: LastResult::Unknown, // filled by enrich
        enabled: true,
        writable: false,
        write_block_reason: Some(if user {
            "v1 is display-only for systemd user timers (no enable/disable via Bellman yet)"
                .into()
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
    // Take leading "Day YYYY-MM-DD HH:MM:SS TZ"
    parse_systemctl_timestamp(line)
}

fn parse_last_from_line(line: &str) -> Option<DateTime<Utc>> {
    // Collect all "YYYY-MM-DD HH:MM:SS" wall times from the line.
    // Normal: NEXT then LAST → two timestamps, LAST is the second.
    // When NEXT is `n/a`, only LAST remains → one timestamp is LAST.
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
    // Skip weekday if present
    let s = line.trim_start();
    let s = if s.len() > 4 && s.as_bytes()[3] == b' ' && s.as_bytes()[0].is_ascii_alphabetic() {
        &s[4..]
    } else {
        s
    };
    parse_ymd_hms_prefix(s)
}

fn parse_ymd_hms_prefix(s: &str) -> Option<DateTime<Utc>> {
    // "2026-07-29 15:38:10 EEST" or without tz
    let s = s.trim_start();
    if s.len() < 19 {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(&s[..19], "%Y-%m-%d %H:%M:%S").ok()?;
    // Interpret in local timezone (systemctl prints local wall time)
    let local = Local.from_local_datetime(&naive).single()?;
    Some(local.with_timezone(&Utc))
}

fn enrich_timer(task: &mut DiscoveredTask, user: bool) {
    let unit = task
        .schedule_expr
        .clone();
    // Active state
    let mut cmd = Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.args(["show", &unit, "-p", "ActiveState", "-p", "UnitFileState", "-p", "Triggers"]);
    if let Ok(out) = cmd.output() {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("ActiveState=") {
                    if v == "inactive" {
                        // still may be enabled
                    }
                }
                if let Some(v) = line.strip_prefix("UnitFileState=") {
                    task.enabled = v == "enabled" || v == "static" || v == "linked" || v == "enabled-runtime";
                    if v == "disabled" || v == "masked" {
                        task.enabled = false;
                    }
                }
                if let Some(v) = line.strip_prefix("Triggers=") {
                    if !v.is_empty() {
                        task.command = format!("{unit} → {v}");
                    }
                }
            }
        }
    }

    // Last result from the triggered service
    let service = task
        .command
        .split("→")
        .nth(1)
        .map(str::trim)
        .filter(|s| s.ends_with(".service"))
        .map(|s| s.to_string());
    if let Some(svc) = service {
        let mut cmd = Command::new("systemctl");
        if user {
            cmd.arg("--user");
        }
        cmd.args(["show", &svc, "-p", "ExecMainStatus", "-p", "Result", "-p", "ExecMainCode"]);
        if let Ok(out) = cmd.output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut status: Option<i32> = None;
                let mut result = String::new();
                for line in text.lines() {
                    if let Some(v) = line.strip_prefix("ExecMainStatus=") {
                        status = v.parse().ok();
                    }
                    if let Some(v) = line.strip_prefix("Result=") {
                        result = v.to_string();
                    }
                }
                if let Some(code) = status {
                    // Only trust when Result indicates a finished run
                    if result == "success" || code == 0 {
                        task.last_result = LastResult::Ok { exit_code: code };
                    } else if !result.is_empty() && result != "success" {
                        task.last_result = LastResult::Failed { exit_code: code };
                    } else {
                        // No real evidence of a completed run
                        task.last_result = LastResult::Unknown;
                    }
                } else {
                    task.last_result = LastResult::Unknown;
                }
            }
        }
    }

    // Prefer NEXT from list-timers (already set). Optionally refine via
    // systemctl show NextElapseUSecRealtime — but list-timers is the acceptance
    // source of truth, so leave next_run as parsed.
    let _ = user;
}

/// Fetch journal logs for a timer's service.
pub fn timer_logs(task: &DiscoveredTask, lines: usize) -> Result<String, String> {
    let user = matches!(task.source_kind, SourceKind::SystemdUser);
    let unit = task
        .command
        .split("→")
        .nth(1)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(task.schedule_expr.as_str());
    let mut cmd = Command::new("journalctl");
    if user {
        cmd.arg("--user");
    }
    cmd.args([
        "-u",
        unit,
        "-n",
        &lines.to_string(),
        "--no-pager",
    ]);
    let out = cmd
        .output()
        .map_err(|e| format!("journalctl: {e}"))?;
    // journalctl may return non-zero if empty; still return stdout
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sample_line() {
        let line = "Wed 2026-07-29 15:38:10 EEST  233ms Wed 2026-07-29 15:37:55 EEST      14s ago oom-protect.timer            oom-protect.service";
        let t = parse_timer_line(line, false).expect("parse");
        assert_eq!(t.schedule_expr, "oom-protect.timer");
        assert!(t.next_run.is_some(), "next_run");
        assert!(t.last_run.is_some(), "last_run");
        assert!(t.source.contains("oom-protect.timer"));
    }

    #[test]
    fn last_run_when_next_is_na() {
        // Auditor REPRO: NEXT = n/a but LAST is present.
        let line = "n/a                         n/a Wed 2026-07-29 15:37:55 EEST      14s ago foo.timer                    foo.service";
        let t = parse_timer_line(line, false).expect("parse");
        assert!(t.next_run.is_none(), "next should be None for n/a");
        let last = t.last_run.expect("last_run must be extracted when NEXT is n/a");
        // Local EEST (UTC+3) 15:37:55 → 12:37:55Z
        assert_eq!(last.to_rfc3339(), "2026-07-29T12:37:55+00:00");
    }
}
