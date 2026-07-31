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
use super::notification::{fires_dir, fire_notification_name, FireNotification};
use super::ReplyEngine;
use crate::store::{RunClaim, Store, Timer, TimerId, TransportProjection};
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

/// Build the transport projection for one fire. Called by the fire
/// transaction; `order` is the database-wide publication order read inside
/// the same transaction.
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
) -> Result<TransportProjection, String> {
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
        folder.join(crate::tree::reply_file_name(claim.run_id)),
    );
    let payload =
        serde_json::to_string(&n).map_err(|e| format!("serialize fire notification: {e}"))?;
    let fires = fires_dir(&engine.data_dir.join("slots"));
    let target = match &engine.fire_slot_file {
        Some(name) => fires.join(name),
        None => fires.join(fire_notification_name(claim.run_id)),
    };
    Ok(TransportProjection {
        run_id: claim.run_id,
        timer_id: timer.id,
        target_path: target.to_string_lossy().into_owned(),
        payload,
        publication_order: order,
        state: TransportProjection::PENDING.to_string(),
        attempts: 0,
        next_attempt_at: now,
        created_at: now,
        published_at: None,
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
pub fn attempt(data_dir: &Path, store: &Store, proj: &TransportProjection) -> Attempt {
    let n: FireNotification = match serde_json::from_str(&proj.payload) {
        Ok(n) => n,
        Err(e) => {
            // Our own payload should always parse; if it does not, retrying
            // cannot help — mark obsolete rather than spin.
            eprintln!("bellman: transport projection {} payload invalid: {e}", proj.run_id);
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

    // Run files precede delivery: never publish a notification whose
    // status/reply channel does not exist yet (R10 reconciliation repairs
    // them first, then the pump retries).
    if !n.status_path.exists() || !n.reply_path.exists() {
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
pub fn pump(data_dir: &Path, store: &Store, limit: usize) -> usize {
    let due = match store.due_transport_projections(Utc::now(), limit) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("bellman: publication pump query: {e}");
            return 0;
        }
    };
    let mut n = 0;
    for proj in &due {
        attempt(data_dir, store, proj);
        n += 1;
    }
    sweep_pickups(data_dir, store, limit);
    n
}

/// R7 pickup, detected against the durable records: `ack_through` advanced
/// past the firing, or any valid reply was ingested for the run (its
/// lifecycle row shows it). `no_ack` stops retries without deleting the file
/// (a late pickup may still revise `no_ack`).
fn sweep_pickups(data_dir: &Path, store: &Store, limit: usize) {
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
            Ok(Some(row))
                if row.acknowledged_at.is_some()
                    || row.heartbeat_at.is_some()
                    || row.completed_at.is_some()
                    || row.failed_at.is_some()
                    || row.progress.is_some()
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
        // Published but the file is gone and no pickup was recorded — the
        // app consumed it silently: redelivery is allowed (consumer `run_id`
        // dedupe keeps it one logical firing).
        if proj.state == TransportProjection::PUBLISHED
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
