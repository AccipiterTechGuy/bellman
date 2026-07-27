//! Domain types persisted by the store.

use crate::occurrence::Occurrence;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Timer primary key (UUID).
pub type TimerId = Uuid;

/// Per-timer misfire / catch-up policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    /// Drop missed fires (default for interval timers).
    Skip,
    /// Coalesce missed backlog into a single recovery fire (default for calendar).
    Coalesce {
        /// Grace window in seconds; recovery only if lateness ≤ grace.
        grace_secs: u64,
    },
}

impl Default for MisfirePolicy {
    fn default() -> Self {
        Self::Coalesce { grace_secs: 3600 }
    }
}

/// What to do when a previous run is still in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    /// Do not start a new run while one is active (product default).
    #[default]
    Skip,
    /// Keep at most one queued follow-up.
    QueueOne,
    /// Allow concurrent runs up to `cap`.
    Parallel { cap: u32 },
    /// Cancel the in-flight run and start the new one.
    Replace,
}

/// Retry policy for a failed wake action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// How many retries after the first attempt (product default: 1).
    pub max_retries: u32,
    /// Delay before the retry, in seconds (product default: 30).
    pub delay_secs: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 1,
            delay_secs: 30,
        }
    }
}

/// Wake action attached to a timer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Launch a process (arg array, no shell).
    Launch {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        workdir: Option<String>,
    },
    /// Desktop notification.
    Notify {
        title: String,
        #[serde(default)]
        body: String,
    },
    /// No-op placeholder (useful in tests / disabled actions).
    #[default]
    None,
}

/// Fully loaded timer row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timer {
    pub id: TimerId,
    pub name: String,
    pub enabled: bool,
    pub occurrence: Occurrence,
    /// Denormalized IANA tz name (mirrors `occurrence.tz`).
    pub tz: String,
    pub next_fire_utc: Option<DateTime<Utc>>,
    pub last_fired: Option<DateTime<Utc>>,
    pub misfire: MisfirePolicy,
    pub overlap: OverlapPolicy,
    pub retry: RetryPolicy,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub max_runs: Option<u64>,
    pub tags: Vec<String>,
    pub action: Action,
    /// Optimistic concurrency token; starts at 1 on insert.
    pub revision: i64,
}

/// Input for [`super::Store::create_timer`].
#[derive(Debug, Clone)]
pub struct NewTimer {
    /// Optional fixed id (tests); random UUID when `None`.
    pub id: Option<TimerId>,
    pub name: String,
    pub enabled: bool,
    pub occurrence: Occurrence,
    pub misfire: MisfirePolicy,
    pub overlap: OverlapPolicy,
    pub retry: RetryPolicy,
    pub tags: Vec<String>,
    pub action: Action,
    /// Seed last_fired (usually `None` for new timers).
    pub last_fired: Option<DateTime<Utc>>,
}

impl NewTimer {
    pub fn new(name: impl Into<String>, occurrence: Occurrence) -> Self {
        Self {
            id: None,
            name: name.into(),
            enabled: true,
            occurrence,
            misfire: MisfirePolicy::default(),
            overlap: OverlapPolicy::default(),
            retry: RetryPolicy::default(),
            tags: Vec::new(),
            action: Action::default(),
            last_fired: None,
        }
    }
}

/// Partial update applied under optimistic revision check.
#[derive(Debug, Clone, Default)]
pub struct TimerPatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub occurrence: Option<Occurrence>,
    pub misfire: Option<MisfirePolicy>,
    pub overlap: Option<OverlapPolicy>,
    pub retry: Option<RetryPolicy>,
    pub tags: Option<Vec<String>>,
    pub action: Option<Action>,
    pub last_fired: Option<Option<DateTime<Utc>>>,
}

/// Optimistic update envelope.
#[derive(Debug, Clone)]
pub struct TimerUpdate {
    pub id: TimerId,
    pub expected_revision: i64,
    pub patch: TimerPatch,
}

/// Status of a row in the at-least-once claim ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// Claimed, work not yet finished (visible after crash recovery).
    Claimed,
    /// Work finished successfully.
    Completed,
}

impl ClaimStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Completed => "completed",
        }
    }
}

impl std::str::FromStr for ClaimStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claimed" => Ok(Self::Claimed),
            "completed" => Ok(Self::Completed),
            other => Err(format!("unknown claim status '{other}'")),
        }
    }
}

/// One row of the runs claim ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunClaim {
    pub run_id: Uuid,
    pub timer_id: TimerId,
    pub scheduled_for: DateTime<Utc>,
    pub status: ClaimStatus,
    pub claimed_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Meta bookkeeping row (single-row table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub schema_version: i32,
    pub last_prune: Option<DateTime<Utc>>,
    pub last_recalibration: Option<DateTime<Utc>>,
    pub tzdata_version: Option<String>,
}
