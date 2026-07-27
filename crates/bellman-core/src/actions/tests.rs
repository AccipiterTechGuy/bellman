//! Action acceptance: launch timeout kill, retry/FAILED path, write-slot, notify.

use super::*;
use crate::events::{read_events, EventKind, EventLog, EventLogConfig};
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::scheduler::{FireAction, FireContext, FireKind};
use crate::store::{Action, OverlapPolicy, RetryPolicy, Timer};
use chrono::Utc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use uuid::Uuid;

/// Serialize tests that assert on process timing / global PATH noise.
fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn sample_timer(action: Action, retry: RetryPolicy) -> Timer {
    let occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 60,
            anchor: Utc::now(),
        },
        "UTC",
    )
    .unwrap();
    Timer {
        id: Uuid::new_v4(),
        name: "t-action".into(),
        enabled: true,
        occurrence: occ,
        tz: "UTC".into(),
        next_fire_utc: Some(Utc::now()),
        last_fired: None,
        misfire: crate::store::MisfirePolicy::Skip,
        overlap: OverlapPolicy::Skip,
        retry,
        valid_from: None,
        valid_until: None,
        max_runs: None,
        tags: vec![],
        action,
        revision: 1,
    }
}

fn ctx<'a>(timer: &'a Timer, run_id: Uuid) -> FireContext<'a> {
    FireContext {
        timer,
        scheduled_for: Utc::now(),
        run_id,
        kind: FireKind::OnTime,
        claimed_at: Utc::now(),
    }
}

#[test]
fn launch_timeout_kills_child() {
    let _g = test_lock();
    // sleep 30s, timeout 200ms → must kill.
    let cfg = LaunchConfig {
        command: "sleep".into(),
        args: vec!["30".into()],
        workdir: None,
        timeout: Duration::from_millis(200),
        output_cap: 1024,
        run_id: Uuid::new_v4(),
    };
    let start = std::time::Instant::now();
    let out = run_launch(&cfg).expect("spawn sleep");
    let elapsed = start.elapsed();
    assert!(out.timed_out, "expected timeout, exit={:?}", out.exit_code);
    assert!(out.killed, "child must be killed");
    assert!(
        elapsed < Duration::from_secs(5),
        "kill must be prompt, took {elapsed:?}"
    );
}

#[test]
fn launch_sets_bellman_run_id_env() {
    let _g = test_lock();
    let run_id = Uuid::new_v4();
    let cfg = LaunchConfig {
        command: "sh".into(),
        args: vec![
            "-c".into(),
            "printf '%s' \"$BELLMAN_RUN_ID\"".into(),
        ],
        workdir: None,
        timeout: Duration::from_secs(5),
        output_cap: 1024,
        run_id,
    };
    let out = run_launch(&cfg).expect("spawn");
    assert!(!out.timed_out);
    assert_eq!(out.exit_code, Some(0));
    assert_eq!(out.output.trim(), run_id.to_string());
}

#[test]
fn launch_no_shell_metacharacters() {
    let _g = test_lock();
    // If shell were used, `echo hi; echo pwned` as a single arg to a binary
    // named with metacharacters wouldn't work — we pass args literally.
    // Command is the literal binary `true` with a weird arg that would expand
    // under a shell.
    let cfg = LaunchConfig {
        command: "true".into(),
        args: vec!["$(echo pwned)".into(), "; reboot".into()],
        workdir: None,
        timeout: Duration::from_secs(5),
        output_cap: 1024,
        run_id: Uuid::new_v4(),
    };
    let out = run_launch(&cfg).expect("spawn true");
    assert_eq!(out.exit_code, Some(0));
}

#[test]
fn retry_then_failed_event() {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(EventLogConfig::new(dir.path().join("logs"))).unwrap();
    let mut runner = ActionRunner::new(ActionRunnerConfig {
        skip_retry_sleep: true,
        launch_timeout: Duration::from_secs(5),
        ..Default::default()
    })
    .with_event_log(log);

    // Command that always fails.
    let timer = sample_timer(
        Action::Launch {
            command: "false".into(),
            args: vec![],
            workdir: None,
        },
        RetryPolicy {
            max_retries: 1,
            delay_secs: 30, // skipped via skip_retry_sleep
        },
    );
    let run_id = Uuid::new_v4();
    let c = ctx(&timer, run_id);
    let err = runner.on_fire(&c).expect_err("must fail after retry");
    assert!(err.contains("FAILED"), "err={err}");

    let log = runner.take_event_log().unwrap();
    let (recs, stats) = read_events(log.current_path()).unwrap();
    assert_eq!(stats.skipped, 0);
    let kinds: Vec<_> = recs.iter().map(|r| r.kind).collect();
    assert!(
        kinds.contains(&EventKind::Fired),
        "expected fired event, got {kinds:?}"
    );
    assert!(
        kinds.contains(&EventKind::WakeFailed),
        "expected wake_failed, got {kinds:?}"
    );
    let failed = recs
        .iter()
        .find(|r| r.kind == EventKind::WakeFailed)
        .unwrap();
    assert_eq!(failed.message.as_deref(), Some("FAILED"));
    assert_eq!(failed.run_id, Some(run_id));
    // 1 initial + 1 retry ⇒ count records the final attempt index (1).
    assert_eq!(failed.count, Some(1));
}

#[test]
fn launch_success_emits_wake_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(EventLogConfig::new(dir.path().join("logs"))).unwrap();
    let mut runner = ActionRunner::new(ActionRunnerConfig::default()).with_event_log(log);
    let timer = sample_timer(
        Action::Launch {
            command: "true".into(),
            args: vec![],
            workdir: None,
        },
        RetryPolicy::default(),
    );
    let c = ctx(&timer, Uuid::new_v4());
    runner.on_fire(&c).expect("true must succeed");
    let log = runner.take_event_log().unwrap();
    let (recs, _) = read_events(log.current_path()).unwrap();
    assert!(recs.iter().any(|r| r.kind == EventKind::WakeDelivered));
}

#[test]
fn notify_stub_succeeds() {
    let mut runner = ActionRunner::new(ActionRunnerConfig::default());
    let timer = sample_timer(
        Action::Notify {
            title: "hello".into(),
            body: "world".into(),
        },
        RetryPolicy::default(),
    );
    let c = ctx(&timer, Uuid::new_v4());
    runner.on_fire(&c).expect("notify stub");
    assert!(
        runner
            .last_message
            .as_deref()
            .is_some_and(|m| m.contains("notify stub")),
        "{:?}",
        runner.last_message
    );
}

#[test]
fn write_output_slot_atomic_json() {
    let dir = tempfile::tempdir().unwrap();
    let slot_dir = dir.path().join("out");
    let mut runner = ActionRunner::new(ActionRunnerConfig {
        write_slot_dir: Some(slot_dir.clone()),
        ..Default::default()
    });
    let timer = sample_timer(Action::None, RetryPolicy::default());
    let run_id = Uuid::new_v4();
    let c = ctx(&timer, run_id);
    runner.on_fire(&c).expect("write slot");
    let path = slot_dir.join(format!("run-{run_id}.json"));
    assert!(path.exists(), "missing {}", path.display());
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(body["run_id"], run_id.to_string());
    assert_eq!(body["timer_name"], "t-action");
}

#[test]
fn overlap_skip_does_not_double_launch() {
    let mut runner = ActionRunner::new(ActionRunnerConfig::default());
    // Manually mark in-flight, then fire with Skip.
    let timer = sample_timer(
        Action::Launch {
            command: "true".into(),
            args: vec![],
            workdir: None,
        },
        RetryPolicy::default(),
    );
    runner.in_flight.insert(timer.id);
    let c = ctx(&timer, Uuid::new_v4());
    runner.on_fire(&c).expect("overlap soft-ok");
    assert_eq!(
        runner.last_message.as_deref(),
        Some("overlap policy skip")
    );
}
