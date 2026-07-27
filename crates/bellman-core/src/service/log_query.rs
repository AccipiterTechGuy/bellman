//! Read access to the JSONL event log for the GUI / Tauri shell.
//!
//! The shell renders a per-timer filtered log tail on the All-timers page; this
//! module keeps the read path in one place so a future read-from-archive
//! upgrade only touches one site.

use crate::events::{read_events, EventRecord, ReadStats};
use std::path::Path;

/// A read-only view of the live event log path.
#[derive(Debug, Clone)]
pub struct LogPath {
    pub current: std::path::PathBuf,
    pub archive_dir: std::path::PathBuf,
}

/// Resolve `<db parent>/logs/events.current.jsonl` (the live append-only file).
pub fn current_log_path(db_path: &Path) -> std::path::PathBuf {
    crate::service::run_now::resolve_logs_dir(db_path).join(crate::events::CURRENT_FILE_NAME)
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
