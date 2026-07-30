//! JSONL appender, weekly + size-triggered rotation, gzip archives, retention.
//!
//! Rotation (IK2 / R12): `events.current.jsonl` stays plain text — grep-ability
//! of the live log is a feature. It rotates at the ISO-week boundary (the
//! weekly prune) **or before an append would take it past
//! `max_current_bytes`** (default 64 MiB), whichever comes first. Rotated
//! archives are gzip-compressed (`events-<YYYY>-W<ww>[.N].jsonl.gz`); readers
//! transparently read both plain (legacy) and compressed archives.
//!
//! Retention: archives older than `retention` are removed first, then oldest
//! archives until `current + archives` fits `budget_bytes` (default 1 GiB).
//! The live current file is never deleted. Every removal is returned to the
//! caller so it can be logged — never silent.

use super::record::EventRecord;
use chrono::{Datelike, Utc};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Current (active) log file name under the logs root.
pub const CURRENT_FILE_NAME: &str = "events.current.jsonl";

/// Default archive retention (product: last 30 days of fired events).
pub const DEFAULT_RETENTION_DAYS: u64 = 30;

/// Default size cap for the live current file — rotate before crossing it.
pub const DEFAULT_MAX_CURRENT_BYTES: u64 = 64 * 1024 * 1024;

/// Default retained-log budget: current + final archives stay within 1 GiB.
pub const DEFAULT_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;

/// Configuration for [`EventLog`].
#[derive(Debug, Clone)]
pub struct EventLogConfig {
    /// Directory holding `events.current.jsonl` and `archive/`.
    pub logs_dir: PathBuf,
    /// Archive retention window.
    pub retention: Duration,
    /// Rotate before an append would take the current file past this size.
    pub max_current_bytes: u64,
    /// Retained-log budget for current + final archives.
    pub budget_bytes: u64,
}

impl EventLogConfig {
    /// Build config for a logs directory with the product default retention.
    pub fn new(logs_dir: impl Into<PathBuf>) -> Self {
        Self {
            logs_dir: logs_dir.into(),
            retention: Duration::from_secs(DEFAULT_RETENTION_DAYS * 24 * 60 * 60),
            max_current_bytes: DEFAULT_MAX_CURRENT_BYTES,
            budget_bytes: DEFAULT_BUDGET_BYTES,
        }
    }

    pub fn with_retention(mut self, retention: Duration) -> Self {
        self.retention = retention;
        self
    }

    pub fn with_max_current_bytes(mut self, bytes: u64) -> Self {
        self.max_current_bytes = bytes;
        self
    }

    pub fn with_budget_bytes(mut self, bytes: u64) -> Self {
        self.budget_bytes = bytes;
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

/// What retention removed in one pass (the caller logs this — never silent).
#[derive(Debug, Clone, Default)]
pub struct RetainReport {
    /// Archives removed because their mtime aged past the retention window.
    pub aged: Vec<PathBuf>,
    /// Oldest-first archives removed to fit the retained-log budget.
    pub budget: Vec<PathBuf>,
    /// Total bytes freed across both passes.
    pub bytes_removed: u64,
}

impl RetainReport {
    pub fn removed_count(&self) -> usize {
        self.aged.len() + self.budget.len()
    }
}

/// Append-only JSONL event log with weekly + size-triggered rotation, gzip
/// archives and age/budget retention.
///
/// - **Append**: one self-contained line; flush after write; no per-line fsync.
///   Several processes can hold `EventLog` handles on the same file, so every
///   append first verifies its handle still points at the live current file
///   (same OS file identity) and reopens when another process rotated it
///   away — appends never land in a renamed-away inode.
/// - **Rotate**: sync + rename current → plain staging archive, gzip the
///   staging file to `events-<ISO-week>[.N].jsonl.gz`, then remove the staging
///   file. A stale writer that still managed to append to the staging inode
///   mid-pass triggers a re-compression so those late lines are kept. A crash
///   leaves the plain staging archive in place — readers read both forms, so
///   rotation never opens a hole in history.
/// - **Retain**: delete archives older than `retention`, then oldest archives
///   until current + archives fit `budget_bytes`. Current is never deleted.
pub struct EventLog {
    config: EventLogConfig,
    /// Open append handle for the current file (lazy).
    file: Option<File>,
}

/// OS file identity used to detect that the path our handle points at was
/// rotated away by another process.
#[cfg(unix)]
fn file_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

/// Windows equivalent of [`file_id`] (volume serial + file index).
#[cfg(windows)]
fn file_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::windows::fs::MetadataExt;
    Some((meta.volume_serial_number()?, meta.file_index()?))
}

/// Platforms without a file-id API skip the stale-handle check.
#[cfg(not(any(unix, windows)))]
fn file_id(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    None
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
    ///
    /// Product writers should prefer [`Self::open_under_configured`], which
    /// honors `config.json` rotation/retention knobs — this one always uses
    /// the product defaults (tests, tooling).
    pub fn open_under(data_dir: impl AsRef<Path>) -> EventLogResult<Self> {
        Self::open(EventLogConfig::new(data_dir.as_ref().join("logs")))
    }

    /// Open under `<data_dir>/logs` honoring the operator's `config.json`:
    /// `retention_days`, `log_rotation_max_bytes` and
    /// `log_retention_budget_bytes`. Missing/corrupt config falls back to the
    /// product defaults.
    pub fn open_under_configured(data_dir: impl AsRef<Path>) -> EventLogResult<Self> {
        let cfg = crate::app_config::AppConfig::load(data_dir.as_ref()).unwrap_or_default();
        Self::open(
            EventLogConfig::new(data_dir.as_ref().join("logs"))
                .with_retention(cfg.retention())
                .with_max_current_bytes(cfg.log_rotation_max_bytes)
                .with_budget_bytes(cfg.log_retention_budget_bytes),
        )
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
    ///
    /// Size trigger: when the line would take the current file past
    /// `max_current_bytes`, the log rotates (and retains) **before** writing
    /// the line, so the live file never crosses the threshold except for a
    /// single oversized line. The rotation is recorded as a `pruned` event on
    /// the fresh current file — never silent.
    pub fn append(&mut self, record: &EventRecord) -> EventLogResult<()> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        // Re-anchor on the live file first: another process may have rotated
        // since our handle was opened. The size check reads the PATH's
        // metadata (never the possibly-stale handle's).
        self.ensure_fresh_handle()?;
        let current_len = fs::metadata(self.current_path()).map_or(0, |m| m.len());
        if current_len > 0 && current_len + line.len() as u64 > self.config.max_current_bytes {
            let (archived, report) = self.rotate_and_retain()?;
            let rotation_note = EventRecord::new(super::record::RunState::Pruned)
                .with_message("log_rotation")
                .with_detail(serde_json::json!({
                    "reason": "size_threshold",
                    "max_current_bytes": self.config.max_current_bytes,
                    "archived": archived.as_ref().map(|p| p.display().to_string()),
                    "archives_removed": report.removed_count(),
                    "bytes_removed": report.bytes_removed,
                }));
            // Append directly: current is fresh and empty, no recursion risk.
            self.append_direct(&rotation_note)?;
        }
        self.append_direct(record)
    }

    /// Builder-style append (moves the record so call sites can chain builders).
    #[allow(clippy::needless_pass_by_value)]
    pub fn emit(&mut self, record: EventRecord) -> EventLogResult<()> {
        self.append(&record)
    }

    fn append_direct(&mut self, record: &EventRecord) -> EventLogResult<()> {
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

    /// Rotation: sync + rename current → plain staging archive, gzip it to
    /// `events-<YYYY>-W<ww>[.N].jsonl.gz`, remove the staging file, open fresh.
    ///
    /// Returns the compressed archive path, or `None` when the current file
    /// was empty / missing (still ensures a fresh empty current file exists).
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
        let staging = unique_archive_path(&archive_dir, Utc::now())?;

        // Atomic same-filesystem rename to the plain staging name. Readers
        // include plain archives, so a crash from here on loses nothing.
        fs::rename(&current, &staging)?;
        // Best-effort fsync of the directory for durability of the rename.
        if let Ok(dir) = File::open(&archive_dir) {
            let _ = dir.sync_all();
        }

        // Compress staging → temp gzip → rename to final. A writer holding a
        // stale handle can still append to the staging inode while we work
        // (its next append re-anchors, but a write already in flight lands
        // here): re-compress until the staging file stops growing so those
        // late lines are folded into the archive instead of being deleted
        // with it. Bounded — well-behaved writers re-anchor immediately.
        let final_path = gz_path_for(&staging);
        let tmp_gz = archive_dir.join(format!(
            ".{}.tmp",
            final_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "archive.jsonl.gz".into())
        ));
        for _pass in 0..5 {
            let before = fs::metadata(&staging).map_or(0, |m| m.len());
            gzip_file(&staging, &tmp_gz)?;
            fs::rename(&tmp_gz, &final_path)?;
            let after = fs::metadata(&staging).map_or(0, |m| m.len());
            if after <= before {
                break;
            }
        }
        fs::remove_file(&staging)?;
        if let Ok(dir) = File::open(&archive_dir) {
            let _ = dir.sync_all();
        }

        self.ensure_open()?;
        Ok(Some(final_path))
    }

    /// Re-anchor the append handle on the live current file. After another
    /// process rotated, our handle points at the renamed-away staging inode
    /// and appends through it would vanish when the staging file is removed.
    /// Compare OS file identities; reopen when they differ.
    fn ensure_fresh_handle(&mut self) -> EventLogResult<()> {
        self.ensure_open()?;
        let Some(handle) = self.file.as_ref() else {
            return Ok(());
        };
        let handle_id = handle.metadata().ok().and_then(|m| file_id(&m));
        let path_id = fs::metadata(self.current_path())
            .ok()
            .and_then(|m| file_id(&m));
        let stale = match (handle_id, path_id) {
            (Some(h), Some(p)) => h != p,
            // Path missing (mid-rotation elsewhere) or no id support: treat a
            // missing path as stale so we recreate the live file.
            (_, None) => fs::metadata(self.current_path()).is_err(),
            (None, Some(_)) => false,
        };
        if stale {
            self.file = None;
            self.ensure_open()?;
        }
        Ok(())
    }

    /// Retention pass: age out old archives, then enforce the byte budget.
    ///
    /// Returns the report of everything removed (the caller logs it).
    pub fn retain(&self) -> EventLogResult<RetainReport> {
        let mut report = RetainReport::default();
        let archive_dir = self.archive_dir();
        if !archive_dir.exists() {
            return Ok(report);
        }

        self.clean_strays(&archive_dir)?;

        // 1. Age: archives (plain or compressed) older than retention.
        let cutoff = SystemTime::now()
            .checked_sub(self.config.retention)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        for entry in fs::read_dir(&archive_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !is_archive_file(&path) {
                continue;
            }
            let meta = entry.metadata()?;
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if mtime < cutoff {
                report.bytes_removed += meta.len();
                fs::remove_file(&path)?;
                report.aged.push(path);
            }
        }

        // 2. Budget: current + archives must fit; oldest archives go first.
        //    The live current file is never deleted.
        let mut archives = list_archives(&archive_dir)?;
        // Oldest first: mtime, then name for stability.
        archives.sort_by(|a, b| {
            let ma = a.1;
            let mb = b.1;
            ma.cmp(&mb).then_with(|| a.0.cmp(&b.0))
        });
        let mut total: u64 = archives.iter().map(|(_, _, len)| *len).sum();
        total += fs::metadata(self.current_path()).map_or(0, |m| m.len());
        for (path, _, len) in archives {
            if total <= self.config.budget_bytes {
                break;
            }
            fs::remove_file(&path)?;
            total = total.saturating_sub(len);
            report.bytes_removed += len;
            report.budget.push(path);
        }

        Ok(report)
    }

    /// Remove interrupted-rotation strays: partial gzip temps, and a plain
    /// staging archive whose compressed twin already exists (crash between
    /// the final rename and the staging cleanup).
    fn clean_strays(&self, archive_dir: &Path) -> EventLogResult<()> {
        for entry in fs::read_dir(archive_dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') && name.ends_with(".tmp") {
                let _ = fs::remove_file(&path);
                continue;
            }
            if name.starts_with("events-") && name.ends_with(".jsonl") {
                let gz = gz_path_for(&path);
                if gz.exists() {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(())
    }

    /// Rotate then retain — the weekly prune step for the JSONL side.
    pub fn rotate_and_retain(&mut self) -> EventLogResult<(Option<PathBuf>, RetainReport)> {
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

/// True for `events-*.jsonl` / `events-*.jsonl.gz` archive files (never the
/// live current file, never temps).
fn is_archive_file(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| {
                n.starts_with("events-") && (n.ends_with(".jsonl") || n.ends_with(".jsonl.gz"))
            })
}

/// Archive files with (path, mtime, len).
fn list_archives(archive_dir: &Path) -> EventLogResult<Vec<(PathBuf, SystemTime, u64)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(archive_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !is_archive_file(&path) {
            continue;
        }
        let meta = entry.metadata()?;
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        out.push((path, mtime, meta.len()));
    }
    Ok(out)
}

/// `events-….jsonl` → `events-….jsonl.gz`.
fn gz_path_for(plain: &Path) -> PathBuf {
    let mut name = plain
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(".gz");
    plain.with_file_name(name)
}

/// Stream-compress `src` to `dst` (gzip), fsyncing the result.
fn gzip_file(src: &Path, dst: &Path) -> EventLogResult<()> {
    let input = File::open(src)?;
    let output = File::create(dst)?;
    let mut encoder = flate2::write::GzEncoder::new(BufWriter::new(output), flate2::Compression::default());
    io::copy(&mut BufReader::new(input), &mut encoder)?;
    let writer = encoder.finish()?;
    let file = writer
        .into_inner()
        .map_err(|e| EventLogError::Io(e.to_string()))?;
    file.sync_all()?;
    Ok(())
}

/// Build `archive_dir/events-YYYY-Www.jsonl`, adding `.N` before `.jsonl` on clash.
fn unique_archive_path(archive_dir: &Path, now: chrono::DateTime<Utc>) -> EventLogResult<PathBuf> {
    let iso = now.iso_week();
    let base = format!("events-{}-W{:02}", iso.year(), iso.week());
    let candidate = archive_dir.join(format!("{base}.jsonl"));
    if !candidate.exists() && !gz_path_for(&candidate).exists() {
        return Ok(candidate);
    }
    for n in 2u32..10_000 {
        let candidate = archive_dir.join(format!("{base}.{n}.jsonl"));
        if !candidate.exists() && !gz_path_for(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(EventLogError::Io(format!(
        "could not allocate unique archive name under {}",
        archive_dir.display()
    )))
}
