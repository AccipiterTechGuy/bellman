//! `run_now` service: claim → act → complete → advance.
//!
//! Extracted from `bellman-cli` (C6) so the C7 Tauri shell can call the exact
//! same path. The optional `write_slot_dir` lets the production GUI mirror
//! `bellman-cli run-now` semantics (launch + write JSON into `slots/done/`).
//!
//! The optional [`NotifySink`] injects the real desktop-notification backend
//! in the Tauri shell; the CLI uses the stub.

use crate::actions::{ActionRunner, ActionRunnerConfig, NotifySink};
use crate::scheduler::{FireAction, FireContext, FireKind};
use crate::store::{
    OpenOptions, SlotRequestRecord, Store, StoreError, Timer, TimerPatch, TimerUpdate,
};
use crate::slots::SlotResponse;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// Caller-controlled knobs for [`run_now`].
#[derive(Default)]
pub struct RunNowOptions {
    /// When set, a successful fire also writes a fire-trigger JSON here
    /// (under the chosen filename). Mirrors `bellman-cli run-now` semantics.
    pub write_slot_dir: Option<PathBuf>,
    pub write_slot_file: Option<String>,
    /// When `Some(true)`, skip the retry sleep (test path).
    pub skip_retry_sleep: bool,
    /// Notification sink to install on the runner. `None` keeps the stub.
    pub notify_sink: Option<Arc<dyn NotifySink>>,
    /// IK3 duration anchors shared with the reply watcher. `None` starts a
    /// fresh registry (CLI one-shots): the terminal event then falls back to
    /// the wall-clock duration, marked `duration_source: "wall_clock"`.
    pub anchors: Option<crate::reply::SharedAnchors>,
    /// IK3 monotonic deadline book shared with the reply watcher. `None`
    /// starts a fresh book (CLI one-shots; the GUI watcher lazily
    /// reconstructs from the persisted wall deadlines).
    pub deadlines: Option<crate::reply::SharedDeadlines>,
}

impl std::fmt::Debug for RunNowOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunNowOptions")
            .field("write_slot_dir", &self.write_slot_dir)
            .field("write_slot_file", &self.write_slot_file)
            .field("skip_retry_sleep", &self.skip_retry_sleep)
            // NotifySink is dyn; report its type name only.
            .field(
                "notify_sink",
                &self
                    .notify_sink
                    .as_ref()
                    .map(|s| std::any::type_name_of_val(s.as_ref())),
            )
            .finish()
    }
}

impl Clone for RunNowOptions {
    fn clone(&self) -> Self {
        Self {
            write_slot_dir: self.write_slot_dir.clone(),
            write_slot_file: self.write_slot_file.clone(),
            skip_retry_sleep: self.skip_retry_sleep,
            notify_sink: self.notify_sink.clone(),
            anchors: self.anchors.clone(),
            deadlines: self.deadlines.clone(),
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
/// Steps (mirrored from the scheduler's `deliver_one`):
/// 1. the R10 fire transaction (`project_fire`): gate → barrier → one commit
///    (supersede + claim + lifecycle row + `fired` event) → projections
/// 2. `ActionRunner::on_fire` (launch / notify / write-slot)
/// 3. `complete_run` / `fail_run` (honest wake outcome)
/// 4. advance `last_fired` + `record_run` so `next_fire` moves past this slot
/// 5. best-effort outbox drain (a CLI-only run must not wait for a GUI tick)
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
    //    atomic commit, then projections. The gate releases before the
    //    action runs (a long action never blocks the reply watcher).
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

    let mut runner = ActionRunner::new(ActionRunnerConfig {
        write_slot_dir: opts.write_slot_dir.clone(),
        write_slot_file: opts.write_slot_file.clone(),
        skip_retry_sleep: opts.skip_retry_sleep,
        ..ActionRunnerConfig::default()
    });
    if let Ok(sink_store) = crate::store::Store::open_with(db_path, Default::default()) {
        runner = runner.with_event_sink(sink_store);
    }
    if let Some(sink) = opts.notify_sink.as_ref() {
        runner = runner.with_notify_sink(sink.clone());
    }

    let ctx = FireContext {
        timer: &timer,
        scheduled_for: claim.scheduled_for,
        run_id: claim.run_id,
        kind: FireKind::OnTime,
        claimed_at: claim.claimed_at,
    };

    let action_res = runner.on_fire(&ctx);
    let message = runner
        .last_message
        .clone()
        .unwrap_or_else(|| "action completed".into());

    // Close the claim even when the action fails so recovery does not loop
    // (mirrors scheduler) — recording delivered vs wake-failed honestly.
    if action_res.is_ok() {
        store
            .complete_run(claim.run_id)
            .map_err(RunNowError::Store)?;
    } else {
        store
            .fail_run(claim.run_id)
            .map_err(RunNowError::Store)?;
    }

    // Best-effort outbox drain: a one-shot CLI run cannot rely on a GUI
    // publisher tick — if this process can take the lease, it publishes its
    // own rows (fdatasync + mark) before exiting.
    if let Some(data_dir) = db_path.parent() {
        crate::events::EventPublisher::drain_best_effort(data_dir, store);
    }

    if let Err(e) = action_res {
        // IK2: status.json stays the firing snapshot — the delivery failure is
        // honest in the claim ledger and the wake_failed event; R5 `failed`
        // is reserved for app reports (IK3).
        return Err(RunNowError::Action(e));
    }

    // Advance last_fired + record_run so next_fire moves past this slot —
    // same bookkeeping as the scheduler's `mark_fired` path.
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
    // and the claim ledger's `Completed` only means wake_delivered.
    if let Some(data_dir) = db_path.parent() {
        let tree = crate::tree::TimersTree::new(data_dir);
        let owner = store.get_timer_owner(timer.id).ok().flatten();
        if let Err(e) = tree.sync_timer_json(&timer, owner.as_deref()) {
            eprintln!("bellman: timer.json refresh failed: {e}");
        }
    }

    Ok(RunNowOutcome {
        timer,
        run_id: claim.run_id,
        scheduled_for,
        message,
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

/// Re-export the slot-output overlay so the Tauri shell can call it from
/// `run_now` without pulling `bellman-cli` into the dependency tree.
pub fn publish_fire_slot_response(
    store: &Store,
    slots_root: &Path,
    timer: &Timer,
    rec: &SlotRequestRecord,
) -> Result<(), String> {
    use crate::slots::{atomic_write_json, SlotRunEvent, SlotStatus, SCHEMA_V1};

    let runs = store
        .unacked_runs_for_timer(timer.id, 64)
        .map_err(|e| e.to_string())?;
    let events: Vec<SlotRunEvent> = runs.iter().map(SlotRunEvent::from_claim).collect();

    let response = SlotResponse {
        schema: SCHEMA_V1.to_string(),
        slot_id: rec.slot_id.clone(),
        request_id: rec.request_id.clone(),
        status: SlotStatus::Ok,
        timer_id: Some(timer.id),
        next_fire_at: timer.next_fire_utc,
        error: None,
        events,
    };
    let name = format!("slot-{}.json", rec.slot_id);
    let done = slots_root.join("done");
    atomic_write_json(&done, &name, &response)
        .map(|_| ())
        .map_err(|e| e.to_string())
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
