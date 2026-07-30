//! R11 publisher tests: outbox drain, sync durability, tail dedupe,
//! journaled rotation and crash-phase recovery, health reporting, and
//! cross-process liveness.

use super::*;
use crate::events::{read_events, EventLogConfig, EventRecord, RunState, CURRENT_FILE_NAME};
use crate::store::{OpenOptions, RotationJournal, RotationPhase, Store};
use std::fs;

struct Harness {
    dir: tempfile::TempDir,
    store: Store,
    publisher: EventPublisher,
}

impl Harness {
    fn new() -> Self {
        Self::with_max_current(u64::MAX)
    }

    fn with_max_current(max_current_bytes: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open_with(dir.path().join("timers.db"), OpenOptions::default()).unwrap();
        let publisher = EventPublisher::with_config(
            EventLogConfig::new(dir.path().join("logs")).with_max_current_bytes(max_current_bytes),
        )
        .unwrap();
        Self {
            dir,
            store,
            publisher,
        }
    }

    fn current(&self) -> PathBuf {
        self.dir.path().join("logs").join(CURRENT_FILE_NAME)
    }

    fn health(&self) -> serde_json::Value {
        let raw = fs::read_to_string(self.dir.path().join("logs").join(HEALTH_FILE_NAME))
            .expect("health file written");
        serde_json::from_str(&raw).unwrap()
    }

    fn all_events(&self) -> Vec<EventRecord> {
        crate::service::log_query::read_log_history(&self.dir.path().join("logs"))
            .unwrap()
            .0
    }
}

fn event(kind: RunState, msg: &str) -> EventRecord {
    EventRecord::new(kind).with_message(msg)
}

#[test]
fn enqueue_then_cycle_appends_syncs_and_empties_the_outbox() {
    let mut h = Harness::new();
    for i in 0..3 {
        h.store
            .enqueue_event(&event(RunState::Fired, &format!("e{i}")))
            .unwrap();
    }
    assert_eq!(h.store.count_pending_events().unwrap(), 3);

    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.leader);
    assert_eq!(report.published, 3);
    assert!(report.error.is_none());
    assert_eq!(h.store.count_pending_events().unwrap(), 0);

    let (recs, stats) = read_events(h.current()).unwrap();
    assert_eq!(recs.len(), 3);
    assert_eq!(stats.skipped, 0);

    let health = h.health();
    assert_eq!(health["schema"], HEALTH_SCHEMA_V1);
    assert_eq!(health["leader"], true);
    assert_eq!(health["pending_events"], 0);
    assert!(health.get("last_error").is_none());
    assert!(health["last_ok_at"].is_string());
}

#[test]
fn append_failure_leaves_rows_pending_and_health_reports_until_retry() {
    let mut h = Harness::new();
    h.store
        .enqueue_event(&event(RunState::Fired, "doomed"))
        .unwrap();

    // Force the append to fail: the current path is a directory.
    let current = h.current();
    let _ = fs::remove_file(&current);
    fs::create_dir(&current).unwrap();

    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_some(), "the failure surfaces");
    assert_eq!(
        h.store.count_pending_events().unwrap(),
        1,
        "a failed append never drops the row"
    );
    let health = h.health();
    assert!(health["last_error"].is_string(), "health visibly reports the error");

    // Repair; a later cycle drains without duplicates.
    fs::remove_dir(&current).unwrap();
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_none());
    assert_eq!(report.published, 1);
    assert_eq!(h.store.count_pending_events().unwrap(), 0);
    let health = h.health();
    assert!(health.get("last_error").is_none(), "recovery clears the error");
    assert_eq!(h.all_events().len(), 1);
}

#[test]
fn crash_between_sync_and_mark_published_is_reconciled_from_the_tail() {
    let mut h = Harness::new();
    let rec = event(RunState::Completed, "synced but unmarked");
    h.store.enqueue_event(&rec).unwrap();

    // Simulate the crash window: the line is durably on disk (append +
    // fdatasync via the raw log) but the outbox row was never deleted.
    let payload = serde_json::to_string(&rec).unwrap() + "\n";
    let mut raw = EventLog::open(EventLogConfig::new(h.dir.path().join("logs"))).unwrap();
    raw.append_line_synced(&payload).unwrap();
    assert_eq!(h.store.count_pending_events().unwrap(), 1);

    // The publisher's reconcile marks it without re-appending.
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_none());
    assert_eq!(h.store.count_pending_events().unwrap(), 0);
    let events = h.all_events();
    assert_eq!(events.len(), 1, "no duplicate from the crash window");
    assert_eq!(events[0].event_id, rec.event_id);
}

#[test]
fn readers_dedupe_a_physically_duplicated_line_by_event_id() {
    let h = Harness::new();
    let rec = event(RunState::Completed, "twice on disk");
    let payload = serde_json::to_string(&rec).unwrap() + "\n";
    let mut raw = EventLog::open(EventLogConfig::new(h.dir.path().join("logs"))).unwrap();
    raw.append_line_synced(&payload).unwrap();
    raw.append_line_synced(&payload).unwrap();

    let events = h.all_events();
    assert_eq!(events.len(), 1, "readers dedupe by event_id");
}

#[test]
fn size_threshold_rotates_through_the_journal() {
    let mut h = Harness::with_max_current(600);
    for i in 0..10 {
        h.store
            .enqueue_event(&event(RunState::Fired, &format!("rotation filler {i}")))
            .unwrap();
    }
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_none());
    assert!(report.rotated.is_some(), "rotation happened under the threshold");
    assert!(h.publisher.current_path().exists());
    assert!(h.store.rotation_journal().unwrap().is_none(), "journal cleared");

    let archive = report.rotated.unwrap();
    assert!(archive.exists());
    assert!(archive.to_string_lossy().ends_with(".jsonl.gz"));
    assert!(
        !h.dir.path().join("logs").join(ROTATING_FILE_NAME).exists(),
        "no working file left behind"
    );
    // Every event is readable across archive + current, exactly once.
    let events = h.all_events();
    let filler = events
        .iter()
        .filter(|e| e.message.as_deref().is_some_and(|m| m.starts_with("rotation filler")))
        .count();
    assert_eq!(filler, 10);
}

/// Seed a crashed rotation at a given phase with real files, then let a
/// fresh publisher recover it.
fn seed_crash(h: &mut Harness, phase: RotationPhase, with_gz_tmp: bool) -> RotationJournal {
    // Real lines in current, then the crash: current renamed to .rotating.
    let mut raw = EventLog::open(EventLogConfig::new(h.dir.path().join("logs"))).unwrap();
    let rec = event(RunState::Completed, "rotated away mid-crash");
    let payload = serde_json::to_string(&rec).unwrap() + "\n";
    raw.append_line_synced(&payload).unwrap();
    raw.close_handle().unwrap();
    // The outbox still holds the row (never marked before the crash).
    h.store.enqueue_event(&rec).unwrap();

    let logs = h.dir.path().join("logs");
    let archive_dir = logs.join("archive");
    fs::create_dir_all(&archive_dir).unwrap();
    let journal = RotationJournal {
        source: logs.join(CURRENT_FILE_NAME),
        rotating: logs.join(ROTATING_FILE_NAME),
        gz_tmp: archive_dir.join(".events-2026-W31.jsonl.gz.tmp"),
        final_path: archive_dir.join("events-2026-W31.jsonl.gz"),
        phase,
        started_at: Utc::now(),
    };
    fs::rename(logs.join(CURRENT_FILE_NAME), &journal.rotating).unwrap();
    match phase {
        RotationPhase::Renamed => {
            if with_gz_tmp {
                fs::write(&journal.gz_tmp, b"\x1f\x8b partial garbage").unwrap();
            }
        }
        RotationPhase::Finalized => {
            gzip_file(&journal.rotating, &journal.gz_tmp).unwrap();
            fs::rename(&journal.gz_tmp, &journal.final_path).unwrap();
        }
        RotationPhase::SourceRemoved => {
            gzip_file(&journal.rotating, &journal.gz_tmp).unwrap();
            fs::rename(&journal.gz_tmp, &journal.final_path).unwrap();
            fs::remove_file(&journal.rotating).unwrap();
        }
    }
    h.store.set_rotation_journal(&journal).unwrap();
    journal
}

fn assert_recovered(h: &mut Harness) {
    assert!(h.store.rotation_journal().unwrap().is_none());
    assert!(h.current().exists());
    assert!(
        !h.dir.path().join("logs").join(ROTATING_FILE_NAME).exists(),
        "redundant source removed"
    );
    let leftovers: Vec<_> = fs::read_dir(h.dir.path().join("logs/archive"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no partial gzip temp survives");
    let events = h.all_events();
    let matching = events
        .iter()
        .filter(|e| e.message.as_deref() == Some("rotated away mid-crash"))
        .count();
    assert_eq!(matching, 1, "every event readable exactly once logically");
}

#[test]
fn crash_after_rename_recovers_and_finishes_the_archive() {
    let mut h = Harness::new();
    seed_crash(&mut h, RotationPhase::Renamed, false);
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_none());
    assert_recovered(&mut h);
}

#[test]
fn crash_with_partial_gzip_temp_recovers_from_the_plain_source() {
    let mut h = Harness::new();
    let j = seed_crash(&mut h, RotationPhase::Renamed, true);
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_none());
    assert_recovered(&mut h);
    assert!(j.final_path.exists());
}

#[test]
fn crash_after_final_before_source_delete_recovers() {
    let mut h = Harness::new();
    seed_crash(&mut h, RotationPhase::Finalized, false);
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_none());
    assert_recovered(&mut h);
}

#[test]
fn crash_after_source_delete_before_journal_clear_recovers() {
    let mut h = Harness::new();
    seed_crash(&mut h, RotationPhase::SourceRemoved, false);
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_none());
    assert_recovered(&mut h);
}

#[test]
fn cross_process_outbox_liveness_via_the_safety_tick() {
    let mut h = Harness::new();
    // The "GUI" leader takes the lease first.
    assert!(h.publisher.ensure_leadership().unwrap());

    // A second process's publisher (the "CLI") cannot lead.
    let mut cli_publisher =
        EventPublisher::with_config(EventLogConfig::new(h.dir.path().join("logs"))).unwrap();
    let cli_store = Store::open_with(h.dir.path().join("timers.db"), OpenOptions::default())
        .unwrap();
    cli_store
        .enqueue_event(&event(RunState::Registered, "from the CLI process"))
        .unwrap();
    let report = cli_publisher.publish_cycle(&cli_store);
    assert!(!report.leader, "the lease is held elsewhere");
    assert_eq!(report.published, 0);
    assert_eq!(h.store.count_pending_events().unwrap(), 1);

    // No in-process signal: the GUI's periodic tick drains the row anyway.
    let report = h.publisher.publish_cycle(&h.store);
    assert_eq!(report.published, 1);
    assert_eq!(h.store.count_pending_events().unwrap(), 0);
    assert_eq!(h.all_events().len(), 1);
}

#[test]
fn one_event_line_stays_under_the_4kb_cap() {
    // R12's last row: with the field caps held, a full event line must fit
    // 4 KiB. Construct the chattiest legal line: 2 KB result + 1 KB reason.
    let detail = serde_json::json!({
        "result": "r".repeat(2048),
        "failure_kind": "reported",
        "duration_source": "wall_clock",
    });
    let rec = EventRecord::new(RunState::Failed)
        .with_timer(Uuid::new_v4(), "timer-name")
        .with_run(Uuid::new_v4())
        .with_scheduled_for(Utc::now())
        .with_duration_ms(12345)
        .with_message("r".repeat(1024))
        .with_detail(detail);
    let line = serde_json::to_string(&rec).unwrap();
    assert!(
        line.len() <= 4096,
        "a capped event line must stay under 4 KiB, got {}",
        line.len()
    );
}

#[test]
fn a_follower_never_clobbers_the_leaders_health_file() {
    let mut h = Harness::new();
    // Step 1: the leader writes leader:true.
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.leader);
    assert_eq!(h.health()["leader"], true);

    // Step 2: the leader holds the lease; a second publisher's cycle is a
    // follower and must NOT overwrite the file with leader:false.
    assert!(h.publisher.ensure_leadership().unwrap());
    let second_store =
        Store::open_with(h.dir.path().join("timers.db"), OpenOptions::default()).unwrap();
    let mut follower =
        EventPublisher::with_config(EventLogConfig::new(h.dir.path().join("logs"))).unwrap();
    let report = follower.publish_cycle(&second_store);
    assert!(!report.leader, "the lease is held elsewhere");
    assert_eq!(
        h.health()["leader"],
        true,
        "the leader's health survives a follower cycle"
    );

    // Step 3: with the lease free again, a follower state's cycle writes
    // honestly (nothing to clobber — the file already says leader:true from
    // a real leader, so this asserts only that leadership still works).
    drop(h.publisher);
    let mut next =
        EventPublisher::with_config(EventLogConfig::new(h.dir.path().join("logs"))).unwrap();
    let report = next.publish_cycle(&second_store);
    assert!(report.leader);
}

#[test]
fn recovery_keeps_the_journal_when_cleanup_fails() {
    let mut h = Harness::new();
    // Seed an interrupted rotation at phase Finalized: the final archive is
    // durable, but the .rotating source still needs cleanup.
    let logs = h.dir.path().join("logs");
    let archive = logs.join("archive");
    fs::create_dir_all(&archive).unwrap();
    let rotating = logs.join(ROTATING_FILE_NAME);
    let final_path = archive.join("events-2026-W31.jsonl.gz");
    let rec = event(RunState::Completed, "archived before the crash");
    let payload = serde_json::to_string(&rec).unwrap() + "\n";
    fs::write(&rotating, &payload).unwrap();
    let gz_tmp = archive.join(".events-2026-W31.jsonl.gz.tmp");
    gzip_file(&rotating, &gz_tmp).unwrap();
    fs::rename(&gz_tmp, &final_path).unwrap();
    let journal = RotationJournal {
        source: logs.join(CURRENT_FILE_NAME),
        rotating: rotating.clone(),
        gz_tmp,
        final_path: final_path.clone(),
        phase: RotationPhase::Finalized,
        started_at: Utc::now(),
    };
    h.store.set_rotation_journal(&journal).unwrap();

    // Fault: the .rotating path is a DIRECTORY, so deletion fails.
    fs::remove_file(&rotating).unwrap();
    fs::create_dir(&rotating).unwrap();

    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_some(), "the cleanup failure surfaces");
    assert!(
        h.store.rotation_journal().unwrap().is_some(),
        "the journal is NOT cleared over a failed cleanup — retry stays possible"
    );
    assert!(final_path.exists(), "the durable archive is never touched");

    // Clear the fault: the next cycle completes the cleanup and clears.
    fs::remove_dir(&rotating).unwrap();
    fs::write(&rotating, &payload).unwrap();
    let report = h.publisher.publish_cycle(&h.store);
    assert!(report.error.is_none());
    assert!(h.store.rotation_journal().unwrap().is_none());
    assert!(!rotating.exists(), "redundant source removed");
    let events = h.all_events();
    assert_eq!(
        events.iter().filter(|e| e.event_id == rec.event_id).count(),
        1,
        "the archived event is readable exactly once"
    );
}
