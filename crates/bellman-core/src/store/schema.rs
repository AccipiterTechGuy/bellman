//! Schema DDL and `user_version` migration scaffold.

use super::error::{StoreError, StoreResult};
use rusqlite::Connection;

/// Current on-disk schema version (also stored in `PRAGMA user_version` and `meta`).
pub const SCHEMA_VERSION: i32 = 5;

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
