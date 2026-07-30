//! The transport-agnostic reply ingest engine.
//!
//! Everything from "here is a parsed reply" onward lives here: validation,
//! state transitions, event outbox rows, watchdog arming. This module does
//! not know a file exists — the file watcher and (later, IK6) the socket
//! transport both call [`ReplyEngine::ingest`]. Event lines are ENQUEUED
//! into the SQLite outbox (R11); the elected publisher appends them.
//!
//! Rules encoded here (docs/todo/json_normalization.md R5–R9, R12):
//!
//! - The log records **state transitions only**. `acknowledged` is logged
//!   when the app says `acknowledged`, or reconstructed when the file's own
//!   `acknowledged_at` proves the app answered earlier — Bellman never
//!   invents it. `running` once (first entry), one line per terminal report.
//!   Heartbeats and progress are NEVER logged.
//! - A terminal report never moves backwards to a non-terminal one — but
//!   the app's latest terminal report always wins on the still-current run,
//!   from either source (its own earlier verdict or Bellman's watchdog
//!   inference; there is no `failure_kind` special case).
//! - Bellman's provisional `no_ack` / watchdog `timed_out` may be revised by
//!   any valid app reply while the run is still current.
//! - Deadlines run on Bellman's MONOTONIC clock (the deadline book keyed by
//!   run id). The persisted wall-clock deadline exists only to rebuild the
//!   countdown after a restart — the same explicit clock-jump limitation R8
//!   documents.
//! - `duration_ms` is Bellman's clock both ends: monotonic fire-commit →
//!   terminal-ingest elapsed, with a wall-clock fallback after a restart
//!   (`duration_source: "wall_clock"` marks the estimate).
//!
//! DB access goes through [`RunDb`] so the fire transaction can compose the
//! barrier ingest, the supersede, the new claim and the fired event into
//! ONE atomic commit. `status.json` projection is always the caller's job
//! (post-commit), via [`ReplyEngine::project_status`].

use crate::events::{EventRecord, RunState};
use crate::store::{
    self, FailureKind, RunClaim, RunStateRow, Store, StoreError, StoreResult, Timer, TimerId,
};
use crate::tree::{self, RunStatus, TimersTree, TreeError};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::Transaction;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::document::{
    cap_result, truncate_text, ReplyDocument, ReplyRejection, MAX_FREE_TEXT_BYTES,
    MAX_RESULT_EVENT_BYTES, MAX_RESULT_STATUS_BYTES,
};

// ── Database abstraction ────────────────────────────────────────────────

/// The read/write surface the reply lifecycle needs, so the same code runs
/// on a plain `Store` (single-statement commits) and inside the R10 fire
/// transaction.
pub trait RunDb {
    fn get_timer(&self, id: TimerId) -> StoreResult<Option<Timer>>;
    fn get_run(&self, run_id: Uuid) -> StoreResult<Option<RunClaim>>;
    fn runs_for_timer(&self, timer_id: TimerId) -> StoreResult<Vec<RunClaim>>;
    fn get_run_state(&self, run_id: Uuid) -> StoreResult<Option<RunStateRow>>;
    fn current_run_state(&self, timer_id: TimerId) -> StoreResult<Option<RunStateRow>>;
    fn update_run_state(&self, row: &RunStateRow) -> StoreResult<()>;
    fn last_acked_sequence(&self, timer_id: TimerId) -> StoreResult<u64>;
    fn armed_deadlines(&self) -> StoreResult<Vec<RunStateRow>>;
    fn enqueue_event(&self, rec: &EventRecord) -> StoreResult<()>;
}

impl RunDb for Store {
    fn get_timer(&self, id: TimerId) -> StoreResult<Option<Timer>> {
        Store::get_timer(self, id)
    }
    fn get_run(&self, run_id: Uuid) -> StoreResult<Option<RunClaim>> {
        Store::get_run(self, run_id)
    }
    fn runs_for_timer(&self, timer_id: TimerId) -> StoreResult<Vec<RunClaim>> {
        Store::runs_for_timer(self, timer_id)
    }
    fn get_run_state(&self, run_id: Uuid) -> StoreResult<Option<RunStateRow>> {
        Store::get_run_state(self, run_id)
    }
    fn current_run_state(&self, timer_id: TimerId) -> StoreResult<Option<RunStateRow>> {
        Store::current_run_state(self, timer_id)
    }
    fn update_run_state(&self, row: &RunStateRow) -> StoreResult<()> {
        Store::update_run_state(self, row)
    }
    fn last_acked_sequence(&self, timer_id: TimerId) -> StoreResult<u64> {
        Store::last_acked_sequence(self, timer_id)
    }
    fn armed_deadlines(&self) -> StoreResult<Vec<RunStateRow>> {
        Store::armed_deadlines(self)
    }
    fn enqueue_event(&self, rec: &EventRecord) -> StoreResult<()> {
        Store::enqueue_event(self, rec)
    }
}

impl RunDb for Transaction<'_> {
    fn get_timer(&self, id: TimerId) -> StoreResult<Option<Timer>> {
        store::get_timer_conn(self, id)
    }
    fn get_run(&self, run_id: Uuid) -> StoreResult<Option<RunClaim>> {
        store::get_run_conn(self, run_id)
    }
    fn runs_for_timer(&self, timer_id: TimerId) -> StoreResult<Vec<RunClaim>> {
        store::runs_for_timer_conn(self, timer_id)
    }
    fn get_run_state(&self, run_id: Uuid) -> StoreResult<Option<RunStateRow>> {
        store::get_run_state_conn(self, run_id)
    }
    fn current_run_state(&self, timer_id: TimerId) -> StoreResult<Option<RunStateRow>> {
        store::current_run_state_conn(self, timer_id)
    }
    fn update_run_state(&self, row: &RunStateRow) -> StoreResult<()> {
        store::update_run_state_conn(self, row)
    }
    fn last_acked_sequence(&self, timer_id: TimerId) -> StoreResult<u64> {
        store::last_acked_sequence_conn(self, timer_id)
    }
    fn armed_deadlines(&self) -> StoreResult<Vec<RunStateRow>> {
        store::armed_deadlines_conn(self)
    }
    fn enqueue_event(&self, rec: &EventRecord) -> StoreResult<()> {
        store::enqueue_event_conn(self, rec)
    }
}

// ── Shared registries ───────────────────────────────────────────────────

/// Monotonic fire-commit anchors for `duration_ms`, keyed by run id. Shared
/// between the fire path (registers at commit) and the ingest path (consumes
/// at the terminal transition). A process restart loses them — the wall-clock
/// fallback in [`ReplyEngine::duration_ms`] covers that case explicitly.
pub type SharedAnchors = Arc<Mutex<HashMap<Uuid, Instant>>>;

/// Fresh empty anchor registry.
pub fn new_anchors() -> SharedAnchors {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Which deadline a book entry is counting down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineKind {
    /// No pickup signal yet — `no_ack` when it lapses.
    Pickup,
    /// The app opted into `error_detection` — `failed`/`timed_out` when it
    /// lapses.
    Watchdog,
}

/// One live countdown: Bellman's monotonic clock, armed at receipt.
#[derive(Debug, Clone, Copy)]
pub struct MonoDeadline {
    pub kind: DeadlineKind,
    pub at: Instant,
}

/// Live monotonic countdowns keyed by run id (R7/R8). The persisted
/// wall-clock deadline on the run row is the restart-reconstruction source
/// only — the active countdown never reads the wall clock, so wall jumps
/// (NTP, suspend, DST) cannot fire a deadline early or late.
pub type SharedDeadlines = Arc<Mutex<HashMap<Uuid, MonoDeadline>>>;

/// Fresh empty deadline book.
pub fn new_deadlines() -> SharedDeadlines {
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
    /// Monotonic deadline book shared with the fire path and the watcher.
    pub deadlines: SharedDeadlines,
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
    Io(std::io::Error),
}

impl std::fmt::Display for ReplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "reply store: {e}"),
            Self::Tree(e) => write!(f, "reply tree: {e}"),
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

impl From<std::io::Error> for ReplyError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub type ReplyResult<T> = Result<T, ReplyError>;

impl ReplyEngine {
    /// Register a just-committed fire: the pickup countdown starts on
    /// Bellman's monotonic clock at the fire transaction commit (R7), and
    /// the duration anchor is set for `duration_ms`.
    pub fn register_fire(&self, run_id: Uuid) {
        let now = Instant::now();
        self.anchors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run_id, now);
        self.deadlines
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                run_id,
                MonoDeadline {
                    kind: DeadlineKind::Pickup,
                    at: now + self.pickup_grace,
                },
            );
    }

    /// Drop any live countdowns for a run (supersede / cancel).
    pub fn clear_deadlines(&self, run_id: Uuid) {
        self.deadlines
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&run_id);
    }

    /// Rebuild the countdowns from the persisted wall-clock deadlines after
    /// a restart (R8: the wall value exists only for this reconstruction).
    /// Existing live entries always win.
    pub fn sync_deadline_book(
        &self,
        db: &dyn RunDb,
        now_wall: DateTime<Utc>,
        mono_now: Instant,
    ) -> ReplyResult<usize> {
        let mut added = 0;
        let mut book = self.deadlines.lock().unwrap_or_else(|p| p.into_inner());
        for row in db.armed_deadlines()? {
            if book.contains_key(&row.run_id) {
                continue;
            }
            let entry = if let Some(deadline) = row.pickup_deadline {
                Some(MonoDeadline {
                    kind: DeadlineKind::Pickup,
                    at: mono_now + remaining(deadline, now_wall),
                })
            } else {
                row.watchdog_deadline.map(|deadline| MonoDeadline {
                    kind: DeadlineKind::Watchdog,
                    at: mono_now + remaining(deadline, now_wall),
                })
            };
            if let Some(entry) = entry {
                book.insert(row.run_id, entry);
                added += 1;
            }
        }
        Ok(added)
    }

    /// Ingest one parsed reply for `timer`. `digest` identifies the exact
    /// reply content (transport-computed) so an exact duplicate is a no-op
    /// and cannot re-arm the watchdog. `now_wall` is Bellman's wall clock at
    /// receipt (timestamps only); `mono_now` is Bellman's monotonic receipt
    /// (the only clock deadlines count on).
    pub fn ingest(
        &self,
        db: &dyn RunDb,
        timer: &Timer,
        doc: &ReplyDocument,
        digest: &str,
        now_wall: DateTime<Utc>,
        mono_now: Instant,
    ) -> ReplyResult<IngestOutcome> {
        self.ingest_inner(db, timer, doc, digest, now_wall, mono_now, None)
    }

    /// Pre-fire barrier variant: treat `as_current` as the current run even
    /// though the next claim already exists in the store. Only the barrier
    /// (which serializes against the fire transaction) may use this.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_as_current(
        &self,
        db: &dyn RunDb,
        timer: &Timer,
        doc: &ReplyDocument,
        digest: &str,
        as_current: Uuid,
        now_wall: DateTime<Utc>,
        mono_now: Instant,
    ) -> ReplyResult<IngestOutcome> {
        self.ingest_inner(db, timer, doc, digest, now_wall, mono_now, Some(as_current))
    }

    #[allow(clippy::too_many_arguments)]
    fn ingest_inner(
        &self,
        db: &dyn RunDb,
        timer: &Timer,
        doc: &ReplyDocument,
        digest: &str,
        now_wall: DateTime<Utc>,
        mono_now: Instant,
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
        let Some(claim) = db.get_run(run_id)? else {
            return Ok(IngestOutcome::Rejected(ReplyRejection::UnknownRun));
        };
        if claim.timer_id != timer.id {
            return Ok(IngestOutcome::Rejected(ReplyRejection::UnknownRun));
        }
        let current = match current_override {
            Some(id) => Some(id),
            None => db.runs_for_timer(timer.id)?.last().map(|c| c.run_id),
        };
        if current != Some(run_id) {
            // A slow app finished after the timer fired again — expected and
            // meaningful. Log `superseded` once per distinct content; never
            // apply it; the transport deletes the stale file.
            let row = db.get_run_state(run_id)?;
            if row
                .as_ref()
                .and_then(|r| r.reply_digest.as_deref())
                == Some(digest)
            {
                return Ok(IngestOutcome::Superseded);
            }
            db.enqueue_event(
                &EventRecord::new(RunState::Superseded)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_message("reply arrived for a run that is no longer current"),
            )?;
            if let Some(mut row) = row {
                row.reply_digest = Some(digest.to_string());
                db.update_run_state(&row)?;
            }
            return Ok(IngestOutcome::Superseded);
        }

        // ── Current run: owner, regression and estimate validation ──────
        let mut row = match db.get_run_state(run_id)? {
            Some(r) => r,
            // Defensive: the run predates run_states (upgrade mid-run). Rebuild
            // a fired row; this reply satisfies pickup immediately below.
            None => RunStateRow::fired(
                run_id,
                timer.id,
                doc.app_name.as_deref().unwrap_or(""),
                RunState::Fired.as_str(),
                claim.claimed_at,
                now_wall,
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

        // ── Accept: accumulate, transition, log ──────────────────────────
        self.apply(db, timer, &claim, &mut row, doc, new_state, digest, now_wall, mono_now)?;
        Ok(IngestOutcome::Applied)
    }

    /// The accept path. Caller validated the transition is legal.
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        db: &dyn RunDb,
        timer: &Timer,
        claim: &RunClaim,
        row: &mut RunStateRow,
        doc: &ReplyDocument,
        new_state: RunState,
        digest: &str,
        now_wall: DateTime<Utc>,
        mono_now: Instant,
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
            row.acknowledged_at = Some(now_wall);
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
                row.completed_at = Some(doc.completed_at.unwrap_or(now_wall));
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
                row.failed_at = Some(doc.failed_at.unwrap_or(now_wall));
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

        // Deadlines on the monotonic book: pickup is consumed by any valid
        // reply; the watchdog rearms from Bellman's RECEIPT of each distinct
        // accepted non-terminal reply while enabled; terminal disarms.
        {
            let mut book = self.deadlines.lock().unwrap_or_else(|p| p.into_inner());
            if book.get(&row.run_id).is_some_and(|d| d.kind == DeadlineKind::Pickup) {
                book.remove(&row.run_id);
            }
            if new_state.is_terminal() {
                book.remove(&row.run_id);
                row.watchdog_deadline = None;
            } else if row.error_detection == Some(true) && row.expected_secs.unwrap_or(0) > 0 {
                let at = self.watchdog_instant(row.expected_secs.unwrap(), mono_now);
                book.insert(
                    row.run_id,
                    MonoDeadline {
                        kind: DeadlineKind::Watchdog,
                        at,
                    },
                );
                // The persisted wall deadline is the restart-reconstruction
                // copy of the same countdown.
                row.watchdog_deadline =
                    Some(self.watchdog_wall(row.expected_secs.unwrap(), now_wall));
            } else {
                if book.get(&row.run_id).is_some_and(|d| d.kind == DeadlineKind::Watchdog) {
                    book.remove(&row.run_id);
                }
            }
        }

        // ── Transition lines (reconstructing what the watcher missed) ───
        // `acknowledged` is logged when the app SAYS acknowledged, or
        // reconstructed when the file's own acknowledged_at proves the app
        // answered earlier — Bellman never invents it (a direct
        // fired → completed with no acknowledged_at logs completed only).
        if !row.acknowledged_logged && row.acknowledged_at.is_some() {
            let mut detail = serde_json::json!({ "app_name": row.app_name });
            if let Some(secs) = row.expected_secs {
                detail["expected_secs"] = serde_json::json!(secs);
            }
            if let Some(detection) = row.error_detection {
                detail["error_detection"] = serde_json::json!(detection);
            }
            db.enqueue_event(
                &EventRecord::new(RunState::Acknowledged)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(row.run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_detail(detail)
                    .with_logged_at(row.acknowledged_at.unwrap_or(now_wall)),
            )?;
            row.acknowledged_logged = true;
        }
        if new_state == RunState::Running && !row.running_logged {
            db.enqueue_event(
                &EventRecord::new(RunState::Running)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(row.run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_logged_at(doc.heartbeat_at.unwrap_or(now_wall)),
            )?;
            row.running_logged = true;
        }
        if new_state.is_terminal() && cur_state != Some(new_state) {
            let (duration_ms, source) = self.duration_ms(row.run_id, row.fired_at, now_wall);
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
                    rec = rec.with_logged_at(row.completed_at.unwrap_or(now_wall));
                }
                RunState::Failed => {
                    detail.insert(
                        "failure_kind".into(),
                        serde_json::json!(FailureKind::Reported.as_str()),
                    );
                    if let Some(reason) = &row.reason {
                        rec = rec.with_message(reason.clone());
                    }
                    rec = rec.with_logged_at(row.failed_at.unwrap_or(now_wall));
                }
                _ => {}
            }
            if !detail.is_empty() {
                rec = rec.with_detail(serde_json::Value::Object(detail));
            }
            db.enqueue_event(&rec)?;
        }

        row.reply_digest = Some(digest.to_string());
        db.update_run_state(row)?;
        Ok(())
    }

    /// Rewrite `status.json` from the accumulated database row — the mirror
    /// holds at every step, and rebuilding it is safe precisely because it
    /// is Bellman's alone. Always a POST-COMMIT projection: callers run it
    /// after the mutating transaction, never inside it.
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

    /// Enqueue a `reply_rejected` event. Called by the transport when its
    /// idempotence rules say this content is newly rejected (file transport:
    /// the quarantine artifact was just created).
    pub fn log_rejection(
        &self,
        db: &dyn RunDb,
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
        db.enqueue_event(&rec)?;
        Ok(())
    }

    /// Pickup satisfied by the slot-feed cursor advancing past this run
    /// (R5/R7): records `acknowledged` — never `running` or completion.
    /// Revises a provisional `no_ack` the same way. Only the current run can
    /// be affected; a later firing makes older cursor movement meaningless.
    /// Returns true when a transition happened (caller projects status).
    pub fn on_ack_through(
        &self,
        db: &dyn RunDb,
        timer: &Timer,
        through_sequence: u64,
        now_wall: DateTime<Utc>,
    ) -> ReplyResult<bool> {
        let Some(row) = db.current_run_state(timer.id)? else {
            return Ok(false);
        };
        let Some(claim) = db.get_run(row.run_id)? else {
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
        self.mark_acknowledged(db, timer, &claim, row, now_wall)?;
        Ok(true)
    }

    /// Shared cursor-pickup / late-cursor-revision transition: the run moves
    /// to `acknowledged` (Bellman-observed receipt time; the feed carries no
    /// app timestamp) and the pickup deadline is consumed.
    fn mark_acknowledged(
        &self,
        db: &dyn RunDb,
        timer: &Timer,
        claim: &RunClaim,
        mut row: RunStateRow,
        now_wall: DateTime<Utc>,
    ) -> ReplyResult<()> {
        row.state = RunState::Acknowledged.as_str().to_string();
        row.acknowledged_at = Some(now_wall);
        row.pickup_deadline = None;
        self.clear_deadlines_kind(row.run_id, DeadlineKind::Pickup);
        if !row.acknowledged_logged {
            db.enqueue_event(
                &EventRecord::new(RunState::Acknowledged)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(row.run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_detail(serde_json::json!({
                        "app_name": row.app_name,
                        "via": "ack_through",
                    }))
                    .with_logged_at(now_wall),
            )?;
            row.acknowledged_logged = true;
        }
        db.update_run_state(&row)?;
        Ok(())
    }

    /// Expire lapsed pickup countdowns (monotonic book). For each: the
    /// slot-feed cursor still counts as pickup — declaring `no_ack` while it
    /// shows the app acked would contradict Bellman's own records. Otherwise
    /// `no_ack`: "no acknowledgement was received" (a filesystem read leaves
    /// no trace). Returns the number of transitions (caller projects status
    /// for each).
    pub fn expire_pickups(
        &self,
        db: &dyn RunDb,
        now_wall: DateTime<Utc>,
        mono_now: Instant,
    ) -> ReplyResult<Vec<Uuid>> {
        let due = self.due_deadlines(DeadlineKind::Pickup, mono_now);
        let mut transitioned = Vec::new();
        for run_id in due {
            let Ok(Some(row)) = db.get_run_state(run_id) else {
                self.clear_deadlines(run_id);
                continue;
            };
            let Ok(Some(timer)) = db.get_timer(row.timer_id) else {
                self.clear_deadlines(run_id);
                continue;
            };
            if self.expire_pickup_one(db, &timer, run_id, now_wall)? {
                transitioned.push(run_id);
            }
        }
        Ok(transitioned)
    }

    /// One pickup-deadline transition for `run_id` of `timer`, re-read under
    /// the caller's gate. Returns true when a transition happened.
    pub fn expire_pickup_one(
        &self,
        db: &dyn RunDb,
        timer: &Timer,
        run_id: Uuid,
        now_wall: DateTime<Utc>,
    ) -> ReplyResult<bool> {
        // Re-check inside the gate: the run must still be current, still
        // pre-pickup, and its deadline still armed.
        let Some(row) = db.get_run_state(run_id)? else {
            return Ok(false);
        };
        if row.pickup_deadline.is_none() {
            return Ok(false);
        }
        let current = db.current_run_state(timer.id)?;
        if current.as_ref().map(|r| r.run_id) != Some(run_id) {
            return Ok(false);
        }
        let Some(claim) = db.get_run(run_id)? else {
            return Ok(false);
        };
        if !matches!(
            row.run_state(),
            Some(RunState::Fired) | Some(RunState::FiredLate) | Some(RunState::Coalesced)
        ) {
            return Ok(false);
        }
        // The slot feed is a durable acknowledgement path that predates the
        // reply channel — it counts.
        if db.last_acked_sequence(timer.id)? >= claim.event_sequence {
            self.mark_acknowledged(db, timer, &claim, row, now_wall)?;
            return Ok(true);
        }
        let mut row = row;
        row.state = RunState::NoAck.as_str().to_string();
        row.no_ack_at = Some(now_wall);
        row.pickup_deadline = None;
        self.clear_deadlines(run_id);
        db.enqueue_event(
            &EventRecord::new(RunState::NoAck)
                .with_timer(timer.id, timer.name.clone())
                .with_run(run_id)
                .with_scheduled_for(claim.scheduled_for)
                .with_message("no acknowledgement was received")
                .with_logged_at(now_wall),
        )?;
        db.update_run_state(&row)?;
        Ok(true)
    }

    /// Expire lapsed opt-in watchdog countdowns (monotonic book): `failed`
    /// with `failure_kind: "timed_out"` — marking is not killing, and the
    /// reply file is never touched. Returns the transitioned runs (caller
    /// projects status).
    pub fn expire_watchdogs(
        &self,
        db: &dyn RunDb,
        now_wall: DateTime<Utc>,
        mono_now: Instant,
    ) -> ReplyResult<Vec<Uuid>> {
        let due = self.due_deadlines(DeadlineKind::Watchdog, mono_now);
        let mut transitioned = Vec::new();
        for run_id in due {
            let Ok(Some(row)) = db.get_run_state(run_id) else {
                self.clear_deadlines(run_id);
                continue;
            };
            let Ok(Some(timer)) = db.get_timer(row.timer_id) else {
                self.clear_deadlines(run_id);
                continue;
            };
            if self.expire_watchdog_one(db, &timer, run_id, now_wall)? {
                transitioned.push(run_id);
            }
        }
        Ok(transitioned)
    }

    /// One watchdog transition for `run_id`, re-read under the caller's gate.
    pub fn expire_watchdog_one(
        &self,
        db: &dyn RunDb,
        timer: &Timer,
        run_id: Uuid,
        now_wall: DateTime<Utc>,
    ) -> ReplyResult<bool> {
        let Some(row) = db.get_run_state(run_id)? else {
            return Ok(false);
        };
        if row.watchdog_deadline.is_none() {
            return Ok(false);
        }
        if row.is_terminal() || row.error_detection != Some(true) {
            return Ok(false);
        }
        let current = db.current_run_state(timer.id)?;
        if current.as_ref().map(|r| r.run_id) != Some(run_id) {
            return Ok(false);
        }
        let Some(claim) = db.get_run(run_id)? else {
            return Ok(false);
        };
        let mut row = row;
        row.state = RunState::Failed.as_str().to_string();
        row.failure_kind = Some(FailureKind::TimedOut);
        row.failed_at = Some(now_wall);
        row.watchdog_deadline = None;
        self.clear_deadlines(run_id);
        let (duration_ms, source) = self.duration_ms(run_id, row.fired_at, now_wall);
        let mut detail = serde_json::json!({ "failure_kind": FailureKind::TimedOut.as_str() });
        if let Some(source) = source {
            detail["duration_source"] = serde_json::json!(source);
        }
        db.enqueue_event(
            &EventRecord::new(RunState::Failed)
                .with_timer(timer.id, timer.name.clone())
                .with_run(run_id)
                .with_scheduled_for(claim.scheduled_for)
                .with_duration_ms(duration_ms)
                .with_detail(detail)
                .with_message("error_detection watchdog expired")
                .with_logged_at(now_wall),
        )?;
        db.update_run_state(&row)?;
        Ok(true)
    }

    /// Book entries of `kind` whose monotonic deadline has lapsed.
    pub fn due_deadlines(&self, kind: DeadlineKind, mono_now: Instant) -> Vec<Uuid> {
        self.deadlines
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, d)| d.kind == kind && d.at <= mono_now)
            .map(|(id, _)| *id)
            .collect()
    }

    fn clear_deadlines_kind(&self, run_id: Uuid, kind: DeadlineKind) {
        let mut book = self.deadlines.lock().unwrap_or_else(|p| p.into_inner());
        if book.get(&run_id).is_some_and(|d| d.kind == kind) {
            book.remove(&run_id);
        }
    }

    /// The watchdog countdown from Bellman's monotonic receipt — never the
    /// app's timestamp (R8).
    fn watchdog_instant(&self, expected_secs: u64, mono_now: Instant) -> Instant {
        mono_now + self.watchdog_span(expected_secs)
    }

    /// The persisted wall-clock copy of the same countdown (restart
    /// reconstruction only).
    fn watchdog_wall(&self, expected_secs: u64, now_wall: DateTime<Utc>) -> DateTime<Utc> {
        now_wall
            + ChronoDuration::from_std(self.watchdog_span(expected_secs))
                .unwrap_or_else(|_| ChronoDuration::seconds(0))
    }

    fn watchdog_span(&self, expected_secs: u64) -> Duration {
        Duration::from_secs_f64((expected_secs as f64 * self.watchdog_factor).max(0.0))
    }

    /// `duration_ms`: Bellman's clock both ends. Monotonic anchor when the
    /// fire happened in this process; after a restart the anchor is gone and
    /// the fallback is wall-clock ingest minus `fired_at` (both Bellman's),
    /// clamped at zero, marked `duration_source: "wall_clock"`.
    fn duration_ms(
        &self,
        run_id: Uuid,
        fired_at: DateTime<Utc>,
        now_wall: DateTime<Utc>,
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
        let ms = now_wall.signed_duration_since(fired_at).num_milliseconds();
        (ms.max(0), Some("wall_clock"))
    }
}

/// Wall-clock remainder until `deadline`, clamped at zero (clock jumps can
/// only shorten the reconstructed countdown, never extend it — the
/// documented restart-fallback limitation).
fn remaining(deadline: DateTime<Utc>, now_wall: DateTime<Utc>) -> Duration {
    deadline
        .signed_duration_since(now_wall)
        .to_std()
        .unwrap_or(Duration::ZERO)
}

/// The one current run of a timer (latest claim), if any.
pub fn current_claim(store: &Store, timer_id: TimerId) -> ReplyResult<Option<RunClaim>> {
    Ok(store.runs_for_timer(timer_id)?.last().cloned())
}
