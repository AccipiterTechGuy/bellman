//! Scheduler loop configuration.

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
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            horizon: Duration::from_hours(24),
            max_sleep: Duration::from_secs(30),
            jump_threshold: Duration::from_secs(3),
        }
    }
}

impl SchedulerConfig {
    pub fn new() -> Self {
        Self::default()
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
}
