//! Schema DDL and `user_version` migration scaffold.

use super::error::{StoreError, StoreResult};
use rusqlite::Connection;

/// Current on-disk schema version (also stored in `PRAGMA user_version` and `meta`).
pub const SCHEMA_VERSION: i32 = 9;

/// Apply pending migrations. Safe to call on every open.
pub fn migrate(conn: &Connection) -> StoreResult<()> {
    let current: i32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| StoreError::Sqlite(format!("read user_version: {e}")))?;

    if current > SCHEMA_VERSION {
        return Err(StoreError::Sqlite(format!(
            "database schema version {current} is newer than supported {SCHEMA_VERSION}"
        )));
    }

    // Already current: no writes at all. Every open used to commit here,
    // and that pointless writer traffic can collide with the IMMEDIATE
    // fire transaction.
    if current == SCHEMA_VERSION {
        return Ok(());
    }

    if current < 1 {
        migrate_v1(conn)?;
    }
    if current < 2 {
        migrate_v2(conn)?;
    }
    if current < 3 {
        migrate_v3(conn)?;
    }
    if current < 4 {
        migrate_v4(conn)?;
    }
    if current < 5 {
        migrate_v5(conn)?;
    }
    if current < 6 {
        migrate_v6(conn)?;
    }
    if current < 7 {
        migrate_v7(conn)?;
    }
    if current < 8 {
        migrate_v8(conn)?;
    }
    if current < 9 {
        migrate_v9(conn)?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| StoreError::Sqlite(format!("set user_version: {e}")))?;

    // Keep meta.schema_version in lock-step.
    conn.execute(
        "UPDATE meta SET schema_version = ?1 WHERE id = 1",
        [SCHEMA_VERSION],
    )
    .map_err(|e| StoreError::Sqlite(format!("sync meta.schema_version: {e}")))?;

    Ok(())
}

fn migrate_v1(conn: &Connection) -> StoreResult<()> {
    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS timers (
            id              TEXT PRIMARY KEY NOT NULL,
            name            TEXT NOT NULL,
            enabled         INTEGER NOT NULL DEFAULT 1,
            occurrence      TEXT NOT NULL,
            tz              TEXT NOT NULL,
            next_fire_utc   TEXT,
            last_fired      TEXT,
            misfire_policy  TEXT NOT NULL,
            overlap_policy  TEXT NOT NULL,
            retry_policy    TEXT NOT NULL,
            valid_from      TEXT,
            valid_until     TEXT,
            max_runs        INTEGER,
            tags            TEXT NOT NULL DEFAULT '[]',
            action          TEXT NOT NULL,
            revision        INTEGER NOT NULL DEFAULT 1
        );

        CREATE INDEX IF NOT EXISTS idx_timers_next_fire
            ON timers (next_fire_utc);

        CREATE INDEX IF NOT EXISTS idx_timers_enabled_next_fire
            ON timers (enabled, next_fire_utc);

        CREATE TABLE IF NOT EXISTS runs (
            run_id          TEXT PRIMARY KEY NOT NULL,
            timer_id        TEXT NOT NULL,
            scheduled_for   TEXT NOT NULL,
            status          TEXT NOT NULL,
            claimed_at      TEXT NOT NULL,
            completed_at    TEXT,
            UNIQUE (timer_id, scheduled_for)
        );

        CREATE INDEX IF NOT EXISTS idx_runs_status
            ON runs (status);

        CREATE TABLE IF NOT EXISTS meta (
            id                   INTEGER PRIMARY KEY CHECK (id = 1),
            schema_version       INTEGER NOT NULL,
            last_prune           TEXT,
            last_recalibration   TEXT,
            tzdata_version       TEXT
        );

        INSERT OR IGNORE INTO meta (id, schema_version, last_prune, last_recalibration, tzdata_version)
        VALUES (1, 1, NULL, NULL, NULL);
        ",
    )
    .map_err(|e| StoreError::Sqlite(format!("migrate v1: {e}")))?;
    Ok(())
}

/// Slot IPC tables: durable idempotency ledger + timer ownership by integrating app.
fn migrate_v2(conn: &Connection) -> StoreResult<()> {
    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS slot_requests (
            request_id      TEXT PRIMARY KEY NOT NULL,
            slot_id         TEXT NOT NULL,
            operation       TEXT NOT NULL,
            app_name        TEXT,
            timer_id        TEXT,
            status          TEXT NOT NULL,
            response_json   TEXT NOT NULL,
            created_at      TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_slot_requests_created
            ON slot_requests (created_at);

        CREATE TABLE IF NOT EXISTS timer_owners (
            timer_id        TEXT PRIMARY KEY NOT NULL,
            app_name        TEXT NOT NULL
        );
        ",
    )
    .map_err(|e| StoreError::Sqlite(format!("migrate v2: {e}")))?;
    Ok(())
}

/// Durable run event sequences + per-timer ack cursors for the slot output feed.
///
/// Idempotent under crash/restart: SQLite commits each DDL statement, so a
/// process that dies after `ALTER TABLE` but before `user_version` advances
/// must reopen without failing on "duplicate column name".
fn migrate_v3(conn: &Connection) -> StoreResult<()> {
    // Guard: only ADD COLUMN when missing (partial-migration safe).
    if !table_has_column(conn, "runs", "event_sequence")? {
        conn.execute("ALTER TABLE runs ADD COLUMN event_sequence INTEGER", [])
            .map_err(|e| StoreError::Sqlite(format!("migrate v3 add event_sequence: {e}")))?;
    }

    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS slot_event_acks (
            timer_id              TEXT PRIMARY KEY NOT NULL,
            last_acked_sequence   INTEGER NOT NULL DEFAULT 0
        );
        ",
    )
    .map_err(|e| StoreError::Sqlite(format!("migrate v3 slot_event_acks: {e}")))?;

    // Backfill sequences for any pre-existing runs (stable order by scheduled_for, run_id).
    // Safe to re-run: only fills NULL sequences.
    conn.execute_batch(
        r"
        UPDATE runs
        SET event_sequence = (
            SELECT COUNT(*)
            FROM runs AS r2
            WHERE r2.timer_id = runs.timer_id
              AND (
                    r2.scheduled_for < runs.scheduled_for
                 OR (r2.scheduled_for = runs.scheduled_for AND r2.run_id <= runs.run_id)
              )
        )
        WHERE event_sequence IS NULL;
        ",
    )
    .map_err(|e| StoreError::Sqlite(format!("migrate v3 backfill: {e}")))?;
    Ok(())
}

/// P5: per-timer jitter + accuracy slack columns.
fn migrate_v4(conn: &Connection) -> StoreResult<()> {
    if !table_has_column(conn, "timers", "jitter_secs")? {
        conn.execute(
            "ALTER TABLE timers ADD COLUMN jitter_secs INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| StoreError::Sqlite(format!("migrate v4 add jitter_secs: {e}")))?;
    }
    if !table_has_column(conn, "timers", "accuracy_slack_secs")? {
        conn.execute(
            "ALTER TABLE timers ADD COLUMN accuracy_slack_secs INTEGER",
            [],
        )
        .map_err(|e| StoreError::Sqlite(format!("migrate v4 add accuracy_slack_secs: {e}")))?;
    }
    Ok(())
}

/// P7: per-timer wake_machine flag for the single-next-wake election.
fn migrate_v5(conn: &Connection) -> StoreResult<()> {
    if !table_has_column(conn, "timers", "wake_machine")? {
        conn.execute(
            "ALTER TABLE timers ADD COLUMN wake_machine INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| StoreError::Sqlite(format!("migrate v5 add wake_machine: {e}")))?;
    }
    Ok(())
}

/// IK3: the app-lifecycle state of integration-owned runs (the reply channel).
///
/// One row per owned run, inserted by the fire transaction with the owner
/// snapshotted onto it. `state` is an R5 wire string; `pickup_deadline` /
/// `watchdog_deadline` are persisted wall-clock deadlines used only to rebuild
/// the live countdowns after a restart (the active countdowns run on
/// Bellman's monotonic clock). All app-reported fields accumulate here so
/// `status.json` can be re-projected from the database alone.
fn migrate_v6(conn: &Connection) -> StoreResult<()> {
    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS run_states (
            run_id             TEXT PRIMARY KEY NOT NULL,
            timer_id           TEXT NOT NULL,
            app_name           TEXT NOT NULL,
            state              TEXT NOT NULL,
            fired_at           TEXT NOT NULL,
            pickup_deadline    TEXT,
            acknowledged_at    TEXT,
            expected_secs      INTEGER,
            error_detection    INTEGER,
            heartbeat_at       TEXT,
            progress           TEXT,
            completed_at       TEXT,
            failed_at          TEXT,
            reason             TEXT,
            failure_kind       TEXT,
            result_json        TEXT,
            result_truncated   INTEGER NOT NULL DEFAULT 0,
            watchdog_deadline  TEXT,
            no_ack_at          TEXT,
            reply_digest       TEXT,
            acknowledged_logged INTEGER NOT NULL DEFAULT 0,
            running_logged      INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_run_states_timer
            ON run_states (timer_id);
        ",
    )
    .map_err(|e| StoreError::Sqlite(format!("migrate v6: {e}")))?;
    Ok(())
}

/// R11: the event-log outbox and the rotation journal.
///
/// Every event producer enqueues into `event_outbox` — SQLite serialises
/// across processes, which is the whole reason it is the funnel. One
/// publisher, elected by an OS file lock, appends + fdatasyncs each line and
/// only then deletes the row. Delivery is at-least-once: a crash between
/// sync and row deletion re-appends, and every reader dedupes by
/// `event_id`. Published rows are deleted so the outbox empties; retention
/// of the log itself lives in the archive pruner.
///
/// `rotation_journal` makes rotation crash-safe: the journal names every
/// artifact of an in-flight rotation; a recovering publisher rolls the
/// interrupted phase forward before appending or rotating again.
fn migrate_v7(conn: &Connection) -> StoreResult<()> {
    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS event_outbox (
            event_id     TEXT PRIMARY KEY NOT NULL,
            payload      TEXT NOT NULL,
            enqueued_at  TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_event_outbox_enqueued
            ON event_outbox (enqueued_at);

        CREATE TABLE IF NOT EXISTS rotation_journal (
            id           INTEGER PRIMARY KEY CHECK (id = 1),
            source       TEXT NOT NULL,
            rotating     TEXT NOT NULL,
            gz_tmp       TEXT NOT NULL,
            final_path   TEXT NOT NULL,
            phase        TEXT NOT NULL,
            started_at   TEXT NOT NULL
        );
        ",
    )
    .map_err(|e| StoreError::Sqlite(format!("migrate v7: {e}")))?;
    Ok(())
}

/// SCH1: dispatch/outcome split on the claim ledger + durable fire-publication
/// routing state.
///
/// `runs.status` moves from `claimed/completed/wake_failed` to
/// `pending/active/finished`, with the R5 delivery outcome in its own column
/// (`finished` is no longer success-by-default), a machine reason, and the
/// durable `cancel_requested` flag a `Replace` fire transaction sets on an
/// active predecessor so any dispatcher process can signal the worker —
/// cross-process cancellation without shared memory.
///
/// `transport_projections` is the retry/routing state for the existing run's
/// fire notification (target path, serialized payload, database-wide
/// `publication_order`); `target_cursors` records, per fixed target path, the
/// greatest `publication_order` ever assigned while any projection for that
/// target remains — an older projection below the cursor is permanently
/// obsolete as a fixed-path wake hint.
fn migrate_v8(conn: &Connection) -> StoreResult<()> {
    if !table_has_column(conn, "runs", "outcome")? {
        conn.execute("ALTER TABLE runs ADD COLUMN outcome TEXT", [])
            .map_err(|e| StoreError::Sqlite(format!("migrate v8 add outcome: {e}")))?;
    }
    if !table_has_column(conn, "runs", "outcome_reason")? {
        conn.execute("ALTER TABLE runs ADD COLUMN outcome_reason TEXT", [])
            .map_err(|e| StoreError::Sqlite(format!("migrate v8 add outcome_reason: {e}")))?;
    }
    if !table_has_column(conn, "runs", "cancel_requested")? {
        conn.execute(
            "ALTER TABLE runs ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| StoreError::Sqlite(format!("migrate v8 add cancel_requested: {e}")))?;
    }

    // Split the legacy status vocabulary into phase + outcome. A legacy
    // `claimed` row is ambiguous between pending and active; mapping it to
    // `pending` is the safe at-least-once choice (re-queue, never lose).
    conn.execute_batch(
        r"
        UPDATE runs SET status = 'finished', outcome = 'wake_delivered'
         WHERE status = 'completed';
        UPDATE runs SET status = 'finished', outcome = 'wake_failed'
         WHERE status = 'wake_failed';
        UPDATE runs SET status = 'pending' WHERE status = 'claimed';
        ",
    )
    .map_err(|e| StoreError::Sqlite(format!("migrate v8 status split: {e}")))?;

    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS transport_projections (
            run_id             TEXT PRIMARY KEY NOT NULL,
            timer_id           TEXT NOT NULL,
            target_path        TEXT NOT NULL,
            payload            TEXT NOT NULL,
            publication_order  INTEGER NOT NULL,
            state              TEXT NOT NULL DEFAULT 'pending',
            attempts           INTEGER NOT NULL DEFAULT 0,
            next_attempt_at    TEXT NOT NULL,
            created_at         TEXT NOT NULL,
            published_at       TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_transport_projections_state
            ON transport_projections (state, next_attempt_at);

        CREATE INDEX IF NOT EXISTS idx_transport_projections_timer
            ON transport_projections (timer_id);

        CREATE TABLE IF NOT EXISTS target_cursors (
            target_path            TEXT PRIMARY KEY NOT NULL,
            max_publication_order  INTEGER NOT NULL
        );
        ",
    )
    .map_err(|e| StoreError::Sqlite(format!("migrate v8 transport projections: {e}")))?;
    Ok(())
}

/// IK6: dual transport — per-timer transport mode, per-run transport
/// recording, and the per-adapter kind on the transport projection.
///
/// - `timers.transport`: the configured mode (`json` | `ipc` | `auto`),
///   default `json` (today's behaviour).
/// - `run_states.selected_transport` / `run_states.transport`: the mode
///   selected at fire (immutable) and the effective delivery (`ipc_fallback`
///   when an `auto` run fell back to files with the same `run_id`). Nullable:
///   runs predating IK6 have no transport record.
/// - `transport_projections.kind`: the adapter (`file` | `ipc`) this
///   projection's payload/target is encoded for — extends the SCH1 row; no
///   second run ledger exists.
fn migrate_v9(conn: &Connection) -> StoreResult<()> {
    if !table_has_column(conn, "timers", "transport")? {
        conn.execute(
            "ALTER TABLE timers ADD COLUMN transport TEXT NOT NULL DEFAULT 'json'",
            [],
        )
        .map_err(|e| StoreError::Sqlite(format!("migrate v9 add timers.transport: {e}")))?;
    }
    if !table_has_column(conn, "run_states", "selected_transport")? {
        conn.execute("ALTER TABLE run_states ADD COLUMN selected_transport TEXT", [])
            .map_err(|e| StoreError::Sqlite(format!("migrate v9 add selected_transport: {e}")))?;
    }
    if !table_has_column(conn, "run_states", "transport")? {
        conn.execute("ALTER TABLE run_states ADD COLUMN transport TEXT", [])
            .map_err(|e| StoreError::Sqlite(format!("migrate v9 add transport: {e}")))?;
    }
    if !table_has_column(conn, "transport_projections", "kind")? {
        conn.execute(
            "ALTER TABLE transport_projections ADD COLUMN kind TEXT NOT NULL DEFAULT 'file'",
            [],
        )
        .map_err(|e| {
            StoreError::Sqlite(format!("migrate v9 add transport_projections.kind: {e}"))
        })?;
    }
    Ok(())
}

/// True when `table` has a column named `column` (via `PRAGMA table_info`).
fn table_has_column(conn: &Connection, table: &str, column: &str) -> StoreResult<bool> {
    // table name is internal; never pass untrusted input.
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&pragma)
        .map_err(|e| StoreError::Sqlite(format!("table_info {table}: {e}")))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| StoreError::Sqlite(format!("table_info query {table}: {e}")))?;
    for row in rows {
        let name = row.map_err(|e| StoreError::Sqlite(format!("table_info row: {e}")))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}
