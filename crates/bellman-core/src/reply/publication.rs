//! SCH1 — fire-notification publication: durable transport projections,
//! at-least-once retry, fixed-target cursor rules, pickup cleanup.
//!
//! The fire transaction stores one **transport projection** per owned run:
//! the immutable target path (`slots/fires/fire-<run_id>.json`, or a
//! configured fixed name under `fires/` used only as an at-least-once wake
//! hint), the serialized notification payload, and a database-wide monotonic
//! `publication_order`. The fire producer attempts publication immediately
//! (only after R10 projected `status.json` and the create-only reply stub —
//! a notification naming missing run files is a broken notification); a
//! bounded local-write failure stays `pending` for the publication pump
//! (dispatcher tick) and startup recovery, never for an action worker.
//!
//! Publication is **at-least-once, not physically exactly-once**: after the
//! atomic replace succeeds but before pickup is recorded the notification may
//! be published again, so consumers deduplicate by `run_id`. Pickup is R7's
//! definition (`ack_through` advancing past the firing, or any valid reply
//! ingested for it) — file presence alone proves neither.
//!
//! An interprocess lock serialises publishers per canonical target path: a
//! bounded stable shard set under `<data_dir>/locks/`, keyed by an FNV-1a
//! hash of the canonical target. Publication acquires the R10 timer shard
//! first, then the target shard — no path ever acquires them in reverse.

use super::gate;
use super::notification::{fire_notification_name, fires_dir, FireNotification, IpcEndpoint};
use super::ReplyEngine;
use crate::store::{
    RunClaim, RunStateRow, Store, Timer, TimerId, TransportMode, TransportProjection,
    TRANSPORT_IPC, TRANSPORT_IPC_FALLBACK, TRANSPORT_JSON,
};
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Bounded stable lock-shard set for publication targets. Shard files live
/// under the data root (never beside a target that may be deleted/replaced).
const PUB_SHARDS: u64 = 64;

/// Backoff ceiling for a deferred publication attempt.
const MAX_BACKOFF_SECS: u64 = 60;

/// Cap on deferred retries; pickup / `no_ack` normally stop them first.
const MAX_ATTEMPTS: u32 = 16;

/// `<data_dir>/locks/pub-<NN>.lock` for a canonical target (FNV-1a hash).
fn target_shard_path(data_dir: &Path, target: &str) -> PathBuf {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in target.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    data_dir
        .join("locks")
        .join(format!("pub-{:02x}.lock", h % PUB_SHARDS))
}

/// The transport mode selected for one firing (IK6). Fixed at fire time,
/// recorded on the run (`selected_transport`), never changes mid-firing —
/// within `auto` the *delivery* may still fall back to files, but that is
/// recorded separately (`transport: ipc_fallback`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedTransport {
    /// The JSON folder adapter (IK3, unchanged): reply stub + fire file.
    Json,
    /// The socket adapter: no stub, the fire message omits `reply_path`.
    Ipc,
}

impl SelectedTransport {
    /// The value recorded on the run (`selected_transport`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => TRANSPORT_JSON,
            Self::Ipc => TRANSPORT_IPC,
        }
    }
}

/// Per-firing transport selection (IK6). `json` is always files; `ipc` and
/// `auto` resolve against a LIVE IPC server — a timer asking for a socket
/// while no server is live in this process falls to files (the fallback
/// must be the thing that already works), and `auto` picks IPC only when a
/// client holding this timer is connected at fire time.
pub fn select_transport(timer: &Timer, engine: &ReplyEngine) -> SelectedTransport {
    match timer.transport {
        TransportMode::Json => SelectedTransport::Json,
        TransportMode::Ipc => match &engine.ipc {
            Some(h) if h.is_live() => SelectedTransport::Ipc,
            _ => SelectedTransport::Json,
        },
        TransportMode::Auto => match &engine.ipc {
            Some(h) if h.is_live() && h.has_client(&timer.id) => SelectedTransport::Ipc,
            _ => SelectedTransport::Json,
        },
    }
}

/// Build the transport projection for one fire. Called by the fire
/// transaction; `order` is the database-wide publication order read inside
/// the same transaction. The selected transport decides the per-adapter
/// encoding: identical identity/timing fields either way; the IPC encoding
/// omits `reply_path` (no stub exists for an IPC firing) and carries the
/// socket endpoint; the file encoding carries the stub path.
#[allow(clippy::too_many_arguments)]
pub fn new_projection(
    engine: &ReplyEngine,
    timer: &Timer,
    claim: &RunClaim,
    fire_state: &str,
    folder: &Path,
    app_name: &str,
    order: u64,
    now: DateTime<Utc>,
    selected: SelectedTransport,
) -> Result<TransportProjection, String> {
    let ipc_endpoint = engine.ipc.as_ref().map(|h| IpcEndpoint {
        socket: h.socket_path().to_path_buf(),
    });
    let (reply_path, kind, target) = match selected {
        SelectedTransport::Json => {
            let fires = fires_dir(&engine.data_dir.join("slots"));
            let target = match &engine.fire_slot_file {
                Some(name) => fires.join(name),
                None => fires.join(fire_notification_name(claim.run_id)),
            };
            (
                Some(folder.join(crate::tree::reply_file_name(claim.run_id))),
                TransportProjection::KIND_FILE,
                target.to_string_lossy().into_owned(),
            )
        }
        SelectedTransport::Ipc => (
            None,
            TransportProjection::KIND_IPC,
            format!("ipc://{}", timer.id),
        ),
    };
    let n = FireNotification::new(
        fire_state,
        timer.occurrence.kind().kind_label(),
        timer.id,
        &timer.name,
        app_name,
        claim.run_id,
        claim.scheduled_for,
        claim.claimed_at,
        folder.join(crate::tree::STATUS_FILE_NAME),
        reply_path,
        ipc_endpoint,
    );
    let payload =
        serde_json::to_string(&n).map_err(|e| format!("serialize fire notification: {e}"))?;
    Ok(TransportProjection {
        run_id: claim.run_id,
        timer_id: timer.id,
        target_path: target,
        payload,
        publication_order: order,
        state: TransportProjection::PENDING.to_string(),
        attempts: 0,
        next_attempt_at: now,
        created_at: now,
        published_at: None,
        kind: kind.to_string(),
    })
}

/// What one publication attempt decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// The atomic replace succeeded (or a newer notification already owns the
    /// target, making this one permanently obsolete).
    Published,
    /// Not eligible yet or a bounded local-write failure — stays `pending`
    /// with backoff for the pump / startup recovery.
    Deferred,
    /// Permanently obsolete as a wake hint (durable cursor or a known newer
    /// firing at the fixed path). Never retried.
    Obsolete,
}

/// Attempt to publish one projection. Best-effort: outcomes are recorded on
/// the projection row; this never fails the caller's primary operation.
/// `ipc` is the live IPC handle (`None` in processes without the server);
/// IPC-kind projections deliver through it, file-kind ones ignore it.
pub fn attempt(
    data_dir: &Path,
    store: &Store,
    proj: &TransportProjection,
    ipc: Option<&crate::ipc::IpcHandle>,
) -> Attempt {
    let n: FireNotification = match serde_json::from_str(&proj.payload) {
        Ok(n) => n,
        Err(e) => {
            // Our own payload should always parse; if it does not, retrying
            // cannot help — mark obsolete rather than spin.
            eprintln!(
                "bellman: transport projection {} payload invalid: {e}",
                proj.run_id
            );
            let _ = store.mark_transport_obsolete(proj.run_id);
            return Attempt::Obsolete;
        }
    };
    // The durable target cursor FIRST: a fixed target whose cursor moved
    // past this projection makes it permanently obsolete — whether or not a
    // file is currently present, and regardless of whether its own run files
    // still exist (a superseded run's reply stub is legitimately gone).
    let cursor = store.target_cursor(&proj.target_path).unwrap_or(0);
    if cursor > proj.publication_order {
        let _ = store.mark_transport_obsolete(proj.run_id);
        return Attempt::Obsolete;
    }

    if proj.kind == TransportProjection::KIND_IPC {
        return attempt_ipc(data_dir, store, proj, &n, ipc);
    }

    // Run files precede delivery: never publish a notification whose
    // status/reply channel does not exist yet (R10 reconciliation repairs
    // them first, then the pump retries).
    let reply_missing = n.reply_path.as_ref().map(|p| !p.exists()).unwrap_or(true);
    if !n.status_path.exists() || reply_missing {
        return defer(store, proj);
    }

    // Lock order: R10 timer shard BEFORE the target shard — never reversed.
    let _timer_gate = match gate::acquire(data_dir, proj.timer_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bellman: publication timer gate: {e}");
            return defer(store, proj);
        }
    };
    let _target_lock = match gate::acquire_file(&target_shard_path(data_dir, &proj.target_path)) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bellman: publication target lock: {e}");
            return defer(store, proj);
        }
    };

    let target = Path::new(&proj.target_path);
    match std::fs::read(target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Missing file (never written, or the app consumed it): write.
            write_target(store, proj, &n)
        }
        Err(e) => {
            eprintln!("bellman: publication read {}: {e}", target.display());
            defer(store, proj)
        }
        Ok(bytes) => {
            let existing: Result<FireNotification, _> = serde_json::from_slice(&bytes);
            match existing {
                Ok(cur) if cur.run_id == proj.run_id => {
                    // Same known run_id already present: suppress the
                    // immediate rewrite, but stay eligible for a later
                    // bounded retry until pickup is recorded.
                    defer(store, proj)
                }
                Ok(cur) => {
                    match store.transport_projection(cur.run_id) {
                        Ok(Some(newer)) if newer.publication_order > proj.publication_order => {
                            // A known NEWER firing already sits at the fixed
                            // path — never overwrite it, even across timers.
                            let _ = store.mark_transport_obsolete(proj.run_id);
                            Attempt::Obsolete
                        }
                        // Older known firing, or one the app consumed: the
                        // newer projection replaces it.
                        _ => write_target(store, proj, &n),
                    }
                }
                // Malformed bytes or an unknown run_id under Bellman's
                // `slots/fires/` namespace: atomically replaced by the newest
                // pending projection (this is Bellman-owned output, not the
                // app-owned reply channel).
                Err(_) => write_target(store, proj, &n),
            }
        }
    }
}

fn write_target(store: &Store, proj: &TransportProjection, n: &FireNotification) -> Attempt {
    let target = Path::new(&proj.target_path);
    let Some(dir) = target.parent() else {
        let _ = store.mark_transport_obsolete(proj.run_id);
        return Attempt::Obsolete;
    };
    let Some(name) = target.file_name().and_then(|s| s.to_str()) else {
        let _ = store.mark_transport_obsolete(proj.run_id);
        return Attempt::Obsolete;
    };
    match crate::slots::atomic_write_json(dir, name, n) {
        Ok(_) => {
            let _ = store.mark_transport_published(proj.run_id);
            Attempt::Published
        }
        Err(e) => {
            eprintln!("bellman: publication write {}: {e}", target.display());
            defer(store, proj)
        }
    }
}

/// R7 pickup, read off the lifecycle row: any app-authored signal (ack,
/// running, heartbeat, completion…) means the app confirmed — the same
/// predicate the pickup sweep uses, shared so both adapters agree on what
/// "confirmed" means.
pub fn has_pickup_signal(row: &RunStateRow) -> bool {
    row.acknowledged_at.is_some()
        || row.heartbeat_at.is_some()
        || row.completed_at.is_some()
        || row.failed_at.is_some()
        || row.progress.is_some()
}

/// IK6 socket delivery: a bounded nonblocking send to the client(s) holding
/// this timer. Rules from the card:
///
/// - **Confirmation first.** If the run already shows a pickup signal the
///   transport is settled: no more sends, never a fallback.
/// - **No client / failed send on an unconfirmed run:** `auto` falls back to
///   files (same `run_id`, `ipc_fallback` on the run); explicit `ipc` defers
///   to the bounded pump schedule and lets R7 reach `no_ack`.
/// - Silence on a live connection is NOT transport failure: the pump retries
///   on its normal schedule; no independent fallback timer exists.
fn attempt_ipc(
    data_dir: &Path,
    store: &Store,
    proj: &TransportProjection,
    n: &FireNotification,
    ipc: Option<&crate::ipc::IpcHandle>,
) -> Attempt {
    // status.json is the always-written mirror — deliver nothing before it.
    if !n.status_path.exists() {
        return defer(store, proj);
    }
    // Confirmation settles the transport for the firing (R7 pickup
    // satisfied); retries stop, and a later disconnect is not a fallback.
    if matches!(store.get_run_state(proj.run_id), Ok(Some(row)) if has_pickup_signal(&row)) {
        record_pickup(data_dir, store, proj);
        return Attempt::Published;
    }
    let Some(handle) = ipc else {
        return defer(store, proj);
    };
    match handle.send(&proj.timer_id, proj.payload.as_bytes()) {
        crate::ipc::SendOutcome::Queued => {
            let _ = store.mark_transport_published(proj.run_id);
            Attempt::Published
        }
        crate::ipc::SendOutcome::NoClient => {
            let auto = matches!(
                store.get_timer(proj.timer_id),
                Ok(Some(timer)) if timer.transport == TransportMode::Auto
            );
            if auto {
                ipc_fallback(data_dir, store, proj, n)
            } else {
                defer(store, proj)
            }
        }
    }
}

/// `auto` fallback, only before delivery is confirmed: create the same run's
/// reply stub create-only, then publish the SAME `run_id` through the file
/// adapter with its `reply_path` added — the fallback encoding changes
/// nothing else about identity or timing, and no second run is ever minted.
/// The run records `transport: ipc_fallback`; the selected mode is untouched.
fn ipc_fallback(
    data_dir: &Path,
    store: &Store,
    proj: &TransportProjection,
    n: &FireNotification,
) -> Attempt {
    let tree = crate::tree::TimersTree::new(data_dir);
    let (Some(folder), Ok(Some(row))) = (
        tree.folder_for(proj.timer_id),
        store.get_run_state(proj.run_id),
    ) else {
        return defer(store, proj);
    };
    // 1. The stub exists BEFORE the file message naming it is eligible.
    if tree
        .create_reply_stub(&folder, proj.run_id, &row.app_name)
        .is_err()
    {
        return defer(store, proj);
    }
    // 2. Same logical message + only the stub's exact path added.
    let mut m = n.clone();
    m.reply_path = Some(folder.join(crate::tree::reply_file_name(proj.run_id)));
    let payload = match serde_json::to_string(&m) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bellman: ipc fallback encode {}: {e}", proj.run_id);
            return defer(store, proj);
        }
    };
    let target = fires_dir(&data_dir.join("slots")).join(fire_notification_name(proj.run_id));
    // 3. Convert the SAME projection row (pending only — a concurrent
    //    pickup/publish already settled the firing).
    match store.convert_projection_to_file(proj.run_id, &target.to_string_lossy(), &payload) {
        Ok(true) => {}
        Ok(false) => return Attempt::Published,
        Err(e) => {
            eprintln!("bellman: ipc fallback convert {}: {e}", proj.run_id);
            return defer(store, proj);
        }
    }
    let _ = store.set_run_transport(proj.run_id, TRANSPORT_IPC_FALLBACK);
    // The mirror said `ipc`; re-project immediately so status.json shows
    // the effective delivery, not the stale selection.
    if let (Ok(Some(timer)), Ok(Some(row)), Ok(Some(claim))) = (
        store.get_timer(proj.timer_id),
        store.get_run_state(proj.run_id),
        store.get_run(proj.run_id),
    ) {
        let status = crate::tree::RunStatus::from_run_state(&timer, &claim, &row);
        if let Err(e) = crate::tree::write_status(&tree, &timer, &status) {
            eprintln!("bellman: ipc fallback status projection: {e}");
        }
    }
    // 4. Deliver through the file adapter immediately (the pump would retry
    //    anyway; the freshly converted row is due now).
    match store.transport_projection(proj.run_id) {
        Ok(Some(converted)) => attempt(data_dir, store, &converted, None),
        _ => Attempt::Deferred,
    }
}

fn defer(store: &Store, proj: &TransportProjection) -> Attempt {
    let attempts = proj.attempts.saturating_add(1);
    if attempts > MAX_ATTEMPTS {
        // Bounded retries exhausted without pickup — stop quietly; the
        // durable feed (SlotRunEvent) still carries the firing.
        let _ = store.mark_transport_obsolete(proj.run_id);
        return Attempt::Obsolete;
    }
    let secs = (1u64 << attempts.min(6)).min(MAX_BACKOFF_SECS);
    let next = Utc::now() + chrono::Duration::seconds(secs as i64);
    let _ = store.defer_transport_projection(proj.run_id, attempts, next);
    Attempt::Deferred
}

/// The publication pump: attempt every due pending projection (bounded).
/// Runs on the dispatcher tick and at startup — never on an action worker.
/// `ipc` is the live IPC handle for socket-kind projections.
pub fn pump(
    data_dir: &Path,
    store: &Store,
    limit: usize,
    ipc: Option<&crate::ipc::IpcHandle>,
) -> usize {
    let due = match store.due_transport_projections(Utc::now(), limit) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("bellman: publication pump query: {e}");
            return 0;
        }
    };
    let mut n = 0;
    for proj in &due {
        attempt(data_dir, store, proj, ipc);
        n += 1;
    }
    sweep_pickups(data_dir, store, limit, ipc);
    n
}

/// R7 pickup, detected against the durable records: `ack_through` advanced
/// past the firing, or any valid reply was ingested for the run (its
/// lifecycle row shows it). `no_ack` stops retries without deleting the file
/// (a late pickup may still revise `no_ack`). IK6: an IPC projection whose
/// client DISCONNECTED before confirmation (no pickup signal, nobody
/// holding the timer) is the same unconfirmed failure as a send error —
/// `auto` falls back to files, explicit `ipc` requeues for bounded retry.
/// A confirmed run never reaches that branch (pickup is checked first).
fn sweep_pickups(
    data_dir: &Path,
    store: &Store,
    limit: usize,
    ipc: Option<&crate::ipc::IpcHandle>,
) {
    let live = match store.live_transport_projections(limit) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bellman: pickup sweep query: {e}");
            return;
        }
    };
    for proj in &live {
        let claim = match store.get_run(proj.run_id) {
            Ok(Some(c)) => c,
            _ => continue,
        };
        // Cursor pickup covers every firing at or below the acked sequence.
        let acked = store
            .last_acked_sequence(proj.timer_id)
            .map(|a| a >= claim.event_sequence)
            .unwrap_or(false);
        // A valid reply for the run is pickup too: the lifecycle row shows
        // any app-authored signal (ack, running, heartbeat, completion…).
        let replied = matches!(
            store.get_run_state(proj.run_id),
            Ok(Some(row)) if has_pickup_signal(&row)
        );
        if acked || replied {
            record_pickup(data_dir, store, proj);
            continue;
        }
        let no_ack = matches!(
            store.get_run_state(proj.run_id),
            Ok(Some(row)) if row.no_ack_at.is_some()
        );
        if no_ack {
            record_no_ack(store, proj.run_id);
            continue;
        }
        // IK6: published to a client that has since disconnected before
        // confirmation — an observed unconfirmed failure. `auto` falls back
        // to files (same run_id); explicit `ipc` requeues so the pump keeps
        // its bounded retry until pickup or `no_ack`.
        if proj.kind == TransportProjection::KIND_IPC
            && proj.state == TransportProjection::PUBLISHED
            && ipc.map(|h| !h.has_client(&proj.timer_id)).unwrap_or(true)
        {
            let auto = matches!(
                store.get_timer(proj.timer_id),
                Ok(Some(timer)) if timer.transport == TransportMode::Auto
            );
            if auto {
                if let Ok(n) = serde_json::from_str::<FireNotification>(&proj.payload) {
                    ipc_fallback(data_dir, store, proj, &n);
                }
            } else {
                let _ = store.requeue_transport_projection(proj.run_id);
            }
            continue;
        }
        // Published but the file is gone and no pickup was recorded — the
        // app consumed it silently: redelivery is allowed (consumer `run_id`
        // dedupe keeps it one logical firing). IPC projections have no file
        // to vanish; their redelivery is the pump's bounded retry.
        if proj.kind == TransportProjection::KIND_FILE
            && proj.state == TransportProjection::PUBLISHED
            && !Path::new(&proj.target_path).exists()
        {
            let _ = store.requeue_transport_projection(proj.run_id);
        }
    }
}

/// Record R7 pickup for one projection: retries stop, and the notification
/// file is removed — compare-before-delete under the publisher lock, so a
/// newer fixed-path wake hint (different `run_id`) is never removed by an
/// older firing's cleanup.
pub fn record_pickup(data_dir: &Path, store: &Store, proj: &TransportProjection) {
    let _ = store.mark_transport_picked_up(proj.run_id);
    let target = Path::new(&proj.target_path);
    if !target.exists() {
        return;
    }
    let Ok(_timer_gate) = gate::acquire(data_dir, proj.timer_id) else {
        return;
    };
    let Ok(_target_lock) = gate::acquire_file(&target_shard_path(data_dir, &proj.target_path))
    else {
        return;
    };
    // Re-read under the lock; delete only when the file still carries THIS
    // projection's run_id.
    let keep = match std::fs::read(target) {
        Ok(bytes) => matches!(
            serde_json::from_slice::<FireNotification>(&bytes),
            Ok(cur) if cur.run_id != proj.run_id
        ),
        Err(_) => false,
    };
    if !keep {
        let _ = std::fs::remove_file(target);
    }
}

/// Pickup via `ack_through`: every live projection of the timer at or below
/// the acknowledged sequence is consumed.
pub fn record_pickup_through(
    data_dir: &Path,
    store: &Store,
    timer_id: TimerId,
    through_sequence: u64,
) {
    let live = match store.live_projections_through(timer_id, through_sequence) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bellman: pickup query: {e}");
            return;
        }
    };
    for proj in &live {
        record_pickup(data_dir, store, proj);
    }
}

/// Pickup via a valid reply ingested for one run.
pub fn record_run_pickup(data_dir: &Path, store: &Store, run_id: Uuid) {
    let live = matches!(
        store.transport_projection(run_id),
        Ok(Some(p)) if p.state == TransportProjection::PENDING
            || p.state == TransportProjection::PUBLISHED
    );
    if !live {
        return;
    }
    if let Ok(Some(proj)) = store.transport_projection(run_id) {
        record_pickup(data_dir, store, &proj);
    }
}

/// A deadline recorded `no_ack` for the run: retries stop (the durable feed
/// and any existing file still allow a late pickup to revise `no_ack`).
pub fn record_no_ack(store: &Store, run_id: Uuid) {
    let _ = store.mark_transport_obsolete(run_id);
}

#[cfg(test)]
mod tests;
