//! SCH1 exit-gate tests: fire-notification publication ownership.
//!
//! The fire notification is published at fire time by the producer — even
//! while the action is queued, skipped, retrying, or ultimately failing —
//! under `slots/fires/` only, with durable transport projections, the
//! fixed-target cursor, compare-before-delete cleanup, and bounded
//! at-least-once redelivery. `slots/done/` belongs to `SlotService` alone.

use bellman_core::occurrence::{Occurrence, OccurrenceKind};
use bellman_core::reply::publication;
use bellman_core::reply::{fires_dir, FireNotification};
use bellman_core::scheduler::FireKind;
use bellman_core::store::{
    Action, ClaimStatus, NewTimer, OpenOptions, OverlapPolicy, RunClaim, RunOutcome, Store, Timer,
    TransportProjection,
};
use bellman_core::TimersTree;
use chrono::{Duration as ChronoDuration, Utc};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

struct Env {
    _dir: tempfile::TempDir,
    data: PathBuf,
    db: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_path_buf();
    let db = data.join("timers.db");
    Env {
        _dir: dir,
        data,
        db,
    }
}

fn store(e: &Env) -> Store {
    Store::open_with(
        &e.db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .unwrap()
}

fn engine(e: &Env, fire_slot_file: Option<&str>) -> bellman_core::reply::ReplyEngine {
    bellman_core::reply::ReplyEngine {
        tree: TimersTree::new(&e.data),
        data_dir: e.data.clone(),
        pickup_grace: Duration::from_secs(60),
        watchdog_factor: 2.0,
        anchors: bellman_core::reply::new_anchors(),
        deadlines: bellman_core::reply::new_deadlines(),
        fire_slot_file: fire_slot_file.map(str::to_string),
        status_listener: None,
        ipc: None,
    }
}

fn owned_timer(store: &mut Store, name: &str, action: Action, overlap: OverlapPolicy) -> Timer {
    let occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 3600,
            anchor: Utc::now() - ChronoDuration::hours(2),
        },
        "UTC",
    )
    .unwrap();
    let mut t = NewTimer::new(name, occ);
    t.action = action;
    t.overlap = overlap;
    let timer = store.create_timer(t).unwrap();
    store.set_timer_owner(timer.id, "testapp").unwrap();
    timer
}

fn fire(e: &Env, store: &mut Store, timer: &Timer, fire_slot_file: Option<&str>) -> RunClaim {
    let eng = engine(e, fire_slot_file);
    bellman_core::project_fire(
        &TimersTree::new(&e.data),
        store,
        timer,
        Utc::now(),
        &FireKind::OnTime,
        &eng,
        Utc::now(),
    )
    .unwrap()
}

fn launch(command: &str, args: &[&str]) -> Action {
    Action::Launch {
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        workdir: None,
    }
}

fn fires_file(e: &Env, run_id: Uuid) -> PathBuf {
    fires_dir(&e.data.join("slots")).join(format!("fire-{run_id}.json"))
}

fn read_notification(path: &Path) -> FireNotification {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn timer_folder(e: &Env, timer_id: Uuid) -> PathBuf {
    TimersTree::new(&e.data).folder_for(timer_id).unwrap()
}

/// A skipped firing still produces its fire notification — publication
/// reports the FIRING; the action outcome lives in the claim.
#[test]
fn skipped_firing_still_publishes_its_notification() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "own", launch("sleep", &["30"]), OverlapPolicy::Skip);

    let c1 = fire(&e, &mut st, &t, None);
    assert!(fires_file(&e, c1.run_id).exists(), "first fire notifies");
    // Hold the first action "in flight" (no dispatcher dequeue needed —
    // publication must not care).
    st.activate_run(c1.run_id).unwrap();

    let t0 = Instant::now();
    let c2 = fire(&e, &mut st, &t, None);
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "publication of the second fire must be immediate"
    );
    let c2 = st.get_run(c2.run_id).unwrap().unwrap();
    assert_eq!(c2.outcome, Some(RunOutcome::SkippedMisfire));
    assert_eq!(c2.outcome_reason.as_deref(), Some("overlap_skip"));
    let n2 = read_notification(&fires_file(&e, c2.run_id));
    assert_eq!(n2.run_id, c2.run_id);
    assert!(
        n2.reply_path.as_ref().expect("file transport carries reply_path").exists(),
        "notification carries a real reply stub"
    );
    assert!(n2.status_path.exists());
}

/// A firing whose action ultimately fails still produced its notification
/// at fire time (the semantic change is load-bearing).
#[test]
fn failing_action_still_published_at_fire_time() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "failer", launch("false", &[]), OverlapPolicy::Skip);
    let c = fire(&e, &mut st, &t, None);
    // No worker has run yet — the notification is already there.
    assert!(fires_file(&e, c.run_id).exists());
    let proj = st.transport_projection(c.run_id).unwrap().unwrap();
    assert_eq!(proj.state, TransportProjection::PUBLISHED);
}

/// With a FIXED configured target, a newer firing's notification is never
/// overwritten or duplicated by the older firing's late retry, and the
/// durable cursor prevents the older one from resurfacing after the newer
/// file is consumed.
#[test]
fn fixed_target_newer_wins_and_cursor_blocks_resurface() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "fixed", launch("sleep", &["30"]), OverlapPolicy::Parallel { cap: 2 });
    let fixed = fires_dir(&e.data.join("slots")).join("wake.json");

    let c1 = fire(&e, &mut st, &t, Some("wake.json"));
    st.activate_run(c1.run_id).unwrap(); // first action still running
    assert_eq!(read_notification(&fixed).run_id, c1.run_id);

    // Second firing publishes while the first action runs.
    let c2 = fire(&e, &mut st, &t, Some("wake.json"));
    assert_eq!(
        read_notification(&fixed).run_id,
        c2.run_id,
        "the newer firing owns the fixed path immediately"
    );
    let after_c2 = std::fs::read(&fixed).unwrap();

    // The first firing's late completion/retry neither overwrites nor
    // duplicates the second's notification.
    let proj1 = st.transport_projection(c1.run_id).unwrap().unwrap();
    let outcome = publication::attempt(&e.data, &st, &proj1, None);
    assert_eq!(outcome, publication::Attempt::Obsolete);
    assert_eq!(std::fs::read(&fixed).unwrap(), after_c2, "byte-identical");
    assert_eq!(
        st.transport_projection(c1.run_id).unwrap().unwrap().state,
        TransportProjection::OBSOLETE
    );

    // The app consumes the newer file without pickup: the older projection
    // must not resurface (durable target cursor).
    std::fs::remove_file(&fixed).unwrap();
    let outcome = publication::attempt(&e.data, &st, &proj1, None);
    assert_eq!(outcome, publication::Attempt::Obsolete);
    assert!(!fixed.exists(), "the cursor keeps the older hint obsolete");
}

/// Cleanup is compare-before-delete under the publisher lock: an older
/// firing's pickup never removes a newer fixed-path wake hint.
#[test]
fn pickup_cleanup_never_removes_newer_fixed_hint() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "fixed2", launch("true", &[]), OverlapPolicy::Parallel { cap: 2 });
    let fixed = fires_dir(&e.data.join("slots")).join("wake.json");

    let c1 = fire(&e, &mut st, &t, Some("wake.json"));
    let c2 = fire(&e, &mut st, &t, Some("wake.json"));
    let after_c2 = std::fs::read(&fixed).unwrap();
    assert_eq!(read_notification(&fixed).run_id, c2.run_id);

    // Late pickup of the FIRST firing: the second's file survives.
    let proj1 = st.transport_projection(c1.run_id).unwrap().unwrap();
    publication::record_pickup(&e.data, &st, &proj1);
    assert_eq!(std::fs::read(&fixed).unwrap(), after_c2, "byte-identical");

    // Pickup of the second firing removes its own file.
    let proj2 = st.transport_projection(c2.run_id).unwrap().unwrap();
    publication::record_pickup(&e.data, &st, &proj2);
    assert!(!fixed.exists());
}

/// Crash windows: before the atomic replace the pump republishes; after the
/// replace but before pickup an unchanged file suppresses only the immediate
/// rewrite — if the app consumed it without pickup, redelivery is allowed
/// and `run_id` dedupe keeps it one logical firing; pickup stops retries.
#[test]
fn crash_windows_and_consumed_without_pickup_redelivery() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "crash", launch("true", &[]), OverlapPolicy::Skip);
    let c = fire(&e, &mut st, &t, None);
    let file = fires_file(&e, c.run_id);
    assert!(file.exists(), "published at fire");

    // Crash window 2: replace done, pickup never recorded, app deletes the
    // file. The pump re-queues and redelivers.
    std::fs::remove_file(&file).unwrap();
    publication::pump(&e.data, &st, 8, None); // sweep: published + missing → pending
    publication::pump(&e.data, &st, 8, None); // attempt: rewrite
    assert!(file.exists(), "redelivery after silent consumption");
    assert_eq!(read_notification(&file).run_id, c.run_id, "same run_id — one logical firing");

    // Crash window 1: crash before the replace — pending projection, file
    // missing; the pump writes it.
    std::fs::remove_file(&file).unwrap();
    st.requeue_transport_projection(c.run_id).unwrap();
    publication::pump(&e.data, &st, 8, None);
    assert!(file.exists(), "pending projection is (re)published by the pump");

    // Unchanged file suppresses the immediate recovery rewrite but the
    // projection stays eligible until pickup.
    let mtime1 = std::fs::metadata(&file).unwrap().modified().unwrap();
    let proj = st.transport_projection(c.run_id).unwrap().unwrap();
    let outcome = publication::attempt(&e.data, &st, &proj, None);
    assert_eq!(outcome, publication::Attempt::Deferred, "same run_id suppresses rewrite");
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        mtime1,
        "file untouched by the suppressed rewrite"
    );

    // Pickup (ack_through past the firing): retries stop, file cleaned.
    st.ack_run_events(t.id, c.event_sequence).unwrap();
    publication::pump(&e.data, &st, 8, None);
    assert_eq!(
        st.transport_projection(c.run_id).unwrap().unwrap().state,
        TransportProjection::PICKED_UP
    );
    assert!(!file.exists(), "pickup removes the notification");
    publication::pump(&e.data, &st, 8, None);
    assert!(!file.exists(), "no resurrection after pickup");
}

/// Malformed bytes at the target are atomically replaced by the newest
/// pending projection (Bellman-owned namespace).
#[test]
fn malformed_target_is_replaced() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "mal", launch("true", &[]), OverlapPolicy::Skip);
    let c = fire(&e, &mut st, &t, None);
    let file = fires_file(&e, c.run_id);
    std::fs::write(&file, b"not json at all").unwrap();
    st.requeue_transport_projection(c.run_id).unwrap();
    publication::pump(&e.data, &st, 8, None);
    assert_eq!(read_notification(&file).run_id, c.run_id);
}

/// Run files precede delivery: while `status.json` or the reply stub is
/// missing, the projection stays pending and no notification appears.
#[test]
fn run_files_precede_delivery() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "order", launch("true", &[]), OverlapPolicy::Skip);
    let c = fire(&e, &mut st, &t, None);
    let file = fires_file(&e, c.run_id);
    let folder = timer_folder(&e, t.id);
    let status = folder.join("status.json");
    let reply = folder.join(format!("reply-{}.json", c.run_id));

    // Fail status.json: no notification is (re)published.
    let saved_status = std::fs::read(&status).unwrap();
    std::fs::remove_file(&status).unwrap();
    std::fs::remove_file(&file).unwrap();
    st.requeue_transport_projection(c.run_id).unwrap();
    let proj = st.transport_projection(c.run_id).unwrap().unwrap();
    let outcome = publication::attempt(&e.data, &st, &proj, None);
    assert_eq!(outcome, publication::Attempt::Deferred);
    assert!(!file.exists(), "no notification while status.json is missing");
    std::fs::write(&status, &saved_status).unwrap();

    // Fail the stub: same.
    std::fs::remove_file(&reply).unwrap();
    let outcome = publication::attempt(&e.data, &st, &proj, None);
    assert_eq!(outcome, publication::Attempt::Deferred);
    assert!(!file.exists(), "no notification while the reply stub is missing");

    // R10 reconciliation repairs the stub (create-only); the pump publishes.
    TimersTree::new(&e.data)
        .create_reply_stub(&folder, c.run_id, "testapp")
        .unwrap();
    let outcome = publication::attempt(&e.data, &st, &proj, None);
    assert_eq!(outcome, publication::Attempt::Published);
    assert!(file.exists());
}

/// The two slot namespaces never collide: `done/slot-<id>.json` is
/// SlotService's response file; fires live under `fires/`, and no post-action
/// overlay is written to `done/`.
#[test]
fn slot_namespaces_never_collide() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "ns", launch("true", &[]), OverlapPolicy::Skip);

    // A SlotService-owned done file.
    let done = e.data.join("slots").join("done");
    std::fs::create_dir_all(&done).unwrap();
    let done_file = done.join("slot-0001.json");
    std::fs::write(&done_file, br#"{"schema":"bellman-slot/1","slot_id":"0001"}"#).unwrap();

    // A full standalone run-now (which used to overlay done/).
    let outcome = bellman_core::run_now(&mut st, &e.db, t.id, &bellman_core::RunNowOptions::default())
        .unwrap();
    assert_eq!(
        std::fs::read(&done_file).unwrap(),
        br#"{"schema":"bellman-slot/1","slot_id":"0001"}"#.to_vec(),
        "run-now never overlays done/"
    );
    assert!(
        fires_file(&e, outcome.run_id).exists(),
        "the fire notification lives under fires/"
    );
}

/// A second fire publishes immediately even while the first action still
/// runs: the folder shows the new run_id and `superseded` is logged at
/// fire time, not after the action.
#[test]
fn second_fire_publishes_immediately_while_first_action_runs() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "imm", launch("sleep", &["30"]), OverlapPolicy::Parallel { cap: 2 });

    let c1 = fire(&e, &mut st, &t, None);
    st.activate_run(c1.run_id).unwrap(); // first action still executing

    let t0 = Instant::now();
    let c2 = fire(&e, &mut st, &t, None);
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "the whole second fire publishes immediately: {:?}",
        t0.elapsed()
    );

    // status.json already shows the new run_id…
    let status: serde_json::Value = serde_json::from_slice(
        &std::fs::read(timer_folder(&e, t.id).join("status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["run_id"].as_str().unwrap(), c2.run_id.to_string());
    // …the first run is already superseded in the durable lifecycle…
    assert_eq!(
        st.get_run_state(c1.run_id).unwrap().unwrap().state,
        "superseded"
    );
    // …the superseded event is in the outbox…
    let events = st.pending_events(64).unwrap();
    assert!(
        events.iter().any(|(_, p)| p.contains("superseded") && p.contains(&c1.run_id.to_string())),
        "superseded event logged at fire time"
    );
    // …the second notification is out…
    assert!(fires_file(&e, c2.run_id).exists());
    // …and the first action is STILL in its lane.
    assert_eq!(
        st.get_run(c1.run_id).unwrap().unwrap().status,
        ClaimStatus::Active
    );
}

/// Startup ordering (SCH1): while Bellman is stopped, a valid reply for the
/// first firing lands. The reply scan must complete BEFORE any pump runs —
/// otherwise the publication pump replays the old notification. Demonstrated
/// both ways, then asserted in the real boot sequence.
#[test]
fn startup_reply_scan_precedes_publication_replay() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "order1", launch("true", &[]), OverlapPolicy::Skip);
    let c1 = fire(&e, &mut st, &t, None);
    let file = fires_file(&e, c1.run_id);
    assert!(file.exists());

    // While "stopped": the app answers `completed`, and the notification is
    // consumed (crash window — the projection is pending again).
    let folder = timer_folder(&e, t.id);
    std::fs::write(
        folder.join(format!("reply-{}.json", c1.run_id)),
        serde_json::to_string_pretty(&serde_json::json!({
            "schema": bellman_core::reply::REPLY_SCHEMA_V1,
            "run_id": c1.run_id,
            "app_name": "testapp",
            "state": "completed",
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::remove_file(&file).unwrap();
    st.requeue_transport_projection(c1.run_id).unwrap();

    // BROKEN ORDER (demonstration, not the boot path): if the publication
    // pump ran before the reply scan, the old notification would be replayed.
    publication::pump(&e.data, &st, 8, None);
    assert!(
        file.exists(),
        "pump-first replays the stale notification — this is what the boot order prevents"
    );

    // Reset the window: consumed again, projection pending again.
    std::fs::remove_file(&file).unwrap();
    st.requeue_transport_projection(c1.run_id).unwrap();

    // REAL BOOT SEQUENCE: the reply scan runs FIRST (drive.rs boot order).
    let eng = engine(&e, None);
    bellman_core::reply::startup_scan(&eng, &st, Utc::now());
    assert_eq!(
        st.get_run_state(c1.run_id).unwrap().unwrap().state,
        "completed",
        "the reply is ingested before any pump runs"
    );

    // Only now the pumps run: pickup is already recorded, so the projection
    // is consumed — the stale notification is never replayed.
    publication::pump(&e.data, &st, 8, None);
    assert_eq!(
        st.transport_projection(c1.run_id).unwrap().unwrap().state,
        TransportProjection::PICKED_UP
    );
    assert!(
        !file.exists(),
        "scan-first: the old notification is never replayed"
    );
}

/// Startup ordering end-to-end: a reply written while stopped is ingested
/// before the boot can publish the second firing — the first run is never
/// superseded, and its `completed` event precedes the second firing's
/// `fired` event in the drained log.
#[test]
fn boot_ingests_stopped_reply_before_second_firing_publishes() {
    let e = env();
    let mut st = store(&e);
    let t = owned_timer(&mut st, "order2", launch("true", &[]), OverlapPolicy::Skip);
    let c1 = fire(&e, &mut st, &t, None);

    // While "stopped": the app answers, and the timer becomes due again.
    let folder = timer_folder(&e, t.id);
    std::fs::write(
        folder.join(format!("reply-{}.json", c1.run_id)),
        serde_json::to_string_pretty(&serde_json::json!({
            "schema": bellman_core::reply::REPLY_SCHEMA_V1,
            "run_id": c1.run_id,
            "app_name": "testapp",
            "state": "completed",
        }))
        .unwrap(),
    )
    .unwrap();
    let fresh = st.get_timer(t.id).unwrap().unwrap();
    st.update_timer(bellman_core::store::TimerUpdate {
        id: t.id,
        expected_revision: fresh.revision,
        patch: bellman_core::store::TimerPatch {
            last_fired: Some(Some(Utc::now() - ChronoDuration::hours(1))),
            ..Default::default()
        },
    })
    .unwrap();

    // Restart: scheduler + dispatcher boot (the real boot path).
    let disp = bellman_core::actions::Dispatcher::spawn(bellman_core::actions::DispatcherConfig {
        db_path: e.db.clone(),
        data_dir: Some(e.data.clone()),
        max_concurrent_actions: 2,
        notify_sink: std::sync::Arc::new(bellman_core::actions::StubNotifySink),
        executor: bellman_core::actions::ExecutorConfig::default(),
        tick: Duration::from_millis(50),
            ipc: None,
    })
    .unwrap();
    let cfg = bellman_core::scheduler::SchedulerConfig::default().with_data_dir(e.data.clone());
    let mut sched = bellman_core::scheduler::Scheduler::new(
        store(&e),
        bellman_core::scheduler::SystemClock::new(),
        disp.clone(),
        cfg,
    );
    let boot_fires = sched.boot().unwrap();

    // The first run was ingested as completed — never superseded.
    assert_eq!(
        st.get_run_state(c1.run_id).unwrap().unwrap().state,
        "completed"
    );
    let pending = st.pending_events(64).unwrap();
    assert!(
        !pending
            .iter()
            .any(|(_, p)| p.contains("superseded") && p.contains(&c1.run_id.to_string())),
        "the first run must not be superseded once its reply was read"
    );

    // The second firing was published during boot (after the ingest).
    let runs = st.runs_for_timer(t.id).unwrap();
    let c2 = runs.last().unwrap().clone();
    assert_ne!(c2.run_id, c1.run_id, "the due timer fired again at boot");
    assert!(
        boot_fires.iter().any(|f| f.run_id == c2.run_id),
        "the second firing was delivered by boot"
    );
    assert!(
        fires_file(&e, c2.run_id).exists(),
        "the second firing's notification was published"
    );

    // The completed transition for run1 precedes the second firing's fired
    // event in the drained log.
    let mut publisher = bellman_core::events::EventPublisher::with_config(
        bellman_core::events::EventLogConfig::new(e.data.join("logs")),
    )
    .unwrap();
    publisher.publish_cycle(&st);
    let content = std::fs::read_to_string(e.data.join("logs").join("events.current.jsonl")).unwrap();
    let pos_completed = content
        .lines()
        .position(|l| l.contains("\"completed\"") && l.contains(&c1.run_id.to_string()))
        .expect("completed event for run1");
    let pos_fired2 = content
        .lines()
        .position(|l| l.contains("\"fired\"") && l.contains(&c2.run_id.to_string()))
        .expect("fired event for run2");
    assert!(
        pos_completed < pos_fired2,
        "the reply ingest commits before the second firing publishes:\n{content}"
    );
    sched.action().shutdown_drain();
}

/// `completed`, the worker later finishes `wake_delivered`, and
/// `status.json` stays the app's `completed`.
/// Worker completion never regresses the app lifecycle: the app reports
/// `completed`, the worker later finishes `wake_delivered`, and
/// `status.json` stays the app's `completed`.
#[test]
fn worker_result_does_not_regress_status_json() {
    let e = env();
    let mut st = store(&e);
    let marker = e.data.join("w.log");
    let cmd = format!("echo ran >> '{}'", marker.display());
    let t = owned_timer(&mut st, "nore", launch("sh", &["-c", &cmd]), OverlapPolicy::Skip);
    let c = fire(&e, &mut st, &t, None);

    // The app reports completed (durable lifecycle + projected file).
    let mut row = st.get_run_state(c.run_id).unwrap().unwrap();
    row.state = "completed".into();
    row.completed_at = Some(Utc::now());
    st.update_run_state(&row).unwrap();
    let status = bellman_core::tree::RunStatus::from_run_state(&t, &c, &row);
    bellman_core::tree::write_status(&TimersTree::new(&e.data), &t, &status).unwrap();

    // The worker executes and commits wake_delivered.
    let disp = bellman_core::actions::Dispatcher::spawn(bellman_core::actions::DispatcherConfig {
        db_path: e.db.clone(),
        data_dir: Some(e.data.clone()),
        max_concurrent_actions: 2,
        notify_sink: std::sync::Arc::new(bellman_core::actions::StubNotifySink),
        executor: bellman_core::actions::ExecutorConfig::default(),
        tick: Duration::from_millis(50),
            ipc: None,
    })
    .unwrap();
    disp.begin_startup();
    let start = Instant::now();
    loop {
        let cur = st.get_run(c.run_id).unwrap().unwrap();
        if cur.status == ClaimStatus::Finished {
            assert_eq!(cur.outcome, Some(RunOutcome::WakeDelivered));
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    disp.shutdown_drain();

    // status.json is untouched by the worker: still the app's completed.
    let after: serde_json::Value = serde_json::from_slice(
        &std::fs::read(timer_folder(&e, t.id).join("status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(after["state"].as_str().unwrap(), "completed");
    assert_eq!(after["run_id"].as_str().unwrap(), c.run_id.to_string());
}
