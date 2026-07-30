//! Store acceptance tests (CRUD, revision, claim ledger, horizon, crash recovery).

use super::*;
use crate::occurrence::{Occurrence, OccurrenceKind, Weekdays};
use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

fn open_tmp() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("timers.db");
    let store = Store::open(&path).expect("open store");
    (dir, store)
}

fn once_at(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> Occurrence {
    let at = NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(hh, mm, ss)
        .unwrap();
    Occurrence::new(OccurrenceKind::Once { at }, "UTC").unwrap()
}

fn all_kinds() -> Vec<(&'static str, Occurrence)> {
    let anchor = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
    vec![
        (
            "once",
            Occurrence::new(
                OccurrenceKind::Once {
                    at: NaiveDateTime::new(
                        NaiveDate::from_ymd_opt(2030, 6, 15).unwrap(),
                        noon,
                    ),
                },
                "UTC",
            )
            .unwrap(),
        ),
        (
            "interval",
            Occurrence::new(
                OccurrenceKind::Interval {
                    every_secs: 60,
                    anchor,
                },
                "UTC",
            )
            .unwrap(),
        ),
        (
            "daily",
            Occurrence::new(OccurrenceKind::Daily { at: noon }, "Europe/Helsinki").unwrap(),
        ),
        (
            "weekly",
            Occurrence::new(
                OccurrenceKind::Weekly {
                    days: Weekdays::from_slice(&[chrono::Weekday::Mon, chrono::Weekday::Wed]),
                    at: noon,
                },
                "UTC",
            )
            .unwrap(),
        ),
        (
            "monthly",
            Occurrence::new(OccurrenceKind::Monthly { day: 15, at: noon }, "UTC").unwrap(),
        ),
        (
            "yearly",
            Occurrence::new(
                OccurrenceKind::Yearly {
                    month: 7,
                    day: 4,
                    at: noon,
                },
                "America/New_York",
            )
            .unwrap(),
        ),
        (
            "cron",
            Occurrence::new(
                OccurrenceKind::Cron {
                    expr: "0 0 12 * * *".into(),
                },
                "UTC",
            )
            .unwrap(),
        ),
    ]
}

#[test]
fn schema_version_is_current_after_open() {
    let (_dir, store) = open_tmp();
    assert_eq!(store.schema_version().unwrap(), 6);
    let meta = store.meta().unwrap();
    assert_eq!(meta.schema_version, 6);
    assert!(meta.last_prune.is_none());
}

/// Crash window: `event_sequence` already present (ALTER committed) but
/// `user_version` still 2. Reopen must finish migrate_v3 (+ v4 + v5) without
/// "duplicate column name" and land on the current schema.
#[test]
fn migrate_v3_partial_restart_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("partial-v3.db");

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw");
        conn.execute_batch(
            r"
            PRAGMA user_version = 2;

            CREATE TABLE meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                schema_version INTEGER NOT NULL,
                last_prune TEXT,
                last_recalibration TEXT,
                tzdata_version TEXT
            );
            INSERT INTO meta VALUES (1, 2, NULL, NULL, NULL);

            CREATE TABLE timers (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                occurrence TEXT NOT NULL,
                tz TEXT NOT NULL,
                next_fire_utc TEXT,
                last_fired TEXT,
                misfire_policy TEXT NOT NULL,
                overlap_policy TEXT NOT NULL,
                retry_policy TEXT NOT NULL,
                valid_from TEXT,
                valid_until TEXT,
                max_runs INTEGER,
                tags TEXT NOT NULL DEFAULT '[]',
                action TEXT NOT NULL,
                revision INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE runs (
                run_id TEXT PRIMARY KEY NOT NULL,
                timer_id TEXT NOT NULL,
                scheduled_for TEXT NOT NULL,
                status TEXT NOT NULL,
                claimed_at TEXT NOT NULL,
                completed_at TEXT,
                event_sequence INTEGER,
                UNIQUE (timer_id, scheduled_for)
            );

            CREATE TABLE slot_requests (
                request_id TEXT PRIMARY KEY NOT NULL,
                slot_id TEXT NOT NULL,
                operation TEXT NOT NULL,
                app_name TEXT,
                timer_id TEXT,
                status TEXT NOT NULL,
                response_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE timer_owners (
                timer_id TEXT PRIMARY KEY NOT NULL,
                app_name TEXT NOT NULL
            );

            -- slot_event_acks intentionally missing: also created by v3.
            ",
        )
        .expect("seed partial v3");
    }

    let store = Store::open_with(
        &path,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .expect("reopen after partial v3 must succeed");
    assert_eq!(store.schema_version().unwrap(), 6);

    // Second open (fully migrated) still succeeds.
    drop(store);
    let store2 = Store::open_with(
        &path,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .expect("second open");
    assert_eq!(store2.schema_version().unwrap(), 6);

    // claim_run uses event_sequence column.
    let mut store2 = store2;
    let t = store2
        .create_timer(NewTimer::new("partial", once_at(2035, 1, 1, 0, 0, 0)))
        .unwrap();
    let claim = store2.claim_run(t.id, Utc::now()).unwrap();
    assert_eq!(claim.event_sequence, 1);
}

#[test]
fn crud_round_trip_every_occurrence_kind() {
    let (_dir, mut store) = open_tmp();
    let mut ids = Vec::new();

    for (name, occ) in all_kinds() {
        let timer = store
            .create_timer(NewTimer::new(format!("t-{name}"), occ.clone()))
            .unwrap_or_else(|e| panic!("create {name}: {e}"));
        assert_eq!(timer.name, format!("t-{name}"));
        assert!(timer.enabled);
        assert_eq!(timer.revision, 1);
        assert_eq!(timer.occurrence.kind(), occ.kind());
        assert_eq!(timer.tz, occ.tz_name());
        // Future schedules must produce a next fire.
        assert!(
            timer.next_fire_utc.is_some(),
            "{name} should have next_fire_utc"
        );
        ids.push((name, timer.id, timer.next_fire_utc));
    }

    // Read back each kind.
    for (name, id, next) in &ids {
        let got = store.get_timer(*id).unwrap().expect("exists");
        assert_eq!(got.name, format!("t-{name}"));
        assert_eq!(got.next_fire_utc, *next);
        assert_eq!(got.revision, 1);
    }

    assert_eq!(store.list_timers().unwrap().len(), 7);

    // Update one, delete one.
    let (name, id, _) = &ids[0];
    let updated = store
        .update_timer(TimerUpdate {
            id: *id,
            expected_revision: 1,
            patch: TimerPatch {
                name: Some(format!("renamed-{name}")),
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(updated.name, format!("renamed-{name}"));
    assert_eq!(updated.revision, 2);

    let (_, del_id, _) = ids[1];
    assert!(store.delete_timer(del_id).unwrap());
    assert!(store.get_timer(del_id).unwrap().is_none());
    assert_eq!(store.list_timers().unwrap().len(), 6);
}

#[test]
fn stale_revision_edit_rejected() {
    let (_dir, mut store) = open_tmp();
    let t = store
        .create_timer(NewTimer::new(
            "rev",
            once_at(2031, 1, 1, 0, 0, 0),
        ))
        .unwrap();
    assert_eq!(t.revision, 1);

    // Fresh edit succeeds and bumps revision.
    let t2 = store
        .update_timer(TimerUpdate {
            id: t.id,
            expected_revision: 1,
            patch: TimerPatch {
                name: Some("rev-2".into()),
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(t2.revision, 2);
    assert_eq!(t2.name, "rev-2");

    // Stale writer still holding revision 1 must fail.
    let err = store
        .update_timer(TimerUpdate {
            id: t.id,
            expected_revision: 1,
            patch: TimerPatch {
                name: Some("stale".into()),
                ..Default::default()
            },
        })
        .unwrap_err();
    match err {
        StoreError::StaleRevision {
            expected, actual, ..
        } => {
            assert_eq!(expected, 1);
            assert_eq!(actual, 2);
        }
        other => panic!("expected StaleRevision, got {other}"),
    }

    // Name unchanged by the rejected edit.
    let still = store.get_timer(t.id).unwrap().unwrap();
    assert_eq!(still.name, "rev-2");
    assert_eq!(still.revision, 2);
}

#[test]
fn claim_ledger_blocks_duplicate_timer_id_scheduled_for() {
    let (_dir, mut store) = open_tmp();
    let t = store
        .create_timer(NewTimer::new("claim", once_at(2031, 2, 1, 0, 0, 0)))
        .unwrap();
    let scheduled = t.next_fire_utc.expect("next fire");

    let c1 = store.claim_run(t.id, scheduled).unwrap();
    assert_eq!(c1.status, ClaimStatus::Claimed);
    assert_eq!(c1.timer_id, t.id);
    assert_eq!(c1.scheduled_for, scheduled);

    let err = store.claim_run(t.id, scheduled).unwrap_err();
    match err {
        StoreError::AlreadyClaimed {
            timer_id,
            scheduled_for,
        } => {
            assert_eq!(timer_id, t.id);
            assert_eq!(scheduled_for, scheduled);
        }
        other => panic!("expected AlreadyClaimed, got {other}"),
    }

    // Completing does not free the unique slot — ledger is permanent for that fire.
    store.complete_run(c1.run_id).unwrap();
    let err2 = store.claim_run(t.id, scheduled).unwrap_err();
    assert!(matches!(err2, StoreError::AlreadyClaimed { .. }));
}

#[test]
fn horizon_query_returns_exactly_the_due_window() {
    let (_dir, mut store) = open_tmp();

    // Three one-shots at 10:00, 12:00, 14:00 UTC on a fixed day.
    let early = store
        .create_timer(NewTimer::new("early", once_at(2030, 3, 1, 10, 0, 0)))
        .unwrap();
    let mid = store
        .create_timer(NewTimer::new("mid", once_at(2030, 3, 1, 12, 0, 0)))
        .unwrap();
    let late = store
        .create_timer(NewTimer::new("late", once_at(2030, 3, 1, 14, 0, 0)))
        .unwrap();
    // Disabled timer inside the window must not appear.
    let mut disabled = NewTimer::new("disabled", once_at(2030, 3, 1, 11, 0, 0));
    disabled.enabled = false;
    let _disabled = store.create_timer(disabled).unwrap();

    let horizon = Utc.with_ymd_and_hms(2030, 3, 1, 12, 0, 0).unwrap();
    let due = store.timers_due_by(horizon).unwrap();
    let names: Vec<_> = due.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["early", "mid"], "due window must be exact");
    assert_eq!(due[0].id, early.id);
    assert_eq!(due[1].id, mid.id);

    // Just before mid: only early.
    let before_mid = horizon - Duration::seconds(1);
    let due2 = store.timers_due_by(before_mid).unwrap();
    assert_eq!(due2.len(), 1);
    assert_eq!(due2[0].id, early.id);

    // Full day: all three enabled.
    let end = Utc.with_ymd_and_hms(2030, 3, 1, 23, 59, 59).unwrap();
    let due3 = store.timers_due_by(end).unwrap();
    assert_eq!(due3.len(), 3);
    assert_eq!(due3[2].id, late.id);
}

#[test]
fn crash_between_claim_and_completion_is_recoverable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timers.db");
    let timer_id;
    let scheduled;
    let run_id;

    {
        let mut store = Store::open(&path).unwrap();
        let t = store
            .create_timer(NewTimer::new("crash", once_at(2030, 4, 1, 8, 0, 0)))
            .unwrap();
        timer_id = t.id;
        scheduled = t.next_fire_utc.unwrap();
        let claim = store.claim_run(timer_id, scheduled).unwrap();
        run_id = claim.run_id;
        assert_eq!(store.pending_claims().unwrap().len(), 1);
        // Drop without complete_run — simulates process crash mid-fire.
    }

    // Reopen: pending claim must still be visible (WAL + durable row).
    {
        let mut store = Store::open(&path).unwrap();
        let pending = store.pending_claims().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].run_id, run_id);
        assert_eq!(pending[0].timer_id, timer_id);
        assert_eq!(pending[0].scheduled_for, scheduled);
        assert_eq!(pending[0].status, ClaimStatus::Claimed);

        // Duplicate claim still blocked after recovery.
        assert!(matches!(
            store.claim_run(timer_id, scheduled).unwrap_err(),
            StoreError::AlreadyClaimed { .. }
        ));

        let done = store.complete_run(run_id).unwrap();
        assert_eq!(done.status, ClaimStatus::Completed);
        assert!(done.completed_at.is_some());
        assert!(store.pending_claims().unwrap().is_empty());
    }
}

#[test]
fn update_recomputes_next_fire_in_same_transaction() {
    let (_dir, mut store) = open_tmp();
    let t = store
        .create_timer(NewTimer::new("recompute", once_at(2030, 5, 1, 9, 0, 0)))
        .unwrap();
    let old_next = t.next_fire_utc.unwrap();

    let new_occ = once_at(2030, 5, 2, 15, 30, 0);
    let updated = store
        .update_timer(TimerUpdate {
            id: t.id,
            expected_revision: 1,
            patch: TimerPatch {
                occurrence: Some(new_occ),
                ..Default::default()
            },
        })
        .unwrap();

    let expected = Utc.with_ymd_and_hms(2030, 5, 2, 15, 30, 0).unwrap();
    assert_eq!(updated.next_fire_utc, Some(expected));
    assert_ne!(updated.next_fire_utc, Some(old_next));
    assert_eq!(updated.tz, "UTC");
}

#[test]
fn wal_and_pragmas_engaged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timers.db");
    let store = Store::open(&path).unwrap();
    let mode: String = store
        .conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
    let sync: i64 = store
        .conn
        .query_row("PRAGMA synchronous", [], |r| r.get(0))
        .unwrap();
    // FULL == 2
    assert_eq!(sync, 2);
    let _ = store; // drop → checkpoint TRUNCATE
}

#[test]
fn new_timer_misfire_defaults_by_occurrence_kind() {
    let anchor = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let interval = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 30,
            anchor,
        },
        "UTC",
    )
    .unwrap();
    let calendar = once_at(2030, 8, 1, 0, 0, 0);

    let i = NewTimer::new("interval", interval);
    assert_eq!(
        i.misfire,
        MisfirePolicy::Skip,
        "interval default must be Skip"
    );

    let c = NewTimer::new("calendar", calendar);
    assert_eq!(
        c.misfire,
        MisfirePolicy::Coalesce {
            grace_secs: MisfirePolicy::CALENDAR_GRACE_SECS
        },
        "calendar default must be Coalesce 1h"
    );

    // Persisted: create_timer keeps the NewTimer defaults.
    let (_dir, mut store) = open_tmp();
    let ti = store
        .create_timer(NewTimer::new(
            "i",
            Occurrence::new(
                OccurrenceKind::Interval {
                    every_secs: 60,
                    anchor,
                },
                "UTC",
            )
            .unwrap(),
        ))
        .unwrap();
    assert_eq!(ti.misfire, MisfirePolicy::Skip);

    let tc = store
        .create_timer(NewTimer::new("c", once_at(2030, 9, 1, 0, 0, 0)))
        .unwrap();
    assert_eq!(
        tc.misfire,
        MisfirePolicy::Coalesce {
            grace_secs: 3600
        }
    );
}

#[test]
fn action_and_policies_round_trip() {
    let (_dir, mut store) = open_tmp();
    let mut new = NewTimer::new("pol", once_at(2030, 6, 1, 0, 0, 0));
    new.misfire = MisfirePolicy::Skip;
    new.overlap = OverlapPolicy::Parallel { cap: 2 };
    new.retry = RetryPolicy {
        max_retries: 3,
        delay_secs: 10,
    };
    new.tags = vec!["a".into(), "b".into()];
    new.action = Action::Launch {
        command: "/bin/true".into(),
        args: vec!["--x".into()],
        workdir: Some("/tmp".into()),
    };
    let t = store.create_timer(new).unwrap();
    let got = store.get_timer(t.id).unwrap().unwrap();
    assert_eq!(got.misfire, MisfirePolicy::Skip);
    assert_eq!(got.overlap, OverlapPolicy::Parallel { cap: 2 });
    assert_eq!(got.retry.max_retries, 3);
    assert_eq!(got.tags, vec!["a", "b"]);
    assert_eq!(
        got.action,
        Action::Launch {
            command: "/bin/true".into(),
            args: vec!["--x".into()],
            workdir: Some("/tmp".into()),
        }
    );
}
