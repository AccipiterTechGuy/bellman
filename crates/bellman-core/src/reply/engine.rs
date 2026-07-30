//! The transport-agnostic reply ingest engine.
//!
//! Everything from "here is a parsed reply" onward lives here: validation,
//! state transitions, event-log lines, `status.json` folding, watchdog
//! arming. This module does not know a file exists — the file watcher and
//! (later, IK6) the socket transport both call [`ReplyEngine::ingest`].
//!
//! Rules encoded here (docs/todo/json_normalization.md R5–R9, R12):
//!
//! - The log records **state transitions only**. `acknowledged` once (the
//!   first answer), `running` once (first entry), one line per terminal
//!   report. Heartbeats and progress are NEVER logged. Transitions the
//!   watcher missed are reconstructed from the accumulated timestamps.
//! - A terminal report never moves backwards to a non-terminal one — but
//!   the app's latest terminal report always wins on the still-current run,
//!   from either source (its own earlier verdict or Bellman's watchdog
//!   inference; there is no `failure_kind` special case).
//! - Bellman's provisional `no_ack` / watchdog `timed_out` may be revised by
//!   any valid app reply while the run is still current.
//! - `status.json` is the mirror: after every accepted reply it shows the
//!   current state and every field reported so far. Bellman accumulates —
//!   a write that omits an earlier field never retracts it.
//! - The app may re-send `expected_secs` mid-run; the latest accepted value
//!   re-anchors the watchdog at Bellman's receipt.
//! - `duration_ms` is Bellman's clock both ends: monotonic fire-commit →
//!   terminal-ingest elapsed, with a wall-clock fallback after a restart
//!   (`duration_source: "wall_clock"` marks the estimate).
//!
//! Concurrency: callers hold the R10 per-timer gate (`reply::gate`) around
//! read-check-write sequences; this engine re-reads nothing it is handed.

use crate::events::{EventLog, EventRecord, RunState};
use crate::store::{FailureKind, RunClaim, RunStateRow, Store, StoreError, Timer, TimerId};
use crate::tree::{self, RunStatus, TimersTree, TreeError};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::document::{
    cap_result, truncate_text, ReplyDocument, ReplyRejection, MAX_FREE_TEXT_BYTES,
    MAX_RESULT_EVENT_BYTES, MAX_RESULT_STATUS_BYTES,
};

/// Monotonic fire-commit anchors for `duration_ms`, keyed by run id. Shared
/// between the fire path (registers at commit) and the ingest path (consumes
/// at the terminal transition). A process restart loses them — the wall-clock
/// fallback in [`ReplyEngine::duration_ms`] covers that case explicitly.
pub type SharedAnchors = Arc<Mutex<HashMap<Uuid, Instant>>>;

/// Fresh empty anchor registry.
pub fn new_anchors() -> SharedAnchors {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Everything the reply lifecycle needs that is not per-call state.
#[derive(Clone)]
pub struct ReplyEngine {
    /// The per-timer folder tree (`<data_dir>/timers`).
    pub tree: TimersTree,
    /// Data root (gate shards, quarantine, slots live under it).
    pub data_dir: PathBuf,
    /// Pickup deadline (R7) — its own knob, seeded from the ack grace.
    pub pickup_grace: Duration,
    /// Opt-in watchdog factor: deadline = `expected_secs × factor`.
    pub watchdog_factor: f64,
    /// Monotonic duration anchors shared with the fire path.
    pub anchors: SharedAnchors,
}

/// What became of one ingested reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Accepted and applied to the current run.
    Applied,
    /// An exact repeat of the last accepted reply — nothing changed, no log
    /// line, and the watchdog deadline did not move.
    Duplicate,
    /// The reply names a previous run of this timer. Logged `superseded`
    /// (once per distinct content); the file transport deletes the stale
    /// file — the current run's own file is never touched.
    Superseded,
    /// Validation refused the reply. The caller logs `reply_rejected` when
    /// its transport's idempotence rules say this content is new (file
    /// transport: only when the quarantine artifact was created).
    Rejected(ReplyRejection),
}

/// Errors from the engine's own I/O (store, tree, log) — distinct from a
/// reply being *rejected*, which is a normal outcome.
#[derive(Debug)]
pub enum ReplyError {
    Store(StoreError),
    Tree(TreeError),
    Log(crate::events::EventLogError),
    Io(std::io::Error),
}

impl std::fmt::Display for ReplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "reply store: {e}"),
            Self::Tree(e) => write!(f, "reply tree: {e}"),
            Self::Log(e) => write!(f, "reply log: {e}"),
            Self::Io(e) => write!(f, "reply io: {e}"),
        }
    }
}

impl std::error::Error for ReplyError {}

impl From<StoreError> for ReplyError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<TreeError> for ReplyError {
    fn from(e: TreeError) -> Self {
        Self::Tree(e)
    }
}

impl From<crate::events::EventLogError> for ReplyError {
    fn from(e: crate::events::EventLogError) -> Self {
        Self::Log(e)
    }
}

impl From<std::io::Error> for ReplyError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub type ReplyResult<T> = Result<T, ReplyError>;

impl ReplyEngine {
    /// Ingest one parsed reply for `timer`. `digest` identifies the exact
    /// reply content (transport-computed) so an exact duplicate is a no-op
    /// and cannot re-arm the watchdog. `now` is Bellman's wall clock at
    /// receipt — the only clock the watchdog ever counts on.
    pub fn ingest(
        &self,
        store: &Store,
        log: &mut EventLog,
        timer: &Timer,
        doc: &ReplyDocument,
        digest: &str,
        now: DateTime<Utc>,
    ) -> ReplyResult<IngestOutcome> {
        self.ingest_inner(store, log, timer, doc, digest, now, None)
    }

    /// Pre-fire barrier variant: treat `as_current` as the current run even
    /// though the next claim already exists in the store. Only the barrier
    /// (which serializes against the fire transaction) may use this.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_as_current(
        &self,
        store: &Store,
        log: &mut EventLog,
        timer: &Timer,
        doc: &ReplyDocument,
        digest: &str,
        as_current: Uuid,
        now: DateTime<Utc>,
    ) -> ReplyResult<IngestOutcome> {
        self.ingest_inner(store, log, timer, doc, digest, now, Some(as_current))
    }

    #[allow(clippy::too_many_arguments)]
    fn ingest_inner(
        &self,
        store: &Store,
        log: &mut EventLog,
        timer: &Timer,
        doc: &ReplyDocument,
        digest: &str,
        now: DateTime<Utc>,
        current_override: Option<Uuid>,
    ) -> ReplyResult<IngestOutcome> {
        // ── Sight-decidable validation (never debounced) ────────────────
        match doc.schema.as_deref() {
            None => return Ok(IngestOutcome::Rejected(ReplyRejection::MissingSchema)),
            Some(super::document::REPLY_SCHEMA_V1) => {}
            Some(_) => return Ok(IngestOutcome::Rejected(ReplyRejection::BadSchema)),
        }
        let Some(run_id) = doc.run_id else {
            return Ok(IngestOutcome::Rejected(ReplyRejection::MissingRunId));
        };
        let state_str = doc.state.as_deref().unwrap_or("");
        let new_state = match RunState::from_wire(state_str) {
            Some(s) if s.is_app_writable() => s,
            _ if doc.state.is_none() => {
                return Ok(IngestOutcome::Rejected(ReplyRejection::MissingState))
            }
            _ => return Ok(IngestOutcome::Rejected(ReplyRejection::ReservedState)),
        };
        if doc.app_name.is_none() {
            return Ok(IngestOutcome::Rejected(ReplyRejection::MissingAppName));
        }

        // ── Run identification: current, previous-of-this-timer, unknown ─
        let Some(claim) = store.get_run(run_id)? else {
            return Ok(IngestOutcome::Rejected(ReplyRejection::UnknownRun));
        };
        if claim.timer_id != timer.id {
            return Ok(IngestOutcome::Rejected(ReplyRejection::UnknownRun));
        }
        let current = match current_override {
            Some(id) => Some(id),
            None => store.runs_for_timer(timer.id)?.last().map(|c| c.run_id),
        };
        if current != Some(run_id) {
            // A slow app finished after the timer fired again — expected and
            // meaningful. Log `superseded` once per distinct content; never
            // apply it; the transport deletes the stale file.
            let row = store.get_run_state(run_id)?;
            if row
                .as_ref()
                .and_then(|r| r.reply_digest.as_deref())
                == Some(digest)
            {
                return Ok(IngestOutcome::Superseded);
            }
            log.emit(
                EventRecord::new(RunState::Superseded)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_message("reply arrived for a run that is no longer current"),
            )?;
            if let Some(mut row) = row {
                row.reply_digest = Some(digest.to_string());
                store.update_run_state(&row)?;
            }
            return Ok(IngestOutcome::Superseded);
        }

        // ── Current run: owner, regression and estimate validation ──────
        let mut row = match store.get_run_state(run_id)? {
            Some(r) => r,
            // Defensive: the run predates run_states (upgrade mid-run). Rebuild
            // a fired row; this reply satisfies pickup immediately below.
            None => RunStateRow::fired(
                run_id,
                timer.id,
                doc.app_name.as_deref().unwrap_or(""),
                RunState::Fired.as_str(),
                claim.claimed_at,
                now,
            ),
        };
        if doc.app_name.as_deref() != Some(row.app_name.as_str()) {
            return Ok(IngestOutcome::Rejected(ReplyRejection::WrongAppName));
        }
        if row.is_app_authored_terminal()
            && matches!(new_state, RunState::Acknowledged | RunState::Running)
        {
            return Ok(IngestOutcome::Rejected(ReplyRejection::TerminalRegression));
        }
        if doc.error_detection == Some(true)
            && doc.expected_secs.or(row.expected_secs).unwrap_or(0) == 0
        {
            return Ok(IngestOutcome::Rejected(
                ReplyRejection::ErrorDetectionWithoutEstimate,
            ));
        }
        if row.reply_digest.as_deref() == Some(digest) {
            return Ok(IngestOutcome::Duplicate);
        }

        // ── Accept: accumulate, transition, log, mirror ──────────────────
        self.apply(store, log, timer, &claim, &mut row, doc, new_state, digest, now)?;
        Ok(IngestOutcome::Applied)
    }

    /// The accept path, separated so deadline/cursor transitions share the
    /// projection helpers. Caller validated the transition is legal.
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        store: &Store,
        log: &mut EventLog,
        timer: &Timer,
        claim: &RunClaim,
        row: &mut RunStateRow,
        doc: &ReplyDocument,
        new_state: RunState,
        digest: &str,
        now: DateTime<Utc>,
    ) -> ReplyResult<()> {
        let cur_state = row.run_state();

        // Accumulate app fields. Omission retains; only an explicit value
        // replaces (a write never retracts what was folded in earlier).
        if let Some(t) = doc.acknowledged_at {
            row.acknowledged_at = Some(t);
        }
        if row.acknowledged_at.is_none() && new_state == RunState::Acknowledged {
            // The app answered without stamping it — receipt time is the
            // honest observation.
            row.acknowledged_at = Some(now);
        }
        if let Some(secs) = doc.expected_secs {
            // The latest accepted estimate wins, for both consumers.
            row.expected_secs = Some(secs);
        }
        if let Some(detection) = doc.error_detection {
            row.error_detection = Some(detection);
            if !detection {
                // An explicit false cancels the pending watchdog; the
                // estimate stays advisory for the GUI.
                row.watchdog_deadline = None;
            }
        }
        if let Some(t) = doc.heartbeat_at {
            row.heartbeat_at = Some(t);
        }
        if let Some(p) = &doc.progress {
            row.progress = Some(truncate_text(p, MAX_FREE_TEXT_BYTES));
        }

        match new_state {
            RunState::Completed => {
                row.completed_at = Some(doc.completed_at.unwrap_or(now));
                if let Some(result) = &doc.result {
                    let (capped, truncated) = cap_result(result, MAX_RESULT_STATUS_BYTES);
                    row.result_json = Some(capped);
                    row.result_truncated = truncated;
                }
                // The latest terminal verdict wins wholesale.
                row.failed_at = None;
                row.reason = None;
                row.failure_kind = None;
            }
            RunState::Failed => {
                row.failed_at = Some(doc.failed_at.unwrap_or(now));
                if let Some(r) = &doc.reason {
                    row.reason = Some(truncate_text(r, MAX_FREE_TEXT_BYTES));
                }
                row.failure_kind = Some(FailureKind::Reported);
                row.completed_at = None;
                row.result_json = None;
                row.result_truncated = false;
            }
            _ => {}
        }

        // Any valid reply satisfies pickup.
        row.pickup_deadline = None;
        row.state = new_state.as_str().to_string();

        // Watchdog: every distinct accepted non-terminal reply while enabled
        // rearms from Bellman's receipt with the latest estimate. Terminal
        // replies disarm — the run is over.
        if new_state.is_terminal() {
            row.watchdog_deadline = None;
        } else if row.error_detection == Some(true) && row.expected_secs.unwrap_or(0) > 0 {
            row.watchdog_deadline = Some(self.watchdog_deadline(row.expected_secs.unwrap(), now));
        }

        // ── Transition lines (reconstructing what the watcher missed) ───
        if !row.acknowledged_logged {
            let mut detail = serde_json::json!({ "app_name": row.app_name });
            if let Some(secs) = row.expected_secs {
                detail["expected_secs"] = serde_json::json!(secs);
            }
            if let Some(detection) = row.error_detection {
                detail["error_detection"] = serde_json::json!(detection);
            }
            log.emit(
                EventRecord::new(RunState::Acknowledged)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(row.run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_detail(detail)
                    .with_logged_at(row.acknowledged_at.unwrap_or(now)),
            )?;
            row.acknowledged_logged = true;
        }
        if new_state == RunState::Running && !row.running_logged {
            log.emit(
                EventRecord::new(RunState::Running)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(row.run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_logged_at(doc.heartbeat_at.unwrap_or(now)),
            )?;
            row.running_logged = true;
        }
        if new_state.is_terminal() && cur_state != Some(new_state) {
            let (duration_ms, source) = self.duration_ms(row.run_id, row.fired_at, now);
            let mut rec = EventRecord::new(new_state)
                .with_timer(timer.id, timer.name.clone())
                .with_run(row.run_id)
                .with_scheduled_for(claim.scheduled_for)
                .with_duration_ms(duration_ms);
            let mut detail = serde_json::Map::new();
            if let Some(source) = source {
                detail.insert("duration_source".into(), serde_json::json!(source));
            }
            match new_state {
                RunState::Completed => {
                    if let Some(result) = &doc.result {
                        let (capped, truncated) = cap_result(result, MAX_RESULT_EVENT_BYTES);
                        detail.insert("result".into(), capped);
                        if truncated {
                            detail.insert("result_truncated".into(), serde_json::json!(true));
                        }
                    }
                    rec = rec.with_logged_at(row.completed_at.unwrap_or(now));
                }
                RunState::Failed => {
                    detail.insert(
                        "failure_kind".into(),
                        serde_json::json!(FailureKind::Reported.as_str()),
                    );
                    if let Some(reason) = &row.reason {
                        rec = rec.with_message(reason.clone());
                    }
                    rec = rec.with_logged_at(row.failed_at.unwrap_or(now));
                }
                _ => {}
            }
            if !detail.is_empty() {
                rec = rec.with_detail(serde_json::Value::Object(detail));
            }
            log.emit(rec)?;
        }

        row.reply_digest = Some(digest.to_string());
        store.update_run_state(row)?;
        self.project_status(store, timer, &row.run_id)?;
        Ok(())
    }

    /// Rewrite `status.json` from the accumulated database row — the mirror
    /// holds at every step, and rebuilding it is safe precisely because it
    /// is Bellman's alone.
    pub fn project_status(
        &self,
        store: &Store,
        timer: &Timer,
        run_id: &Uuid,
    ) -> ReplyResult<()> {
        let (Some(claim), Some(row)) = (store.get_run(*run_id)?, store.get_run_state(*run_id)?)
        else {
            return Ok(());
        };
        let status = RunStatus::from_run_state(timer, &claim, &row);
        tree::write_status(&self.tree, timer, &status)?;
        Ok(())
    }

    /// Log a `reply_rejected` event. Called by the transport when its
    /// idempotence rules say this content is newly rejected (file transport:
    /// the quarantine artifact was just created).
    pub fn log_rejection(
        &self,
        log: &mut EventLog,
        timer: &Timer,
        run_id: Option<Uuid>,
        reason: &str,
    ) -> ReplyResult<()> {
        let mut rec = EventRecord::new(RunState::ReplyRejected)
            .with_timer(timer.id, timer.name.clone())
            .with_message(reason);
        if let Some(run_id) = run_id {
            rec = rec.with_run(run_id);
        }
        log.emit(rec)?;
        Ok(())
    }

    /// Pickup satisfied by the slot-feed cursor advancing past this run
    /// (R5/R7): records `acknowledged` — never `running` or completion.
    /// Revises a provisional `no_ack` the same way. Only the current run can
    /// be affected; a later firing makes older cursor movement meaningless.
    pub fn on_ack_through(
        &self,
        store: &Store,
        log: &mut EventLog,
        timer: &Timer,
        through_sequence: u64,
        now: DateTime<Utc>,
    ) -> ReplyResult<bool> {
        let Some(row) = store.current_run_state(timer.id)? else {
            return Ok(false);
        };
        let Some(claim) = store.get_run(row.run_id)? else {
            return Ok(false);
        };
        if claim.event_sequence > through_sequence {
            return Ok(false);
        }
        let state = row.run_state();
        let pickup_pending = row.pickup_deadline.is_some()
            && matches!(
                state,
                Some(RunState::Fired) | Some(RunState::FiredLate) | Some(RunState::Coalesced)
            );
        let revise_no_ack = state == Some(RunState::NoAck);
        if !pickup_pending && !revise_no_ack {
            return Ok(false);
        }
        self.mark_acknowledged(store, log, timer, &claim, row, now)?;
        Ok(true)
    }

    /// Shared cursor-pickup / late-cursor-revision transition: the run moves
    /// to `acknowledged` (Bellman-observed receipt time; the feed carries no
    /// app timestamp) and the pickup deadline is consumed.
    fn mark_acknowledged(
        &self,
        store: &Store,
        log: &mut EventLog,
        timer: &Timer,
        claim: &RunClaim,
        mut row: RunStateRow,
        now: DateTime<Utc>,
    ) -> ReplyResult<()> {
        row.state = RunState::Acknowledged.as_str().to_string();
        row.acknowledged_at = Some(now);
        row.pickup_deadline = None;
        if !row.acknowledged_logged {
            log.emit(
                EventRecord::new(RunState::Acknowledged)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(row.run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_detail(serde_json::json!({
                        "app_name": row.app_name,
                        "via": "ack_through",
                    }))
                    .with_logged_at(now),
            )?;
            row.acknowledged_logged = true;
        }
        store.update_run_state(&row)?;
        self.project_status(store, timer, &row.run_id)?;
        Ok(())
    }

    /// Expire lapsed pickup deadlines (R7). For each: the slot-feed cursor
    /// still counts as pickup — declaring `no_ack` while it shows the app
    /// acked would contradict Bellman's own records. Otherwise `no_ack`:
    /// "no acknowledgement was received" (a filesystem read leaves no trace).
    pub fn expire_pickups(
        &self,
        store: &Store,
        log: &mut EventLog,
        now: DateTime<Utc>,
    ) -> ReplyResult<usize> {
        let mut n = 0;
        for stale in store.expired_pickups(now)? {
            let Some(timer) = store.get_timer(stale.timer_id)? else {
                continue;
            };
            n += self.expire_pickup_one(store, log, &timer, stale.run_id, now)?;
        }
        Ok(n)
    }

    /// One pickup-deadline transition for `run_id` of `timer`, re-read under
    /// the caller's gate. Returns 1 when a transition happened.
    pub fn expire_pickup_one(
        &self,
        store: &Store,
        log: &mut EventLog,
        timer: &Timer,
        run_id: Uuid,
        now: DateTime<Utc>,
    ) -> ReplyResult<usize> {
        // Re-check inside the gate: the run must still be current, still
        // pre-pickup, and its deadline still pending.
        let Some(row) = store.get_run_state(run_id)? else {
            return Ok(0);
        };
        if row.pickup_deadline.is_none_or(|d| d > now) {
            return Ok(0);
        }
        let current = store.current_run_state(timer.id)?;
        if current.as_ref().map(|r| r.run_id) != Some(run_id) {
            return Ok(0);
        }
        let Some(claim) = store.get_run(run_id)? else {
            return Ok(0);
        };
        if !matches!(
            row.run_state(),
            Some(RunState::Fired) | Some(RunState::FiredLate) | Some(RunState::Coalesced)
        ) {
            return Ok(0);
        }
        // The slot feed is a durable acknowledgement path that predates the
        // reply channel — it counts.
        if store.last_acked_sequence(timer.id)? >= claim.event_sequence {
            self.mark_acknowledged(store, log, timer, &claim, row, now)?;
            return Ok(1);
        }
        let mut row = row;
        row.state = RunState::NoAck.as_str().to_string();
        row.no_ack_at = Some(now);
        row.pickup_deadline = None;
        log.emit(
            EventRecord::new(RunState::NoAck)
                .with_timer(timer.id, timer.name.clone())
                .with_run(run_id)
                .with_scheduled_for(claim.scheduled_for)
                .with_message("no acknowledgement was received")
                .with_logged_at(now),
        )?;
        store.update_run_state(&row)?;
        self.project_status(store, timer, &run_id)?;
        Ok(1)
    }

    /// Expire lapsed opt-in watchdogs (R8): `failed` with
    /// `failure_kind: "timed_out"` — marking is not killing, and the reply
    /// file is never touched (the divergence between the two files is the
    /// point: `reply.json` is what the app said, `status.json` is the truth).
    pub fn expire_watchdogs(
        &self,
        store: &Store,
        log: &mut EventLog,
        now: DateTime<Utc>,
    ) -> ReplyResult<usize> {
        let mut n = 0;
        for stale in store.expired_watchdogs(now)? {
            let Some(timer) = store.get_timer(stale.timer_id)? else {
                continue;
            };
            n += self.expire_watchdog_one(store, log, &timer, stale.run_id, now)?;
        }
        Ok(n)
    }

    /// One watchdog transition for `run_id`, re-read under the caller's gate.
    pub fn expire_watchdog_one(
        &self,
        store: &Store,
        log: &mut EventLog,
        timer: &Timer,
        run_id: Uuid,
        now: DateTime<Utc>,
    ) -> ReplyResult<usize> {
        let Some(row) = store.get_run_state(run_id)? else {
            return Ok(0);
        };
        if row.watchdog_deadline.is_none_or(|d| d > now) {
            return Ok(0);
        }
        if row.is_terminal() || row.error_detection != Some(true) {
            return Ok(0);
        }
        let current = store.current_run_state(timer.id)?;
        if current.as_ref().map(|r| r.run_id) != Some(run_id) {
            return Ok(0);
        }
        let Some(claim) = store.get_run(run_id)? else {
            return Ok(0);
        };
        let mut row = row;
        row.state = RunState::Failed.as_str().to_string();
        row.failure_kind = Some(FailureKind::TimedOut);
        row.failed_at = Some(now);
        row.watchdog_deadline = None;
        let (duration_ms, source) = self.duration_ms(run_id, row.fired_at, now);
        let mut detail = serde_json::json!({ "failure_kind": FailureKind::TimedOut.as_str() });
        if let Some(source) = source {
            detail["duration_source"] = serde_json::json!(source);
        }
        log.emit(
            EventRecord::new(RunState::Failed)
                .with_timer(timer.id, timer.name.clone())
                .with_run(run_id)
                .with_scheduled_for(claim.scheduled_for)
                .with_duration_ms(duration_ms)
                .with_detail(detail)
                .with_message("error_detection watchdog expired")
                .with_logged_at(now),
        )?;
        store.update_run_state(&row)?;
        self.project_status(store, timer, &run_id)?;
        Ok(1)
    }

    /// The watchdog deadline from Bellman's receipt (`now`) — never the app's
    /// timestamp (R8): a skewed app clock must not fail a healthy app or
    /// extend its own deadline.
    fn watchdog_deadline(&self, expected_secs: u64, now: DateTime<Utc>) -> DateTime<Utc> {
        let millis = (expected_secs as f64 * self.watchdog_factor * 1000.0).max(0.0);
        now + ChronoDuration::milliseconds(millis as i64)
    }

    /// `duration_ms`: Bellman's clock both ends. Monotonic anchor when the
    /// fire happened in this process; after a restart the anchor is gone and
    /// the fallback is wall-clock ingest minus `fired_at` (both Bellman's),
    /// clamped at zero, marked `duration_source: "wall_clock"`.
    fn duration_ms(
        &self,
        run_id: Uuid,
        fired_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> (i64, Option<&'static str>) {
        if let Some(anchor) = self
            .anchors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&run_id)
        {
            let ms = i64::try_from(anchor.elapsed().as_millis()).unwrap_or(i64::MAX);
            return (ms.max(0), None);
        }
        let ms = now.signed_duration_since(fired_at).num_milliseconds();
        (ms.max(0), Some("wall_clock"))
    }
}

/// The one current run of a timer (latest claim), if any.
pub fn current_claim(store: &Store, timer_id: TimerId) -> ReplyResult<Option<RunClaim>> {
    Ok(store.runs_for_timer(timer_id)?.last().cloned())
}
