//! SCH1 exit-gate tests: the bounded fire dispatcher and worker lanes.
//!
//! Covered here: the card test (a 30 s-class action never delays the next
//! timer), the global cap, per-timer lane semantics for every overlap
//! policy (decided at fire commit, asserted before dequeue), full-queue
//! drain without a restart, kill-mid-action at-least-once recovery, and
//! `run_now` entering the same dispatch service (live dispatcher and
//! standalone lock acquisition).

use bellman_core::actions::{Dispatcher, DispatcherConfig, ExecutorConfig};
use bellman_core::occurrence::{Occurrence, OccurrenceKind};
use bellman_core::scheduler::{
    FireKind, Scheduler, SchedulerConfig, SystemClock,
};
use bellman_core::store::{
    Action, ClaimStatus, NewTimer, OpenOptions, OverlapPolicy, RetryPolicy, RunClaim, RunOutcome,
    Store, Timer,
};
use bellman_core::{RunNowOptions, TimersTree};
use chrono::{Duration as ChronoDuration, Utc};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// These are wall-clock integration tests with real worker threads, real
/// `sleep` children and many SQLite connections; run them one at a time so
/// a loaded host cannot starve a pump tick past a timeout (the assertions
/// themselves stay wall-clock strict).
fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap()
}

// ── Helpers ─────────────────────────────────────────────────────────────

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

fn engine(e: &Env) -> bellman_core::reply::ReplyEngine {
    bellman_core::reply::ReplyEngine {
        tree: TimersTree::new(&e.data),
        data_dir: e.data.clone(),
        pickup_grace: Duration::from_secs(60),
        watchdog_factor: 2.0,
        anchors: bellman_core::reply::new_anchors(),
        deadlines: bellman_core::reply::new_deadlines(),
        fire_slot_file: None,
    }
}

fn interval_timer(
    store: &mut Store,
    name: &str,
    action: Action,
    overlap: OverlapPolicy,
) -> Timer {
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
    t.retry = RetryPolicy {
        max_retries: 0,
        delay_secs: 0,
    };
    store.create_timer(t).unwrap()
}

/// The real fire path: R10 transaction + projections + overlap disposition.
fn fire(e: &Env, store: &mut Store, timer: &Timer) -> RunClaim {
    let eng = engine(e);
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

fn spawn_dispatcher(e: &Env, max: usize) -> Dispatcher {
    Dispatcher::spawn(DispatcherConfig {
        db_path: e.db.clone(),
        data_dir: Some(e.data.clone()),
        max_concurrent_actions: max,
        notify_sink: Arc::new(bellman_core::actions::StubNotifySink),
        executor: ExecutorConfig::default(),
        tick: Duration::from_millis(50),
    })
    .unwrap()
}

/// In-memory (lock-free) dispatcher for pure-store tests.
fn spawn_dispatcher_inmem(e: &Env, max: usize) -> Dispatcher {
    Dispatcher::spawn(DispatcherConfig {
        db_path: e.db.clone(),
        data_dir: None,
        max_concurrent_actions: max,
        notify_sink: Arc::new(bellman_core::actions::StubNotifySink),
        executor: ExecutorConfig::default(),
        tick: Duration::from_millis(50),
    })
    .unwrap()
}

fn wait_finished(store: &Store, run_id: Uuid, timeout: Duration) -> RunClaim {
    let start = Instant::now();
    loop {
        if let Some(c) = store.get_run(run_id).unwrap() {
            if c.status == ClaimStatus::Finished {
                return c;
            }
        }
        assert!(
            start.elapsed() < timeout,
            "claim {run_id} did not finish within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_status(store: &Store, run_id: Uuid, status: ClaimStatus, timeout: Duration) -> RunClaim {
    let start = Instant::now();
    loop {
        if let Some(c) = store.get_run(run_id).unwrap() {
            if c.status == status {
                return c;
            }
        }
        assert!(
            start.elapsed() < timeout,
            "claim {run_id} did not reach {status:?} within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn launch(command: &str, args: &[&str]) -> Action {
    Action::Launch {
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        workdir: None,
    }
}

// ── The card test ───────────────────────────────────────────────────────

/// A timer whose action takes seconds does not delay a timer due right
/// after — asserted on wall-clock, the second timer recorded `on_time`.
/// This fails on the pre-SCH1 synchronous loop.
#[test]
fn slow_action_does_not_delay_the_next_timer() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    // A: due now, action takes 4 s. B: due 400 ms later, instant action.
    // (next_fire = anchor + k·period strictly after last_fired, so B's
    // 400 ms offset needs its own anchor.)
    let now = Utc::now();
    let a = {
        let occ = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 3600,
                anchor: now - ChronoDuration::hours(2),
            },
            "UTC",
        )
        .unwrap();
        let mut t = NewTimer::new("slow-a", occ);
        t.action = launch("sleep", &["4"]);
        t.last_fired = Some(now - ChronoDuration::hours(1));
        st.create_timer(t).unwrap()
    };
    let b = {
        let occ = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 3600,
                anchor: now - ChronoDuration::hours(2) + ChronoDuration::milliseconds(400),
            },
            "UTC",
        )
        .unwrap();
        let mut t = NewTimer::new("fast-b", occ);
        t.action = launch("true", &[]);
        t.last_fired = Some(now - ChronoDuration::hours(1) + ChronoDuration::milliseconds(400));
        st.create_timer(t).unwrap()
    };

    let disp = spawn_dispatcher(&e, 2);
    let cfg = SchedulerConfig::default()
        .with_data_dir(e.data.clone())
        .with_max_sleep(Duration::from_millis(50));
    let mut sched = Scheduler::new(store(&e), SystemClock::new(), disp.clone(), cfg);

    // A is already due: boot's misfire pass fires it. The scheduler thread
    // must return immediately even though the action runs 4 s.
    let t0 = Instant::now();
    let boot_fires = sched.boot().unwrap();
    let boot_elapsed = t0.elapsed();
    assert!(
        boot_elapsed < Duration::from_secs(2),
        "scheduler loop blocked on the action: {boot_elapsed:?}"
    );
    let fire_a = boot_fires
        .iter()
        .find(|f| f.timer_id == a.id)
        .expect("A fired at boot")
        .clone();

    // B becomes due while A's action is still running.
    std::thread::sleep(Duration::from_millis(500));
    let t1 = Instant::now();
    let r2 = sched.tick().unwrap();
    assert!(
        t1.elapsed() < Duration::from_secs(2),
        "the tick firing B blocked on A's action: {:?}",
        t1.elapsed()
    );
    let fire_b = r2
        .fires
        .iter()
        .find(|f| f.timer_id == b.id)
        .expect("B fired while A runs");
    assert!(
        matches!(fire_b.kind, FireKind::OnTime),
        "B must be recorded on_time, not Late: {:?}",
        fire_b.kind
    );

    // B's action finishes while A's is still in its lane.
    let store2 = store(&e);
    let claim_b = wait_finished(&store2, fire_b.run_id, Duration::from_secs(5));
    assert_eq!(claim_b.outcome, Some(RunOutcome::WakeDelivered));
    let claim_a_now = store2.get_run(fire_a.run_id).unwrap().unwrap();
    assert_eq!(
        claim_a_now.status,
        ClaimStatus::Active,
        "A must still be executing when B completed (parallel lanes)"
    );

    // A completes truthfully as well.
    let claim_a = wait_finished(&store2, fire_a.run_id, Duration::from_secs(10));
    assert_eq!(claim_a.outcome, Some(RunOutcome::WakeDelivered));
    sched.action().shutdown_drain();
}

// ── Global cap ──────────────────────────────────────────────────────────

/// Peak in-flight actually reaches the cap under a mass-fire, and never
/// exceeds it (LimiterStats already reports this).
#[test]
fn mass_fire_peak_reaches_cap_never_exceeds() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    let mut claims = Vec::new();
    for i in 0..12 {
        let t = interval_timer(
            &mut st,
            &format!("mass-{i}"),
            launch("sh", &["-c", "sleep 0.4"]),
            OverlapPolicy::Skip,
        );
        claims.push(st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap());
    }
    let disp = spawn_dispatcher_inmem(&e, 4);
    disp.begin_startup();
    for c in &claims {
        wait_finished(&st, c.run_id, Duration::from_secs(30));
    }
    let stats = disp.limiter().stats();
    assert_eq!(stats.peak_in_flight, 4, "peak must reach the cap");
    assert!(stats.peak_in_flight <= 4);
    assert_eq!(stats.completed, 12);
    disp.shutdown_drain();
}

/// Different timers run in parallel up to the cap; a full queue drains
/// without a restart (the pending claim is dispatched by the pump).
#[test]
fn full_queue_drains_without_restart() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    let mut claims = Vec::new();
    // 2 workers → queue holds 4 hints; 7 claims force backpressure.
    for i in 0..7 {
        let t = interval_timer(
            &mut st,
            &format!("q-{i}"),
            launch("sh", &["-c", "sleep 0.3"]),
            OverlapPolicy::Skip,
        );
        claims.push(st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap());
    }
    let disp = spawn_dispatcher_inmem(&e, 2);
    disp.begin_startup();
    for c in &claims {
        let c = wait_finished(&st, c.run_id, Duration::from_secs(30));
        assert_eq!(c.outcome, Some(RunOutcome::WakeDelivered));
    }
    assert_eq!(disp.limiter().stats().completed, 7);
    disp.shutdown_drain();
}

// ── Overlap: decided at fire commit, asserted before any dequeue ────────

/// Race two fires past a HELD dispatcher (primed, pump never started):
/// every policy's outcome matches the state at each fire commit.
#[test]
fn overlap_decisions_happen_at_fire_commit_not_dequeue() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    let _held = spawn_dispatcher(&e, 2); // never begin_startup: queue held.

    // Skip: an unfinished older claim skips the new one at commit.
    let t_skip = interval_timer(&mut st, "skip", launch("sleep", &["30"]), OverlapPolicy::Skip);
    let c1 = fire(&e, &mut st, &t_skip);
    assert_eq!(c1.status, ClaimStatus::Pending);
    let c2 = fire(&e, &mut st, &t_skip);
    let c2 = st.get_run(c2.run_id).unwrap().unwrap();
    assert_eq!(c2.status, ClaimStatus::Finished);
    assert_eq!(c2.outcome, Some(RunOutcome::SkippedMisfire));
    assert_eq!(c2.outcome_reason.as_deref(), Some("overlap_skip"));

    // QueueOne: one follow-up admitted; the third firing skips.
    let t_q = interval_timer(&mut st, "queue", launch("sleep", &["30"]), OverlapPolicy::QueueOne);
    let q1 = fire(&e, &mut st, &t_q);
    let q2 = fire(&e, &mut st, &t_q);
    assert_eq!(q2.status, ClaimStatus::Pending, "one follow-up admitted");
    let q3 = fire(&e, &mut st, &t_q);
    let q3 = st.get_run(q3.run_id).unwrap().unwrap();
    assert_eq!(q3.outcome, Some(RunOutcome::SkippedMisfire));
    assert_eq!(q3.outcome_reason.as_deref(), Some("overlap_queue_full"));
    // The skipped third firing must not replace the queued follow-up.
    assert_eq!(
        st.get_run(q2.run_id).unwrap().unwrap().status,
        ClaimStatus::Pending
    );
    assert_eq!(
        st.get_run(q1.run_id).unwrap().unwrap().status,
        ClaimStatus::Pending
    );

    // Parallel { cap: 1 }: the second concurrent claim skips.
    let t_p = interval_timer(
        &mut st,
        "par",
        launch("sleep", &["30"]),
        OverlapPolicy::Parallel { cap: 1 },
    );
    let _p1 = fire(&e, &mut st, &t_p);
    let p2 = fire(&e, &mut st, &t_p);
    let p2 = st.get_run(p2.run_id).unwrap().unwrap();
    assert_eq!(p2.outcome, Some(RunOutcome::SkippedMisfire));
    assert_eq!(p2.outcome_reason.as_deref(), Some("overlap_parallel_cap"));

    // Parallel { cap: 0 } admits none.
    let t_p0 = interval_timer(
        &mut st,
        "par0",
        launch("true", &[]),
        OverlapPolicy::Parallel { cap: 0 },
    );
    let p0 = fire(&e, &mut st, &t_p0);
    let p0 = st.get_run(p0.run_id).unwrap().unwrap();
    assert_eq!(p0.outcome, Some(RunOutcome::SkippedMisfire));
    assert_eq!(p0.outcome_reason.as_deref(), Some("overlap_parallel_cap"));

    // Replace vs a PENDING predecessor: finished before it ever started.
    let t_r = interval_timer(&mut st, "rep", launch("sleep", &["30"]), OverlapPolicy::Replace);
    let r1 = fire(&e, &mut st, &t_r);
    let r2 = fire(&e, &mut st, &t_r);
    let r1 = st.get_run(r1.run_id).unwrap().unwrap();
    assert_eq!(r1.status, ClaimStatus::Finished);
    assert_eq!(r1.outcome, Some(RunOutcome::WakeFailed));
    assert_eq!(
        r1.outcome_reason.as_deref(),
        Some("overlap_replace_before_start"),
        "a pending predecessor is finished before start, never mislabeled delivered"
    );
    assert_eq!(r2.status, ClaimStatus::Pending);

    // Replace vs an ACTIVE predecessor: durable cancel request; the newest
    // claim stays pending (eligible only after the predecessor finishes).
    let t_r2 = interval_timer(&mut st, "rep2", launch("sleep", &["30"]), OverlapPolicy::Replace);
    let a1 = fire(&e, &mut st, &t_r2);
    st.activate_run(a1.run_id).unwrap(); // simulate a worker holding the lane
    let a2 = fire(&e, &mut st, &t_r2);
    let a1 = st.get_run(a1.run_id).unwrap().unwrap();
    assert_eq!(a1.status, ClaimStatus::Active);
    assert!(a1.cancel_requested, "Replace marks the active predecessor");
    assert_eq!(a2.status, ClaimStatus::Pending);
}

// ── Lane execution semantics ────────────────────────────────────────────

/// Two fires of the same serial-policy timer execute in order, never
/// concurrently; the retained QueueOne follow-up executes exactly once.
#[test]
fn serial_lane_executes_in_order_never_concurrent() {
    let _g = test_lock();
    let e = env();
    let marker = e.data.join("order.log");
    let mut st = store(&e);
    let cmd = format!(
        "echo S:$BELLMAN_RUN_ID >> '{}'; sleep 0.4; echo E:$BELLMAN_RUN_ID >> '{}'",
        marker.display(),
        marker.display()
    );
    let t = interval_timer(&mut st, "serial", launch("sh", &["-c", &cmd]), OverlapPolicy::QueueOne);
    let c1 = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(2)).unwrap();
    let c2 = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap();

    let disp = spawn_dispatcher_inmem(&e, 4);
    disp.begin_startup();
    let f1 = wait_finished(&st, c1.run_id, Duration::from_secs(15));
    let f2 = wait_finished(&st, c2.run_id, Duration::from_secs(15));
    assert_eq!(f1.outcome, Some(RunOutcome::WakeDelivered));
    assert_eq!(f2.outcome, Some(RunOutcome::WakeDelivered));
    disp.shutdown_drain();

    let log = std::fs::read_to_string(&marker).unwrap();
    let expect = format!("S:{}\nE:{}\nS:{}\nE:{}\n", c1.run_id, c1.run_id, c2.run_id, c2.run_id);
    assert_eq!(log, expect, "serial lane must run oldest first, never overlap");
}

/// A `Parallel { cap: 2 }` timer runs two actions concurrently (asserted),
/// never three (asserted), inside the global cap.
#[test]
fn parallel_cap_two_concurrent_never_three() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    let t = interval_timer(
        &mut st,
        "par2",
        launch("sh", &["-c", "sleep 0.6"]),
        OverlapPolicy::Parallel { cap: 2 },
    );
    let c1 = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(3)).unwrap();
    let c2 = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(2)).unwrap();
    let c3 = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap();

    let disp = spawn_dispatcher_inmem(&e, 8);
    disp.begin_startup();

    // Two become active at once (concurrency asserted); the third stays pending.
    wait_status(&st, c1.run_id, ClaimStatus::Active, Duration::from_secs(10));
    wait_status(&st, c2.run_id, ClaimStatus::Active, Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        st.get_run(c3.run_id).unwrap().unwrap().status,
        ClaimStatus::Pending,
        "the third claim must wait for a lane"
    );

    let f1 = wait_finished(&st, c1.run_id, Duration::from_secs(15));
    let f3 = wait_finished(&st, c3.run_id, Duration::from_secs(15));
    wait_finished(&st, c2.run_id, Duration::from_secs(15));
    // The third started only after a predecessor finished.
    let c3_completed = f3.completed_at.unwrap();
    assert!(
        c3_completed >= f1.completed_at.unwrap()
            || c3_completed >= st.get_run(c2.run_id).unwrap().unwrap().completed_at.unwrap(),
        "cap 2 means the third runs only after a lane frees"
    );
    assert!(disp.limiter().stats().peak_in_flight <= 8);
    disp.shutdown_drain();
}

/// `Replace` interrupts a running launch: the first firing is truthfully
/// `wake_failed(overlap_replace)` — never `wake_delivered` — and the
/// replacement starts only after the predecessor stopped.
#[test]
fn replace_interrupts_running_action_then_runs_new() {
    let _g = test_lock();
    let e = env();
    let started = e.data.join("started.log");
    let cmd = format!("echo $BELLMAN_RUN_ID >> '{}'; sleep 5", started.display());
    let mut st = store(&e);
    let t = interval_timer(&mut st, "repl", launch("sh", &["-c", &cmd]), OverlapPolicy::Replace);

    let disp = spawn_dispatcher(&e, 2);
    disp.begin_startup();

    let c1 = fire(&e, &mut st, &t);
    wait_status(&st, c1.run_id, ClaimStatus::Active, Duration::from_secs(10));
    // Let the child actually spawn before the replacement fires.
    std::thread::sleep(Duration::from_millis(300));

    let t0 = Instant::now();
    let c2 = fire(&e, &mut st, &t);
    assert_eq!(c2.status, ClaimStatus::Pending);
    disp.submit(c2.run_id);

    let f1 = wait_finished(&st, c1.run_id, Duration::from_secs(15));
    assert_eq!(f1.outcome, Some(RunOutcome::WakeFailed));
    assert_eq!(
        f1.outcome_reason.as_deref(),
        Some("overlap_replace"),
        "an interrupted action is wake_failed(overlap_replace), got {:?}",
        f1.outcome_reason
    );
    assert!(
        t0.elapsed() < Duration::from_secs(4),
        "cancellation must interrupt the 5 s launch promptly: {:?}",
        t0.elapsed()
    );

    let f2 = wait_finished(&st, c2.run_id, Duration::from_secs(20));
    assert_eq!(f2.outcome, Some(RunOutcome::WakeDelivered));
    // Never overlapped: the replacement completed after the predecessor.
    assert!(f2.completed_at.unwrap() >= f1.completed_at.unwrap());
    disp.shutdown_drain();

    let log = std::fs::read_to_string(&started).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec![c1.run_id.to_string(), c2.run_id.to_string()],
        "both actions started, in order"
    );
}

// ── Crash recovery (at-least-once, never exactly-once) ──────────────────

/// Kill mid-action: restart re-queues the unfinished claim with the SAME
/// run_id; a `finished` claim never executes again.
#[test]
fn restart_requeues_unfinished_same_run_id_finished_never_reruns() {
    let _g = test_lock();
    let e = env();
    let marker = e.data.join("runs.log");
    let cmd = format!("echo $BELLMAN_RUN_ID >> '{}'", marker.display());
    let mut st = store(&e);
    let t = interval_timer(&mut st, "crash", launch("sh", &["-c", &cmd]), OverlapPolicy::Skip);

    // Simulate a crashed dispatcher: a claim left `active` with no worker.
    let crashed = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(2)).unwrap();
    st.activate_run(crashed.run_id).unwrap();
    // And one already finished — it must never run again.
    let done = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap();
    st.activate_run(done.run_id).unwrap();
    st.complete_run(done.run_id).unwrap();

    // New process: acquires the dispatcher lock, returns active → pending,
    // re-queues the same run_id.
    let disp = spawn_dispatcher(&e, 2);
    assert!(disp.owns_lock());
    disp.begin_startup();
    let f = wait_finished(&st, crashed.run_id, Duration::from_secs(15));
    assert_eq!(f.run_id, crashed.run_id, "same run_id is re-queued");
    assert_eq!(f.outcome, Some(RunOutcome::WakeDelivered));

    std::thread::sleep(Duration::from_millis(300));
    let log = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec![crashed.run_id.to_string()],
        "the unfinished claim ran once; the finished claim never re-ran"
    );
    disp.shutdown_drain();
}

/// At-least-once across a crash: an `active` claim re-queued while its first
/// executor is still running may execute the action twice — the claim still
/// finishes exactly once. The boundary is explicit, never exactly-once.
#[test]
fn active_claim_requeued_may_execute_twice_finishes_once() {
    let _g = test_lock();
    let e = env();
    let marker = e.data.join("twice.log");
    let cmd = format!("echo $BELLMAN_RUN_ID >> '{}'; sleep 2", marker.display());
    let mut st = store(&e);
    let t = interval_timer(&mut st, "twice", launch("sh", &["-c", &cmd]), OverlapPolicy::Parallel { cap: 2 });
    let claim = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap();

    // First executor (lock-free in-memory dispatcher).
    let a = spawn_dispatcher_inmem(&e, 2);
    a.begin_startup();
    wait_status(&st, claim.run_id, ClaimStatus::Active, Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(200));

    // "Crash" recovery while the first run is still executing: the lock
    // holder returns active → pending and a second dispatcher re-queues it.
    st.repend_all_active().unwrap();
    let b = spawn_dispatcher_inmem(&e, 2);
    b.begin_startup();

    let f = wait_finished(&st, claim.run_id, Duration::from_secs(20));
    assert_eq!(f.outcome, Some(RunOutcome::WakeDelivered));
    // Wait out the first executor's 2 s sleep, then count side effects.
    std::thread::sleep(Duration::from_secs(3));
    let log = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(
        log.lines().count(),
        2,
        "at-least-once: the side effect may happen twice, the claim finished once"
    );
    assert_eq!(f.status, ClaimStatus::Finished);
    b.shutdown_drain();
}

/// A follower dispatcher (lost the OS lock) must stay an idle follower:
/// `begin_startup` does not start its pump, and `pump_once` guards on lock
/// ownership — so two processes can never execute claims of one serial
/// timer concurrently. Regression test for the dual-process QueueOne race.
#[test]
fn follower_dispatcher_never_pumps_beside_the_owner() {
    let _g = test_lock();
    let e = env();
    let marker = e.data.join("follower.log");
    let cmd = format!(
        "echo S:$BELLMAN_RUN_ID >> '{}'; sleep 0.4; echo E:$BELLMAN_RUN_ID >> '{}'",
        marker.display(),
        marker.display()
    );
    let mut st = store(&e);
    let t = interval_timer(&mut st, "lane", launch("sh", &["-c", &cmd]), OverlapPolicy::QueueOne);
    let c1 = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(2)).unwrap();
    let c2 = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap();

    let owner = spawn_dispatcher(&e, 4);
    assert!(owner.owns_lock());
    let follower = spawn_dispatcher(&e, 4);
    assert!(
        !follower.owns_lock(),
        "second dispatcher on one data dir must be a follower"
    );
    // Both go through the same boot path.
    owner.begin_startup();
    follower.begin_startup();
    // The follower even gets nudged directly — it must not pump.
    follower.submit(c1.run_id);
    follower.submit(c2.run_id);

    let f1 = wait_finished(&st, c1.run_id, Duration::from_secs(20));
    let f2 = wait_finished(&st, c2.run_id, Duration::from_secs(20));
    assert_eq!(f1.outcome, Some(RunOutcome::WakeDelivered));
    assert_eq!(f2.outcome, Some(RunOutcome::WakeDelivered));

    let log = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(
        log,
        format!("S:{}\nE:{}\nS:{}\nE:{}\n", c1.run_id, c1.run_id, c2.run_id, c2.run_id),
        "serial lane holds even with a second process present: {log:?}"
    );
    owner.shutdown_drain();

    // After the owner releases the lock, the follower's tick acquires it and
    // the normal recovery continues pending claims there.
    let c3 = st.claim_run(t.id, Utc::now()).unwrap();
    let f3 = wait_finished(&st, c3.run_id, Duration::from_secs(20));
    assert_eq!(f3.outcome, Some(RunOutcome::WakeDelivered));
    assert!(
        follower.owns_lock(),
        "follower acquires the lock once the owner is gone"
    );
    follower.shutdown_drain();
}

// ── Shutdown ────────────────────────────────────────────────────────────

/// Shutdown with lanes busy drains: in-flight lanes finish, the claim
/// commits, and pending outbox rows are synced through the R11 publisher
/// before exit — nothing is truncated.
#[test]
fn shutdown_drains_busy_lanes_and_outbox() {
    let _g = test_lock();
    let e = env();
    let marker = e.data.join("drain.log");
    let cmd = format!("echo done >> '{}'", marker.display());
    let mut st = store(&e);
    let t = interval_timer(&mut st, "drain", launch("sh", &["-c", &cmd]), OverlapPolicy::Skip);
    let c = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap();

    let disp = spawn_dispatcher(&e, 2);
    disp.begin_startup();
    // Generous activation bound: under full-suite load the pump tick and the
    // worker's store open contend with many parallel test processes.
    wait_status(&st, c.run_id, ClaimStatus::Active, Duration::from_secs(30));

    let t0 = Instant::now();
    disp.shutdown_drain();
    assert!(
        t0.elapsed() < Duration::from_secs(15),
        "drain waited for the lane, bounded by the action: {:?}",
        t0.elapsed()
    );
    let f = st.get_run(c.run_id).unwrap().unwrap();
    assert_eq!(f.status, ClaimStatus::Finished);
    assert_eq!(f.outcome, Some(RunOutcome::WakeDelivered));
    assert!(marker.exists(), "the in-flight action completed during drain");
    // The outbox row was published (drained), not truncated.
    assert_eq!(
        st.count_pending_events().unwrap(),
        0,
        "outbox drained on shutdown"
    );
    let log = e.data.join("logs").join("events.current.jsonl");
    let content = std::fs::read_to_string(log).unwrap();
    assert!(
        content.contains("wake_delivered"),
        "the result event landed in the JSONL log: {content}"
    );
}

/// The OTHER SQLite commit order: the worker commits success BEFORE the
/// `Replace` fire transaction — there is nothing left to cancel, so the
/// predecessor keeps its truthful `wake_delivered` and the replacement
/// still runs (after it, never overlapping).
#[test]
fn replace_after_worker_committed_success_keeps_truthful_wake_delivered() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    let t = interval_timer(&mut st, "replwin", launch("true", &[]), OverlapPolicy::Replace);

    let disp = spawn_dispatcher(&e, 2);
    disp.begin_startup();

    let c1 = fire(&e, &mut st, &t);
    let f1 = wait_finished(&st, c1.run_id, Duration::from_secs(15));
    assert_eq!(f1.outcome, Some(RunOutcome::WakeDelivered));

    // The Replace fire commits only now — after the worker's success commit.
    let c2 = fire(&e, &mut st, &t);
    disp.submit(c2.run_id);
    let f2 = wait_finished(&st, c2.run_id, Duration::from_secs(15));
    assert_eq!(f2.outcome, Some(RunOutcome::WakeDelivered));

    // The predecessor is untouched: no cancel request, no replace reason,
    // no overlap_replace_before_start event for it.
    let c1_after = st.get_run(c1.run_id).unwrap().unwrap();
    assert_eq!(c1_after.outcome, Some(RunOutcome::WakeDelivered));
    assert!(!c1_after.cancel_requested);
    assert!(
        !c1_after
            .outcome_reason
            .as_deref()
            .unwrap_or("")
            .contains("overlap_replace"),
        "predecessor reason must not be rewritten: {:?}",
        c1_after.outcome_reason
    );
    let events = st.pending_events(64).unwrap();
    assert!(
        !events.iter().any(|(_, p)| p.contains("overlap_replace")
            && p.contains(&c1.run_id.to_string())),
        "no replace event may name the already-delivered predecessor"
    );

    // Never overlapped: the replacement finished strictly after.
    assert!(f2.completed_at.unwrap() >= f1.completed_at.unwrap());
    disp.shutdown_drain();
}

// ── Crash windows ───────────────────────────────────────────────────────

/// Crash after the completion transaction but before JSONL publication:
/// the claim is already `finished` with its R11 outbox row committed, so a
/// restart drains the event to the log without rerunning the action.
#[test]
fn crash_after_completion_tx_drains_event_without_rerunning_action() {
    let _g = test_lock();
    let e = env();
    let marker = e.data.join("crashwin.log");
    let cmd = format!("echo $BELLMAN_RUN_ID >> '{}'", marker.display());
    let mut st = store(&e);
    let t = interval_timer(&mut st, "crashwin", launch("sh", &["-c", &cmd]), OverlapPolicy::Skip);
    let c = st.claim_run(t.id, Utc::now() - ChronoDuration::minutes(1)).unwrap();

    let disp = spawn_dispatcher_inmem(&e, 1);
    disp.begin_startup();
    let f = wait_finished(&st, c.run_id, Duration::from_secs(15));
    assert_eq!(f.outcome, Some(RunOutcome::WakeDelivered));
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().lines().count(),
        1,
        "action ran exactly once before the crash"
    );
    assert_eq!(
        st.count_pending_events().unwrap(),
        1,
        "the result event sits unpublished in the outbox at the crash window"
    );

    // CRASH: the process dies here — no shutdown drain, no publish.
    drop(disp); // (threads keep the leaked inner alive; nothing drains)

    // RESTART: the elected publisher drains the outbox row; a fresh
    // dispatcher must never re-queue the finished claim.
    let st2 = store(&e);
    let mut publisher = bellman_core::events::EventPublisher::with_config(
        bellman_core::events::EventLogConfig::new(e.data.join("logs")),
    )
    .unwrap();
    publisher.publish_cycle(&st2);
    assert_eq!(st2.count_pending_events().unwrap(), 0, "outbox drained");
    let content = std::fs::read_to_string(e.data.join("logs").join("events.current.jsonl")).unwrap();
    let hits: Vec<&str> = content
        .lines()
        .filter(|l| l.contains("wake_delivered") && l.contains(&c.run_id.to_string()))
        .collect();
    assert_eq!(hits.len(), 1, "the event reached the log exactly once");

    let disp2 = spawn_dispatcher_inmem(&e, 1);
    disp2.begin_startup();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().lines().count(),
        1,
        "a finished claim is never re-run after the restart"
    );
    disp2.shutdown_drain();
}

// ── run_now through the same dispatch service ───────────────────────────

/// Standalone path: no process owns the dispatcher lock — `run_now` spins up
/// the bounded dispatcher, executes the action, and returns its durable
/// result. No second ActionRunner, no slot overlay.
#[test]
fn run_now_standalone_executes_and_returns_durable_result() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    let marker = e.data.join("runnow.log");
    let cmd = format!("echo ran >> '{}'", marker.display());
    let t = interval_timer(&mut st, "rn", launch("sh", &["-c", &cmd]), OverlapPolicy::Skip);

    let outcome = bellman_core::run_now(&mut st, &e.db, t.id, &RunNowOptions::default()).unwrap();
    assert!(
        outcome.message.contains("launch ok"),
        "message={}",
        outcome.message
    );
    let claim = st.get_run(outcome.run_id).unwrap().unwrap();
    assert_eq!(claim.status, ClaimStatus::Finished);
    assert_eq!(claim.outcome, Some(RunOutcome::WakeDelivered));
    assert!(marker.exists(), "the action really ran");
    // next_fire advanced at fire time.
    let fresh = st.get_timer(t.id).unwrap().unwrap();
    assert_eq!(fresh.last_fired, Some(outcome.scheduled_for));
    // No `slots/done` overlay is ever written by run-now.
    assert!(
        !e.data.join("slots").join("done").exists(),
        "slots/done is SlotService-only; run-now writes no overlay"
    );
}

/// GUI-style path: the caller passes the live dispatcher; `run_now` submits
/// the claim and waits for its durable result while the dispatcher serves
/// other timers.
#[test]
fn run_now_with_live_dispatcher_waits_for_the_claim_result() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    let marker = e.data.join("live.log");
    let cmd = format!("echo ran >> '{}'", marker.display());
    let t = interval_timer(&mut st, "rn-live", launch("sh", &["-c", &cmd]), OverlapPolicy::Skip);

    let disp = spawn_dispatcher(&e, 2);
    assert!(disp.owns_lock());
    disp.begin_startup();
    let opts = RunNowOptions {
        dispatcher: Some(disp.clone()),
        ..RunNowOptions::default()
    };
    let outcome = bellman_core::run_now(&mut st, &e.db, t.id, &opts).unwrap();
    assert!(outcome.message.contains("launch ok"), "{}", outcome.message);
    assert!(marker.exists());
    disp.shutdown_drain();
}

/// A manual fire obeys the configured overlap lane: the second `run_now`
/// while the first action runs is an overlap skip, decided at commit.
#[test]
fn run_now_second_fire_obeys_overlap_lane() {
    let _g = test_lock();
    let e = env();
    let mut st = store(&e);
    let t = interval_timer(&mut st, "rn-skip", launch("sleep", &["3"]), OverlapPolicy::Skip);

    let disp = spawn_dispatcher(&e, 2);
    disp.begin_startup();
    let opts = RunNowOptions {
        dispatcher: Some(disp.clone()),
        ..RunNowOptions::default()
    };

    // First fire in the background (it blocks on the 3 s action).
    let db = e.db.clone();
    let opts2 = opts.clone();
    let first = std::thread::spawn(move || {
        let mut st = Store::open_with(
            &db,
            OpenOptions {
                refuse_network_fs: false,
                ..OpenOptions::default()
            },
        )
        .unwrap();
        bellman_core::run_now(&mut st, &db, t.id, &opts2)
    });
    std::thread::sleep(Duration::from_millis(500));

    // Second fire: publication is immediate, the action is skipped at commit.
    let t0 = Instant::now();
    let outcome2 = bellman_core::run_now(&mut st, &e.db, t.id, &opts).unwrap();
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "the second run_now must not wait for the first action"
    );
    assert!(
        outcome2.message.contains("overlap_skip"),
        "message={}",
        outcome2.message
    );
    let claim2 = st.get_run(outcome2.run_id).unwrap().unwrap();
    assert_eq!(claim2.outcome, Some(RunOutcome::SkippedMisfire));

    let first = first.join().unwrap().unwrap();
    let claim1 = st.get_run(first.run_id).unwrap().unwrap();
    assert_eq!(claim1.outcome, Some(RunOutcome::WakeDelivered));
    disp.shutdown_drain();
}
