//! R11 — one interprocess writer owns the event log.
//!
//! Every producer enqueues into the SQLite outbox (`Store::enqueue_event`) —
//! SQLite already serialises across processes, which is the whole reason it
//! is the funnel. One publisher, elected by an OS file lock
//! (`logs/publisher.lock`), performs every append **and** every rotation:
//!
//! - A line counts as durable when it is **synced** (`fdatasync`), not merely
//!   flushed — the outbox row is deleted only after the sync. Delivery is
//!   at-least-once: a crash between sync and delete re-appends the same
//!   event, so on leadership acquisition the publisher scans the current
//!   tail (and the journal's `.rotating` source) for pending `event_id`s and
//!   marks the ones already physically present; every reader still dedupes
//!   by `event_id`.
//! - The publisher has a live feeder: `publish_cycle` runs after local
//!   enqueues, after recovery/rotation, and on a periodic safety tick (the
//!   watcher's 1 s loop) — a different process's SQLite commit cannot rely
//!   on an in-process wakeup.
//! - Append errors surface: a failed append leaves the outbox row pending
//!   for retry and is reported in `logs/publisher_health.json` until a
//!   later cycle succeeds.
//! - Rotation is crash-safe through the durable SQLite journal: record
//!   intent → rename current to `.rotating` → compress to temp → rename to
//!   the final archive → delete the source → new current → clear journal.
//!   Recovery after a crash or newly acquired lease rolls the interrupted
//!   phase forward BEFORE any append or another rotation.

use super::log::{gz_path_for, gzip_file, unique_archive_path, EventLog, EventLogConfig};
use super::{EventLogError, EventLogResult, EventRecord, RetainReport, RunState};
use crate::reply::gate::{self, GateGuard};
use crate::store::{RotationJournal, RotationPhase, Store};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Publisher election lease, under the logs dir.
pub const PUBLISHER_LEASE_NAME: &str = "publisher.lock";
/// Operator-visible publisher health, under the logs dir.
pub const HEALTH_FILE_NAME: &str = "publisher_health.json";
/// The plain working file of an in-flight rotation (readers include it
/// while the journal is active; never the partial gzip temp).
pub const ROTATING_FILE_NAME: &str = "events.rotating.jsonl";
/// Health doc wire schema (R1).
pub const HEALTH_SCHEMA_V1: &str = "bellman-publisher-health/1";

/// How far back from the end of a file the publisher scans for already
/// physically present `event_id`s. Events are low-rate; 256 KiB of tail is
/// far more than any append-then-crash window can outrun.
const TAIL_SCAN_BYTES: u64 = 256 * 1024;

/// One publish cycle's outcome.
#[derive(Debug, Default, Clone)]
pub struct PublishReport {
    /// Whether this process held the lease this cycle.
    pub leader: bool,
    /// Lines appended + synced + marked published.
    pub published: usize,
    /// Archive produced by a size-triggered rotation this cycle.
    pub rotated: Option<PathBuf>,
    /// The cycle-ending error, if any (rows stay pending for retry).
    pub error: Option<String>,
}

/// Operator-visible publisher health (`publisher_health.json`).
#[derive(Debug, Clone, Serialize)]
pub struct PublisherHealth {
    pub schema: String,
    pub leader: bool,
    pub pending_events: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok_at: Option<DateTime<Utc>>,
}

/// The elected single writer. Cheap to construct; leadership is attempted
/// per cycle, so a CLI that enqueues while the GUI is up simply fails to
/// acquire and lets the GUI's tick drain.
pub struct EventPublisher {
    log: EventLog,
    lease_path: PathBuf,
    lease: Option<GateGuard>,
    last_error: Option<String>,
    last_error_at: Option<DateTime<Utc>>,
    last_ok_at: Option<DateTime<Utc>>,
    /// Last health doc written, to avoid rewriting an unchanged file.
    last_written: Option<(bool, Option<String>, u64)>,
}

impl EventPublisher {
    /// Open the publisher over `<data_dir>/logs` honoring `config.json`
    /// rotation/retention knobs.
    pub fn open(data_dir: impl AsRef<Path>) -> EventLogResult<Self> {
        let cfg = crate::app_config::AppConfig::load(data_dir.as_ref()).unwrap_or_default();
        Self::with_config(
            EventLogConfig::new(data_dir.as_ref().join("logs"))
                .with_retention(cfg.retention())
                .with_max_current_bytes(cfg.log_rotation_max_bytes)
                .with_budget_bytes(cfg.log_retention_budget_bytes),
        )
    }

    /// Open with an explicit log config (tests).
    pub fn with_config(config: EventLogConfig) -> EventLogResult<Self> {
        let lease_path = config.logs_dir.join(PUBLISHER_LEASE_NAME);
        Ok(Self {
            log: EventLog::open(config)?,
            lease_path,
            lease: None,
            last_error: None,
            last_error_at: None,
            last_ok_at: None,
            last_written: None,
        })
    }

    pub fn current_path(&self) -> PathBuf {
        self.log.current_path()
    }

    pub fn is_leader(&self) -> bool {
        self.lease.is_some()
    }

    /// Attempt (or confirm) leadership. `Ok(false)` means another process
    /// holds the lease — a follower, not an error.
    pub fn ensure_leadership(&mut self) -> EventLogResult<bool> {
        if self.lease.is_some() {
            return Ok(true);
        }
        match gate::try_acquire_file(&self.lease_path) {
            Ok(Some(guard)) => {
                self.lease = Some(guard);
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => Err(EventLogError::Io(format!("publisher lease: {e}"))),
        }
    }

    /// Best-effort drain for one-shot producers (CLI): take the lease if
    /// free and run one cycle. A GUI holding the lease drains within its
    /// own safety tick instead — either way the row is never stranded.
    pub fn drain_best_effort(data_dir: &Path, store: &Store) {
        if let Ok(mut publisher) = EventPublisher::open(data_dir) {
            let report = publisher.publish_cycle(store);
            if let Some(e) = report.error {
                eprintln!("bellman: event publisher: {e}");
            }
        }
    }

    /// The full cycle: recover an interrupted rotation, reconcile the tail,
    /// drain the outbox (append + fdatasync + mark), rotate when the size
    /// threshold would be crossed, and publish health. This is the live
    /// feeder — it runs after local enqueues and on the periodic tick, so a
    /// row committed by another process never waits for an in-process signal.
    pub fn publish_cycle(&mut self, store: &Store) -> PublishReport {
        let mut report = PublishReport::default();
        match self.ensure_leadership() {
            Ok(false) => {
                self.write_health(store, false, None);
                return report;
            }
            Ok(true) => {}
            Err(e) => {
                report.error = Some(e.to_string());
                self.set_error(e.to_string());
                self.write_health(store, false, report.error.clone());
                return report;
            }
        }
        report.leader = true;
        if let Err(e) = self.recover_rotation(store) {
            report.error = Some(e.to_string());
            self.set_error(e.to_string());
            self.write_health(store, true, report.error.clone());
            return report;
        }
        if let Err(e) = self.reconcile_tail(store) {
            report.error = Some(e.to_string());
            self.set_error(e.to_string());
            self.write_health(store, true, report.error.clone());
            return report;
        }
        match self.drain(store, &mut report) {
            Ok(()) => {
                self.last_error = None;
                self.last_error_at = None;
                self.last_ok_at = Some(Utc::now());
            }
            Err(e) => {
                report.error = Some(e.to_string());
                self.set_error(e.to_string());
            }
        }
        self.write_health(store, true, report.error.clone());
        report
    }

    /// Drain pending outbox rows in enqueue order: append, **fdatasync**,
    /// then delete the row. The first failure stops the drain — rows behind
    /// it stay pending and the error is operator-visible until a later cycle
    /// succeeds.
    fn drain(&mut self, store: &Store, report: &mut PublishReport) -> EventLogResult<()> {
        for (event_id, payload) in pending(store)? {
            let line_len = payload.len() as u64 + 1;
            let current_len = self.log.current_len();
            if current_len > 0 && current_len + line_len > self.log.config().max_current_bytes {
                if let Some(archive) = self.rotate_journaled(store)? {
                    report.rotated = Some(archive.clone());
                    // Record the rotation itself — enqueued, drained next cycle.
                    store.enqueue_event(&rotation_note(
                        "size_threshold",
                        self.log.config().max_current_bytes,
                        &archive,
                        0,
                        0,
                    ))?;
                }
            }
            let mut line = payload;
            line.push('\n');
            self.log.append_line_synced(&line)?;
            store.mark_event_published(event_id)?;
            report.published += 1;
        }
        Ok(())
    }

    /// On leadership acquisition (and every cycle, cheaply): pending rows
    /// whose `event_id` is already physically present in the current tail —
    /// or in the journal's `.rotating` source — were appended before a crash
    /// but never marked. Mark them published without re-appending. Readers
    /// still dedupe by `event_id` regardless; this shrinks the window.
    fn reconcile_tail(&mut self, store: &Store) -> EventLogResult<()> {
        let mut present = tail_event_ids(&self.log.current_path());
        if let Ok(Some(journal)) = store.rotation_journal() {
            present.extend(tail_event_ids(&journal.rotating));
        }
        if present.is_empty() {
            return Ok(());
        }
        for (event_id, _) in pending(store)? {
            if present.contains(&event_id) {
                store.mark_event_published(event_id)?;
            }
        }
        Ok(())
    }

    /// Journaled rotation (the only rotation path). Phases per R11; each is
    /// durable in SQLite before the next filesystem step.
    pub fn rotate_journaled(&mut self, store: &Store) -> EventLogResult<Option<PathBuf>> {
        // Never rotate with a journal outstanding — recover it first.
        self.recover_rotation(store)?;

        self.log.close_handle()?;
        let current = self.log.current_path();
        if fs::metadata(&current).map_or(0, |m| m.len()) == 0 {
            self.log.reopen()?;
            return Ok(None);
        }
        let archive_dir = self.log.archive_dir();
        fs::create_dir_all(&archive_dir)?;
        let final_path = gz_path_for(&unique_archive_path(&archive_dir, Utc::now())?);
        let gz_tmp = archive_dir.join(format!(
            ".{}.tmp",
            final_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "archive.jsonl.gz".into())
        ));
        let journal = RotationJournal {
            source: current.clone(),
            rotating: self.log.config().logs_dir.join(ROTATING_FILE_NAME),
            gz_tmp,
            final_path,
            phase: RotationPhase::Renamed,
            started_at: Utc::now(),
        };

        // 1. Record intent, rename current → .rotating, sync the parent dir.
        store.set_rotation_journal(&journal)?;
        fs::rename(&current, &journal.rotating)?;
        sync_dir(&self.log.config().logs_dir);

        // 2. Compress → temp, sync, rename to the final archive, sync dir.
        gzip_file(&journal.rotating, &journal.gz_tmp)?;
        fs::rename(&journal.gz_tmp, &journal.final_path)?;
        sync_dir(&archive_dir);
        let mut j = journal.clone();
        j.phase = RotationPhase::Finalized;
        store.set_rotation_journal(&j)?;

        // 3. Delete the .rotating source, sync the parent dir.
        fs::remove_file(&journal.rotating)?;
        sync_dir(&archive_dir);
        j.phase = RotationPhase::SourceRemoved;
        store.set_rotation_journal(&j)?;

        // 4. New current only after the archive is durable; clear journal.
        self.log.reopen()?;
        store.clear_rotation_journal()?;
        Ok(Some(journal.final_path))
    }

    /// Roll an interrupted rotation forward BEFORE any append or another
    /// rotation: reconcile pending `event_id`s against current AND the
    /// `.rotating` source, finish the archive, delete redundant
    /// source/temp artifacts only after verifying the final archive, ensure
    /// a new current exists, then clear the journal.
    pub fn recover_rotation(&mut self, store: &Store) -> EventLogResult<()> {
        let Ok(Some(journal)) = store.rotation_journal() else {
            return Ok(());
        };
        // The handle may point at the renamed-away inode — drop it so the
        // new current is recreated, not the old one appended to.
        self.log.close_handle()?;
        // Reconcile first: a synced line renamed just before the crash is in
        // the .rotating source, not the current tail.
        let mut present = tail_event_ids(&self.log.current_path());
        present.extend(tail_event_ids(&journal.rotating));
        for (event_id, _) in pending(store)? {
            if present.contains(&event_id) {
                store.mark_event_published(event_id)?;
            }
        }

        let final_ok = journal.final_path.exists() && verify_gzip(&journal.final_path);
        if final_ok {
            // Final archive verified — redundant source/temp go.
            let _ = fs::remove_file(&journal.rotating);
            let _ = fs::remove_file(&journal.gz_tmp);
        } else if journal.rotating.exists() {
            // Roll forward from the plain source (never the partial temp).
            let _ = fs::remove_file(&journal.gz_tmp);
            gzip_file(&journal.rotating, &journal.gz_tmp)?;
            fs::rename(&journal.gz_tmp, &journal.final_path)?;
            sync_dir(&self.log.archive_dir());
            let _ = fs::remove_file(&journal.rotating);
        } else {
            let _ = fs::remove_file(&journal.gz_tmp);
        }
        // Ensure the new current exists (the old handle was renamed away).
        self.log.reopen()?;
        store.clear_rotation_journal()?;
        Ok(())
    }

    /// Journaled rotation + archive retention (the prune path). Retention
    /// also runs under the lease — the publisher owns every rotation.
    pub fn rotate_and_retain(
        &mut self,
        store: &Store,
    ) -> EventLogResult<(Option<PathBuf>, RetainReport)> {
        let mut out = (None, RetainReport::default());
        if !self.ensure_leadership()? {
            return Ok(out);
        }
        self.recover_rotation(store)?;
        out.0 = self.rotate_journaled(store)?;
        out.1 = self.log.retain()?;
        Ok(out)
    }

    /// Record an error for the health doc.
    fn set_error(&mut self, e: String) {
        self.last_error = Some(e);
        self.last_error_at = Some(Utc::now());
    }

    /// Write the operator-visible health doc when it changed.
    fn write_health(&mut self, store: &Store, leader: bool, cycle_error: Option<String>) {
        let pending_events = store.count_pending_events().unwrap_or(0);
        let error = cycle_error.or_else(|| self.last_error.clone());
        let key = (leader, error.clone(), pending_events);
        if self.last_written.as_ref() == Some(&key) {
            return;
        }
        let doc = PublisherHealth {
            schema: HEALTH_SCHEMA_V1.to_string(),
            leader,
            pending_events,
            last_error: error,
            last_error_at: self.last_error_at,
            last_ok_at: self.last_ok_at,
        };
        if crate::slots::atomic_write_json(
            &self.log.config().logs_dir,
            HEALTH_FILE_NAME,
            &doc,
        )
        .is_ok()
        {
            self.last_written = Some(key);
        }
    }
}

/// The `pruned` note recording a rotation (never silent).
fn rotation_note(
    reason: &str,
    max_current_bytes: u64,
    archive: &Path,
    archives_removed: usize,
    bytes_removed: u64,
) -> EventRecord {
    EventRecord::new(RunState::Pruned)
        .with_message("log_rotation")
        .with_detail(serde_json::json!({
            "reason": reason,
            "max_current_bytes": max_current_bytes,
            "archived": archive.display().to_string(),
            "archives_removed": archives_removed,
            "bytes_removed": bytes_removed,
        }))
}

/// Pending rows, mapped through the store error type.
fn pending(store: &Store) -> EventLogResult<Vec<(Uuid, String)>> {
    store
        .pending_events(10_000)
        .map_err(|e| EventLogError::Io(format!("read outbox: {e}")))
}

/// `event_id`s found in the last [`TAIL_SCAN_BYTES`] of `path`. Tolerant:
/// unparseable lines (a torn tail) are skipped.
fn tail_event_ids(path: &Path) -> HashSet<Uuid> {
    let mut out = HashSet::new();
    let Ok(meta) = fs::metadata(path) else {
        return out;
    };
    let len = meta.len();
    if len == 0 {
        return out;
    }
    let start = len.saturating_sub(TAIL_SCAN_BYTES);
    let Ok(mut file) = fs::File::open(path) else {
        return out;
    };
    use std::io::{Read, Seek};
    if file.seek(std::io::SeekFrom::Start(start)).is_err() {
        return out;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return out;
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if start > 0 {
        // Starting mid-file: the first line is partial — skip it.
        lines.next();
    }
    for line in lines {
        if let Ok(rec) = serde_json::from_str::<EventRecord>(line) {
            out.insert(rec.event_id);
        }
    }
    out
}

/// Verify a gzip archive is fully decodable.
fn verify_gzip(path: &Path) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut decoder = flate2::read::GzDecoder::new(file);
    let mut sink = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut sink).is_ok()
}

/// Best-effort directory fsync (rename durability where the platform
/// supports it).
fn sync_dir(dir: &Path) {
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(test)]
mod tests;
