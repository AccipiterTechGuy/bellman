//! Scan snapshot persistence for `bellman scan --diff` drift detection.

use crate::visible::types::{DiscoveredTask, ScanDiff, ScanResult, TaskChange};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

/// Default snapshot path under Bellman data dir.
pub fn default_snapshot_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".bellman").join("scan-snapshot.json")
}

/// Load previous scan result, if any.
pub fn load_snapshot(path: &Path) -> Result<Option<ScanResult>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|e| format!("read snapshot: {e}"))?;
    let r: ScanResult =
        serde_json::from_str(&text).map_err(|e| format!("parse snapshot: {e}"))?;
    Ok(Some(r))
}

/// Persist scan result for future --diff.
pub fn save_snapshot(path: &Path, result: &ScanResult) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir snapshot: {e}"))?;
    }
    let text = serde_json::to_string_pretty(result).map_err(|e| format!("serialize: {e}"))?;
    fs::write(path, text).map_err(|e| format!("write snapshot: {e}"))?;
    Ok(())
}

/// Diff current scan against previous snapshot.
pub fn diff_scans(previous: Option<&ScanResult>, current: &ScanResult) -> ScanDiff {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    let prev_map: std::collections::BTreeMap<&str, &DiscoveredTask> = previous
        .map(|p| p.tasks.iter().map(|t| (t.id.as_str(), t)).collect())
        .unwrap_or_default();
    let curr_map: std::collections::BTreeMap<&str, &DiscoveredTask> =
        current.tasks.iter().map(|t| (t.id.as_str(), t)).collect();

    for (id, t) in &curr_map {
        match prev_map.get(id) {
            None => added.push((*t).clone()),
            Some(old) => {
                push_changes(&mut changed, old, t);
            }
        }
    }
    for (id, t) in &prev_map {
        if !curr_map.contains_key(id) {
            removed.push((*t).clone());
        }
    }

    ScanDiff {
        previous_at: previous.map(|p| p.scanned_at),
        current_at: current.scanned_at,
        added,
        removed,
        changed,
    }
}

fn push_changes(out: &mut Vec<TaskChange>, old: &DiscoveredTask, new: &DiscoveredTask) {
    let pairs = [
        ("command", old.command.as_str(), new.command.as_str()),
        (
            "schedule_expr",
            old.schedule_expr.as_str(),
            new.schedule_expr.as_str(),
        ),
        ("enabled", bool_str(old.enabled), bool_str(new.enabled)),
        ("source", old.source.as_str(), new.source.as_str()),
        ("owner", old.owner.as_str(), new.owner.as_str()),
    ];
    for (field, b, a) in pairs {
        if b != a {
            out.push(TaskChange {
                id: new.id.clone(),
                field: field.into(),
                before: b.to_string(),
                after: a.to_string(),
            });
        }
    }
    let nb = old
        .next_run
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "-".into());
    let na = new
        .next_run
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "-".into());
    // next_run naturally drifts — only report if schedule/command also same? Spec
    // says "changed since last scan"; include next_run changes.
    if nb != na {
        out.push(TaskChange {
            id: new.id.clone(),
            field: "next_run".into(),
            before: nb,
            after: na,
        });
    }
    let _ = Utc::now();
}

fn bool_str(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visible::types::{LastResult, SourceKind};
    use chrono::TimeZone;

    fn task(id: &str, cmd: &str) -> DiscoveredTask {
        DiscoveredTask {
            id: id.into(),
            source_kind: SourceKind::CronUser,
            source: "crontab:t".into(),
            owner: "t".into(),
            command: cmd.into(),
            stdin_payload: None,
            schedule_expr: "0 * * * *".into(),
            human_explanation: String::new(),
            next_run: None,
            last_run: None,
            last_result: LastResult::Unknown,
            enabled: true,
            writable: true,
            write_block_reason: None,
            timezone: None,
            line_no: Some(1),
            raw_line: None,
            disabled_original: None,
            platform_note: None,
        }
    }

    #[test]
    fn detects_added_removed_changed() {
        let prev = ScanResult {
            scanned_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            platform: "linux".into(),
            filter: "all".into(),
            count: 2,
            tasks: vec![task("a", "old"), task("b", "keep")],
            warnings: vec![],
        };
        let curr = ScanResult {
            scanned_at: Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap(),
            platform: "linux".into(),
            filter: "all".into(),
            count: 2,
            tasks: vec![task("b", "keep"), task("c", "new"), task("a", "newcmd")],
            warnings: vec![],
        };
        let d = diff_scans(Some(&prev), &curr);
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].id, "c");
        assert!(d.removed.is_empty());
        assert!(d.changed.iter().any(|c| c.id == "a" && c.field == "command"));
    }
}
