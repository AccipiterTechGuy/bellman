//! Persistent timer store (SQLite WAL, rusqlite bundled).
//!
//! ## Scheduler-facing API (narrow surface for C3)
//!
//! - [`Store::timers_due_by`] — near-horizon query: enabled timers with
//!   `next_fire_utc <= horizon` (inclusive), ordered by fire time.
//! - [`Store::claim_run`] / [`Store::complete_run`] / [`Store::pending_claims`] —
//!   at-least-once claim ledger keyed by `(timer_id, scheduled_for)`.
//!
//! SQLite is authoritative for *what will happen*; the claim ledger makes
//! crash-between-claim-and-completion recoverable on reopen.

mod error;
mod fs_guard;
mod models;
mod schema;

pub use error::{StoreError, StoreResult};
pub use models::{
    Action, ClaimStatus, FailureKind, Meta, MisfirePolicy, NewTimer, OverlapPolicy, RetryPolicy,
    RotationJournal, RotationPhase, RunClaim, RunOutcome, RunStateRow, SlotRequestRecord, Timer,
    TimerId, TimerPatch, TimerUpdate, TransportMode, TransportProjection, TRANSPORT_IPC,
    TRANSPORT_IPC_FALLBACK, TRANSPORT_JSON,
};

use crate::occurrence::Occurrence;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use schema::{migrate, SCHEMA_VERSION};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Column list for `runs` reads (phase + outcome + dispatch fields, SCH1).
const RUN_COLS: &str = "run_id, timer_id, scheduled_for, status, claimed_at, completed_at,
     COALESCE(event_sequence, 0), outcome, outcome_reason,
     COALESCE(cancel_requested, 0)";

/// Open options for a store database.
#[derive(Debug, Clone, Copy)]
pub struct OpenOptions {
    /// When true (default), refuse paths on network filesystems.
    pub refuse_network_fs: bool,
    /// SQLite busy timeout in milliseconds (default 5_000).
    pub busy_timeout_ms: u32,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            refuse_network_fs: true,
            busy_timeout_ms: 5_000,
        }
    }
}

/// SQLite-backed timer store.
///
/// On clean drop, checkpoints the WAL with `TRUNCATE` so `-wal`/`-shm`
/// sidecars do not linger for backup tools.
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open (or create) a store at `path`. Applies migrations and PRAGMAs.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        Self::open_with(path, OpenOptions::default())
    }

    /// Open with explicit options (tests may disable the network-FS check).
    pub fn open_with(path: impl AsRef<Path>, opts: OpenOptions) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    StoreError::Io(format!("create parent dir {}: {e}", parent.display()))
                })?;
            }
        }

        if opts.refuse_network_fs {
            fs_guard::refuse_network_fs(&path)?;
        }

        let conn = Connection::open(&path)
            .map_err(|e| StoreError::Sqlite(format!("open {}: {e}", path.display())))?;

        apply_pragmas(&conn, opts.busy_timeout_ms)?;
        migrate(&conn)?;

        Ok(Self { conn, path })
    }

    /// Filesystem path of the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Begin an IMMEDIATE transaction (crate-internal). The R10 fire
    /// transaction composes barrier ingest + supersede + claim + lifecycle
    /// row + fired event into one atomic commit; IMMEDIATE takes the write
    /// lock up front so the SELECT-then-INSERT claim cannot die with
    /// `BUSY_SNAPSHOT` against a concurrent commit (which `busy_timeout`
    /// deliberately does not retry).
    pub(crate) fn transaction(&mut self) -> StoreResult<Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?)
    }

    /// Current schema version from `meta` / `user_version`.
    pub fn schema_version(&self) -> StoreResult<i32> {
        let v: i32 =
            self.conn
                .query_row("SELECT schema_version FROM meta WHERE id = 1", [], |r| {
                    r.get(0)
                })?;
        Ok(v)
    }

    /// Read meta bookkeeping row.
    pub fn meta(&self) -> StoreResult<Meta> {
        let (schema_version, last_prune, last_recalibration, tzdata_version): (
            i32,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = self
            .conn
            .query_row(
                "SELECT schema_version, last_prune, last_recalibration, tzdata_version
                 FROM meta WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(StoreError::from)?;
        Ok(Meta {
            schema_version,
            last_prune: parse_opt_dt(last_prune)?,
            last_recalibration: parse_opt_dt(last_recalibration)?,
            tzdata_version,
        })
    }

    /// Stamp `meta.last_prune` (weekly prune bookkeeping).
    pub fn set_last_prune(&mut self, when: DateTime<Utc>) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE meta SET last_prune = ?1 WHERE id = 1",
            params![fmt_dt(when)],
        )?;
        Ok(())
    }

    /// Stamp `meta.last_recalibration` (Jan-1 consistency pass).
    pub fn set_last_recalibration(&mut self, when: DateTime<Utc>) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE meta SET last_recalibration = ?1 WHERE id = 1",
            params![fmt_dt(when)],
        )?;
        Ok(())
    }

    // ── Timer CRUD ──────────────────────────────────────────────────────

    /// Insert a new timer. Computes `next_fire_utc` in the same transaction.
    pub fn create_timer(&mut self, new: NewTimer) -> StoreResult<Timer> {
        let tx = self.conn.transaction()?;
        let timer = create_timer_tx(&tx, new)?;
        tx.commit()?;
        Ok(timer)
    }

    /// Fetch a timer by id.
    pub fn get_timer(&self, id: TimerId) -> StoreResult<Option<Timer>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
                    misfire_policy, overlap_policy, retry_policy,
                    valid_from, valid_until, max_runs, tags, action, revision,
                    COALESCE(jitter_secs, 0), accuracy_slack_secs,
                    COALESCE(wake_machine, 0), COALESCE(transport, 'json')
             FROM timers WHERE id = ?1",
        )?;
        let row = stmt
            .query_row(params![id.to_string()], row_to_timer)
            .optional()?;
        Ok(row)
    }

    /// List all timers (any enabled state), ordered by name then id.
    pub fn list_timers(&self) -> StoreResult<Vec<Timer>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
                    misfire_policy, overlap_policy, retry_policy,
                    valid_from, valid_until, max_runs, tags, action, revision,
                    COALESCE(jitter_secs, 0), accuracy_slack_secs,
                    COALESCE(wake_machine, 0), COALESCE(transport, 'json')
             FROM timers ORDER BY name, id",
        )?;
        let rows = stmt.query_map([], row_to_timer)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Optimistic update: succeeds only when `expected_revision` matches.
    /// Recomputes `next_fire_utc` in the same transaction. IMMEDIATE: the
    /// read-modify-write must not die with BUSY_SNAPSHOT against a concurrent
    /// worker/producer commit (busy_timeout deliberately does not retry those).
    pub fn update_timer(&mut self, update: TimerUpdate) -> StoreResult<Timer> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let timer = update_timer_tx(&tx, update)?;
        tx.commit()?;
        Ok(timer)
    }

    /// Delete a timer by id. Returns true if a row was removed.
    pub fn delete_timer(&mut self, id: TimerId) -> StoreResult<bool> {
        let n = self
            .conn
            .execute("DELETE FROM timers WHERE id = ?1", params![id.to_string()])?;
        Ok(n > 0)
    }

    // ── Horizon query (scheduler) ───────────────────────────────────────

    /// Enabled timers whose `next_fire_utc` is at or before `horizon` (UTC).
    ///
    /// Ordered ascending by `next_fire_utc`, then id. Timers with a NULL
    /// next fire (exhausted / disabled schedule) are never returned.
    pub fn timers_due_by(&self, horizon: DateTime<Utc>) -> StoreResult<Vec<Timer>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
                    misfire_policy, overlap_policy, retry_policy,
                    valid_from, valid_until, max_runs, tags, action, revision,
                    COALESCE(jitter_secs, 0), accuracy_slack_secs,
                    COALESCE(wake_machine, 0), COALESCE(transport, 'json')
             FROM timers
             WHERE enabled = 1
               AND next_fire_utc IS NOT NULL
               AND next_fire_utc <= ?1
             ORDER BY next_fire_utc ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![fmt_dt(horizon)], row_to_timer)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ── Claim ledger (at-least-once) ────────────────────────────────────

    /// Claim a scheduled fire. Fails with [`StoreError::AlreadyClaimed`] if
    /// `(timer_id, scheduled_for)` already exists — even after a crash, the
    /// pending claim remains visible so the scheduler can recover.
    ///
    /// Assigns a durable monotonic `event_sequence` per timer for the slot
    /// output feed.
    pub fn claim_run(
        &mut self,
        timer_id: TimerId,
        scheduled_for: DateTime<Utc>,
    ) -> StoreResult<RunClaim> {
        // IMMEDIATE: the MAX(event_sequence) read and the claim INSERT must
        // not be split by a concurrent commit (BUSY_SNAPSHOT, and a duplicate
        // sequence).
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let claim = claim_run_conn(&tx, timer_id, scheduled_for)?;
        tx.commit()?;
        Ok(claim)
    }

    /// Mark a previously claimed run as completed (wake action delivered).
    pub fn complete_run(&mut self, run_id: Uuid) -> StoreResult<RunClaim> {
        self.finish_run(run_id, RunOutcome::WakeDelivered, None)
    }

    /// Mark a run as wake-failed (action failed after retries). Terminal for
    /// recovery like [`Store::complete_run`], but the failure is recorded
    /// instead of being rewritten as success.
    pub fn fail_run(&mut self, run_id: Uuid) -> StoreResult<RunClaim> {
        self.finish_run(run_id, RunOutcome::WakeFailed, None)
    }

    /// Finish a run with an explicit outcome and machine reason (SCH1).
    ///
    /// Accepts the transition from `pending` (fire-transaction skips,
    /// `Replace` before-start) or `active` (worker results); an already
    /// `finished` claim is never rewritten — [`StoreError::RunNotFound`].
    pub fn finish_run(
        &mut self,
        run_id: Uuid,
        outcome: RunOutcome,
        reason: Option<&str>,
    ) -> StoreResult<RunClaim> {
        let completed_at = Utc::now();
        let n = self.conn.execute(
            "UPDATE runs SET status = ?1, outcome = ?2, outcome_reason = ?3, completed_at = ?4
             WHERE run_id = ?5 AND status IN (?6, ?7)",
            params![
                ClaimStatus::Finished.as_str(),
                outcome.as_str(),
                reason,
                fmt_dt(completed_at),
                run_id.to_string(),
                ClaimStatus::Pending.as_str(),
                ClaimStatus::Active.as_str(),
            ],
        )?;
        if n != 1 {
            return Err(StoreError::RunNotFound(run_id));
        }
        self.get_run(run_id)?.ok_or(StoreError::RunNotFound(run_id))
    }

    /// CAS `pending → active` (SCH1): a worker must win this transition
    /// before executing. Duplicate or stale queue hints lose it and do
    /// nothing, so a live process cannot execute one claim concurrently twice.
    pub fn activate_run(&mut self, run_id: Uuid) -> StoreResult<bool> {
        let n = self.conn.execute(
            "UPDATE runs SET status = ?1 WHERE run_id = ?2 AND status = ?3",
            params![
                ClaimStatus::Active.as_str(),
                run_id.to_string(),
                ClaimStatus::Pending.as_str(),
            ],
        )?;
        Ok(n == 1)
    }

    /// Return one `active` claim to `pending` (in-process worker-supervisor
    /// recovery after a worker panic/exit — the lane never had a chance to
    /// commit, so the at-least-once rule re-queues the same `run_id`).
    pub fn repend_run(&mut self, run_id: Uuid) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?1 WHERE run_id = ?2 AND status = ?3",
            params![
                ClaimStatus::Pending.as_str(),
                run_id.to_string(),
                ClaimStatus::Active.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Return every `active` claim to `pending`. ONLY the process holding the
    /// dispatcher OS lock may call this (at startup, after acquiring the lock
    /// it knows the previous dispatcher is gone). An arbitrary CLI must not.
    pub fn repend_all_active(&mut self) -> StoreResult<u64> {
        let n = self.conn.execute(
            "UPDATE runs SET status = ?1 WHERE status = ?2",
            params![
                ClaimStatus::Pending.as_str(),
                ClaimStatus::Active.as_str(),
            ],
        )?;
        Ok(n as u64)
    }

    /// Active claims with a durable cancellation request (`Replace`) that the
    /// dispatcher has not necessarily signalled to a worker token yet.
    pub fn cancel_requested_active(&self) -> StoreResult<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id FROM runs
             WHERE status = ?1 AND COALESCE(cancel_requested, 0) = 1
             ORDER BY run_id ASC",
        )?;
        let rows = stmt.query_map(params![ClaimStatus::Active.as_str()], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for r in rows {
            let s = r?;
            out.push(
                Uuid::parse_str(&s)
                    .map_err(|e| StoreError::Internal(format!("bad run_id '{s}': {e}")))?,
            );
        }
        Ok(out)
    }

    /// Fetch a run claim by id.
    pub fn get_run(&self, run_id: Uuid) -> StoreResult<Option<RunClaim>> {
        get_run_conn(&self.conn, run_id)
    }

    /// Pending (not yet active) run claims — the dispatcher pump surface.
    pub fn pending_claims(&self) -> StoreResult<Vec<RunClaim>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RUN_COLS} FROM runs WHERE status = ?1
             ORDER BY claimed_at ASC, run_id ASC"
        ))?;
        let rows = stmt.query_map(params![ClaimStatus::Pending.as_str()], row_to_claim)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Look up the claim ledger row for `(timer_id, scheduled_for)`, if any.
    pub fn get_claim_for(
        &self,
        timer_id: TimerId,
        scheduled_for: DateTime<Utc>,
    ) -> StoreResult<Option<RunClaim>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RUN_COLS} FROM runs WHERE timer_id = ?1 AND scheduled_for = ?2"
        ))?;
        let row = stmt
            .query_row(
                params![timer_id.to_string(), fmt_dt(scheduled_for)],
                row_to_claim,
            )
            .optional()?;
        Ok(row)
    }

    /// Checkpoint WAL with TRUNCATE (also invoked on clean drop).
    pub fn checkpoint_truncate(&self) -> StoreResult<()> {
        self.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(|e| StoreError::Sqlite(format!("wal_checkpoint TRUNCATE: {e}")))
    }

    // ── Slot IPC: idempotency + ownership ───────────────────────────────

    /// Look up a prior slot request by its `request_id` (idempotency key).
    pub fn get_slot_request(&self, request_id: &str) -> StoreResult<Option<SlotRequestRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT request_id, slot_id, operation, app_name, timer_id, status,
                    response_json, created_at
             FROM slot_requests WHERE request_id = ?1",
        )?;
        let row = stmt
            .query_row(params![request_id], row_to_slot_request)
            .optional()?;
        Ok(row)
    }

    /// Most recent successful slot request that created/touched `timer_id`.
    ///
    /// Used by wake delivery to rewrite `done/slot-<id>.json` with fire events
    /// for the integrating app (PLAN: launch + write output-slot JSON).
    pub fn latest_slot_request_for_timer(
        &self,
        timer_id: TimerId,
    ) -> StoreResult<Option<SlotRequestRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT request_id, slot_id, operation, app_name, timer_id, status,
                    response_json, created_at
             FROM slot_requests
             WHERE timer_id = ?1 AND status = 'ok'
             ORDER BY created_at DESC, request_id DESC
             LIMIT 1",
        )?;
        let row = stmt
            .query_row(params![timer_id.to_string()], row_to_slot_request)
            .optional()?;
        Ok(row)
    }

    /// Persist a completed slot request response (first write wins for duplicates).
    ///
    /// Returns `true` if this call inserted the row; `false` if `request_id`
    /// was already present (caller should re-read the stored response).
    ///
    /// Prefer [`Store::slot_execute_once`] for new code — it binds reservation,
    /// timer mutations, and the ledger write in a single transaction.
    pub fn put_slot_request(&mut self, rec: &SlotRequestRecord) -> StoreResult<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO slot_requests (
                request_id, slot_id, operation, app_name, timer_id,
                status, response_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                rec.request_id,
                rec.slot_id,
                rec.operation,
                rec.app_name,
                rec.timer_id.map(|id| id.to_string()),
                rec.status,
                rec.response_json,
                fmt_dt(rec.created_at),
            ],
        )?;
        Ok(n > 0)
    }

    /// Atomically execute a slot request: idempotent by `request_id`.
    ///
    /// Uses `BEGIN IMMEDIATE` so concurrent consumers serialize. If the
    /// `request_id` already has a terminal ledger row, returns that record
    /// without running `apply`. Otherwise runs `apply` inside the same
    /// transaction as the ledger insert (timer CRUD + ownership + response).
    pub fn slot_execute_once<F>(
        &mut self,
        request_id: &str,
        apply: F,
    ) -> StoreResult<SlotRequestRecord>
    where
        F: FnOnce(&Transaction<'_>) -> StoreResult<SlotRequestRecord>,
    {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        if let Some(existing) = get_slot_request_tx(&tx, request_id)? {
            // Terminal result already committed by a prior (or concurrent) worker.
            tx.commit()?;
            return Ok(existing);
        }

        let rec = apply(&tx)?;
        insert_slot_request_tx(&tx, &rec)?;
        tx.commit()?;
        Ok(rec)
    }

    /// Create a timer inside an open transaction (used by slot_execute_once).
    pub fn create_timer_in_tx(tx: &Transaction<'_>, new: NewTimer) -> StoreResult<Timer> {
        create_timer_tx(tx, new)
    }

    /// Update a timer inside an open transaction.
    pub fn update_timer_in_tx(tx: &Transaction<'_>, update: TimerUpdate) -> StoreResult<Timer> {
        update_timer_tx(tx, update)
    }

    /// Delete a timer inside an open transaction. Returns true if a row was removed.
    pub fn delete_timer_in_tx(tx: &Transaction<'_>, id: TimerId) -> StoreResult<bool> {
        let n = tx.execute("DELETE FROM timers WHERE id = ?1", params![id.to_string()])?;
        Ok(n > 0)
    }

    /// Fetch a timer inside an open transaction.
    pub fn get_timer_in_tx(tx: &Transaction<'_>, id: TimerId) -> StoreResult<Option<Timer>> {
        get_timer_tx(tx, id)
    }

    /// Record which integrating app created a timer (slot ownership).
    pub fn set_timer_owner(&mut self, timer_id: TimerId, app_name: &str) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO timer_owners (timer_id, app_name) VALUES (?1, ?2)
             ON CONFLICT(timer_id) DO UPDATE SET app_name = excluded.app_name",
            params![timer_id.to_string(), app_name],
        )?;
        Ok(())
    }

    /// Set timer owner inside an open transaction.
    pub fn set_timer_owner_in_tx(
        tx: &Transaction<'_>,
        timer_id: TimerId,
        app_name: &str,
    ) -> StoreResult<()> {
        tx.execute(
            "INSERT INTO timer_owners (timer_id, app_name) VALUES (?1, ?2)
             ON CONFLICT(timer_id) DO UPDATE SET app_name = excluded.app_name",
            params![timer_id.to_string(), app_name],
        )?;
        Ok(())
    }

    /// Owner app_name for a timer created via the slot layer, if any.
    pub fn get_timer_owner(&self, timer_id: TimerId) -> StoreResult<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT app_name FROM timer_owners WHERE timer_id = ?1")?;
        let row = stmt
            .query_row(params![timer_id.to_string()], |r| r.get::<_, String>(0))
            .optional()?;
        Ok(row)
    }

    /// Owner lookup inside an open transaction.
    pub fn get_timer_owner_in_tx(
        tx: &Transaction<'_>,
        timer_id: TimerId,
    ) -> StoreResult<Option<String>> {
        let mut stmt = tx.prepare("SELECT app_name FROM timer_owners WHERE timer_id = ?1")?;
        let row = stmt
            .query_row(params![timer_id.to_string()], |r| r.get::<_, String>(0))
            .optional()?;
        Ok(row)
    }

    /// Drop ownership row when a timer is deleted via slots (or cleanup).
    pub fn clear_timer_owner(&mut self, timer_id: TimerId) -> StoreResult<()> {
        self.conn.execute(
            "DELETE FROM timer_owners WHERE timer_id = ?1",
            params![timer_id.to_string()],
        )?;
        Ok(())
    }

    /// Clear owner inside an open transaction.
    pub fn clear_timer_owner_in_tx(tx: &Transaction<'_>, timer_id: TimerId) -> StoreResult<()> {
        tx.execute(
            "DELETE FROM timer_owners WHERE timer_id = ?1",
            params![timer_id.to_string()],
        )?;
        Ok(())
    }

    /// All run-claim rows for a timer (ordered by event_sequence).
    pub fn runs_for_timer(&self, timer_id: TimerId) -> StoreResult<Vec<RunClaim>> {
        runs_for_timer_conn(&self.conn, timer_id)
    }

    /// Run-claim rows whose `scheduled_for` falls in `[from, to)` (UTC half-open).
    ///
    /// Used by the calendar truth model so past cells can surface durable
    /// ledger evidence even when JSONL history has been pruned.
    pub fn runs_in_range(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> StoreResult<Vec<RunClaim>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RUN_COLS} FROM runs
             WHERE scheduled_for >= ?1 AND scheduled_for < ?2
             ORDER BY scheduled_for ASC, run_id ASC"
        ))?;
        let rows = stmt.query_map(params![fmt_dt(from), fmt_dt(to)], row_to_claim)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Last acknowledged event_sequence for a timer (0 if never acked).
    pub fn last_acked_sequence(&self, timer_id: TimerId) -> StoreResult<u64> {
        last_acked_sequence_conn(&self.conn, timer_id)
    }

    /// Last acked sequence inside an open transaction.
    pub fn last_acked_sequence_in_tx(
        tx: &Transaction<'_>,
        timer_id: TimerId,
    ) -> StoreResult<u64> {
        let mut stmt = tx.prepare(
            "SELECT last_acked_sequence FROM slot_event_acks WHERE timer_id = ?1",
        )?;
        let row: Option<i64> = stmt
            .query_row(params![timer_id.to_string()], |r| r.get(0))
            .optional()?;
        Ok(row.unwrap_or(0) as u64)
    }

    /// Advance the durable ack cursor (never moves backwards).
    pub fn ack_run_events(&mut self, timer_id: TimerId, through_sequence: u64) -> StoreResult<u64> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let new = ack_run_events_tx(&tx, timer_id, through_sequence)?;
        tx.commit()?;
        Ok(new)
    }

    /// Ack inside an open transaction.
    pub fn ack_run_events_in_tx(
        tx: &Transaction<'_>,
        timer_id: TimerId,
        through_sequence: u64,
    ) -> StoreResult<u64> {
        ack_run_events_tx(tx, timer_id, through_sequence)
    }

    /// Un-acked run events for a timer with sequence > last_ack, ordered, limited.
    pub fn unacked_runs_for_timer(
        &self,
        timer_id: TimerId,
        limit: usize,
    ) -> StoreResult<Vec<RunClaim>> {
        let last_ack = self.last_acked_sequence(timer_id)?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RUN_COLS} FROM runs
             WHERE timer_id = ?1 AND COALESCE(event_sequence, 0) > ?2
             ORDER BY event_sequence ASC, run_id ASC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![timer_id.to_string(), last_ack as i64, limit as i64],
            row_to_claim,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Un-acked runs inside an open transaction.
    pub fn unacked_runs_for_timer_in_tx(
        tx: &Transaction<'_>,
        timer_id: TimerId,
        limit: usize,
    ) -> StoreResult<Vec<RunClaim>> {
        let last_ack = Self::last_acked_sequence_in_tx(tx, timer_id)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {RUN_COLS} FROM runs
             WHERE timer_id = ?1 AND COALESCE(event_sequence, 0) > ?2
             ORDER BY event_sequence ASC, run_id ASC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![timer_id.to_string(), last_ack as i64, limit as i64],
            row_to_claim,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ── Run states (IK3 reply channel) ──────────────────────────────────

    /// Insert the fire-time row for an owned run (owner snapshotted).
    pub fn insert_run_state(&self, row: &RunStateRow) -> StoreResult<()> {
        insert_run_state_conn(&self.conn, row)
    }

    /// Fetch the app-lifecycle row for a run, if the run is integration-owned.
    pub fn get_run_state(&self, run_id: Uuid) -> StoreResult<Option<RunStateRow>> {
        get_run_state_conn(&self.conn, run_id)
    }

    /// App-lifecycle row for the timer's CURRENT run (latest by event
    /// sequence), if that run is integration-owned.
    pub fn current_run_state(&self, timer_id: TimerId) -> StoreResult<Option<RunStateRow>> {
        current_run_state_conn(&self.conn, timer_id)
    }

    /// Overwrite every mutable column of a run-state row (the accumulated
    /// reply view, deadlines, transition flags).
    pub fn update_run_state(&self, row: &RunStateRow) -> StoreResult<()> {
        update_run_state_conn(&self.conn, row)
    }

    /// Owned runs with an armed deadline (pickup pending or watchdog
    /// ticking) — the restart-reconstruction surface for the monotonic
    /// deadline book.
    pub fn armed_deadlines(&self) -> StoreResult<Vec<RunStateRow>> {
        armed_deadlines_conn(&self.conn)
    }

    // ── Event outbox (R11) ──────────────────────────────────────────────

    /// Enqueue one event for the elected publisher (single-statement, so
    /// atomic by itself; SQLite serialises across processes — that is the
    /// whole reason the outbox is the funnel). Idempotent by `event_id`.
    pub fn enqueue_event(&self, rec: &crate::events::EventRecord) -> StoreResult<()> {
        enqueue_event_conn(&self.conn, rec)
    }

    /// Enqueue inside an open transaction — this is how the fire transaction
    /// commits run state and its events atomically.
    pub fn enqueue_event_in_tx(
        tx: &Transaction<'_>,
        rec: &crate::events::EventRecord,
    ) -> StoreResult<()> {
        let payload = serde_json::to_string(rec)
            .map_err(|e| StoreError::Serde(format!("serialize event: {e}")))?;
        tx.execute(
            "INSERT OR IGNORE INTO event_outbox (event_id, payload, enqueued_at)
             VALUES (?1, ?2, ?3)",
            params![rec.event_id.to_string(), payload, fmt_dt(rec.logged_at)],
        )?;
        Ok(())
    }

    /// Pending (unpublished) outbox rows in enqueue order.
    pub fn pending_events(&self, limit: usize) -> StoreResult<Vec<(Uuid, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, payload FROM event_outbox
             ORDER BY enqueued_at ASC, event_id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, payload) = r?;
            let id = Uuid::parse_str(&id)
                .map_err(|e| StoreError::Internal(format!("bad outbox event_id: {e}")))?;
            out.push((id, payload));
        }
        Ok(out)
    }

    /// Number of pending outbox rows (publisher health).
    pub fn count_pending_events(&self) -> StoreResult<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM event_outbox", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Mark a row published — DELETE, so the outbox actually empties. Called
    /// only after the line is appended AND fdatasynced (a crash between sync
    /// and this delete re-appends; readers dedupe by `event_id`).
    pub fn mark_event_published(&self, event_id: Uuid) -> StoreResult<()> {
        self.conn.execute(
            "DELETE FROM event_outbox WHERE event_id = ?1",
            params![event_id.to_string()],
        )?;
        Ok(())
    }

    /// Mark inside an open transaction.
    pub fn mark_event_published_in_tx(tx: &Transaction<'_>, event_id: Uuid) -> StoreResult<()> {
        tx.execute(
            "DELETE FROM event_outbox WHERE event_id = ?1",
            params![event_id.to_string()],
        )?;
        Ok(())
    }

    // ── Rotation journal (R11) ──────────────────────────────────────────

    /// The in-flight rotation journal, if any.
    pub fn rotation_journal(&self) -> StoreResult<Option<RotationJournal>> {
        let mut stmt = self.conn.prepare(
            "SELECT source, rotating, gz_tmp, final_path, phase, started_at
             FROM rotation_journal WHERE id = 1",
        )?;
        let row = stmt
            .query_row([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .optional()?;
        let Some((source, rotating, gz_tmp, final_path, phase, started_at)) = row else {
            return Ok(None);
        };
        Ok(Some(RotationJournal {
            source: source.into(),
            rotating: rotating.into(),
            gz_tmp: gz_tmp.into(),
            final_path: final_path.into(),
            phase: RotationPhase::from_wire(&phase)
                .ok_or_else(|| StoreError::Internal(format!("bad journal phase '{phase}'")))?,
            started_at: parse_dt(&started_at)?,
        }))
    }

    /// Record/advance the rotation journal (single-row upsert).
    pub fn set_rotation_journal(&self, journal: &RotationJournal) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO rotation_journal (id, source, rotating, gz_tmp, final_path, phase, started_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                source = excluded.source, rotating = excluded.rotating,
                gz_tmp = excluded.gz_tmp, final_path = excluded.final_path,
                phase = excluded.phase, started_at = excluded.started_at",
            params![
                journal.source.to_string_lossy(),
                journal.rotating.to_string_lossy(),
                journal.gz_tmp.to_string_lossy(),
                journal.final_path.to_string_lossy(),
                journal.phase.as_str(),
                fmt_dt(journal.started_at),
            ],
        )?;
        Ok(())
    }

    /// Clear the journal once the new current file exists and the archive is
    /// durable.
    pub fn clear_rotation_journal(&self) -> StoreResult<()> {
        self.conn
            .execute("DELETE FROM rotation_journal WHERE id = 1", [])?;
        Ok(())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Best-effort clean close: TRUNCATE so sidecars vanish for backup tools.
        let _ = self.conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");
    }
}

// ── Internals ───────────────────────────────────────────────────────────

fn apply_pragmas(conn: &Connection, busy_timeout_ms: u32) -> StoreResult<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| StoreError::Sqlite(format!("journal_mode=WAL: {e}")))?;
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(|e| StoreError::Sqlite(format!("synchronous=FULL: {e}")))?;
    conn.pragma_update(None, "busy_timeout", busy_timeout_ms)
        .map_err(|e| StoreError::Sqlite(format!("busy_timeout: {e}")))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| StoreError::Sqlite(format!("foreign_keys: {e}")))?;
    // Confirm WAL actually engaged (some FS refuse it).
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .map_err(|e| StoreError::Sqlite(format!("read journal_mode: {e}")))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Sqlite(format!(
            "expected journal_mode=WAL, got {mode}"
        )));
    }
    let _ = SCHEMA_VERSION; // keep import live for docs
    Ok(())
}

/// Next fire strictly after `last_fired` (or `now` if never fired), in UTC.
fn compute_next_fire(
    occ: &mut Occurrence,
    last_fired: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let after = last_fired.unwrap_or(now);
    let tz = occ.timezone();
    let after_local = after.with_timezone(&tz);
    occ.next_fire(after_local).map(|dt| dt.with_timezone(&Utc))
}

fn apply_patch(timer: &mut Timer, patch: TimerPatch) -> StoreResult<()> {
    if let Some(name) = patch.name {
        timer.name = name;
    }
    if let Some(enabled) = patch.enabled {
        timer.enabled = enabled;
    }
    if let Some(occ) = patch.occurrence {
        occ.kind()
            .validate()
            .map_err(StoreError::InvalidOccurrence)?;
        timer.occurrence = occ;
    }
    if let Some(m) = patch.misfire {
        timer.misfire = m;
    }
    if let Some(o) = patch.overlap {
        timer.overlap = o;
    }
    if let Some(r) = patch.retry {
        timer.retry = r;
    }
    if let Some(tags) = patch.tags {
        timer.tags = tags;
    }
    if let Some(action) = patch.action {
        timer.action = action;
    }
    if let Some(lf) = patch.last_fired {
        timer.last_fired = lf;
    }
    if let Some(j) = patch.jitter_secs {
        timer.jitter_secs = j;
    }
    if let Some(a) = patch.accuracy_slack_secs {
        timer.accuracy_slack_secs = a;
    }
    if let Some(w) = patch.wake_machine {
        timer.wake_machine = w;
    }
    if let Some(t) = patch.transport {
        timer.transport = t;
    }
    Ok(())
}

fn create_timer_tx(tx: &Transaction<'_>, new: NewTimer) -> StoreResult<Timer> {
    new.occurrence
        .kind()
        .validate()
        .map_err(StoreError::InvalidOccurrence)?;

    let id = new.id.unwrap_or_else(Uuid::new_v4);
    let now = Utc::now();
    let mut occ = new.occurrence;
    let next = compute_next_fire(&mut occ, new.last_fired, now);
    let revision = 1i64;
    let tags_json = serde_json::to_string(&new.tags)?;
    let action_json = serde_json::to_string(&new.action)?;
    let occ_json = serde_json::to_string(&occ)?;
    let misfire = serde_json::to_string(&new.misfire)?;
    let overlap = serde_json::to_string(&new.overlap)?;
    let retry = serde_json::to_string(&new.retry)?;
    let tz = occ.tz_name().to_string();
    let max_runs = occ_max_runs(&occ);
    let valid_from = occ_valid_from(&occ);
    let valid_until = occ_valid_until(&occ);

    tx.execute(
        "INSERT INTO timers (
            id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
            misfire_policy, overlap_policy, retry_policy,
            valid_from, valid_until, max_runs, tags, action, revision,
            jitter_secs, accuracy_slack_secs, wake_machine, transport
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10,
            ?11, ?12, ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20
         )",
        params![
            id.to_string(),
            new.name,
            i64::from(new.enabled),
            occ_json,
            tz,
            next.map(fmt_dt),
            new.last_fired.map(fmt_dt),
            misfire,
            overlap,
            retry,
            valid_from.map(fmt_dt),
            valid_until.map(fmt_dt),
            max_runs_to_sql(max_runs),
            tags_json,
            action_json,
            revision,
            i64::from(new.jitter_secs),
            new.accuracy_slack_secs.map(i64::from),
            i64::from(new.wake_machine),
            new.transport.as_str(),
        ],
    )?;
    get_timer_tx(tx, id)?.ok_or_else(|| StoreError::Internal("timer missing after insert".into()))
}

fn update_timer_tx(tx: &Transaction<'_>, update: TimerUpdate) -> StoreResult<Timer> {
    let current = get_timer_tx(tx, update.id)?.ok_or(StoreError::NotFound(update.id))?;

    if current.revision != update.expected_revision {
        return Err(StoreError::StaleRevision {
            id: update.id,
            expected: update.expected_revision,
            actual: current.revision,
        });
    }

    let mut next = current;
    apply_patch(&mut next, update.patch)?;

    let mut occ = next.occurrence.clone();
    let now = Utc::now();
    next.next_fire_utc = compute_next_fire(&mut occ, next.last_fired, now);
    next.occurrence = occ;
    next.tz = next.occurrence.tz_name().to_string();
    next.max_runs = occ_max_runs(&next.occurrence);
    next.valid_from = occ_valid_from(&next.occurrence);
    next.valid_until = occ_valid_until(&next.occurrence);
    next.revision = next.revision.saturating_add(1);

    let n = tx.execute(
        "UPDATE timers SET
            name = ?1, enabled = ?2, occurrence = ?3, tz = ?4,
            next_fire_utc = ?5, last_fired = ?6,
            misfire_policy = ?7, overlap_policy = ?8, retry_policy = ?9,
            valid_from = ?10, valid_until = ?11, max_runs = ?12,
            tags = ?13, action = ?14, revision = ?15,
            jitter_secs = ?16, accuracy_slack_secs = ?17, wake_machine = ?18,
            transport = ?19
         WHERE id = ?20 AND revision = ?21",
        params![
            next.name,
            i64::from(next.enabled),
            serde_json::to_string(&next.occurrence)?,
            next.tz,
            next.next_fire_utc.map(fmt_dt),
            next.last_fired.map(fmt_dt),
            serde_json::to_string(&next.misfire)?,
            serde_json::to_string(&next.overlap)?,
            serde_json::to_string(&next.retry)?,
            next.valid_from.map(fmt_dt),
            next.valid_until.map(fmt_dt),
            max_runs_to_sql(next.max_runs),
            serde_json::to_string(&next.tags)?,
            serde_json::to_string(&next.action)?,
            next.revision,
            i64::from(next.jitter_secs),
            next.accuracy_slack_secs.map(i64::from),
            i64::from(next.wake_machine),
            next.transport.as_str(),
            next.id.to_string(),
            update.expected_revision,
        ],
    )?;
    if n != 1 {
        return Err(StoreError::StaleRevision {
            id: update.id,
            expected: update.expected_revision,
            actual: current_revision_tx(tx, update.id)?,
        });
    }
    get_timer_tx(tx, update.id)?
        .ok_or_else(|| StoreError::Internal("timer missing after update".into()))
}

pub(crate) fn get_timer_conn(conn: &Connection, id: TimerId) -> StoreResult<Option<Timer>> {
    get_timer_tx_inner(conn, id)
}

fn get_timer_tx(tx: &Transaction<'_>, id: TimerId) -> StoreResult<Option<Timer>> {
    get_timer_tx_inner(tx, id)
}

fn get_timer_tx_inner(tx: &Connection, id: TimerId) -> StoreResult<Option<Timer>> {
    let mut stmt = tx.prepare(
        "SELECT id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
                misfire_policy, overlap_policy, retry_policy,
                valid_from, valid_until, max_runs, tags, action, revision,
                COALESCE(jitter_secs, 0), accuracy_slack_secs,
                COALESCE(wake_machine, 0), COALESCE(transport, 'json')
         FROM timers WHERE id = ?1",
    )?;
    let row = stmt
        .query_row(params![id.to_string()], row_to_timer)
        .optional()?;
    Ok(row)
}

fn get_slot_request_tx(
    tx: &Transaction<'_>,
    request_id: &str,
) -> StoreResult<Option<SlotRequestRecord>> {
    let mut stmt = tx.prepare(
        "SELECT request_id, slot_id, operation, app_name, timer_id, status,
                response_json, created_at
         FROM slot_requests WHERE request_id = ?1",
    )?;
    let row = stmt
        .query_row(params![request_id], row_to_slot_request)
        .optional()?;
    Ok(row)
}

fn insert_slot_request_tx(tx: &Transaction<'_>, rec: &SlotRequestRecord) -> StoreResult<()> {
    tx.execute(
        "INSERT INTO slot_requests (
            request_id, slot_id, operation, app_name, timer_id,
            status, response_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            rec.request_id,
            rec.slot_id,
            rec.operation,
            rec.app_name,
            rec.timer_id.map(|id| id.to_string()),
            rec.status,
            rec.response_json,
            fmt_dt(rec.created_at),
        ],
    )?;
    Ok(())
}

fn ack_run_events_tx(
    tx: &Transaction<'_>,
    timer_id: TimerId,
    through_sequence: u64,
) -> StoreResult<u64> {
    let current: i64 = {
        let mut stmt = tx.prepare(
            "SELECT last_acked_sequence FROM slot_event_acks WHERE timer_id = ?1",
        )?;
        stmt.query_row(params![timer_id.to_string()], |r| r.get(0))
            .optional()?
            .unwrap_or(0)
    };
    let new = current.max(through_sequence as i64);
    tx.execute(
        "INSERT INTO slot_event_acks (timer_id, last_acked_sequence) VALUES (?1, ?2)
         ON CONFLICT(timer_id) DO UPDATE SET
           last_acked_sequence = MAX(slot_event_acks.last_acked_sequence, excluded.last_acked_sequence)",
        params![timer_id.to_string(), new],
    )?;
    Ok(new as u64)
}

fn current_revision_tx(tx: &Transaction<'_>, id: TimerId) -> StoreResult<i64> {
    let rev: Option<i64> = tx
        .query_row(
            "SELECT revision FROM timers WHERE id = ?1",
            params![id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    rev.ok_or(StoreError::NotFound(id))
}

fn row_to_timer(r: &rusqlite::Row<'_>) -> rusqlite::Result<Timer> {
    let id_s: String = r.get(0)?;
    let id = Uuid::parse_str(&id_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let name: String = r.get(1)?;
    let enabled: i64 = r.get(2)?;
    let occ_json: String = r.get(3)?;
    let tz: String = r.get(4)?;
    let next_s: Option<String> = r.get(5)?;
    let last_s: Option<String> = r.get(6)?;
    let misfire_json: String = r.get(7)?;
    let overlap_json: String = r.get(8)?;
    let retry_json: String = r.get(9)?;
    let valid_from_s: Option<String> = r.get(10)?;
    let valid_until_s: Option<String> = r.get(11)?;
    let max_runs: Option<i64> = r.get(12)?;
    let tags_json: String = r.get(13)?;
    let action_json: String = r.get(14)?;
    let revision: i64 = r.get(15)?;
    let jitter_secs_i: i64 = r.get(16)?;
    let accuracy_slack_i: Option<i64> = r.get(17)?;
    let wake_machine_i: i64 = r.get(18)?;
    let transport_s: String = r.get(19)?;

    let occurrence: Occurrence = serde_json::from_str(&occ_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let misfire: MisfirePolicy = serde_json::from_str(&misfire_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let overlap: OverlapPolicy = serde_json::from_str(&overlap_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let retry: RetryPolicy = serde_json::from_str(&retry_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let tags: Vec<String> = serde_json::from_str(&tags_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(13, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let action: Action = serde_json::from_str(&action_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(14, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let jitter_secs = u32::try_from(jitter_secs_i.max(0)).unwrap_or(u32::MAX);
    let accuracy_slack_secs = accuracy_slack_i.and_then(|n| {
        if n < 0 {
            None
        } else {
            u32::try_from(n).ok()
        }
    });

    Ok(Timer {
        id,
        name,
        enabled: enabled != 0,
        occurrence,
        tz,
        next_fire_utc: parse_opt_dt(next_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            )
        })?,
        last_fired: parse_opt_dt(last_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            )
        })?,
        misfire,
        overlap,
        retry,
        valid_from: parse_opt_dt(valid_from_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            )
        })?,
        valid_until: parse_opt_dt(valid_until_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                11,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            )
        })?,
        max_runs: max_runs_from_sql(max_runs),
        tags,
        action,
        revision,
        jitter_secs,
        accuracy_slack_secs,
        wake_machine: wake_machine_i != 0,
        transport: TransportMode::from_wire(&transport_s).unwrap_or_default(),
    })
}

fn row_to_slot_request(r: &rusqlite::Row<'_>) -> rusqlite::Result<SlotRequestRecord> {
    let request_id: String = r.get(0)?;
    let slot_id: String = r.get(1)?;
    let operation: String = r.get(2)?;
    let app_name: Option<String> = r.get(3)?;
    let timer_id_s: Option<String> = r.get(4)?;
    let status: String = r.get(5)?;
    let response_json: String = r.get(6)?;
    let created_s: String = r.get(7)?;

    let timer_id = match timer_id_s {
        Some(s) => Some(Uuid::parse_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?),
        None => None,
    };
    let created_at = parse_dt(&created_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )),
        )
    })?;

    Ok(SlotRequestRecord {
        request_id,
        slot_id,
        operation,
        app_name,
        timer_id,
        status,
        response_json,
        created_at,
    })
}

fn row_to_claim(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunClaim> {
    let run_id_s: String = r.get(0)?;
    let timer_id_s: String = r.get(1)?;
    let sched_s: String = r.get(2)?;
    let status_s: String = r.get(3)?;
    let claimed_s: String = r.get(4)?;
    let completed_s: Option<String> = r.get(5)?;
    let event_sequence: i64 = r.get(6)?;
    let outcome_s: Option<String> = r.get(7)?;
    let outcome_reason: Option<String> = r.get(8)?;
    let cancel_requested_i: i64 = r.get(9)?;

    let run_id = Uuid::parse_str(&run_id_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let timer_id = Uuid::parse_str(&timer_id_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let scheduled_for = parse_dt(&sched_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )),
        )
    })?;
    let status: ClaimStatus = status_s.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    let claimed_at = parse_dt(&claimed_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )),
        )
    })?;
    let completed_at = parse_opt_dt(completed_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )),
        )
    })?;
    let outcome = outcome_s.and_then(|s| RunOutcome::from_wire(&s));

    Ok(RunClaim {
        run_id,
        timer_id,
        scheduled_for,
        status,
        claimed_at,
        completed_at,
        event_sequence: event_sequence.max(0) as u64,
        outcome,
        outcome_reason,
        cancel_requested: cancel_requested_i != 0,
    })
}

fn fmt_dt(dt: DateTime<Utc>) -> String {
    // Nanoseconds: interval anchors and next_fire use full chrono precision.
    // Millis truncation made `next_fire(after=stored)` return the same truncated
    // instant (true fire was still strictly after the truncated value), so the
    // scheduler could not advance past a just-fired slot.
    dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn parse_dt(s: &str) -> StoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| StoreError::Internal(format!("bad datetime '{s}': {e}")))
}

fn parse_opt_dt(s: Option<String>) -> StoreResult<Option<DateTime<Utc>>> {
    match s {
        None => Ok(None),
        Some(s) => parse_dt(&s).map(Some),
    }
}

/// Column list for `run_states` reads (kept in one place for the joins).
const RUN_STATE_COLS: &str = "rs.run_id, rs.timer_id, rs.app_name, rs.state, rs.fired_at,
     rs.pickup_deadline, rs.acknowledged_at, rs.expected_secs, rs.error_detection,
     rs.heartbeat_at, rs.progress, rs.completed_at, rs.failed_at, rs.reason,
     rs.failure_kind, rs.result_json, rs.result_truncated, rs.watchdog_deadline,
     rs.no_ack_at, rs.reply_digest, rs.acknowledged_logged, rs.running_logged,
     rs.selected_transport, rs.transport";

fn row_to_run_state(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunStateRow> {
    let conv = |i: usize, msg: String| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, msg.into())
    };
    let run_id_s: String = r.get(0)?;
    let timer_id_s: String = r.get(1)?;
    let fired_s: String = r.get(4)?;
    let result_s: Option<String> = r.get(15)?;
    let failure_s: Option<String> = r.get(14)?;
    Ok(RunStateRow {
        run_id: Uuid::parse_str(&run_id_s).map_err(|e| conv(0, e.to_string()))?,
        timer_id: Uuid::parse_str(&timer_id_s).map_err(|e| conv(1, e.to_string()))?,
        app_name: r.get(2)?,
        state: r.get(3)?,
        fired_at: parse_dt(&fired_s).map_err(|e| conv(4, e.to_string()))?,
        pickup_deadline: parse_opt_dt(r.get(5)?).map_err(|e| conv(5, e.to_string()))?,
        acknowledged_at: parse_opt_dt(r.get(6)?).map_err(|e| conv(6, e.to_string()))?,
        expected_secs: r.get::<_, Option<i64>>(7)?.map(|s| s.max(0) as u64),
        error_detection: r.get(8)?,
        heartbeat_at: parse_opt_dt(r.get(9)?).map_err(|e| conv(9, e.to_string()))?,
        progress: r.get(10)?,
        completed_at: parse_opt_dt(r.get(11)?).map_err(|e| conv(11, e.to_string()))?,
        failed_at: parse_opt_dt(r.get(12)?).map_err(|e| conv(12, e.to_string()))?,
        reason: r.get(13)?,
        failure_kind: failure_s.and_then(|s| FailureKind::from_wire(&s)),
        result_json: result_s.and_then(|s| serde_json::from_str(&s).ok()),
        result_truncated: r.get(16)?,
        watchdog_deadline: parse_opt_dt(r.get(17)?).map_err(|e| conv(17, e.to_string()))?,
        no_ack_at: parse_opt_dt(r.get(18)?).map_err(|e| conv(18, e.to_string()))?,
        reply_digest: r.get(19)?,
        acknowledged_logged: r.get(20)?,
        running_logged: r.get(21)?,
        selected_transport: r.get(22)?,
        transport: r.get(23)?,
    })
}

// Occurrence does not expose valid_from/valid_until/max_runs getters yet —
// we re-derive denormalized columns from the serialized JSON after clone via
// serde round-trip helpers. Prefer dedicated accessors when they land.

fn occ_max_runs(occ: &Occurrence) -> Option<u64> {
    occ.max_runs()
}

/// `max_runs` for the database column, saturating instead of wrapping.
///
/// The column is a denormalized mirror used for querying; the authoritative
/// cap lives in the serialized occurrence, so clamping here loses nothing. A
/// bare `as i64` would turn a cap above `i64::MAX` negative — i.e. a timer
/// that reads back as already exhausted.
fn max_runs_to_sql(max_runs: Option<u64>) -> Option<i64> {
    max_runs.map(|n| i64::try_from(n).unwrap_or(i64::MAX))
}

/// Inverse of [`max_runs_to_sql`]. Saturates the same direction, so a value
/// that clamped on the way in stays "effectively unlimited" on the way out.
fn max_runs_from_sql(max_runs: Option<i64>) -> Option<u64> {
    max_runs.map(|n| u64::try_from(n).unwrap_or(u64::MAX))
}

fn occ_valid_from(occ: &Occurrence) -> Option<DateTime<Utc>> {
    occ.valid_from()
}

fn occ_valid_until(occ: &Occurrence) -> Option<DateTime<Utc>> {
    occ.valid_until()
}

// ── Connection-level operations (crate-internal) ────────────────────────
//
// These take any `&Connection` — a `Store`'s own connection or a
// `Transaction` (Deref) — so the R10 fire transaction can compose claim,
// lifecycle and outbox writes into ONE atomic commit.

/// A `BEGIN IMMEDIATE` guard usable from `&Store` (rusqlite's `transaction`
/// needs `&mut`, and `unchecked_transaction` is deferred-only). Commits
/// explicitly; rolls back on drop otherwise. The busy_timeout applies to
/// the initial lock acquisition as usual.
pub(crate) struct ImmediateTx<'c> {
    conn: &'c Connection,
    done: bool,
}

impl<'c> ImmediateTx<'c> {
    pub(crate) fn begin(conn: &'c Connection) -> StoreResult<Self> {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(StoreError::from)?;
        Ok(Self { conn, done: false })
    }

    pub(crate) fn commit(mut self) -> StoreResult<()> {
        self.conn.execute_batch("COMMIT").map_err(StoreError::from)?;
        self.done = true;
        Ok(())
    }
}

impl Drop for ImmediateTx<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

impl std::ops::Deref for ImmediateTx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
    }
}

impl Store {
    /// Begin an IMMEDIATE transaction from a shared store reference (the
    /// reply engine's per-transition atomicity — R10).
    pub(crate) fn immediate_tx(&self) -> StoreResult<ImmediateTx<'_>> {
        ImmediateTx::begin(&self.conn)
    }
}

/// The claim insert: new run id + durable event_sequence, in the caller's
/// transaction/connection. `AlreadyClaimed` on the UNIQUE guard. The claim is
/// born `pending` (SCH1): admitted for execution unless the fire transaction
/// finishes it as an overlap skip in the same commit.
pub(crate) fn claim_run_conn(
    conn: &Connection,
    timer_id: TimerId,
    scheduled_for: DateTime<Utc>,
) -> StoreResult<RunClaim> {
    let run_id = Uuid::new_v4();
    let claimed_at = Utc::now();
    let next_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(event_sequence), 0) + 1 FROM runs WHERE timer_id = ?1",
        params![timer_id.to_string()],
        |r| r.get(0),
    )?;
    let result = conn.execute(
        "INSERT INTO runs (
            run_id, timer_id, scheduled_for, status, claimed_at, completed_at, event_sequence
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
        params![
            run_id.to_string(),
            timer_id.to_string(),
            fmt_dt(scheduled_for),
            ClaimStatus::Pending.as_str(),
            fmt_dt(claimed_at),
            next_seq,
        ],
    );
    match result {
        Ok(1) => Ok(RunClaim {
            run_id,
            timer_id,
            scheduled_for,
            status: ClaimStatus::Pending,
            claimed_at,
            completed_at: None,
            event_sequence: next_seq as u64,
            outcome: None,
            outcome_reason: None,
            cancel_requested: false,
        }),
        Ok(_) => Err(StoreError::Internal("claim insert affected 0 rows".into())),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Err(StoreError::AlreadyClaimed {
                timer_id,
                scheduled_for,
            })
        }
        Err(e) => Err(StoreError::from(e)),
    }
}

pub(crate) fn get_run_conn(conn: &Connection, run_id: Uuid) -> StoreResult<Option<RunClaim>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLS} FROM runs WHERE run_id = ?1"
    ))?;
    let row = stmt
        .query_row(params![run_id.to_string()], row_to_claim)
        .optional()?;
    Ok(row)
}

pub(crate) fn runs_for_timer_conn(
    conn: &Connection,
    timer_id: TimerId,
) -> StoreResult<Vec<RunClaim>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLS} FROM runs WHERE timer_id = ?1
         ORDER BY event_sequence ASC, scheduled_for ASC, run_id ASC"
    ))?;
    let rows = stmt.query_map(params![timer_id.to_string()], row_to_claim)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn get_run_state_conn(
    conn: &Connection,
    run_id: Uuid,
) -> StoreResult<Option<RunStateRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_STATE_COLS} FROM run_states rs WHERE rs.run_id = ?1"
    ))?;
    let row = stmt
        .query_row(params![run_id.to_string()], row_to_run_state)
        .optional()?;
    Ok(row)
}

pub(crate) fn current_run_state_conn(
    conn: &Connection,
    timer_id: TimerId,
) -> StoreResult<Option<RunStateRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_STATE_COLS} FROM run_states rs
         JOIN runs r ON r.run_id = rs.run_id
         WHERE rs.timer_id = ?1
         ORDER BY r.event_sequence DESC, r.scheduled_for DESC, r.run_id DESC
         LIMIT 1"
    ))?;
    let row = stmt
        .query_row(params![timer_id.to_string()], row_to_run_state)
        .optional()?;
    Ok(row)
}

pub(crate) fn insert_run_state_conn(conn: &Connection, row: &RunStateRow) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO run_states (
            run_id, timer_id, app_name, state, fired_at, pickup_deadline,
            acknowledged_at, expected_secs, error_detection, heartbeat_at,
            progress, completed_at, failed_at, reason, failure_kind,
            result_json, result_truncated, watchdog_deadline, no_ack_at,
            reply_digest, acknowledged_logged, running_logged,
            selected_transport, transport
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, NULL, NULL,
                   NULL, NULL, NULL, NULL, NULL,
                   NULL, 0, NULL, NULL, NULL, 0, 0, ?7, ?8)",
        params![
            row.run_id.to_string(),
            row.timer_id.to_string(),
            row.app_name.as_str(),
            row.state.as_str(),
            fmt_dt(row.fired_at),
            row.pickup_deadline.map(fmt_dt),
            row.selected_transport.as_deref(),
            row.transport.as_deref(),
        ],
    )?;
    Ok(())
}

pub(crate) fn update_run_state_conn(conn: &Connection, row: &RunStateRow) -> StoreResult<()> {
    conn.execute(
        "UPDATE run_states SET
            state = ?2, pickup_deadline = ?3, acknowledged_at = ?4,
            expected_secs = ?5, error_detection = ?6, heartbeat_at = ?7,
            progress = ?8, completed_at = ?9, failed_at = ?10, reason = ?11,
            failure_kind = ?12, result_json = ?13, result_truncated = ?14,
            watchdog_deadline = ?15, no_ack_at = ?16, reply_digest = ?17,
            acknowledged_logged = ?18, running_logged = ?19, transport = ?20
         WHERE run_id = ?1",
        params![
            row.run_id.to_string(),
            row.state.as_str(),
            row.pickup_deadline.map(fmt_dt),
            row.acknowledged_at.map(fmt_dt),
            row.expected_secs.map(|s| s as i64),
            row.error_detection,
            row.heartbeat_at.map(fmt_dt),
            row.progress.as_deref(),
            row.completed_at.map(fmt_dt),
            row.failed_at.map(fmt_dt),
            row.reason.as_deref(),
            row.failure_kind.map(|k| k.as_str()),
            row.result_json
                .as_ref()
                .map(serde_json::Value::to_string),
            row.result_truncated,
            row.watchdog_deadline.map(fmt_dt),
            row.no_ack_at.map(fmt_dt),
            row.reply_digest.as_deref(),
            row.acknowledged_logged,
            row.running_logged,
            row.transport.as_deref(),
        ],
    )?;
    Ok(())
}

pub(crate) fn last_acked_sequence_conn(conn: &Connection, timer_id: TimerId) -> StoreResult<u64> {
    let mut stmt = conn.prepare(
        "SELECT last_acked_sequence FROM slot_event_acks WHERE timer_id = ?1",
    )?;
    let row: Option<i64> = stmt
        .query_row(params![timer_id.to_string()], |r| r.get(0))
        .optional()?;
    Ok(row.unwrap_or(0) as u64)
}

/// Owned runs with an armed deadline (pickup pending or watchdog ticking) —
/// the restart-reconstruction surface for the monotonic deadline book.
pub(crate) fn armed_deadlines_conn(conn: &Connection) -> StoreResult<Vec<RunStateRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_STATE_COLS} FROM run_states rs
         WHERE rs.pickup_deadline IS NOT NULL OR rs.watchdog_deadline IS NOT NULL
         ORDER BY rs.run_id ASC"
    ))?;
    let rows = stmt.query_map([], row_to_run_state)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn enqueue_event_conn(
    conn: &Connection,
    rec: &crate::events::EventRecord,
) -> StoreResult<()> {
    let payload = serde_json::to_string(rec)
        .map_err(|e| StoreError::Serde(format!("serialize event: {e}")))?;
    conn.execute(
        "INSERT OR IGNORE INTO event_outbox (event_id, payload, enqueued_at)
         VALUES (?1, ?2, ?3)",
        params![rec.event_id.to_string(), payload, fmt_dt(rec.logged_at)],
    )?;
    Ok(())
}

// ── SCH1: overlap admission (fire transaction) ──────────────────────────
//
// The fire transaction — not whichever worker dequeues later — examines the
// older executable claims (`pending` or `active`) and records the durable
// disposition: execute (stays `pending`), skip (finished `skipped_misfire`
// with a reason), or cancel (finish older `pending`, mark older `active`
// `cancel_requested`). Queue timing never re-decides policy.

/// Count a timer's older executable (unfinished) claims, excluding `exclude`.
pub(crate) fn unfinished_claims_count_conn(
    conn: &Connection,
    timer_id: TimerId,
    exclude: Uuid,
) -> StoreResult<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM runs
         WHERE timer_id = ?1 AND run_id != ?2 AND status IN (?3, ?4)",
        params![
            timer_id.to_string(),
            exclude.to_string(),
            ClaimStatus::Pending.as_str(),
            ClaimStatus::Active.as_str(),
        ],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as usize)
}

/// Finish one claim inside the caller's transaction (fire-tx skip, or a
/// `Replace` before-start). Only `pending`/`active` rows move — a worker that
/// already committed `finished` wins the race (SQLite commit order settles it).
/// Returns `true` when THIS call transitioned the row; a `false` means the
/// other side won and the caller must not emit its own outcome event.
pub(crate) fn finish_run_conn(
    conn: &Connection,
    run_id: Uuid,
    outcome: RunOutcome,
    reason: &str,
) -> StoreResult<bool> {
    let n = conn.execute(
        "UPDATE runs SET status = ?1, outcome = ?2, outcome_reason = ?3, completed_at = ?4
         WHERE run_id = ?5 AND status IN (?6, ?7)",
        params![
            ClaimStatus::Finished.as_str(),
            outcome.as_str(),
            reason,
            fmt_dt(Utc::now()),
            run_id.to_string(),
            ClaimStatus::Pending.as_str(),
            ClaimStatus::Active.as_str(),
        ],
    )?;
    Ok(n == 1)
}

/// A timer's `pending` claims except `exclude` (oldest first) — the claims a
/// `Replace` fire finishes `wake_failed(overlap_replace_before_start)`.
pub(crate) fn pending_claims_for_timer_conn(
    conn: &Connection,
    timer_id: TimerId,
    exclude: Uuid,
) -> StoreResult<Vec<RunClaim>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLS} FROM runs
         WHERE timer_id = ?1 AND run_id != ?2 AND status = ?3
         ORDER BY event_sequence ASC, run_id ASC"
    ))?;
    let rows = stmt.query_map(
        params![
            timer_id.to_string(),
            exclude.to_string(),
            ClaimStatus::Pending.as_str()
        ],
        row_to_claim,
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Mark every `active` claim of a timer (except `exclude`) `cancel_requested`;
/// returns the affected run ids so the dispatcher can signal their tokens.
pub(crate) fn request_cancel_active_conn(
    conn: &Connection,
    timer_id: TimerId,
    exclude: Uuid,
) -> StoreResult<Vec<Uuid>> {
    let mut stmt = conn.prepare(
        "SELECT run_id FROM runs
         WHERE timer_id = ?1 AND run_id != ?2 AND status = ?3
           AND COALESCE(cancel_requested, 0) = 0",
    )?;
    let rows = stmt.query_map(
        params![
            timer_id.to_string(),
            exclude.to_string(),
            ClaimStatus::Active.as_str()
        ],
        |r| r.get::<_, String>(0),
    )?;
    let mut ids = Vec::new();
    for r in rows {
        let s = r?;
        ids.push(
            Uuid::parse_str(&s)
                .map_err(|e| StoreError::Internal(format!("bad run_id '{s}': {e}")))?,
        );
    }
    conn.execute(
        "UPDATE runs SET cancel_requested = 1
         WHERE timer_id = ?1 AND run_id != ?2 AND status = ?3",
        params![
            timer_id.to_string(),
            exclude.to_string(),
            ClaimStatus::Active.as_str()
        ],
    )?;
    Ok(ids)
}

// ── SCH1: transport projections (fire-notification routing state) ────────

/// Next database-wide publication order. Call inside the fire transaction;
/// the IMMEDIATE write lock makes the read+insert pair atomic.
pub(crate) fn next_publication_order_conn(conn: &Connection) -> StoreResult<u64> {
    let n: i64 = conn.query_row(
        "SELECT COALESCE(MAX(publication_order), 0) + 1 FROM transport_projections",
        [],
        |r| r.get(0),
    )?;
    Ok(n.max(1) as u64)
}

/// Insert the projection and advance the fixed-target cursor in the same
/// transaction. The cursor records the greatest order ever assigned while any
/// projection for that target remains: an older projection below it is
/// permanently obsolete as a fixed-path wake hint.
pub(crate) fn insert_transport_projection_conn(
    conn: &Connection,
    proj: &TransportProjection,
) -> StoreResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO transport_projections (
            run_id, timer_id, target_path, payload, publication_order,
            state, attempts, next_attempt_at, created_at, published_at, kind
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            proj.run_id.to_string(),
            proj.timer_id.to_string(),
            proj.target_path,
            proj.payload,
            proj.publication_order as i64,
            proj.state,
            i64::from(proj.attempts),
            fmt_dt(proj.next_attempt_at),
            fmt_dt(proj.created_at),
            proj.published_at.map(fmt_dt),
            proj.kind,
        ],
    )?;
    conn.execute(
        "INSERT INTO target_cursors (target_path, max_publication_order)
         VALUES (?1, ?2)
         ON CONFLICT(target_path) DO UPDATE SET
           max_publication_order = MAX(target_cursors.max_publication_order, excluded.max_publication_order)",
        params![proj.target_path, proj.publication_order as i64],
    )?;
    Ok(())
}

fn row_to_projection(r: &rusqlite::Row<'_>) -> rusqlite::Result<TransportProjection> {
    let conv = |i: usize, msg: String| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, msg.into())
    };
    let run_id_s: String = r.get(0)?;
    let timer_id_s: String = r.get(1)?;
    let order: i64 = r.get(4)?;
    let attempts: i64 = r.get(6)?;
    let next_s: String = r.get(7)?;
    let created_s: String = r.get(8)?;
    let published_s: Option<String> = r.get(9)?;
    let kind: Option<String> = r.get(10)?;
    Ok(TransportProjection {
        run_id: Uuid::parse_str(&run_id_s).map_err(|e| conv(0, e.to_string()))?,
        timer_id: Uuid::parse_str(&timer_id_s).map_err(|e| conv(1, e.to_string()))?,
        target_path: r.get(2)?,
        payload: r.get(3)?,
        publication_order: order.max(0) as u64,
        state: r.get(5)?,
        attempts: u32::try_from(attempts.max(0)).unwrap_or(u32::MAX),
        next_attempt_at: parse_dt(&next_s).map_err(|e| conv(7, e.to_string()))?,
        created_at: parse_dt(&created_s).map_err(|e| conv(8, e.to_string()))?,
        published_at: parse_opt_dt(published_s).map_err(|e| conv(9, e.to_string()))?,
        kind: kind.unwrap_or_else(|| TransportProjection::KIND_FILE.to_string()),
    })
}

const PROJECTION_COLS: &str = "run_id, timer_id, target_path, payload, publication_order,
     state, attempts, next_attempt_at, created_at, published_at, kind";

impl Store {
    /// The transport projection for one run, if the fire transaction stored one.
    pub fn transport_projection(&self, run_id: Uuid) -> StoreResult<Option<TransportProjection>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROJECTION_COLS} FROM transport_projections WHERE run_id = ?1"
        ))?;
        let row = stmt
            .query_row(params![run_id.to_string()], row_to_projection)
            .optional()?;
        Ok(row)
    }

    /// Pending projections eligible for a publication attempt now (bounded).
    pub fn due_transport_projections(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> StoreResult<Vec<TransportProjection>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROJECTION_COLS} FROM transport_projections
             WHERE state = ?1 AND next_attempt_at <= ?2
             ORDER BY publication_order ASC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![
                TransportProjection::PENDING,
                fmt_dt(now),
                limit as i64
            ],
            row_to_projection,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Mark a projection published (the atomic replace succeeded).
    pub fn mark_transport_published(&self, run_id: Uuid) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE transport_projections
             SET state = ?2, published_at = ?3
             WHERE run_id = ?1 AND state = ?4",
            params![
                run_id.to_string(),
                TransportProjection::PUBLISHED,
                fmt_dt(Utc::now()),
                TransportProjection::PENDING,
            ],
        )?;
        Ok(())
    }

    /// Mark a projection obsolete (a newer firing owns the fixed path, or the
    /// durable target cursor moved past it). Terminal: never republished.
    pub fn mark_transport_obsolete(&self, run_id: Uuid) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE transport_projections SET state = ?2
             WHERE run_id = ?1 AND state IN (?3, ?4)",
            params![
                run_id.to_string(),
                TransportProjection::OBSOLETE,
                TransportProjection::PENDING,
                TransportProjection::PUBLISHED,
            ],
        )?;
        Ok(())
    }

    /// Move a `published` projection back to `pending` for redelivery (the
    /// app consumed/deleted the file without pickup being recorded — its
    /// `run_id` dedupe keeps it one logical firing).
    pub fn requeue_transport_projection(&self, run_id: Uuid) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE transport_projections
             SET state = ?2, next_attempt_at = ?3
             WHERE run_id = ?1 AND state = ?4",
            params![
                run_id.to_string(),
                TransportProjection::PENDING,
                fmt_dt(Utc::now()),
                TransportProjection::PUBLISHED,
            ],
        )?;
        Ok(())
    }
    /// Record a failed/eligibility-deferred attempt with bounded backoff.
    pub fn defer_transport_projection(
        &self,
        run_id: Uuid,
        attempts: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE transport_projections
             SET attempts = ?2, next_attempt_at = ?3
             WHERE run_id = ?1 AND state = ?4",
            params![
                run_id.to_string(),
                i64::from(attempts),
                fmt_dt(next_attempt_at),
                TransportProjection::PENDING,
            ],
        )?;
        Ok(())
    }

    /// IK6 `auto` fallback: convert an IPC projection to the file adapter in
    /// place — same `run_id`, new per-adapter encoding (the create-only
    /// stub's `reply_path` added) and file target. Never mints a second run
    /// or row. Matches both `pending` and `published`: a queued-but-
    /// unconfirmed IPC send (client lost after the bounded write) must be
    /// convertible too. Returns false when the projection already settled
    /// (picked up / obsolete — confirmation won, no fallback needed).
    pub fn convert_projection_to_file(
        &self,
        run_id: Uuid,
        target_path: &str,
        payload: &str,
    ) -> StoreResult<bool> {
        let n = self.conn.execute(
            "UPDATE transport_projections
             SET kind = ?2, target_path = ?3, payload = ?4, state = ?5
             WHERE run_id = ?1 AND state IN (?6, ?7)",
            params![
                run_id.to_string(),
                TransportProjection::KIND_FILE,
                target_path,
                payload,
                TransportProjection::PENDING,
                TransportProjection::PENDING,
                TransportProjection::PUBLISHED,
            ],
        )?;
        Ok(n == 1)
    }

    /// IK6: record the effective delivery transport on the run's lifecycle
    /// row (`ipc_fallback` after an `auto` fallback). The selected transport
    /// is immutable and never written here.
    pub fn set_run_transport(&self, run_id: Uuid, transport: &str) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE run_states SET transport = ?2 WHERE run_id = ?1",
            params![run_id.to_string(), transport],
        )?;
        Ok(())
    }

    /// Mark a projection picked up (ack advanced past its firing, or a valid
    /// reply was ingested for the run). Terminal: retries stop.
    pub fn mark_transport_picked_up(&self, run_id: Uuid) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE transport_projections SET state = ?2
             WHERE run_id = ?1 AND state IN (?3, ?4)",
            params![
                run_id.to_string(),
                TransportProjection::PICKED_UP,
                TransportProjection::PENDING,
                TransportProjection::PUBLISHED,
            ],
        )?;
        Ok(())
    }

    /// Live (pending/published) projections for a timer whose run's
    /// `event_sequence` is at or below `through_sequence` — the pickup surface
    /// when `ack_through` advances.
    pub fn live_projections_through(
        &self,
        timer_id: TimerId,
        through_sequence: u64,
    ) -> StoreResult<Vec<TransportProjection>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROJECTION_COLS} FROM transport_projections tp
             JOIN runs r ON r.run_id = tp.run_id
             WHERE tp.timer_id = ?1
               AND COALESCE(r.event_sequence, 0) <= ?2
               AND tp.state IN (?3, ?4)
             ORDER BY tp.publication_order ASC"
        ))?;
        let rows = stmt.query_map(
            params![
                timer_id.to_string(),
                through_sequence as i64,
                TransportProjection::PENDING,
                TransportProjection::PUBLISHED,
            ],
            row_to_projection,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// All live (pending/published) projections, oldest first — the pickup
    /// sweep surface for the publication pump.
    pub fn live_transport_projections(
        &self,
        limit: usize,
    ) -> StoreResult<Vec<TransportProjection>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROJECTION_COLS} FROM transport_projections
             WHERE state IN (?1, ?2)
             ORDER BY publication_order ASC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![
                TransportProjection::PENDING,
                TransportProjection::PUBLISHED,
                limit as i64
            ],
            row_to_projection,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// The durable cursor for a fixed target path (0 when never assigned).
    pub fn target_cursor(&self, target_path: &str) -> StoreResult<u64> {
        let row: Option<i64> = self
            .conn
            .query_row(
                "SELECT max_publication_order FROM target_cursors WHERE target_path = ?1",
                params![target_path],
                |r| r.get(0),
            )
            .optional()?;
        Ok(row.unwrap_or(0).max(0) as u64)
    }

    /// Drop projections + orphan cursors for a deleted timer. A cursor is
    /// pruned only when no retained projection still references its target.
    pub fn prune_transport_for_timer(&mut self, timer_id: TimerId) -> StoreResult<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM transport_projections WHERE timer_id = ?1",
            params![timer_id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM target_cursors
             WHERE NOT EXISTS (
                 SELECT 1 FROM transport_projections tp
                 WHERE tp.target_path = target_cursors.target_path
             )",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
