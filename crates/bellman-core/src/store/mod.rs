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
    Action, ClaimStatus, MisfirePolicy, NewTimer, OverlapPolicy, RetryPolicy, RunClaim, Timer,
    TimerId, TimerPatch, TimerUpdate,
};

use crate::occurrence::Occurrence;
use chrono::{DateTime, Utc};
use models::Meta;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use schema::{migrate, SCHEMA_VERSION};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Open options for a store database.
#[derive(Debug, Clone)]
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
        let v: i32 = self
            .conn
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

    // ── Timer CRUD ──────────────────────────────────────────────────────

    /// Insert a new timer. Computes `next_fire_utc` in the same transaction.
    pub fn create_timer(&mut self, new: NewTimer) -> StoreResult<Timer> {
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

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO timers (
                id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
                misfire_policy, overlap_policy, retry_policy,
                valid_from, valid_until, max_runs, tags, action, revision
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16
             )",
            params![
                id.to_string(),
                new.name,
                new.enabled as i64,
                occ_json,
                tz,
                next.map(fmt_dt),
                new.last_fired.map(fmt_dt),
                misfire,
                overlap,
                retry,
                valid_from.map(fmt_dt),
                valid_until.map(fmt_dt),
                max_runs.map(|n| n as i64),
                tags_json,
                action_json,
                revision,
            ],
        )?;
        tx.commit()?;

        self.get_timer(id)?
            .ok_or_else(|| StoreError::Internal("timer missing after insert".into()))
    }

    /// Fetch a timer by id.
    pub fn get_timer(&self, id: TimerId) -> StoreResult<Option<Timer>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
                    misfire_policy, overlap_policy, retry_policy,
                    valid_from, valid_until, max_runs, tags, action, revision
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
                    valid_from, valid_until, max_runs, tags, action, revision
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
        let current = get_timer_tx(&tx, update.id)?
            .ok_or(StoreError::NotFound(update.id))?;

        if current.revision != update.expected_revision {
            return Err(StoreError::StaleRevision {
                id: update.id,
                expected: update.expected_revision,
                actual: current.revision,
            });
        }

        let mut next = current;
        apply_patch(&mut next, update.patch)?;

        // Recompute next fire from the (possibly new) occurrence + last_fired.
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
                tags = ?13, action = ?14, revision = ?15
             WHERE id = ?16 AND revision = ?17",
            params![
                next.name,
                next.enabled as i64,
                serde_json::to_string(&next.occurrence)?,
                next.tz,
                next.next_fire_utc.map(fmt_dt),
                next.last_fired.map(fmt_dt),
                serde_json::to_string(&next.misfire)?,
                serde_json::to_string(&next.overlap)?,
                serde_json::to_string(&next.retry)?,
                next.valid_from.map(fmt_dt),
                next.valid_until.map(fmt_dt),
                next.max_runs.map(|n| n as i64),
                serde_json::to_string(&next.tags)?,
                serde_json::to_string(&next.action)?,
                next.revision,
                next.id.to_string(),
                update.expected_revision,
            ],
        )?;
        if n != 1 {
            return Err(StoreError::StaleRevision {
                id: update.id,
                expected: update.expected_revision,
                actual: current_revision_tx(&tx, update.id)?,
            });
        }
        tx.commit()?;
        self.get_timer(update.id)?
            .ok_or_else(|| StoreError::Internal("timer missing after update".into()))
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
                    valid_from, valid_until, max_runs, tags, action, revision
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
    pub fn claim_run(
        &mut self,
        timer_id: TimerId,
        scheduled_for: DateTime<Utc>,
    ) -> StoreResult<RunClaim> {
        let run_id = Uuid::new_v4();
        let claimed_at = Utc::now();
        let result = self.conn.execute(
            "INSERT INTO runs (run_id, timer_id, scheduled_for, status, claimed_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                run_id.to_string(),
                timer_id.to_string(),
                fmt_dt(scheduled_for),
                ClaimStatus::Claimed.as_str(),
                fmt_dt(claimed_at),
            ],
        );
        match result {
            Ok(1) => Ok(RunClaim {
                run_id,
                timer_id,
                scheduled_for,
                status: ClaimStatus::Claimed,
                claimed_at,
                completed_at: None,
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
        self.get_run(run_id)?
            .ok_or(StoreError::RunNotFound(run_id))
    }

    /// Fetch a run claim by id.
    pub fn get_run(&self, run_id: Uuid) -> StoreResult<Option<RunClaim>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, timer_id, scheduled_for, status, claimed_at, completed_at
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
            "SELECT run_id, timer_id, scheduled_for, status, claimed_at, completed_at
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

    /// Checkpoint WAL with TRUNCATE (also invoked on clean drop).
    pub fn checkpoint_truncate(&self) -> StoreResult<()> {
        self.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(|e| StoreError::Sqlite(format!("wal_checkpoint TRUNCATE: {e}")))
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
    Ok(())
}

fn get_timer_tx(tx: &Transaction<'_>, id: TimerId) -> StoreResult<Option<Timer>> {
    let mut stmt = tx.prepare(
        "SELECT id, name, enabled, occurrence, tz, next_fire_utc, last_fired,
                misfire_policy, overlap_policy, retry_policy,
                valid_from, valid_until, max_runs, tags, action, revision
         FROM timers WHERE id = ?1",
    )?;
    let row = stmt
        .query_row(params![id.to_string()], row_to_timer)
        .optional()?;
    Ok(row)
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
    let vf_s: Option<String> = r.get(10)?;
    let vu_s: Option<String> = r.get(11)?;
    let max_runs: Option<i64> = r.get(12)?;
    let tags_json: String = r.get(13)?;
    let action_json: String = r.get(14)?;
    let revision: i64 = r.get(15)?;

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
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
            )
        })?,
        last_fired: parse_opt_dt(last_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
            )
        })?,
        misfire,
        overlap,
        retry,
        valid_from: parse_opt_dt(vf_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
            )
        })?,
        valid_until: parse_opt_dt(vu_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                11,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
            )
        })?,
        max_runs: max_runs.map(|n| n as u64),
        tags,
        action,
        revision,
    })
}

fn row_to_claim(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunClaim> {
    let run_id_s: String = r.get(0)?;
    let timer_id_s: String = r.get(1)?;
    let sched_s: String = r.get(2)?;
    let status_s: String = r.get(3)?;
    let claimed_s: String = r.get(4)?;
    let completed_s: Option<String> = r.get(5)?;

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
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
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
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        )
    })?;
    let completed_at = parse_opt_dt(completed_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        )
    })?;

    Ok(RunClaim {
        run_id,
        timer_id,
        scheduled_for,
        status,
        claimed_at,
        completed_at,
    })
}

fn fmt_dt(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

fn occ_valid_from(occ: &Occurrence) -> Option<DateTime<Utc>> {
    occ.valid_from()
}

fn occ_valid_until(occ: &Occurrence) -> Option<DateTime<Utc>> {
    occ.valid_until()
}

#[cfg(test)]
mod tests;
