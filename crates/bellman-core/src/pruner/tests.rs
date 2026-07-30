//! Prune correctness on fixture logs + year recalibration idempotency.

use super::*;
use crate::events::{read_events, EventLog, EventLogConfig, EventPublisher, EventRecord, RunState};
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::store::{ClaimStatus, NewTimer, OpenOptions, Store};
use chrono::{Duration as ChronoDuration, NaiveDate, NaiveTime, TimeZone, Utc};
use std::fs;
use std::time::{Duration, SystemTime};

fn open_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_with(dir.path().join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();
    (dir, store)
}

fn once_at(days_ago: i64) -> Occurrence {
    let at = (Utc::now() - ChronoDuration::days(days_ago))
        .naive_utc();
    Occurrence::new(OccurrenceKind::Once { at }, "UTC").unwrap()
}

#[test]
fn system_prune_timer_is_created_once() {
    let (_dir, mut store) = open_store();
    let t1 = ensure_system_prune_timer(&mut store).unwrap();
    let t2 = ensure_system_prune_timer(&mut store).unwrap();
    assert_eq!(t1.id, t2.id);
    assert_eq!(t1.name, SYSTEM_PRUNE_NAME);
    assert!(is_system_prune_timer(&t1));
    let all = store.list_timers().unwrap();
    assert_eq!(all.iter().filter(|t| t.name == SYSTEM_PRUNE_NAME).count(), 1);
}

#[test]
fn prune_rotates_jsonl_and_respects_retention_edges() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();

    let mut publisher = EventPublisher::with_config(
        EventLogConfig::new(data.join("logs")).with_retention(Duration::from_secs(1)),
    )
    .unwrap();
    // Seed one event through the outbox so the current file is non-empty and
    // the prune rotation has work to do.
    store
        .enqueue_event(&EventRecord::new(RunState::Fired).with_message("keep-me"))
        .unwrap();
    publisher.publish_cycle(&store);

    // Plant an archive file with an old mtime so retention deletes it.
    let archive_dir = data.join("logs/archive");
    fs::create_dir_all(&archive_dir).unwrap();
    let old_arch = archive_dir.join("events-2020-W01.jsonl");
    fs::write(&old_arch, b"{\"kind\":\"fired\"}\n").unwrap();
    let old_mtime = SystemTime::now() - Duration::from_secs(90 * 24 * 3600);
    filetime_set_mtime(&old_arch, old_mtime);

    // Fresh archive that must be retained.
    let fresh_arch = archive_dir.join("events-2026-W30.jsonl");
    fs::write(&fresh_arch, b"{\"kind\":\"fired\"}\n").unwrap();

    let cfg = PruneConfig {
        retention: Duration::from_secs(7 * 24 * 3600),
        interval: Duration::from_secs(1),
        ack_grace: Duration::from_secs(0),
        ..PruneConfig::default()
    };
    let now = Utc::now();
    let report = run_prune(&mut store, &mut publisher, &cfg, now, true, None).unwrap();
    assert!(report.archived.is_some(), "non-empty current should rotate");
    assert!(
        report.archives_removed >= 1,
        "old archive must be retained-away"
    );
    assert!(fresh_arch.exists(), "fresh archive must survive retention");
    assert!(!old_arch.exists(), "old archive must be deleted");
    assert!(store.meta().unwrap().last_prune.is_some());
}

/// Set mtime via `touch -d` (no extra crate dependency).
fn filetime_set_mtime(path: &std::path::Path, _mtime: SystemTime) {
    let status = std::process::Command::new("touch")
        .arg("-d")
        .arg("2020-01-01T00:00:00Z")
        .arg(path)
        .status()
        .expect("spawn touch");
    assert!(status.success(), "touch -d failed for {}", path.display());
}

#[test]
fn prune_deletes_terminal_oneshots_and_writes_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();

    // Terminal fired one-shot: last_fired set, next_fire None (exhausted).
    let fired = NewTimer::new("done-once", once_at(2));
    let mut t = store.create_timer(fired).unwrap();
    // Simulate fire: bump last_fired + max_runs exhaustion via occurrence.
    let mut occ = t.occurrence.clone();
    occ.record_run();
    // Force next_fire None by max_runs=1 already done.
    let occ = Occurrence::new(
        OccurrenceKind::Once {
            at: (Utc::now() - ChronoDuration::days(2)).naive_utc(),
        },
        "UTC",
    )
    .unwrap()
    .with_max_runs(1)
    .with_runs_done(1);
    t = store
        .update_timer(crate::store::TimerUpdate {
            id: t.id,
            expected_revision: t.revision,
            patch: crate::store::TimerPatch {
                occurrence: Some(occ),
                last_fired: Some(Some(Utc::now() - ChronoDuration::hours(2))),
                ..Default::default()
            },
        })
        .unwrap();
    assert!(t.next_fire_utc.is_none(), "exhausted once has no next fire");
    let fired_id = t.id;

    // Non-terminal: pending one-shot still in the future.
    let pending = store
        .create_timer(NewTimer::new(
            "pending-once",
            Occurrence::new(
                OccurrenceKind::Once {
                    at: (Utc::now() + ChronoDuration::days(3)).naive_utc(),
                },
                "UTC",
            )
            .unwrap(),
        ))
        .unwrap();
    let pending_id = pending.id;

    // Recurring must never be pruned.
    let daily = store
        .create_timer(NewTimer::new(
            "daily",
            Occurrence::new(
                OccurrenceKind::Daily {
                    at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                },
                "UTC",
            )
            .unwrap(),
        ))
        .unwrap();
    let daily_id = daily.id;

    let mut publisher =
        EventPublisher::with_config(EventLogConfig::new(data.join("logs"))).unwrap();
    // IK2: give all three timers folders; the pruned one-shot must lose its
    // folder, the survivors keep theirs.
    let tree = crate::tree::TimersTree::new(data);
    tree.create_for_timer(&t, None).unwrap();
    tree.create_for_timer(&pending, None).unwrap();
    tree.create_for_timer(&daily, None).unwrap();
    let fired_folder = tree.folder_for(fired_id).unwrap();
    let cfg = PruneConfig {
        retention: Duration::from_secs(30 * 24 * 3600),
        interval: Duration::from_secs(1),
        ack_grace: Duration::from_secs(0),
        ..PruneConfig::default()
    };
    let report = run_prune(&mut store, &mut publisher, &cfg, Utc::now(), true, Some(&tree)).unwrap();
    assert_eq!(report.timers_pruned, 1);
    assert!(report.pruned_timer_ids.contains(&fired_id));
    assert!(store.get_timer(fired_id).unwrap().is_none());
    assert!(store.get_timer(pending_id).unwrap().is_some());
    assert!(store.get_timer(daily_id).unwrap().is_some());
    assert!(!fired_folder.exists(), "pruned one-shot loses its folder");
    assert!(tree.folder_for(pending_id).is_some());
    assert!(tree.folder_for(daily_id).is_some());

    // Tombstones sit in the outbox; drain through the publisher to read them.
    publisher.publish_cycle(&store);
    let (recs, _) = read_events(publisher.current_path()).unwrap();
    let tombs: Vec<_> = recs.iter().filter(|r| r.kind == RunState::Pruned).collect();
    assert_eq!(tombs.len(), 1);
    assert_eq!(tombs[0].timer_id, Some(fired_id));
}

#[test]
fn prune_preserves_oneshot_with_pending_claim() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();

    let past = Utc::now() - ChronoDuration::hours(3);
    let t = store
        .create_timer(NewTimer::new(
            "claimed-once",
            Occurrence::new(
                OccurrenceKind::Once {
                    at: past.naive_utc(),
                },
                "UTC",
            )
            .unwrap()
            .with_max_runs(1)
            .with_runs_done(1),
        ))
        .unwrap();
    // Force terminal shape then open a claimed run.
    let t = store
        .update_timer(crate::store::TimerUpdate {
            id: t.id,
            expected_revision: t.revision,
            patch: crate::store::TimerPatch {
                last_fired: Some(Some(past)),
                ..Default::default()
            },
        })
        .unwrap();
    let _claim = store.claim_run(t.id, past).unwrap();
    assert_eq!(
        store.get_run(_claim.run_id).unwrap().unwrap().status,
        ClaimStatus::Claimed
    );

    let mut publisher =
        EventPublisher::with_config(EventLogConfig::new(data.join("logs"))).unwrap();
    let cfg = PruneConfig {
        retention: Duration::from_secs(30 * 24 * 3600),
        interval: Duration::from_secs(1),
        ack_grace: Duration::from_secs(0),
        ..PruneConfig::default()
    };
    let report = run_prune(&mut store, &mut publisher, &cfg, Utc::now(), true, None).unwrap();
    assert_eq!(report.timers_pruned, 0, "pending claim must block prune");
    assert!(store.get_timer(t.id).unwrap().is_some());
}

#[test]
fn prune_not_due_skips_when_recent() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();
    store.set_last_prune(Utc::now()).unwrap();
    let mut publisher =
        EventPublisher::with_config(EventLogConfig::new(data.join("logs"))).unwrap();
    let cfg = PruneConfig {
        retention: Duration::from_secs(30 * 24 * 3600),
        interval: Duration::from_secs(7 * 24 * 3600),
        ack_grace: Duration::from_secs(0),
        ..PruneConfig::default()
    };
    let report = run_prune(&mut store, &mut publisher, &cfg, Utc::now(), false, None).unwrap();
    assert!(report.skipped_not_due);
}

#[test]
fn prune_sweeps_orphan_timer_folders_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();

    // A live timer and an orphan: folder exists but the database row is gone
    // (crash between the database delete and the folder delete).
    let live = store
        .create_timer(NewTimer::new(
            "live",
            Occurrence::new(
                OccurrenceKind::Daily {
                    at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                },
                "UTC",
            )
            .unwrap(),
        ))
        .unwrap();
    let orphan = store
        .create_timer(NewTimer::new(
            "orphan",
            Occurrence::new(
                OccurrenceKind::Daily {
                    at: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
                },
                "UTC",
            )
            .unwrap(),
        ))
        .unwrap();
    let tree = crate::tree::TimersTree::new(data);
    tree.create_for_timer(&live, None).unwrap();
    let orphan_folder = tree.create_for_timer(&orphan, None).unwrap();
    // Delete the row directly, bypassing the folder cleanup (the crash window).
    store.delete_timer(orphan.id).unwrap();
    assert!(orphan_folder.exists());

    let mut publisher =
        EventPublisher::with_config(EventLogConfig::new(data.join("logs"))).unwrap();
    let cfg = PruneConfig {
        interval: Duration::from_secs(1),
        ..PruneConfig::default()
    };
    let report = run_prune(&mut store, &mut publisher, &cfg, Utc::now(), true, Some(&tree)).unwrap();
    assert_eq!(report.orphan_folders_removed, 1);
    assert!(!orphan_folder.exists(), "orphan folder must be swept");
    assert!(tree.folder_for(live.id).is_some(), "live folder stays");

    // Logged, never silent — drain the outbox through the publisher to read.
    publisher.publish_cycle(&store);
    let (recs, _) = read_events(publisher.current_path()).unwrap();
    let orphan_notes: Vec<_> = recs
        .iter()
        .filter(|r| r.kind == RunState::Pruned && r.message.as_deref() == Some("orphan_timer_folder"))
        .collect();
    assert_eq!(orphan_notes.len(), 1);
}

#[test]
fn year_recalibrate_is_idempotent_within_year() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();
    store
        .create_timer(NewTimer::new(
            "d",
            Occurrence::new(
                OccurrenceKind::Daily {
                    at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
                },
                "UTC",
            )
            .unwrap(),
        ))
        .unwrap();

    let mut publisher =
        EventPublisher::with_config(EventLogConfig::new(data.join("logs"))).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap();
    assert!(needs_year_recalibration(&store, now).unwrap());
    let r1 = run_year_recalibration(&mut store, true, now).unwrap();
    assert!(!r1.skipped_idempotent);
    assert_eq!(r1.timers_checked, 1);

    let r2 = run_year_recalibration(&mut store, true, now).unwrap();
    assert!(r2.skipped_idempotent);

    // Next year forces a fresh pass.
    let next_year = Utc.with_ymd_and_hms(2027, 1, 2, 0, 0, 0).unwrap();
    assert!(needs_year_recalibration(&store, next_year).unwrap());
    assert_eq!(year_start(next_year).date_naive(), NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());

    publisher.publish_cycle(&store);
    let (recs, _) = read_events(publisher.current_path()).unwrap();
    assert_eq!(
        recs.iter()
            .filter(|r| r.kind == RunState::YearRecalibrate)
            .count(),
        1,
        "second pass must not emit another event"
    );
}

#[test]
fn startup_year_recalibration_honors_configured_log_cap() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();
    store
        .create_timer(NewTimer::new(
            "d",
            Occurrence::new(
                OccurrenceKind::Daily {
                    at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
                },
                "UTC",
            )
            .unwrap(),
        ))
        .unwrap();
    // Prune ran recently (no catch-up); recalibration is due.
    store.set_last_prune(Utc::now()).unwrap();

    // Pre-fill the live log just under a small configured cap so the
    // year_recalibrate event crosses it — rotation must honor the CONFIGURED
    // cap, not the 64 MiB default.
    let cap = 4096u64;
    let cfg = PruneConfig {
        max_current_bytes: cap,
        ..PruneConfig::default()
    };
    let mut seed = EventLog::open(
        EventLogConfig::new(data.join("logs")).with_max_current_bytes(cap),
    )
    .unwrap();
    while std::fs::metadata(seed.current_path()).unwrap().len() < cap - 300 {
        seed.emit(EventRecord::new(RunState::Fired).with_message("x".repeat(200)))
            .unwrap();
    }
    let before = std::fs::metadata(seed.current_path()).unwrap().len();
    assert!(before < cap && before > cap - 300);
    drop(seed);

    startup_maintenance(&mut store, data, &cfg, Utc::now()).unwrap();

    // The recalibration event waits in the outbox; the elected publisher —
    // carrying the CONFIGURED cap, not the 64 MiB default — appends it and
    // rotates when the append would cross the cap.
    let mut publisher = EventPublisher::with_config(
        EventLogConfig::new(data.join("logs")).with_max_current_bytes(cap),
    )
    .unwrap();
    publisher.publish_cycle(&store);

    // The recalibration append crossed the configured cap and rotated.
    let archive = data.join("logs/archive");
    let archives: Vec<_> = std::fs::read_dir(&archive)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".jsonl.gz"))
        .collect();
    assert_eq!(archives.len(), 1, "configured cap must trigger rotation");
    // The year_recalibrate event survives across archive/current.
    let (recs, _) = crate::service::log_query::read_log_history(&data.join("logs")).unwrap();
    assert!(
        recs.iter().any(|r| r.kind == RunState::YearRecalibrate),
        "year_recalibrate event must be retained"
    );
}

#[test]
fn startup_catchup_when_last_prune_stale() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();
    // last_prune 10 days ago → due.
    store
        .set_last_prune(Utc::now() - ChronoDuration::days(10))
        .unwrap();
    let cfg = PruneConfig::default();
    let notes = startup_maintenance(&mut store, data, &cfg, Utc::now()).unwrap();
    assert!(notes.iter().any(|n| n.contains("prune catch-up")));
    assert!(store.get_timer(system_prune_id()).unwrap().is_some());
}

#[test]
fn prune_with_lease_elsewhere_reports_skipped_and_never_stamps_last_prune() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();

    // The "live watcher" publisher holds the lease across the whole test.
    let mut watcher_publisher =
        EventPublisher::with_config(EventLogConfig::new(data.join("logs"))).unwrap();
    assert!(watcher_publisher.ensure_leadership().unwrap());

    // Seed one event so rotation would have work to do.
    store
        .enqueue_event(&EventRecord::new(RunState::Fired).with_message("needs-rotation"))
        .unwrap();

    // The scheduled prune's own publisher cannot lead.
    let mut prune_publisher =
        EventPublisher::with_config(EventLogConfig::new(data.join("logs"))).unwrap();
    let cfg = PruneConfig {
        interval: Duration::from_secs(0),
        ..PruneConfig::default()
    };
    let report = run_prune(&mut store, &mut prune_publisher, &cfg, Utc::now(), true, None).unwrap();
    assert!(report.skipped_not_leader, "follower prune reports the skip");
    assert!(report.archived.is_none(), "nothing was rotated");
    assert!(
        store.meta().unwrap().last_prune.is_none(),
        "last_prune must NOT be stamped when maintenance was not performed"
    );

    // The watcher releases at the end of its cycle; the next scheduled
    // prune wins the lease and really rotates.
    watcher_publisher.publish_cycle(&store);
    let report = run_prune(&mut store, &mut prune_publisher, &cfg, Utc::now(), true, None).unwrap();
    assert!(!report.skipped_not_leader);
    assert!(report.archived.is_some(), "rotation happened once the lease was free");
    assert!(store.meta().unwrap().last_prune.is_some());
}
