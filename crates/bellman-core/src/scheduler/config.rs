//! Scheduler loop configuration.

use crate::app_config::AppConfig;
use std::path::PathBuf;
use std::time::Duration;

/// Period threshold for "high-frequency" interval timers (product: 5 minutes).
///
/// Timers repeating faster than this always stay resident on the heap, even if
/// a short horizon would otherwise exclude them.
pub const HIGH_FREQ_PERIOD_SECS: u64 = 5 * 60;

/// Tunables for the heap loop.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// How far ahead of wall-now to keep on the heap (default 24 h).
    pub horizon: Duration,
    /// Max sleep chunk before re-reading the wall clock (default 30 s).
    pub max_sleep: Duration,
    /// Unconditional horizon-rebuild floor (default 60 s). External writers
    /// (`bellman slot-submit` applies slot requests on its own connection)
    /// commit to the store without any control message; the loop notices them
    /// via `PRAGMA data_version` on each wake, and rebuilds at least this
    /// often even if the probe somehow stays silent. Set very large in tests
    /// to prove the probe path alone.
    pub external_rebuild_interval: Duration,
    /// Wall-vs-monotonic divergence that counts as a clock jump / suspend
    /// (default 3 s).
    pub jump_threshold: Duration,
    /// Global concurrent wake-action cap (default 16).
    pub max_concurrent_actions: usize,
    /// Default accuracy slack for high-frequency timers (default 1 s).
    pub accuracy_slack: Duration,
    /// Data directory for prune / event-log maintenance (optional).
    pub data_dir: Option<PathBuf>,
    /// JSONL retention used by the weekly prune pass.
    pub retention: Duration,
    /// Prune cadence (default 7 days) for startup catch-up.
    pub prune_interval: Duration,
    /// Ack grace before terminal one-shots are eligible for prune.
    pub ack_grace: Duration,
    /// Rotate the live event log before it crosses this size (default 64 MiB).
    pub log_rotation_max_bytes: u64,
    /// Retained-log budget for current + archives (default 1 GiB).
    pub log_retention_budget_bytes: u64,
    /// Aggregate ceiling for the reply quarantine (default 64 MiB).
    pub quarantine_budget_bytes: u64,
    /// Pickup deadline for integration-owned runs (IK3; default 60 s).
    pub pickup_grace: Duration,
    /// Opt-in watchdog factor: deadline = `expected_secs × factor` (IK3).
    pub watchdog_factor: f64,
    /// Monotonic duration anchors shared with the reply watcher (IK3).
    pub anchors: crate::reply::SharedAnchors,
    /// Monotonic deadline book shared with the reply watcher (IK3).
    pub deadlines: crate::reply::SharedDeadlines,
    /// Optional fixed fire-notification filename under `slots/fires/` (SCH1
    /// transport route; `None` → per-run `fire-<run_id>.json`).
    pub fire_slot_file: Option<String>,
    /// IK5: invalidation sink wired into the reply engine this configuration
    /// builds (fire projections and scheduler-heap deadline expiries).
    pub status_listener: Option<crate::reply::StatusListener>,
    /// IK6: the live local-socket handle, shared with the dispatcher's
    /// publication pump. The scheduler's fire path needs it because the
    /// per-firing transport choice (`select_transport`) tests it: without a
    /// handle here every scheduled fire resolves to files, whatever the
    /// timer's `transport.mode` says. `None` in headless unit tests.
    pub ipc: Option<crate::ipc::IpcHandle>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self::from_app_config(&AppConfig::default())
    }
}

impl SchedulerConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from a loaded [`AppConfig`] (horizon, retention, concurrency, …).
    pub fn from_app_config(cfg: &AppConfig) -> Self {
        Self {
            horizon: cfg.horizon(),
            max_sleep: Duration::from_secs(30),
            external_rebuild_interval: Duration::from_secs(60),
            jump_threshold: Duration::from_secs(3),
            max_concurrent_actions: cfg.max_concurrent_actions.max(1),
            accuracy_slack: cfg.accuracy_slack(),
            data_dir: None,
            retention: cfg.retention(),
            prune_interval: cfg.prune_interval(),
            ack_grace: cfg.ack_grace(),
            log_rotation_max_bytes: cfg.log_rotation_max_bytes,
            log_retention_budget_bytes: cfg.log_retention_budget_bytes,
            quarantine_budget_bytes: cfg.quarantine_budget_bytes,
            pickup_grace: cfg.pickup_grace(),
            watchdog_factor: cfg.watchdog_factor,
            anchors: crate::reply::new_anchors(),
            deadlines: crate::reply::new_deadlines(),
            fire_slot_file: None,
            status_listener: None,
            ipc: None,
        }
    }

    pub fn with_horizon(mut self, horizon: Duration) -> Self {
        self.horizon = horizon;
        self
    }

    pub fn with_max_sleep(mut self, max_sleep: Duration) -> Self {
        self.max_sleep = max_sleep;
        self
    }

    /// Unconditional horizon-rebuild floor for external store writers (SCH2).
    pub fn with_external_rebuild_interval(mut self, interval: Duration) -> Self {
        self.external_rebuild_interval = interval;
        self
    }

    pub fn with_jump_threshold(mut self, jump_threshold: Duration) -> Self {
        self.jump_threshold = jump_threshold;
        self
    }

    pub fn with_max_concurrent_actions(mut self, n: usize) -> Self {
        self.max_concurrent_actions = n.max(1);
        self
    }

    pub fn with_accuracy_slack(mut self, slack: Duration) -> Self {
        self.accuracy_slack = slack;
        self
    }

    pub fn with_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// Share a duration-anchor registry with the reply watcher (IK3). The
    /// GUI wires one registry into both so `duration_ms` stays monotonic
    /// across the fire thread and the ingest thread.
    pub fn with_anchors(mut self, anchors: crate::reply::SharedAnchors) -> Self {
        self.anchors = anchors;
        self
    }

    /// Share the monotonic deadline book with the reply watcher (IK3).
    pub fn with_deadlines(mut self, deadlines: crate::reply::SharedDeadlines) -> Self {
        self.deadlines = deadlines;
        self
    }

    /// Fixed fire-notification filename under `slots/fires/` (SCH1).
    pub fn with_fire_slot_file(mut self, file: Option<String>) -> Self {
        self.fire_slot_file = file;
        self
    }

    /// IK5: invalidation sink for the reply engine this configuration builds.
    pub fn with_status_listener(mut self, listener: Option<crate::reply::StatusListener>) -> Self {
        self.status_listener = listener;
        self
    }

    /// IK6: share the live IPC handle with the fire path so a timer whose
    /// `transport.mode` is `ipc` / `auto` can actually be delivered over the
    /// socket when a client holds it.
    pub fn with_ipc(mut self, ipc: Option<crate::ipc::IpcHandle>) -> Self {
        self.ipc = ipc;
        self
    }

    /// Build the IK3 reply engine for this configuration (None when no data
    /// dir is set — unit tests keep a pure store).
    pub fn reply_engine(&self) -> Option<crate::reply::ReplyEngine> {
        let data_dir = self.data_dir.clone()?;
        Some(crate::reply::ReplyEngine {
            tree: crate::tree::TimersTree::new(&data_dir),
            data_dir,
            pickup_grace: self.pickup_grace,
            watchdog_factor: self.watchdog_factor,
            anchors: self.anchors.clone(),
            deadlines: self.deadlines.clone(),
            fire_slot_file: self.fire_slot_file.clone(),
            status_listener: self.status_listener.clone(),
            ipc: self.ipc.clone(),
        })
    }
}
