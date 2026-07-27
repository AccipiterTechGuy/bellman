//! JSONL appender, weekly rotation, archive retention.

use super::record::EventRecord;
use chrono::{Datelike, Utc};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Current (active) log file name under the logs root.
pub const CURRENT_FILE_NAME: &str = "events.current.jsonl";

/// Default archive retention (product: last 30 days of fired events).
pub const DEFAULT_RETENTION_DAYS: u64 = 30;

/// Configuration for [`EventLog`].
#[derive(Debug, Clone)]
pub struct EventLogConfig {
    /// Directory holding `events.current.jsonl` and `archive/`.
    pub logs_dir: PathBuf,
    /// Archive retention window.
    pub retention: Duration,
}

impl EventLogConfig {
    /// Build config for a logs directory with the product default retention.
    pub fn new(logs_dir: impl Into<PathBuf>) -> Self {
        Self {
            logs_dir: logs_dir.into(),
            retention: Duration::from_secs(DEFAULT_RETENTION_DAYS * 24 * 60 * 60),
        }
    }

    pub fn with_retention(mut self, retention: Duration) -> Self {
        self.retention = retention;
        self
    }
}

/// Errors from the event log.
#[derive(Debug)]
pub enum EventLogError {
    Io(String),
    Serialize(String),
}

impl std::fmt::Display for EventLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(s) | Self::Serialize(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for EventLogError {}

impl From<io::Error> for EventLogError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<serde_json::Error> for EventLogError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialize(e.to_string())
    }
}

pub type EventLogResult<T> = Result<T, EventLogError>;

/// Append-only JSONL event log with weekly rotation and archive retention.
///
/// - **Append**: one self-contained line; flush after write; no per-line fsync.
/// - **Rotate**: atomic rename of current → `archive/events-<ISO-week>.jsonl`,
///   then open a fresh current file (sync of the old file happens before rename).
/// - **Retain**: delete archive files whose mtime is older than `retention`.
pub struct EventLog {
    config: EventLogConfig,
    /// Open append handle for the current file (lazy).
    file: Option<File>,
}

impl EventLog {
    /// Open (or create) the log under `config.logs_dir`.
    pub fn open(config: EventLogConfig) -> EventLogResult<Self> {
        fs::create_dir_all(&config.logs_dir)?;
        fs::create_dir_all(config.logs_dir.join("archive"))?;
        let mut log = Self {
            config,
            file: None,
        };
        log.ensure_open()?;
        Ok(log)
    }

    /// Convenience: open under `<data_dir>/logs` with default retention.
    pub fn open_under(data_dir: impl AsRef<Path>) -> EventLogResult<Self> {
        Self::open(EventLogConfig::new(data_dir.as_ref().join("logs")))
    }

    pub fn config(&self) -> &EventLogConfig {
        &self.config
    }

    pub fn current_path(&self) -> PathBuf {
        self.config.logs_dir.join(CURRENT_FILE_NAME)
    }

    pub fn archive_dir(&self) -> PathBuf {
        self.config.logs_dir.join("archive")
    }

    /// Append one event as a single JSON line and flush (no fsync).
    pub fn append(&mut self, record: &EventRecord) -> EventLogResult<()> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        self.ensure_open()?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| EventLogError::Io("event log file not open".into()))?;
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    /// Builder-style append (moves the record).
    pub fn emit(&mut self, record: EventRecord) -> EventLogResult<()> {
        self.append(&record)
    }

    /// Weekly rotation: sync + atomic rename current → dated archive, open fresh.
    ///
    /// Archive name: `events-<YYYY>-W<WW>.jsonl` (ISO week, zero-padded).
    /// If that archive name already exists (re-rotate same week), a numeric
    /// suffix is added: `events-2026-W31.2.jsonl`.
    ///
    /// Returns the archive path, or `None` when the current file was empty /
    /// missing (still ensures a fresh empty current file exists).
    pub fn rotate(&mut self) -> EventLogResult<Option<PathBuf>> {
        // Drop the open handle so rename is uncontested.
        if let Some(file) = self.file.take() {
            file.sync_all()?;
            drop(file);
        }

        let current = self.current_path();
        if !current.exists() {
            self.ensure_open()?;
            return Ok(None);
        }
        let meta = fs::metadata(&current)?;
        if meta.len() == 0 {
            // Empty: leave as-is (or recreate).
            self.ensure_open()?;
            return Ok(None);
        }

        let archive_dir = self.archive_dir();
        fs::create_dir_all(&archive_dir)?;
        let archive_path = unique_archive_path(&archive_dir, Utc::now())?;

        // Atomic same-filesystem rename.
        fs::rename(&current, &archive_path)?;
        // Best-effort fsync of the directory for durability of the rename.
        if let Ok(dir) = File::open(&archive_dir) {
            let _ = dir.sync_all();
        }

        self.ensure_open()?;
        Ok(Some(archive_path))
    }

    /// Delete archive files whose mtime is older than the configured retention.
    ///
    /// Returns the number of files removed.
    pub fn retain(&self) -> EventLogResult<usize> {
        let archive_dir = self.archive_dir();
        if !archive_dir.exists() {
            return Ok(0);
        }
        let cutoff = SystemTime::now()
            .checked_sub(self.config.retention)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut removed = 0usize;
        for entry in fs::read_dir(&archive_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("events-") || !name.ends_with(".jsonl") {
                continue;
            }
            let meta = entry.metadata()?;
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if mtime < cutoff {
                fs::remove_file(&path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Rotate then retain — the weekly prune step for the JSONL side.
    pub fn rotate_and_retain(&mut self) -> EventLogResult<(Option<PathBuf>, usize)> {
        let archived = self.rotate()?;
        let removed = self.retain()?;
        Ok((archived, removed))
    }

    fn ensure_open(&mut self) -> EventLogResult<()> {
        if self.file.is_some() {
            return Ok(());
        }
        fs::create_dir_all(&self.config.logs_dir)?;
        let path = self.current_path();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        self.file = Some(file);
        Ok(())
    }
}

/// Build `archive_dir/events-YYYY-Www.jsonl`, adding `.N` before `.jsonl` on clash.
fn unique_archive_path(archive_dir: &Path, now: chrono::DateTime<Utc>) -> EventLogResult<PathBuf> {
    let iso = now.iso_week();
    let base = format!("events-{}-W{:02}", iso.year(), iso.week());
    let candidate = archive_dir.join(format!("{base}.jsonl"));
    if !candidate.exists() {
        return Ok(candidate);
    }
    for n in 2u32..10_000 {
        let candidate = archive_dir.join(format!("{base}.{n}.jsonl"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(EventLogError::Io(format!(
        "could not allocate unique archive name under {}",
        archive_dir.display()
    )))
}
