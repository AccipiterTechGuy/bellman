//! `run_now` service: pre-fire barrier → claim/commit → publish → dispatch.
//!
//! A manual fire is a different PRODUCER of the same claim, not a second
//! executor (SCH1): it keeps today's `FireKind::OnTime`, enters the R10 fire
//! transaction exactly like a scheduled fire, publishes immediately, and its
//! action obeys the configured overlap lane — executed by a dispatcher
//! worker, never inline here.
//!
//! - Inside the scheduler process, the caller passes the live [`Dispatcher`]
//!   and this waits for that claim's durable result while the dispatcher
//!   keeps serving other timers.
//! - A standalone CLI never executes the action beside a live GUI
//!   dispatcher. When no dispatcher owns the OS lock it may acquire it, run
//!   the same bounded dispatcher until its claim finishes, then release it;
//!   otherwise it relies on the owner's periodic DB pump and polls for the
//!   claim result.

use crate::actions::{Dispatcher, DispatcherConfig, ExecutorConfig, NotifySink};
use crate::scheduler::FireKind;
use crate::store::{
    ClaimStatus, OpenOptions, RunOutcome, SlotRequestRecord, Store, StoreError, Timer, TimerPatch,
    TimerUpdate,
};
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Caller-controlled knobs for [`run_now`].
#[derive(Default)]
pub struct RunNowOptions {
    /// When `Some(true)`, skip the retry sleep (test path).
    pub skip_retry_sleep: bool,
    /// Notification sink for the executor that may run in this process.
    /// `None` keeps the stub.
    pub notify_sink: Option<Arc<dyn NotifySink>>,
    /// IK3 duration anchors shared with the reply watcher. `None` starts a
    /// fresh registry (CLI one-shots): the terminal event then falls back to
    /// the wall-clock duration, marked `duration_source: "wall_clock"`.
    pub anchors: Option<crate::reply::SharedAnchors>,
    /// IK3 monotonic deadline book shared with the reply watcher. `None`
    /// starts a fresh book (CLI one-shots; the GUI watcher lazily
    /// reconstructs from the persisted wall deadlines).
    pub deadlines: Option<crate::reply::SharedDeadlines>,
    /// The live in-process dispatcher (GUI / scheduler process). The claim
    /// is submitted to it and the wait rides its lanes. Standalone callers
    /// leave `None`: a bounded dispatcher is spun up if the OS lock is free,
    /// otherwise the owning process's pump picks the claim up (≤ 1 s).
    pub dispatcher: Option<Dispatcher>,
}

impl std::fmt::Debug for RunNowOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunNowOptions")
            .field("skip_retry_sleep", &self.skip_retry_sleep)
            .field(
                "notify_sink",
                &self
                    .notify_sink
                    .as_ref()
                    .map(|s| std::any::type_name_of_val(s.as_ref())),
            )
            .field("dispatcher", &self.dispatcher.as_ref().map(|d| d.owns_lock()))
            .finish()
    }
}

impl Clone for RunNowOptions {
    fn clone(&self) -> Self {
        Self {
            skip_retry_sleep: self.skip_retry_sleep,
            notify_sink: self.notify_sink.clone(),
            anchors: self.anchors.clone(),
            deadlines: self.deadlines.clone(),
            dispatcher: self.dispatcher.clone(),
        }
    }
}

/// Result of [`run_now`].
#[derive(Debug, Clone)]
pub struct RunNowOutcome {
    pub timer: Timer,
    pub run_id: Uuid,
    pub scheduled_for: DateTime<Utc>,
    pub message: String,
}

/// Resolve a slot-id → file mapping for a given timer (if the timer was
/// created via the slot layer). `None` when the timer has no owning slot.
pub fn slot_record_for_timer(store: &Store, timer_id: Uuid) -> Result<Option<SlotRequestRecord>, StoreError> {
    store.latest_slot_request_for_timer(timer_id)
}

/// Run the timer action immediately through the real fire path.
///
/// Steps (the one `pre-fire barrier → claim/commit → publish → dispatch`
/// service, shared with scheduled delivery):
/// 1. the R10 fire transaction (`project_fire`): gate → barrier → one commit
///    (supersede + claim + SCH1 overlap disposition + lifecycle row + `fired`
///    event + transport projection) → `status.json` / reply-stub projections
///    → immediate fire-notification attempt
/// 2. advance `last_fired` + `record_run` so `next_fire` moves past this slot
///    — at fire time, never behind the action
/// 3. submit the claim to the dispatcher and wait for its durable result
///    (assembled into `message` — not shared scheduler state)
/// 4. best-effort outbox drain (a CLI-only run must not wait for a GUI tick)
pub fn run_now(
    store: &mut Store,
    db_path: &Path,
    timer_id: Uuid,
    opts: &RunNowOptions,
) -> Result<RunNowOutcome, RunNowError> {
    let timer = store
        .get_timer(timer_id)
        .map_err(RunNowError::Store)?
        .ok_or(RunNowError::NotFound { timer_id })?;
    let scheduled_for = Utc::now();

    // Honor config.json knobs when present (view writers opened ad-hoc by
    // the CLI otherwise run with product defaults).
    let app_cfg = db_path
        .parent()
        .map(|d| crate::app_config::AppConfig::load(d).unwrap_or_default())
        .unwrap_or_default();

    // 1. The R10 fire transaction: required gate, pre-fire barrier, one
    //    atomic commit, then projections. The gate releases before any wait
    //    (a long action never blocks the reply watcher).
    let engine = crate::reply::ReplyEngine {
        tree: crate::tree::TimersTree::new(db_path.parent().unwrap_or(Path::new("."))),
        data_dir: db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
        pickup_grace: app_cfg.pickup_grace(),
        watchdog_factor: app_cfg.watchdog_factor,
        anchors: opts
            .anchors
            .clone()
            .unwrap_or_else(crate::reply::new_anchors),
        deadlines: opts
            .deadlines
            .clone()
            .unwrap_or_else(crate::reply::new_deadlines),
        fire_slot_file: None,
        status_listener: None,
        ipc: None,
    };
    let claim = crate::tree::project_fire(
        &engine.tree.clone(),
        store,
        &timer,
        scheduled_for,
        &FireKind::OnTime,
        &engine,
        Utc::now(),
    )
    .map_err(|e| RunNowError::Other(format!("fire transaction: {e}")))?;

    // 2. Advance last_fired + record_run so next_fire moves past this slot —
    //    same bookkeeping as the scheduler's `mark_fired` path, at fire time.
    let mut occ = timer.occurrence.clone();
    occ.record_run();
    let timer = store
        .update_timer(TimerUpdate {
            id: timer.id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                last_fired: Some(Some(scheduled_for)),
                occurrence: Some(occ),
                ..Default::default()
            },
        })
        .map_err(RunNowError::Store)?;

    // IK2: refresh timer.json with the advanced next_fire. status.json stays
    // the firing snapshot: the R5 `completed` state is an app report (IK3),
    // and the claim ledger's outcome is delivery bookkeeping.
    if let Some(data_dir) = db_path.parent() {
        let tree = crate::tree::TimersTree::new(data_dir);
        let owner = store.get_timer_owner(timer.id).ok().flatten();
        if let Err(e) = tree.sync_timer_json(&timer, owner.as_deref()) {
            eprintln!("bellman: timer.json refresh failed: {e}");
        }
    }

    // 3. Dispatch + wait for the durable result. A claim already `finished`
    //    was skipped by the overlap policy at fire commit — nothing to run.
    let message = if claim.status == ClaimStatus::Finished {
        claim
            .outcome_reason
            .clone()
            .unwrap_or_else(|| "overlap policy skip".into())
    } else {
        dispatch_and_wait(store, db_path, &timer, &claim, opts)?
    };

    // 4. Best-effort outbox drain: a one-shot CLI run cannot rely on a GUI
    //    publisher tick — if this process can take the lease, it publishes
    //    its own rows (fdatasync + mark) before exiting.
    if let Some(data_dir) = db_path.parent() {
        crate::events::EventPublisher::drain_best_effort(data_dir, store);
    }

    // A wake-failed action surfaces as an error, as before (the failure is
    // honest in the claim ledger and the wake_failed event).
    let final_claim = store.get_run(claim.run_id).map_err(RunNowError::Store)?;
    if let Some(c) = &final_claim {
        if c.outcome == Some(RunOutcome::WakeFailed) {
            return Err(RunNowError::Action(
                c.outcome_reason.clone().unwrap_or_else(|| "FAILED".into()),
            ));
        }
    }

    Ok(RunNowOutcome {
        timer,
        run_id: claim.run_id,
        scheduled_for,
        message,
    })
}

/// Submit the claim to a dispatcher and wait for its durable result,
/// assembling the human message from the outcome (a `run_now` response
/// concern, not shared scheduler state).
fn dispatch_and_wait(
    store: &mut Store,
    db_path: &Path,
    timer: &Timer,
    claim: &crate::store::RunClaim,
    opts: &RunNowOptions,
) -> Result<String, RunNowError> {
    // Generous wait bound: worst-case action time plus margin for lane
    // queueing and a foreign owner's pump tick.
    let exec_cfg = ExecutorConfig {
        skip_retry_sleep: opts.skip_retry_sleep,
        ..ExecutorConfig::default()
    };
    let wait = exec_cfg.launch_timeout.mul_f64((timer.retry.max_retries + 1) as f64)
        + Duration::from_secs(timer.retry.delay_secs.saturating_mul(u64::from(timer.retry.max_retries)))
        + Duration::from_secs(60);

    // Standalone CLI: spin up the bounded dispatcher when no process owns
    // the OS lock. The lock holder must run R10 first (reply scan) before
    // any pump — same ordering as the scheduler boot.
    let mut local: Option<Dispatcher> = None;
    let dispatcher = match &opts.dispatcher {
        Some(d) => d.clone(),
        None => {
            let d = Dispatcher::spawn(DispatcherConfig {
                db_path: db_path.to_path_buf(),
                data_dir: db_path.parent().map(Path::to_path_buf),
                max_concurrent_actions: crate::app_config::AppConfig::load(
                    db_path.parent().unwrap_or(Path::new(".")),
                )
                .unwrap_or_default()
                .max_concurrent_actions,
                notify_sink: opts
                    .notify_sink
                    .clone()
                    .unwrap_or_else(|| Arc::new(crate::actions::StubNotifySink)),
                executor: exec_cfg.clone(),
                tick: Duration::from_millis(100),
                ipc: None,
            })
            .map_err(|e| RunNowError::Other(format!("dispatcher spawn: {e}")))?;
            if d.owns_lock() {
                if let Some(data_dir) = db_path.parent() {
                    let engine = crate::reply::ReplyEngine {
                        tree: crate::tree::TimersTree::new(data_dir),
                        data_dir: data_dir.to_path_buf(),
                        pickup_grace: Duration::from_secs(60),
                        watchdog_factor: 2.0,
                        anchors: opts
                            .anchors
                            .clone()
                            .unwrap_or_else(crate::reply::new_anchors),
                        deadlines: opts
                            .deadlines
                            .clone()
                            .unwrap_or_else(crate::reply::new_deadlines),
                        fire_slot_file: None,
                        status_listener: None,
                        ipc: None,
                    };
                    crate::reply::startup_scan(&engine, store, Utc::now());
                }
                d.begin_startup();
            }
            local = Some(d);
            local.as_ref().expect("just set").clone()
        }
    };

    dispatcher.submit(claim.run_id);
    let finished = dispatcher
        .wait_finished(store, claim.run_id, wait)
        .map_err(RunNowError::Store)?;

    if let Some(d) = local {
        // The CLI's bounded dispatcher runs until its claim finished, then
        // releases the OS lock (drop of the guard) — in-flight lanes drain.
        d.shutdown_drain();
    }

    let Some(c) = finished else {
        return Ok(format!(
            "dispatched run_id={} (still running; result lands in the claim ledger)",
            claim.run_id
        ));
    };
    Ok(match c.outcome {
        Some(RunOutcome::WakeDelivered) => c
            .outcome_reason
            .clone()
            .unwrap_or_else(|| "action completed".into()),
        Some(RunOutcome::WakeFailed) => format!(
            "FAILED: {}",
            c.outcome_reason.clone().unwrap_or_else(|| "unknown".into())
        ),
        Some(RunOutcome::SkippedMisfire) => c
            .outcome_reason
            .clone()
            .unwrap_or_else(|| "overlap policy skip".into()),
        None => "action completed".into(),
    })
}

/// Open the store with the same defaults the CLI uses.
pub fn open_store(db_path: &Path) -> Result<Store, StoreError> {
    Store::open_with(
        db_path,
        OpenOptions {
            refuse_network_fs: true,
            ..OpenOptions::default()
        },
    )
}

/// Resolve `<db parent>/logs/` (or `$BELLMAN_LOGS`) for the JSONL log.
pub fn resolve_logs_dir(db_path: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("BELLMAN_LOGS") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    db_path
        .parent().map_or_else(|| PathBuf::from("logs"), |p| p.join("logs"))
}

/// Optional slots root for fire-trigger JSON writes.
pub fn resolve_slots_root_optional(db_path: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BELLMAN_SLOTS") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let candidate = db_path.parent()?.join("slots");
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

/// Failure modes surfaced by [`run_now`].
#[derive(Debug)]
pub enum RunNowError {
    NotFound { timer_id: Uuid },
    Store(StoreError),
    Action(String),
    Other(String),
}

impl std::fmt::Display for RunNowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { timer_id } => write!(f, "timer not found: {timer_id}"),
            Self::Store(e) => write!(f, "{e}"),
            Self::Action(s) => write!(f, "{s}"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for RunNowError {}
