//! `at` queue discovery (`atq` / `at -c`).

use crate::visible::explain::explain_at;
use crate::visible::id::task_id;
use crate::visible::types::{DiscoveredTask, LastResult, SourceKind};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use std::process::Command;

pub fn discover_at() -> (Vec<DiscoveredTask>, Vec<String>) {
    let mut warnings = Vec::new();
    let out = match Command::new("atq").output() {
        Ok(o) => o,
        Err(e) => {
            warnings.push(format!("atq not available: {e}"));
            return (Vec::new(), warnings);
        }
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.trim().is_empty() {
            warnings.push(format!("atq: {}", err.trim()));
        }
        // empty queue often still exits 0; non-zero with empty → treat as none
        return (Vec::new(), warnings);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut tasks = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(t) = parse_atq_line(line) {
            tasks.push(t);
        }
    }
    (tasks, warnings)
}

/// `atq` format (typical):
/// `job_number\tdate time queue user`
/// e.g. `2\tWed Jul 29 16:00:00 2026 a sami`
fn parse_atq_line(line: &str) -> Option<DiscoveredTask> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let job_id = parts[0];
    // rest is date… queue user — last is user, second-to-last is queue letter
    let (user, when_str) = if parts.len() >= 3 {
        let user = parts[parts.len() - 1];
        let when = parts[1..parts.len() - 2].join(" ");
        (user.to_string(), when)
    } else {
        ("?".into(), parts[1..].join(" "))
    };

    let command = fetch_at_command(job_id).unwrap_or_else(|| format!("(at job {job_id})"));
    let next = parse_at_time(&when_str);
    let source = format!("at:{job_id}");
    let id = task_id(SourceKind::At, &[&source, &user, &command]);

    Some(DiscoveredTask {
        id,
        source_kind: SourceKind::At,
        source,
        owner: user,
        command,
        schedule_expr: when_str.clone(),
        human_explanation: explain_at(&when_str),
        next_run: next,
        last_run: None,
        last_result: LastResult::Never,
        enabled: true,
        writable: false,
        write_block_reason: Some(
            "v1 is display-only for at jobs (use `atrm` manually to cancel)".into(),
        ),
        timezone: None,
        line_no: None,
        raw_line: Some(line.to_string()),
        disabled_original: None,
        platform_note: None,
    })
}

fn fetch_at_command(job_id: &str) -> Option<String> {
    let out = Command::new("at")
        .args(["-c", job_id])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Script ends with user command after a marker; take last non-empty non-shell line
    // Heuristic: lines after "\n}" of shell setup — take last non-comment non-empty.
    let mut last = None;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with("export ")
            || t.starts_with("umask ")
            || t.starts_with("cd ")
            || t == "{"
            || t == "}"
        {
            continue;
        }
        last = Some(t.to_string());
    }
    last
}

fn parse_at_time(s: &str) -> Option<DateTime<Utc>> {
    // Try common atq formats
    let formats = [
        "%a %b %d %H:%M:%S %Y",
        "%Y-%m-%d %H:%M:%S",
        "%b %d, %Y %H:%M",
    ];
    for fmt in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s.trim(), fmt) {
            if let Some(local) = Local.from_local_datetime(&naive).single() {
                return Some(local.with_timezone(&Utc));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line() {
        let line = "2\tWed Jul 29 16:00:00 2026 a sami";
        // tab may be spaces in our split
        let line = line.replace('\t', " ");
        let t = parse_atq_line(&line).expect("parse");
        assert_eq!(t.owner, "sami");
        assert!(t.source.starts_with("at:"));
    }
}
