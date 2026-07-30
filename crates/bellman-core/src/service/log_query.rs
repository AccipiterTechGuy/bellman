//! Read access to the JSONL event log for the GUI / Tauri shell.
//!
//! The shell renders a per-timer filtered log tail on the All-timers page; this
//! module keeps the read path in one place. Calendar truth also needs retained
//! archives after weekly rotation — see [`read_log_history`].

use crate::events::{read_events, EventRecord, ReadStats, CURRENT_FILE_NAME};
use std::fs;
use std::path::{Path, PathBuf};

/// A read-only view of the live event log path.
#[derive(Debug, Clone)]
pub struct LogPath {
    pub current: std::path::PathBuf,
    pub archive_dir: std::path::PathBuf,
}

/// Resolve `<db parent>/logs/events.current.jsonl` (the live append-only file).
pub fn current_log_path(db_path: &Path) -> std::path::PathBuf {
    crate::service::run_now::resolve_logs_dir(db_path).join(CURRENT_FILE_NAME)
}

/// Resolve the `logs/` directory that holds `events.current.jsonl` + `archive/`.
pub fn logs_dir_from_data(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// Read all parseable events from the live log file.
///
/// `limit` (when set) caps the number of records returned (most-recent first
/// after filtering). When `timer_id` is `Some`, only records whose
/// `timer_id` matches are returned.
pub fn read_log_tail(
    path: &Path,
    timer_id: Option<uuid::Uuid>,
    limit: Option<usize>,
) -> std::io::Result<(Vec<EventRecord>, ReadStats)> {
    let (mut recs, stats) = read_events(path)?;
    if let Some(id) = timer_id {
        recs.retain(|r| r.timer_id == Some(id));
    }
    if let Some(n) = limit {
        // Most recent first; the file is append-only so a Vec reversal + take
        // is the cheapest correct path.
        recs.reverse();
        recs.truncate(n);
        recs.reverse();
    }
    Ok((recs, stats))
}

/// Read retained history: `logs/archive/events-*.jsonl` (sorted) then
/// `logs/events.current.jsonl`.
///
/// Used by the calendar truth model so weekly rotation does not erase
/// durable outcomes from Week/Month views while archives remain in the
/// retention window. Missing dirs/files yield empty (not error).
pub fn read_log_history(logs_dir: &Path) -> std::io::Result<(Vec<EventRecord>, ReadStats)> {
    let mut all = Vec::new();
    let mut stats = ReadStats::default();

    let archive_dir = logs_dir.join("archive");
    if archive_dir.is_dir() {
        let mut paths: Vec<PathBuf> = fs::read_dir(&archive_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("events-") && n.ends_with(".jsonl"))
            })
            .collect();
        paths.sort();
        for path in paths {
            match read_events(&path) {
                Ok((recs, s)) => {
                    stats.records += s.records;
                    stats.skipped += s.skipped;
                    stats.lines += s.lines;
                    all.extend(recs);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }

    let current = logs_dir.join(CURRENT_FILE_NAME);
    if current.is_file() {
        match read_events(&current) {
            Ok((recs, s)) => {
                stats.records += s.records;
                stats.skipped += s.skipped;
                stats.lines += s.lines;
                all.extend(recs);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }

    Ok((all, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{RunState, EventLog, EventLogConfig, EventRecord};
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn read_log_history_includes_rotated_archive() {
        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        let mut log = EventLog::open(
            EventLogConfig::new(&logs).with_retention(Duration::from_secs(30 * 24 * 3600)),
        )
        .unwrap();
        log.emit(
            EventRecord::new(RunState::WakeFailed)
                .with_message("in-current-then-rotated")
                .with_error("boom"),
        )
        .unwrap();
        let archived = log.rotate().unwrap();
        assert!(archived.is_some(), "rotate should produce an archive");
        // Fresh current with a second event.
        log.emit(EventRecord::new(RunState::Fired).with_message("still-current"))
            .unwrap();

        let (recs, stats) = read_log_history(&logs).unwrap();
        assert!(stats.records >= 2);
        assert_eq!(recs.len(), 2);
        let kinds: Vec<_> = recs.iter().map(|r| r.kind).collect();
        assert!(kinds.contains(&RunState::WakeFailed));
        assert!(kinds.contains(&RunState::Fired));
        // Archived failure still present after rotation.
        assert!(recs.iter().any(|r| {
            r.kind == RunState::WakeFailed
                && r.message.as_deref() == Some("in-current-then-rotated")
        }));
    }

    #[test]
    fn read_log_history_empty_when_missing() {
        let dir = TempDir::new().unwrap();
        let (recs, stats) = read_log_history(&dir.path().join("logs")).unwrap();
        assert!(recs.is_empty());
        assert_eq!(stats.records, 0);
    }
}
