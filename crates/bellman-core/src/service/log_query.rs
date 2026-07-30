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

/// Read the retained tail for the GUI / CLI: archives + the in-flight
/// `.rotating` source + current, deduplicated by `event_id`, with the timer
/// filter and limit applied AFTER dedupe. This is the one read path for log
/// tails (R11: physical duplicates from the at-least-once outbox and an
/// in-flight rotation are invisible to readers). Missing dirs/files yield
/// empty, never an error.
pub fn read_log_tail(
    logs_dir: &Path,
    timer_id: Option<uuid::Uuid>,
    limit: Option<usize>,
) -> std::io::Result<(Vec<EventRecord>, ReadStats)> {
    let (mut recs, mut stats) = read_log_history(logs_dir)?;
    if let Some(id) = timer_id {
        recs.retain(|r| r.timer_id == Some(id));
    }
    // The limit applies to the most recent records (the file is append-only).
    if let Some(n) = limit {
        if recs.len() > n {
            let drop = recs.len() - n;
            recs.drain(..drop);
        }
    }
    stats.records = recs.len();
    Ok((recs, stats))
}

/// Read retained history: `logs/archive/events-*.jsonl[.gz]` (sorted), then
/// the R11 `.rotating` source while a rotation journal is active (never the
/// partial gzip temp), then `logs/events.current.jsonl`.
///
/// Used by the calendar truth model so weekly rotation does not erase
/// durable outcomes from Week/Month views while archives remain in the
/// retention window. Missing dirs/files yield empty (not error). Records are
/// deduplicated by `event_id` (a crash mid-rotation can briefly leave both a
/// plain staging archive and its compressed twin — and the at-least-once
/// outbox can re-append after a crash between sync and mark-published).
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
                        .is_some_and(|n| {
                            n.starts_with("events-")
                                && (n.ends_with(".jsonl") || n.ends_with(".jsonl.gz"))
                        })
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

    // The plain `.rotating` source of an in-flight/interrupted rotation:
    // while the journal is active these lines are nowhere else, so history
    // must not briefly lose them.
    let rotating = logs_dir.join(crate::events::ROTATING_FILE_NAME);
    if rotating.is_file() {
        match read_events(&rotating) {
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

    // Dedupe by event_id (first occurrence wins), preserving read order.
    let mut seen = std::collections::HashSet::new();
    let dupes = all.len();
    all.retain(|r| seen.insert(r.event_id));
    let dupes = dupes - all.len();
    stats.records -= dupes;

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

    #[test]
    fn read_log_tail_dedupes_by_event_id_and_includes_rotating() {
        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        let mut log = EventLog::open(EventLogConfig::new(&logs)).unwrap();
        let timer = uuid::Uuid::new_v4();
        let rec = EventRecord::new(RunState::Completed)
            .with_timer(timer, "t")
            .with_message("synced-but-unmarked");
        // The at-least-once crash window: the same line twice on disk.
        log.emit(rec.clone()).unwrap();
        log.emit(rec.clone()).unwrap();

        // Tail: one logical record, not two.
        let (recs, _) = read_log_tail(&logs, None, None).unwrap();
        assert_eq!(recs.len(), 1, "readers dedupe by event_id");

        // Mid-rotation: current renamed away to .rotating — history is
        // still live, never a false empty.
        fs::rename(
            logs.join(CURRENT_FILE_NAME),
            logs.join(crate::events::ROTATING_FILE_NAME),
        )
        .unwrap();
        let (recs, _) = read_log_tail(&logs, None, None).unwrap();
        assert_eq!(recs.len(), 1, ".rotating source is read while in flight");
        assert_eq!(recs[0].message.as_deref(), Some("synced-but-unmarked"));

        // Filter + limit apply after dedupe.
        let (recs, _) = read_log_tail(&logs, Some(uuid::Uuid::new_v4()), None).unwrap();
        assert!(recs.is_empty());
        let (recs, _) = read_log_tail(&logs, Some(timer), Some(1)).unwrap();
        assert_eq!(recs.len(), 1);
    }
}
