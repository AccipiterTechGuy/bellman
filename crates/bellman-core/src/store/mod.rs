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
    Action, ClaimStatus, Meta, MisfirePolicy, NewTimer, OverlapPolicy, RetryPolicy, RunClaim,
    SlotRequestRecord, Timer, TimerId, TimerPatch, TimerUpdate,
};

use crate::occurrence::Occurrence;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use schema::{migrate, SCHEMA_VERSION};
use std::path::{Path, PathBuf};
use uuid::Uuid;

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
                    COALESCE(jitter_secs, 0), accuracy_slack_secs
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
                    COALESCE(jitter_secs, 0), accuracy_slack_secs
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
    /// Recomputes `next_fire_utc` in the same transaction.
    pub fn update_timer(&mut self, update: TimerUpdate) -> StoreResult<Timer> {
        let tx = self.conn.transaction()?;
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
                    COALESCE(jitter_secs, 0), accuracy_slack_secs
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
        let run_id = Uuid::new_v4();
        let claimed_at = Utc::now();
        let tx = self.conn.transaction()?;
        let next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(event_sequence), 0) + 1 FROM runs WHERE timer_id = ?1",
            params![timer_id.to_string()],
            |r| r.get(0),
        )?;
        let result = tx.execute(
            "INSERT INTO runs (
                run_id, timer_id, scheduled_for, status, claimed_at, completed_at, event_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![
                run_id.to_string(),
                timer_id.to_string(),
                fmt_dt(scheduled_for),
                ClaimStatus::Claimed.as_str(),
                fmt_dt(claimed_at),
                next_seq,
            ],
        );
        match result {
            Ok(1) => {
                tx.commit()?;
                Ok(RunClaim {
                    run_id,
                    timer_id,
                    scheduled_for,
                    status: ClaimStatus::Claimed,
                    claimed_at,
                    completed_at: None,
                    event_sequence: next_seq as u64,
                })
            }
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

    /// Mark a previously claimed run as completed.
    pub fn complete_run(&mut self, run_id: Uuid) -> StoreResult<RunClaim> {
        let completed_at = Utc::now();
        let n = self.conn.execute(
            "UPDATE runs SET status = ?1, completed_at = ?2
             WHERE run_id = ?3 AND status = ?4",
            params![
                ClaimStatus::Completed.as_str(),
                fmt_dt(completed_at),
                run_id.to_string(),
                ClaimStatus::Claimed.as_str(),
            ],
        )?;
        if n != 1 {
            return Err(StoreError::RunNotFound(run_id));
        }
        self.get_run(run_id)?.ok_or(StoreError::RunNotFound(run_id))
    }

    /// Fetch a run claim by id.
    pub fn get_run(&self, run_id: Uuid) -> StoreResult<Option<RunClaim>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, timer_id, scheduled_for, status, claimed_at, completed_at,
                    COALESCE(event_sequence, 0)
             FROM runs WHERE run_id = ?1",
        )?;
        let row = stmt
            .query_row(params![run_id.to_string()], row_to_claim)
            .optional()?;
        Ok(row)
    }

    /// Pending (claimed, not completed) run claims — recovery surface after crash.
    pub fn pending_claims(&self) -> StoreResult<Vec<RunClaim>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, timer_id, scheduled_for, status, claimed_at, completed_at,
                    COALESCE(event_sequence, 0)
             FROM runs WHERE status = ?1
             ORDER BY claimed_at ASC, run_id ASC",
        )?;
        let rows = stmt.query_map(params![ClaimStatus::Claimed.as_str()], row_to_claim)?;
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
        let mut stmt = self.conn.prepare(
            "SELECT run_id, timer_id, scheduled_for, status, claimed_at, completed_at,
                    COALESCE(event_sequence, 0)
             FROM runs WHERE timer_id = ?1 AND scheduled_for = ?2",
        )?;
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
        let mut stmt = self.conn.prepare(
            "SELECT run_id, timer_id, scheduled_for, status, claimed_at, completed_at,
                    COALESCE(event_sequence, 0)
             FROM runs WHERE timer_id = ?1
             ORDER BY event_sequence ASC, scheduled_for ASC, run_id ASC",
        )?;
        let rows = stmt.query_map(params![timer_id.to_string()], row_to_claim)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Last acknowledged event_sequence for a timer (0 if never acked).
    pub fn last_acked_sequence(&self, timer_id: TimerId) -> StoreResult<u64> {
        let mut stmt = self.conn.prepare(
            "SELECT last_acked_sequence FROM slot_event_acks WHERE timer_id = ?1",
        )?;
        let row: Option<i64> = stmt
            .query_row(params![timer_id.to_string()], |r| r.get(0))
            .optional()?;
        Ok(row.unwrap_or(0) as u64)
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
        let tx = self.conn.transaction()?;
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
        let mut stmt = self.conn.prepare(
            "SELECT run_id, timer_id, scheduled_for, status, claimed_at, completed_at,
                    COALESCE(event_sequence, 0)
             FROM runs
             WHERE timer_id = ?1 AND COALESCE(event_sequence, 0) > ?2
             ORDER BY event_sequence ASC, run_id ASC
             LIMIT ?3",
        )?;
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
        let mut stmt = tx.prepare(
            "SELECT run_id, timer_id, scheduled_for, status, claimed_at, completed_at,
                    COALESCE(event_sequence, 0)
             FROM runs
             WHERE timer_id = ?1 AND COALESCE(event_sequence, 0) > ?2
             ORDER BY event_sequence ASC, run_id ASC
             LIMIT ?3",
        )?;
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
            jitter_secs, accuracy_slack_secs
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10,
            ?11, ?12, ?13, ?14, ?15, ?16,
            ?17, ?18
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
            jitter_secs = ?16, accuracy_slack_secs = ?17
         WHERE id = ?18 AND revision = ?19",
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

fn get_timer_tx(tx: &Transaction<'_>, id: TimerId) -> StoreResult<Option<Timer>> {
    let mut stmt = tx.prepare(
        "SELECT id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
                misfire_policy, overlap_policy, retry_policy,
                valid_from, valid_until, max_runs, tags, action, revision,
                COALESCE(jitter_secs, 0), accuracy_slack_secs
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

    Ok(RunClaim {
        run_id,
        timer_id,
        scheduled_for,
        status,
        claimed_at,
        completed_at,
        event_sequence: event_sequence.max(0) as u64,
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

#[cfg(test)]
mod tests;
