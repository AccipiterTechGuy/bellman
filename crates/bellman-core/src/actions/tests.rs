//! Action acceptance: launch timeout kill, cancellation, retry outcomes, notify.

use super::*;
use crate::occurrence::{Occurrence, OccurrenceKind};
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
        jitter_secs: 0,
        accuracy_slack_secs: None,
        wake_machine: false,
        transport: crate::store::TransportMode::default(),
    }
}

fn executor(skip_retry_sleep: bool) -> ActionExecutor {
    ActionExecutor::with_defaults(
        ExecutorConfig {
            skip_retry_sleep,
            launch_timeout: Duration::from_secs(5),
            ..ExecutorConfig::default()
        },
        std::sync::Arc::new(ActionLimiter::new(4)),
    )
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
        cancel: None,
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
fn launch_cancel_token_kills_child() {
    let _g = test_lock();
    // SCH1 `Replace`: the token interrupts the 20 ms try_wait loop; the
    // child is killed and reaped, and the outcome is `cancelled` — distinct
    // from a timeout kill.
    let token = CancellationToken::new();
    let cfg = LaunchConfig {
        command: "sleep".into(),
        args: vec!["30".into()],
        workdir: None,
        timeout: Duration::from_secs(60),
        output_cap: 1024,
        run_id: Uuid::new_v4(),
        cancel: Some(token.clone()),
    };
    let start = std::time::Instant::now();
    let t2 = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        t2.cancel();
    });
    let out = run_launch(&cfg).expect("spawn sleep");
    let elapsed = start.elapsed();
    assert!(out.cancelled, "expected cancelled outcome: {out:?}");
    assert!(out.killed, "cancelled child must be killed+reaped");
    assert!(!out.timed_out, "a cancel is not a timeout");
    assert!(
        elapsed < Duration::from_secs(5),
        "cancel must be prompt, took {elapsed:?}"
    );
}

#[test]
fn launch_sets_bellman_run_id_env() {
    let _g = test_lock();
    let run_id = Uuid::new_v4();
    let cfg = LaunchConfig {
        command: "sh".into(),
        args: vec!["-c".into(), "printf '%s' \"$BELLMAN_RUN_ID\"".into()],
        workdir: None,
        timeout: Duration::from_secs(5),
        output_cap: 1024,
        run_id,
        cancel: None,
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
        cancel: None,
    };
    let out = run_launch(&cfg).expect("spawn true");
    assert_eq!(out.exit_code, Some(0));
}

#[test]
fn retry_then_failed_outcome() {
    let exec = executor(true);
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
    let token = CancellationToken::new();
    match exec.execute(&timer, Uuid::new_v4(), &token) {
        ExecOutcome::Failed { error, attempts } => {
            assert!(error.contains("launch exit=1"), "error={error}");
            // 1 initial + 1 retry ⇒ the final attempt index is 1.
            assert_eq!(attempts, 1);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn launch_success_delivers() {
    let exec = executor(true);
    let timer = sample_timer(
        Action::Launch {
            command: "true".into(),
            args: vec![],
            workdir: None,
        },
        RetryPolicy::default(),
    );
    let token = CancellationToken::new();
    match exec.execute(&timer, Uuid::new_v4(), &token) {
        ExecOutcome::Delivered { message, attempts } => {
            assert!(message.contains("launch ok exit=0"), "message={message}");
            assert_eq!(attempts, 0);
        }
        other => panic!("expected Delivered, got {other:?}"),
    }
}

#[test]
fn notify_stub_succeeds() {
    let exec = executor(true);
    let timer = sample_timer(
        Action::Notify {
            title: "hello".into(),
            body: "world".into(),
        },
        RetryPolicy::default(),
    );
    let token = CancellationToken::new();
    match exec.execute(&timer, Uuid::new_v4(), &token) {
        ExecOutcome::Delivered { message, .. } => {
            assert!(message.contains("notify stub"), "message={message}");
        }
        other => panic!("expected Delivered, got {other:?}"),
    }
}

#[test]
fn cancel_during_retry_backoff_stops_execution() {
    let _g = test_lock();
    // skip_retry_sleep = false so the backoff really runs; the token must
    // interrupt it before the second attempt.
    let exec = executor(false);
    let timer = sample_timer(
        Action::Launch {
            command: "false".into(),
            args: vec![],
            workdir: None,
        },
        RetryPolicy {
            max_retries: 3,
            delay_secs: 60,
        },
    );
    let token = CancellationToken::new();
    let t2 = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        t2.cancel();
    });
    let start = std::time::Instant::now();
    let outcome = exec.execute(&timer, Uuid::new_v4(), &token);
    assert!(
        matches!(outcome, ExecOutcome::Cancelled),
        "expected Cancelled, got {outcome:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "backoff must be interrupted, took {:?}",
        start.elapsed()
    );
}

#[test]
fn failed_launch_with_multibyte_output_returns_failed_not_panic() {
    let _g = test_lock();
    let exec = executor(true);
    // Print 100 euro signs then exit 1 — captured output crosses a multi-byte
    // boundary at byte 200; truncate must not panic.
    let timer = sample_timer(
        Action::Launch {
            command: "python3".into(),
            args: vec![
                "-c".into(),
                "import sys; print(chr(8364)*100); sys.exit(1)".into(),
            ],
            workdir: None,
        },
        RetryPolicy {
            max_retries: 1,
            delay_secs: 0,
        },
    );
    let token = CancellationToken::new();
    match exec.execute(&timer, Uuid::new_v4(), &token) {
        ExecOutcome::Failed { error, .. } => {
            assert!(error.contains("launch exit=1"), "error={error}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
