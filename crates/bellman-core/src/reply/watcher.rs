//! The file transport for the reply channel.
//!
//! This module owns everything file-aware: safe reading (no-follow, regular
//! file, size cap), the mid-write debounce, quarantine on stable-invalid
//! bytes, stale-file deletion, the pre-fire barrier, the startup scan, the
//! periodic reconciler and the background watcher thread. Its job ends at
//! "here is a parsed reply" — validation, transitions, folding and logging
//! all live in [`super::engine`], which does not know a file exists (IK6's
//! socket transport calls the same function).
//!
//! Watcher events are latency hints; the periodic full rescan is truth — the
//! same philosophy as the slot watcher, whose debounce constant is reused.

use crate::events::EventLog;
use crate::store::{Store, Timer, TimerId};
use crate::tree::{reply_file_name, STATUS_FILE_NAME};
use chrono::{DateTime, Utc};
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use uuid::Uuid;

use super::document::{ReplyDocument, MAX_REPLY_FILE_BYTES};
use super::engine::{ReplyEngine, ReplyResult};
use super::gate;
use super::notification::{
    fire_notification_name, fires_dir, write_fire_notification, FireNotification,
};
use super::quarantine::{
    fnv1a64_hex, quarantine_bytes, quarantine_dir, quarantine_unread,
};

/// Reuse the slot watcher's debounce: a parse failure is usually a file
/// being written, so identical invalid bytes must be stable across this
/// window before they are rejected.
pub const DEFAULT_DEBOUNCE: Duration = crate::slots::DEFAULT_DEBOUNCE;

/// How often the watcher rescans (the periodic truth pass).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Reconcile the folder projections from the database every this many polls.
const RECONCILE_EVERY_POLLS: u32 = 60;

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
            Ok(fd) => unsafe { std::fs::File::from_raw_fd(std::os::unix::io::IntoRawFd::into_raw_fd(fd)) },
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
    let file = {
        compile_error!("reply file transport: unsupported platform");
    };

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
/// pickup/watchdog deadline pass. Acquires the R10 per-timer gate around
/// each timer's read-check-write, and the quarantine lock (after the timer
/// shard — never the reverse) around artifact creation.
pub fn poll_once(
    engine: &ReplyEngine,
    store: &Store,
    log: &mut EventLog,
    now: DateTime<Utc>,
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
        if let Err(e) = poll_timer(engine, store, log, timer, now, tracker, &mut stats) {
            eprintln!("bellman: reply watcher: timer {}: {e}", timer.id);
            stats.errors += 1;
        }
    }
    stats.deadline_transitions += expire_all_deadlines(engine, store, log, now, &mut stats);
    stats
}

fn poll_timer(
    engine: &ReplyEngine,
    store: &Store,
    log: &mut EventLog,
    timer: &Timer,
    now: DateTime<Utc>,
    tracker: &mut InvalidTracker,
    stats: &mut PollStats,
) -> ReplyResult<()> {
    let _gate = gate::acquire(&engine.data_dir, timer.id)
        .map_err(|e| super::engine::ReplyError::Store(crate::store::StoreError::Io(e.to_string())))?;
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
        let Some(run_id) = reply_run_id_from_name(&name) else {
            continue;
        };
        let path = entry.path();
        match read_reply_file(&path) {
            Ok(ReplyRead::Missing) => {}
            Ok(ReplyRead::Oversize(len)) => {
                condemn_unread(engine, log, timer, &path, run_id, len, "oversize", stats);
            }
            Ok(ReplyRead::Special(kind)) => {
                condemn_unread(engine, log, timer, &path, run_id, 0, kind, stats);
            }
            Ok(ReplyRead::Bytes(bytes)) => {
                handle_bytes(engine, store, log, timer, &path, run_id, bytes, now, tracker, stats);
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

#[allow(clippy::too_many_arguments)]
fn handle_bytes(
    engine: &ReplyEngine,
    store: &Store,
    log: &mut EventLog,
    timer: &Timer,
    path: &Path,
    run_id: Uuid,
    bytes: Vec<u8>,
    now: DateTime<Utc>,
    tracker: &mut InvalidTracker,
    stats: &mut PollStats,
) {
    let digest = fnv1a64_hex(&bytes);
    let doc: ReplyDocument = match serde_json::from_slice(&bytes) {
        Ok(d) => d,
        Err(_) => {
            track_invalid(engine, log, timer, path, run_id, &bytes, digest, now, tracker, stats);
            return;
        }
    };
    tracker.by_path.remove(path);
    if doc.state.is_none() && doc.hint.is_some() {
        // The untouched, pre-filled stub: `state: null` is how Bellman tells
        // "stub" from "the app answered". Bellman does not act on it.
        return;
    }
    match engine.ingest(store, log, timer, &doc, &digest, now) {
        Ok(super::engine::IngestOutcome::Applied) => stats.applied += 1,
        Ok(super::engine::IngestOutcome::Duplicate) => stats.duplicates += 1,
        Ok(super::engine::IngestOutcome::Superseded) => {
            // A previous run's own file: ingest is done, the stale file goes.
            // The current run's file is never touched by this path.
            if let Err(e) = std::fs::remove_file(path) {
                eprintln!("bellman: reply watcher: remove stale {}: {e}", path.display());
            }
            stats.superseded += 1;
        }
        Ok(super::engine::IngestOutcome::Rejected(reason)) => {
            condemn_bytes(engine, log, timer, path, run_id, &bytes, reason.as_str(), stats);
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
    log: &mut EventLog,
    timer: &Timer,
    path: &Path,
    run_id: Uuid,
    bytes: &[u8],
    digest: String,
    now: DateTime<Utc>,
    tracker: &mut InvalidTracker,
    stats: &mut PollStats,
) {
    match tracker.by_path.get_mut(path) {
        Some(entry) if entry.digest == digest => {
            if entry.condemned {
                return; // one rejection + one artifact per distinct content
            }
            if now.signed_duration_since(entry.since).to_std().unwrap_or_default()
                >= DEFAULT_DEBOUNCE
            {
                condemn_bytes(engine, log, timer, path, run_id, bytes, "invalid JSON", stats);
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
                    since: now,
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
    log: &mut EventLog,
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
            if let Err(e) = engine.log_rejection(log, timer, Some(run_id), reason) {
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
    log: &mut EventLog,
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
            if let Err(e) = engine.log_rejection(log, timer, Some(run_id), reason) {
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

/// Pickup + watchdog deadlines, each re-checked under its timer's gate.
fn expire_all_deadlines(
    engine: &ReplyEngine,
    store: &Store,
    log: &mut EventLog,
    now: DateTime<Utc>,
    stats: &mut PollStats,
) -> usize {
    let mut by_timer: HashMap<TimerId, (Vec<Uuid>, Vec<Uuid>)> = HashMap::new();
    match store.expired_pickups(now) {
        Ok(rows) => {
            for r in rows {
                by_timer.entry(r.timer_id).or_default().0.push(r.run_id);
            }
        }
        Err(e) => {
            eprintln!("bellman: reply watcher: expired pickups: {e}");
            stats.errors += 1;
        }
    }
    match store.expired_watchdogs(now) {
        Ok(rows) => {
            for r in rows {
                by_timer.entry(r.timer_id).or_default().1.push(r.run_id);
            }
        }
        Err(e) => {
            eprintln!("bellman: reply watcher: expired watchdogs: {e}");
            stats.errors += 1;
        }
    }
    let mut n = 0;
    for (timer_id, (pickups, watchdogs)) in by_timer {
        let Ok(_gate) = gate::acquire(&engine.data_dir, timer_id) else {
            stats.errors += 1;
            continue;
        };
        let Ok(Some(timer)) = store.get_timer(timer_id) else {
            continue;
        };
        for run_id in pickups {
            match engine.expire_pickup_one(store, log, &timer, run_id, now) {
                Ok(k) => n += k,
                Err(e) => {
                    eprintln!("bellman: reply watcher: pickup expiry {run_id}: {e}");
                    stats.errors += 1;
                }
            }
        }
        for run_id in watchdogs {
            match engine.expire_watchdog_one(store, log, &timer, run_id, now) {
                Ok(k) => n += k,
                Err(e) => {
                    eprintln!("bellman: reply watcher: watchdog expiry {run_id}: {e}");
                    stats.errors += 1;
                }
            }
        }
    }
    n
}

/// The pre-fire barrier (R10): synchronously read and ingest the previous
/// run's reply file before the fire transaction may supersede it. Bounded:
/// one existing debounce window — a partial writer that keeps changing the
/// file must not hold firing forever, and is never quarantined here. A valid
/// reply completed after the final barrier read is the accepted
/// true-simultaneity race and is later rejected as superseded.
///
/// `prev_run_id` is still treated as the current run for this ingest: the
/// new claim already exists in the store, so without the override the reply
/// would be misrouted to the previous-run (superseded) branch.
pub fn barrier_ingest(
    engine: &ReplyEngine,
    store: &Store,
    log: &mut EventLog,
    timer: &Timer,
    folder: &Path,
    prev_run_id: Uuid,
    now: DateTime<Utc>,
) -> ReplyResult<()> {
    let path = folder.join(reply_file_name(prev_run_id));
    let bytes = match read_reply_file(&path) {
        Ok(ReplyRead::Bytes(b)) => b,
        _ => return Ok(()), // nothing readable to fold in; normal paths handle it
    };
    let digest = fnv1a64_hex(&bytes);
    match serde_json::from_slice::<ReplyDocument>(&bytes) {
        Ok(doc) if doc.state.is_none() && doc.hint.is_some() => {
            // Untouched stub — nothing to fold in.
        }
        Ok(doc) => {
            engine.ingest_as_current(store, log, timer, &doc, &digest, prev_run_id, now)?;
        }
        Err(_) => {
            std::thread::sleep(DEFAULT_DEBOUNCE);
            let Ok(ReplyRead::Bytes(second)) = read_reply_file(&path) else {
                return Ok(());
            };
            match serde_json::from_slice::<ReplyDocument>(&second) {
                Ok(doc) => {
                    engine.ingest_as_current(
                        store,
                        log,
                        timer,
                        &doc,
                        &fnv1a64_hex(&second),
                        prev_run_id,
                        now,
                    )?;
                }
                Err(_) if second == bytes => {
                    // Identical invalid bytes, stable past the window: reject
                    // on sight (the watcher quarantines idempotently).
                    condemn_bytes(
                        engine,
                        log,
                        timer,
                        &path,
                        prev_run_id,
                        &second,
                        "invalid JSON",
                        &mut PollStats::default(),
                    );
                }
                Err(_) => {} // still changing: let the firing proceed
            }
        }
    }
    Ok(())
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
        folder.join(reply_file_name(claim.run_id)),
    );
    write_fire_notification(&engine.data_dir.join("slots"), &n)?;
    Ok(())
}

/// Rebuild the database-owned projections for every live timer:
/// `status.json` from the accumulated row (re-read inside the gate, never a
/// stale snapshot), a missing stub create-only, and a missing fire
/// notification last. This is the same recovery startup performs — a failed
/// post-commit write is repaired here without waiting for a restart.
pub fn reconcile(engine: &ReplyEngine, store: &Store, log: &mut EventLog) -> usize {
    let mut repaired = 0;
    let timers = match store.list_timers() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bellman: reply reconciler: list timers: {e}");
            return 0;
        }
    };
    for timer in &timers {
        if let Err(e) = reconcile_timer(engine, store, log, timer, &mut repaired) {
            eprintln!("bellman: reply reconciler: timer {}: {e}", timer.id);
        }
    }
    repaired
}

fn reconcile_timer(
    engine: &ReplyEngine,
    store: &Store,
    log: &mut EventLog,
    timer: &Timer,
    repaired: &mut usize,
) -> ReplyResult<()> {
    let _gate = gate::acquire(&engine.data_dir, timer.id)
        .map_err(|e| super::engine::ReplyError::Store(crate::store::StoreError::Io(e.to_string())))?;
    let Some(folder) = engine.tree.folder_for(timer.id) else {
        return Ok(());
    };
    let Some(claim) = super::engine::current_claim(store, timer.id)? else {
        return Ok(());
    };
    let Some(row) = store.get_run_state(claim.run_id)? else {
        return Ok(());
    };
    // status.json: re-project from the row just read (inside the gate).
    let status_path = folder.join(STATUS_FILE_NAME);
    let status = crate::tree::RunStatus::from_run_state(timer, &claim, &row);
    let fresh = serde_json::to_vec_pretty(&status)
        .map_err(|e| super::engine::ReplyError::Tree(crate::tree::TreeError::Serialize(e.to_string())))?;
    let stale = std::fs::read(&status_path).map(|b| b != fresh).unwrap_or(true);
    if stale {
        crate::tree::write_status(&engine.tree, timer, &status)?;
        *repaired += 1;
    }
    // Missing stub: create-only — an app can be writing at this exact
    // moment, and a lost O_EXCL race to a real reply is the correct outcome.
    let stub_path = folder.join(reply_file_name(claim.run_id));
    if !stub_path.exists() {
        engine
            .tree
            .create_reply_stub(&folder, claim.run_id, &row.app_name)?;
        *repaired += 1;
    }
    // Fire notification last (never eligible before the two projections).
    let fire_path = fires_dir(&engine.data_dir.join("slots")).join(fire_notification_name(claim.run_id));
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
    let _ = log; // reconciler is a repair pass; transitions are logged by their actors
    Ok(())
}

/// Startup (R10): scan every `reply-*.json` BEFORE the scheduler fires
/// anything — an app can answer while Bellman is stopped, and superseding
/// before reading would silently record the outcome unknown. Then rebuild
/// stale projections and sweep quarantine temporaries.
pub fn startup_scan(engine: &ReplyEngine, store: &Store, log: &mut EventLog, now: DateTime<Utc>) {
    let mut tracker = InvalidTracker::default();
    let stats = poll_once(engine, store, log, now, &mut tracker);
    if stats.errors > 0 {
        eprintln!("bellman: reply startup scan: {} error(s)", stats.errors);
    }
    let bad = quarantine_dir(engine.tree.root());
    if let Ok(_lock) = gate::acquire_quarantine(&engine.data_dir) {
        if let Err(e) = super::quarantine::startup_sweep(&bad) {
            eprintln!("bellman: quarantine startup sweep: {e}");
        }
    }
    reconcile(engine, store, log);
}

/// Handle to stop the background reply watcher thread.
pub struct ReplyWatcherStop {
    stop: mpsc::Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ReplyWatcherStop {
    /// Signal shutdown and join the thread.
    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the reply watcher thread: a notify debouncer over the timers tree
/// (recursive — per-timer subfolders) feeding latency hints, plus a periodic
/// full rescan which is the truth. The same loop drives pickup/watchdog
/// deadlines and the bounded reconciler.
pub fn spawn_reply_thread(
    data_dir: PathBuf,
    db_path: PathBuf,
    engine: ReplyEngine,
    poll_interval: Duration,
) -> std::io::Result<ReplyWatcherStop> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let handle = std::thread::Builder::new()
        .name("bellman-replies".to_string())
        .spawn(move || {
            if let Err(e) = run_reply_loop(data_dir, db_path, engine, poll_interval, stop_rx) {
                eprintln!("bellman: reply watcher exited: {e}");
            }
        })?;
    Ok(ReplyWatcherStop {
        stop: stop_tx,
        handle: Some(handle),
    })
}

fn run_reply_loop(
    data_dir: PathBuf,
    db_path: PathBuf,
    engine: ReplyEngine,
    poll_interval: Duration,
    stop_rx: mpsc::Receiver<()>,
) -> std::io::Result<()> {
    let store = Store::open_with(
        &db_path,
        crate::store::OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut log = EventLog::open_under_configured(&data_dir)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    // Latency hints only; the periodic rescan is truth.
    let (hint_tx, hint_rx) = mpsc::channel::<()>();
    let _debouncer = {
        let hint = hint_tx.clone();
        new_debouncer(
            DEFAULT_DEBOUNCE,
            None,
            move |res: DebounceEventResult| {
                if res.is_ok() {
                    let _ = hint.send(());
                }
            },
        )
        .and_then(|mut d| {
            d.watch(engine.tree.root(), RecursiveMode::Recursive)?;
            Ok(d)
        })
        .map_err(|e| std::io::Error::other(e.to_string()))?
    };

    let mut tracker = InvalidTracker::default();
    let mut polls: u32 = 0;
    // Initial pass before entering the loop.
    let stats = poll_once(&engine, &store, &mut log, Utc::now(), &mut tracker);
    if stats.errors > 0 {
        eprintln!("bellman: reply watcher: initial pass: {} error(s)", stats.errors);
    }
    reconcile(&engine, &store, &mut log);
    loop {
        if stop_rx.try_recv().is_ok() {
            return Ok(());
        }
        match hint_rx.recv_timeout(poll_interval) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if stop_rx.try_recv().is_ok() {
            return Ok(());
        }
        let stats = poll_once(&engine, &store, &mut log, Utc::now(), &mut tracker);
        if stats.errors > 0 {
            eprintln!("bellman: reply watcher: {} error(s) this pass", stats.errors);
        }
        polls += 1;
        if polls.is_multiple_of(RECONCILE_EVERY_POLLS) {
            reconcile(&engine, &store, &mut log);
        }
    }
}
