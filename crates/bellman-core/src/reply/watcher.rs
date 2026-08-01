//! The file transport for the reply channel.
//!
//! This module owns everything file-aware: safe reading (no-follow, regular
//! file, size cap), the mid-write debounce, quarantine on stable-invalid
//! bytes, stale-file deletion, the pre-fire barrier, the startup scan and
//! the periodic reconciler. Its job ends at "here is a parsed reply" —
//! validation, transitions and outbox rows all live in [`super::engine`],
//! which does not know a file exists (IK6's socket transport calls the same
//! function). The background thread that drives this scan lives in the ONE
//! watcher (`crate::slots::watcher`) — this module adds no parallel loop.
//!
//! Watcher events are latency hints; the periodic full rescan is truth — the
//! same philosophy as the slot watcher, whose debounce constant is reused.
//!
//! Gate discipline: the R10 per-timer gate is acquired by THIS module's
//! outer phases (reply scan, deadline pass, reconcile, barrier caller),
//! never inside engine functions — a shard is a per-open-description flock,
//! so re-entering it in the same thread would self-deadlock.

use crate::store::{Store, Timer, TimerId};
use crate::tree::{reply_file_name, STATUS_FILE_NAME};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::document::{ReplyDocument, MAX_REPLY_FILE_BYTES, REPLY_SCHEMA_V1};
use super::engine::{DeadlineKind, ReplyEngine, ReplyResult};
use super::gate;
use super::notification::{
    fire_notification_name, fires_dir, write_fire_notification, FireNotification,
};
use super::quarantine::{fnv1a64_hex, quarantine_bytes, quarantine_dir, quarantine_unread};

/// Reuse the slot watcher's debounce: a parse failure is usually a file
/// being written, so identical invalid bytes must be stable across this
/// window before they are rejected.
pub const DEFAULT_DEBOUNCE: Duration = crate::slots::DEFAULT_DEBOUNCE;

/// How often the watcher rescans (the periodic truth pass) — also the
/// publisher safety tick and the deadline granularity.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Reconcile the folder projections from the database every this many polls.
pub const RECONCILE_EVERY_POLLS: u32 = 60;

/// What the safe reader found at a reply path.
enum ReplyRead {
    Bytes(Vec<u8>),
    /// Over the whole-file cap — the body is never read.
    Oversize(u64),
    /// Symlink / reparse point / not a regular file (FIFO, device, …).
    Special(&'static str),
    Missing,
}

/// Read a reply path safely (R9/R12):
///
/// - open **no-follow** and non-blocking (a FIFO can never park us),
/// - verify a regular file on the OPENED HANDLE (never a second path lookup),
/// - size-check from that handle; over 64 KB the body is never read,
/// - read at most the cap from the same handle.
fn read_reply_file(path: &Path) -> std::io::Result<ReplyRead> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::io::FromRawFd;
        let flags = rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK;
        match rustix::fs::open(path, flags, rustix::fs::Mode::empty()) {
            Ok(fd) => unsafe {
                std::fs::File::from_raw_fd(std::os::unix::io::IntoRawFd::into_raw_fd(fd))
            },
            Err(rustix::io::Errno::NOENT) => return Ok(ReplyRead::Missing),
            Err(rustix::io::Errno::LOOP) => return Ok(ReplyRead::Special("symlink")),
            Err(e) => return Err(std::io::Error::from_raw_os_error(e.raw_os_error())),
        }
    };
    #[cfg(windows)]
    let file = {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ReplyRead::Missing),
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            return Ok(ReplyRead::Special("symlink"));
        }
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Ok(ReplyRead::Special("reparse point"));
            }
        }
        std::fs::File::open(path)?
    };
    #[cfg(not(any(unix, windows)))]
    compile_error!("reply file transport: unsupported platform");

    let meta = file.metadata()?;
    if !meta.is_file() {
        return Ok(ReplyRead::Special("not a regular file"));
    }
    let len = meta.len();
    if len > MAX_REPLY_FILE_BYTES {
        return Ok(ReplyRead::Oversize(len));
    }
    let mut buf = Vec::with_capacity(len as usize);
    let mut take = file.take(MAX_REPLY_FILE_BYTES + 1);
    take.read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_REPLY_FILE_BYTES {
        return Ok(ReplyRead::Oversize(buf.len() as u64));
    }
    Ok(ReplyRead::Bytes(buf))
}

/// Mid-write tracking: a parse failure is usually a file being written, so
/// only bytes that are stable-invalid across the debounce window are
/// condemned. Changed bytes reset the window; condemned content that never
/// changes produces exactly one rejection and one artifact.
#[derive(Default)]
pub struct InvalidTracker {
    by_path: HashMap<PathBuf, InvalidEntry>,
}

struct InvalidEntry {
    digest: String,
    since: DateTime<Utc>,
    condemned: bool,
}

/// Counts from one watcher pass (mostly for tests and ops logging).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PollStats {
    pub applied: usize,
    pub duplicates: usize,
    pub superseded: usize,
    pub rejected: usize,
    pub deadline_transitions: usize,
    pub errors: usize,
}

/// One full scan of every `reply-*.json` under the timers tree, plus the
/// monotonic deadline pass. `mono_now` is Bellman's monotonic clock (the
/// only clock deadlines count on); `now_wall` stamps transitions.
pub fn poll_once(
    engine: &ReplyEngine,
    store: &Store,
    now_wall: DateTime<Utc>,
    mono_now: Instant,
    tracker: &mut InvalidTracker,
) -> PollStats {
    let mut stats = PollStats::default();
    let timers = match store.list_timers() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bellman: reply watcher: list timers: {e}");
            stats.errors += 1;
            return stats;
        }
    };
    for timer in &timers {
        if let Err(e) = poll_timer(
            engine, store, timer, now_wall, mono_now, tracker, &mut stats,
        ) {
            eprintln!("bellman: reply watcher: timer {}: {e}", timer.id);
            stats.errors += 1;
        }
    }
    stats.deadline_transitions +=
        expire_all_deadlines(engine, store, now_wall, mono_now, &mut stats);
    stats
}

#[allow(clippy::too_many_arguments)]
fn poll_timer(
    engine: &ReplyEngine,
    store: &Store,
    timer: &Timer,
    now_wall: DateTime<Utc>,
    mono_now: Instant,
    tracker: &mut InvalidTracker,
    stats: &mut PollStats,
) -> ReplyResult<()> {
    let _gate = gate::acquire(&engine.data_dir, timer.id)?;
    let Some(folder) = engine.tree.folder_for(timer.id) else {
        return Ok(());
    };
    let entries = match std::fs::read_dir(&folder) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(file_run_id) = reply_run_id_from_name(&name) else {
            continue;
        };
        let path = entry.path();
        match read_reply_file(&path) {
            Ok(ReplyRead::Missing) => {}
            Ok(ReplyRead::Oversize(len)) => {
                condemn_unread(
                    engine,
                    store,
                    timer,
                    &path,
                    file_run_id,
                    len,
                    "oversize",
                    stats,
                );
            }
            Ok(ReplyRead::Special(kind)) => {
                condemn_unread(engine, store, timer, &path, file_run_id, 0, kind, stats);
            }
            Ok(ReplyRead::Bytes(bytes)) => {
                handle_bytes(
                    engine,
                    store,
                    timer,
                    &path,
                    file_run_id,
                    bytes,
                    now_wall,
                    mono_now,
                    tracker,
                    stats,
                );
            }
            Err(e) => {
                eprintln!("bellman: reply watcher: read {}: {e}", path.display());
                stats.errors += 1;
            }
        }
    }
    Ok(())
}

/// Parse the run id out of a `reply-<full run_id>.json` filename. Anything
/// else in the folder is not a reply channel and is ignored.
fn reply_run_id_from_name(name: &str) -> Option<Uuid> {
    let stem = name.strip_prefix("reply-")?.strip_suffix(".json")?;
    Uuid::parse_str(stem).ok()
}

/// The expected owner for the run a reply file names: the lifecycle row's
/// snapshot when present, else the timer's current integration owner.
fn expected_owner(store: &Store, timer: &Timer, run_id: Uuid) -> Option<String> {
    match store.get_run_state(run_id) {
        Ok(Some(row)) => Some(row.app_name),
        _ => store.get_timer_owner(timer.id).ok().flatten(),
    }
}

/// Exactly the pre-filled stub Bellman wrote, and nothing else: right
/// schema, `state: null`, the hint field, and — critically — the run id of
/// THIS filename with the expected owner. A hand edit that merely adds
/// `"hint"` and leaves `state: null` is NOT the stub and goes through
/// normal validation (and rejection).
fn is_untouched_stub(doc: &ReplyDocument, file_run_id: Uuid, expected_owner: Option<&str>) -> bool {
    doc.schema.as_deref() == Some(REPLY_SCHEMA_V1)
        && doc.state.is_none()
        && doc.hint.is_some()
        && doc.run_id == Some(file_run_id)
        && expected_owner.is_some()
        && doc.app_name.as_deref() == expected_owner
}

#[allow(clippy::too_many_arguments)]
fn handle_bytes(
    engine: &ReplyEngine,
    store: &Store,
    timer: &Timer,
    path: &Path,
    file_run_id: Uuid,
    bytes: Vec<u8>,
    now_wall: DateTime<Utc>,
    mono_now: Instant,
    tracker: &mut InvalidTracker,
    stats: &mut PollStats,
) {
    let digest = fnv1a64_hex(&bytes);
    let doc: ReplyDocument = match serde_json::from_slice(&bytes) {
        Ok(d) => d,
        Err(_) => {
            track_invalid(
                engine,
                store,
                timer,
                path,
                file_run_id,
                &bytes,
                digest,
                now_wall,
                tracker,
                stats,
            );
            return;
        }
    };
    tracker.by_path.remove(path);

    // The untouched stub: `state: null` is how Bellman tells "stub" from
    // "the app answered". Recognized EXACTLY — anything merely stub-shaped
    // falls through to validation.
    let owner = expected_owner(store, timer, file_run_id);
    if is_untouched_stub(&doc, file_run_id, owner.as_deref()) {
        return;
    }

    // A hand-edited run_id is never trusted: the document must name the run
    // its filename names, otherwise the previous-run path could be turned
    // into a deletion of the live current channel.
    if doc.run_id.is_some() && doc.run_id != Some(file_run_id) {
        condemn_bytes(
            engine,
            store,
            timer,
            path,
            file_run_id,
            &bytes,
            "run_id does not match the reply filename",
            stats,
        );
        return;
    }

    match engine.ingest(store, timer, &doc, &digest, now_wall, mono_now) {
        Ok(super::engine::IngestOutcome::Applied) => {
            stats.applied += 1;
            if let Err(e) = engine.project_status(store, timer, &file_run_id) {
                eprintln!("bellman: reply watcher: status projection: {e}");
                stats.errors += 1;
            }
        }
        Ok(super::engine::IngestOutcome::Duplicate) => stats.duplicates += 1,
        Ok(super::engine::IngestOutcome::Superseded) => {
            // A previous run's own file: ingest is done, the stale file goes.
            // The current run's file is never touched by this path — the
            // filename/document identity check above guarantees it.
            if let Err(e) = std::fs::remove_file(path) {
                eprintln!(
                    "bellman: reply watcher: remove stale {}: {e}",
                    path.display()
                );
            }
            stats.superseded += 1;
        }
        Ok(super::engine::IngestOutcome::Rejected(reason)) => {
            condemn_bytes(
                engine,
                store,
                timer,
                path,
                file_run_id,
                &bytes,
                reason.as_str(),
                stats,
            );
        }
        Err(e) => {
            eprintln!("bellman: reply watcher: ingest {}: {e}", path.display());
            stats.errors += 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn track_invalid(
    engine: &ReplyEngine,
    store: &Store,
    timer: &Timer,
    path: &Path,
    run_id: Uuid,
    bytes: &[u8],
    digest: String,
    now_wall: DateTime<Utc>,
    tracker: &mut InvalidTracker,
    stats: &mut PollStats,
) {
    match tracker.by_path.get_mut(path) {
        Some(entry) if entry.digest == digest => {
            if entry.condemned {
                return; // one rejection + one artifact per distinct content
            }
            if now_wall
                .signed_duration_since(entry.since)
                .to_std()
                .unwrap_or_default()
                >= DEFAULT_DEBOUNCE
            {
                condemn_bytes(
                    engine,
                    store,
                    timer,
                    path,
                    run_id,
                    bytes,
                    "invalid JSON",
                    stats,
                );
                entry.condemned = true;
            }
        }
        _ => {
            // First sight of these bytes (or they changed mid-write): start
            // the debounce window. Never quarantine on the first read.
            tracker.by_path.insert(
                path.to_path_buf(),
                InvalidEntry {
                    digest,
                    since: now_wall,
                    condemned: false,
                },
            );
        }
    }
}

/// Quarantine already-read condemned bytes (COPY semantics — the live file
/// stays in place) and log one `reply_rejected` per distinct content.
#[allow(clippy::too_many_arguments)]
fn condemn_bytes(
    engine: &ReplyEngine,
    store: &Store,
    timer: &Timer,
    path: &Path,
    run_id: Uuid,
    bytes: &[u8],
    reason: &str,
    stats: &mut PollStats,
) {
    let created = quarantine_locked(engine, |bad| {
        quarantine_bytes(
            bad,
            path,
            bytes,
            reason,
            serde_json::json!({ "timer_id": timer.id, "run_id": run_id }),
        )
    });
    match created {
        Ok(true) => {
            if let Err(e) = engine.log_rejection(store, timer, Some(run_id), reason) {
                eprintln!("bellman: reply watcher: log rejection: {e}");
            }
            stats.rejected += 1;
        }
        Ok(false) => {} // unchanged content: no new artifact, no new event
        Err(e) => {
            eprintln!("bellman: reply watcher: quarantine {}: {e}", path.display());
            stats.errors += 1;
        }
    }
}

/// Metadata-only quarantine for oversize / special files: no body bytes are
/// read or copied, the live path is left untouched.
#[allow(clippy::too_many_arguments)]
fn condemn_unread(
    engine: &ReplyEngine,
    store: &Store,
    timer: &Timer,
    path: &Path,
    run_id: Uuid,
    observed_len: u64,
    reason: &str,
    stats: &mut PollStats,
) {
    let created = quarantine_locked(engine, |bad| {
        quarantine_unread(
            bad,
            path,
            observed_len,
            reason,
            serde_json::json!({ "timer_id": timer.id, "run_id": run_id }),
        )
    });
    match created {
        Ok(true) => {
            if let Err(e) = engine.log_rejection(store, timer, Some(run_id), reason) {
                eprintln!("bellman: reply watcher: log rejection: {e}");
            }
            stats.rejected += 1;
        }
        Ok(false) => {}
        Err(e) => {
            eprintln!("bellman: reply watcher: quarantine {}: {e}", path.display());
            stats.errors += 1;
        }
    }
}

/// Run `f` holding the quarantine lock. Callers already hold the timer
/// shard — lock order is timer shard THEN `bad/` lock, never the reverse.
fn quarantine_locked(
    engine: &ReplyEngine,
    f: impl FnOnce(&Path) -> std::io::Result<super::quarantine::QuarantineOutcome>,
) -> std::io::Result<bool> {
    let bad = quarantine_dir(engine.tree.root());
    let _lock = gate::acquire_quarantine(&engine.data_dir)?;
    Ok(f(&bad)?.created)
}

/// Monotonic deadline pass: lazily reconstruct the book from persisted
/// wall deadlines (restart), expire what is due by the MONOTONIC clock,
/// and project status for each transition — each timer under its own gate.
fn expire_all_deadlines(
    engine: &ReplyEngine,
    store: &Store,
    now_wall: DateTime<Utc>,
    mono_now: Instant,
    stats: &mut PollStats,
) -> usize {
    if let Err(e) = engine.sync_deadline_book(store, now_wall, mono_now) {
        eprintln!("bellman: reply watcher: deadline book sync: {e}");
        stats.errors += 1;
    }
    let mut by_timer: HashMap<TimerId, Vec<(Uuid, DeadlineKind)>> = HashMap::new();
    for kind in [DeadlineKind::Pickup, DeadlineKind::Watchdog] {
        for run_id in engine.due_deadlines(kind, mono_now) {
            match store.get_run_state(run_id) {
                Ok(Some(row)) => by_timer
                    .entry(row.timer_id)
                    .or_default()
                    .push((run_id, kind)),
                _ => engine.clear_deadlines(run_id),
            }
        }
    }
    let mut n = 0;
    for (timer_id, runs) in by_timer {
        let Ok(_gate) = gate::acquire(&engine.data_dir, timer_id) else {
            stats.errors += 1;
            continue;
        };
        let Ok(Some(timer)) = store.get_timer(timer_id) else {
            continue;
        };
        for (run_id, kind) in runs {
            let transitioned = match kind {
                DeadlineKind::Pickup => engine.expire_pickup_one(store, &timer, run_id, now_wall),
                DeadlineKind::Watchdog => {
                    engine.expire_watchdog_one(store, &timer, run_id, now_wall)
                }
            };
            match transitioned {
                Ok(true) => {
                    n += 1;
                    if let Err(e) = engine.project_status(store, &timer, &run_id) {
                        eprintln!("bellman: reply watcher: deadline projection: {e}");
                        stats.errors += 1;
                    }
                }
                Ok(false) => engine.clear_deadlines(run_id),
                Err(e) => {
                    eprintln!("bellman: reply watcher: deadline {run_id}: {e}");
                    stats.errors += 1;
                }
            }
        }
    }
    n
}

/// What the pre-fire barrier read found.
pub enum BarrierRead {
    /// A parsed document naming THIS filename's run — ingest inside the
    /// fire transaction. `bytes` are kept for the post-commit quarantine
    /// copy when the ingest is semantically rejected.
    Valid {
        doc: Box<ReplyDocument>,
        digest: String,
        bytes: Vec<u8>,
    },
    /// Rejected on sight (identity mismatch or stable-invalid bytes):
    /// already quarantined (COPY semantics) and logged — the fire proceeds.
    Rejected,
    /// Nothing readable, the untouched stub, or a writer still mid-write
    /// past the bounded window (never quarantined here).
    None,
}

/// The pre-fire barrier read (R10): synchronously READ and parse the
/// previous run's reply file before the fire transaction may supersede it.
/// The same strict identity rule as the ordinary watcher applies: a
/// document whose `run_id` does not match its filename is rejected and
/// quarantined (copy-only), never carried into the transaction. Bounded:
/// one existing debounce window — a partial writer that keeps changing the
/// file must not hold firing forever, and is never quarantined here.
pub fn barrier_read(
    engine: &ReplyEngine,
    store: &Store,
    timer: &Timer,
    folder: &Path,
    prev_run_id: Uuid,
) -> BarrierRead {
    let path = folder.join(reply_file_name(prev_run_id));
    let bytes = match read_reply_file(&path) {
        Ok(ReplyRead::Bytes(b)) => b,
        _ => return BarrierRead::None, // nothing readable to fold in
    };
    let mut stats = PollStats::default();
    match serde_json::from_slice::<ReplyDocument>(&bytes) {
        Ok(doc)
            if is_untouched_stub(
                &doc,
                prev_run_id,
                expected_owner(store, timer, prev_run_id).as_deref(),
            ) =>
        {
            BarrierRead::None
        }
        Ok(doc) => {
            // A hand-edited run_id is never trusted — the document must
            // name the run its filename names.
            if doc.run_id.is_some() && doc.run_id != Some(prev_run_id) {
                condemn_bytes(
                    engine,
                    store,
                    timer,
                    &path,
                    prev_run_id,
                    &bytes,
                    "run_id does not match the reply filename",
                    &mut stats,
                );
                return BarrierRead::Rejected;
            }
            BarrierRead::Valid {
                doc: Box::new(doc),
                digest: fnv1a64_hex(&bytes),
                bytes,
            }
        }
        Err(_) => {
            std::thread::sleep(DEFAULT_DEBOUNCE);
            let Ok(ReplyRead::Bytes(second)) = read_reply_file(&path) else {
                return BarrierRead::None;
            };
            match serde_json::from_slice::<ReplyDocument>(&second) {
                Ok(doc) => {
                    if doc.run_id.is_some() && doc.run_id != Some(prev_run_id) {
                        condemn_bytes(
                            engine,
                            store,
                            timer,
                            &path,
                            prev_run_id,
                            &second,
                            "run_id does not match the reply filename",
                            &mut stats,
                        );
                        return BarrierRead::Rejected;
                    }
                    BarrierRead::Valid {
                        doc: Box::new(doc),
                        digest: fnv1a64_hex(&second),
                        bytes: second,
                    }
                }
                Err(_) if second == bytes => {
                    // Identical invalid bytes, stable past the window.
                    condemn_bytes(
                        engine,
                        store,
                        timer,
                        &path,
                        prev_run_id,
                        &second,
                        "invalid JSON",
                        &mut stats,
                    );
                    BarrierRead::Rejected
                }
                Err(_) => BarrierRead::None, // still changing: proceed
            }
        }
    }
}

/// Copy semantically rejected bytes into the quarantine (COPY semantics —
/// the live file is left in place). Idempotent per distinct content. Used
/// by the fire path after an in-transaction semantic rejection.
pub(crate) fn quarantine_rejected_bytes(
    engine: &ReplyEngine,
    timer: &Timer,
    path: &Path,
    run_id: Uuid,
    bytes: &[u8],
    reason: &str,
) {
    let _ = quarantine_locked(engine, |bad| {
        quarantine_bytes(
            bad,
            path,
            bytes,
            reason,
            serde_json::json!({ "timer_id": timer.id, "run_id": run_id }),
        )
    });
}

/// Write the fire notification under `slots/fires/` — only after
/// `status.json` and the reply stub exist (the notification is not eligible
/// while either required projection is missing).
pub fn publish_fire_notification(
    engine: &ReplyEngine,
    timer: &Timer,
    claim: &crate::store::RunClaim,
    fire_state: &str,
    folder: &Path,
    app_name: &str,
) -> std::io::Result<()> {
    let n = FireNotification::new(
        fire_state,
        timer.occurrence.kind().kind_label(),
        timer.id,
        &timer.name,
        app_name,
        claim.run_id,
        claim.scheduled_for,
        claim.claimed_at,
        folder.join(STATUS_FILE_NAME),
        Some(folder.join(reply_file_name(claim.run_id))),
        engine
            .ipc
            .as_ref()
            .map(|h| super::notification::IpcEndpoint {
                socket: h.socket_path().to_path_buf(),
            }),
    );
    write_fire_notification(&engine.data_dir.join("slots"), &n)?;
    Ok(())
}

/// Rebuild the database-owned projections for every live timer:
/// `status.json` from the accumulated row (re-read inside the gate, never a
/// stale snapshot), a missing stub create-only, and a missing fire
/// notification last. This is the same recovery startup performs — a failed
/// post-commit write is repaired here without waiting for a restart.
pub fn reconcile(engine: &ReplyEngine, store: &Store) -> usize {
    let mut repaired = 0;
    let timers = match store.list_timers() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bellman: reply reconciler: list timers: {e}");
            return 0;
        }
    };
    for timer in &timers {
        match reconcile_timer(engine, store, timer, &mut repaired) {
            Ok(attempt) => {
                // The gate is released — publication takes the timer shard
                // itself (flock is per open-file-description; re-acquiring
                // under our guard would deadlock).
                if let Some(proj) = attempt {
                    crate::reply::publication::attempt(
                        &engine.data_dir,
                        store,
                        &proj,
                        engine.ipc.as_ref(),
                    );
                }
            }
            Err(e) => eprintln!("bellman: reply reconciler: timer {}: {e}", timer.id),
        }
    }
    repaired
}

fn reconcile_timer(
    engine: &ReplyEngine,
    store: &Store,
    timer: &Timer,
    repaired: &mut usize,
) -> ReplyResult<Option<crate::store::TransportProjection>> {
    let _gate = gate::acquire(&engine.data_dir, timer.id)?;
    let Some(folder) = engine.tree.folder_for(timer.id) else {
        return Ok(None);
    };
    let Some(claim) = super::engine::current_claim(store, timer.id)? else {
        return Ok(None);
    };
    let Some(row) = store.get_run_state(claim.run_id)? else {
        return Ok(None);
    };
    // status.json: re-project from the row just read (inside the gate).
    let status_path = folder.join(STATUS_FILE_NAME);
    let status = crate::tree::RunStatus::from_run_state(timer, &claim, &row);
    let fresh = serde_json::to_vec_pretty(&status).map_err(|e| {
        super::engine::ReplyError::Tree(crate::tree::TreeError::Serialize(e.to_string()))
    })?;
    let stale = std::fs::read(&status_path)
        .map(|b| b != fresh)
        .unwrap_or(true);
    if stale {
        crate::tree::write_status(&engine.tree, timer, &status)?;
        *repaired += 1;
    }
    // Missing stub: create-only — an app can be writing at this exact
    // moment, and a lost O_EXCL race to a real reply is the correct outcome.
    // IK6: an IPC-selected run deliberately has no stub; never repair one
    // into existence (the folder README explains its absence).
    let stub_path = folder.join(reply_file_name(claim.run_id));
    let ipc_selected = row.selected_transport.as_deref() == Some(crate::store::TRANSPORT_IPC);
    if !ipc_selected && !stub_path.exists() {
        engine
            .tree
            .create_reply_stub(&folder, claim.run_id, &row.app_name)?;
        *repaired += 1;
    }
    // Fire notification last (never eligible before the two projections).
    // SCH1: when the fire transaction stored a transport projection for this
    // run, publication ownership lies there (cursor/obsolete rules, bounded
    // retry) — the reconciler returns an eligible projection for the caller
    // to attempt AFTER the gate drops. Rows that predate SCH1 keep the
    // legacy idempotent rewrite.
    match store.transport_projection(claim.run_id) {
        Ok(Some(proj)) => {
            if proj.state == crate::store::TransportProjection::PENDING
                && proj.next_attempt_at <= Utc::now()
            {
                return Ok(Some(proj));
            }
            // Published but the file vanished without pickup (crash window /
            // silent consumption): redelivery is allowed. IPC projections
            // have no file to vanish — their retry is the pump's.
            if proj.kind == crate::store::TransportProjection::KIND_FILE
                && proj.state == crate::store::TransportProjection::PUBLISHED
                && !std::path::Path::new(&proj.target_path).exists()
            {
                let _ = store.requeue_transport_projection(proj.run_id);
                return Ok(Some(proj));
            }
        }
        _ => {
            let fire_path = fires_dir(&engine.data_dir.join("slots"))
                .join(fire_notification_name(claim.run_id));
            if !fire_path.exists() && stub_path.exists() {
                publish_fire_notification(
                    engine,
                    timer,
                    &claim,
                    &row.state,
                    &folder,
                    &row.app_name,
                )?;
                *repaired += 1;
            }
        }
    }
    Ok(None)
}

/// Startup (R10): scan every `reply-*.json` BEFORE the scheduler fires
/// anything — an app can answer while Bellman is stopped, and superseding
/// before reading would silently record the outcome unknown. Then rebuild
/// stale projections and sweep quarantine temporaries.
pub fn startup_scan(engine: &ReplyEngine, store: &Store, now_wall: DateTime<Utc>) {
    let mut tracker = InvalidTracker::default();
    let stats = poll_once(engine, store, now_wall, Instant::now(), &mut tracker);
    if stats.errors > 0 {
        eprintln!("bellman: reply startup scan: {} error(s)", stats.errors);
    }
    let bad = quarantine_dir(engine.tree.root());
    if let Ok(_lock) = gate::acquire_quarantine(&engine.data_dir) {
        if let Err(e) = super::quarantine::startup_sweep(&bad) {
            eprintln!("bellman: quarantine startup sweep: {e}");
        }
    }
    reconcile(engine, store);
}
