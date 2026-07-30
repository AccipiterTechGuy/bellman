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
            jump_threshold: Duration::from_secs(3),
            max_concurrent_actions: cfg.max_concurrent_actions.max(1),
            accuracy_slack: cfg.accuracy_slack(),
            data_dir: None,
            retention: cfg.retention(),
            prune_interval: cfg.prune_interval(),
            ack_grace: cfg.ack_grace(),
            log_rotation_max_bytes: cfg.log_rotation_max_bytes,
            log_retention_budget_bytes: cfg.log_retention_budget_bytes,
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
}


