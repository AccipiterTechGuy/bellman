//! Domain types persisted by the store.

use crate::occurrence::Occurrence;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Timer primary key (UUID).
pub type TimerId = Uuid;

/// Per-timer misfire / catch-up policy.
///
/// Product defaults (PLAN / BUILD_PLAN):
/// - **calendar** kinds (once/daily/weekly/monthly/yearly/cron) →
///   [`MisfirePolicy::Coalesce`] with 1 h grace
/// - **interval** (elapsed-time) → [`MisfirePolicy::Skip`] (grace = one period
///   at the scheduler layer; the policy itself is skip)
/// - **catch_up** is optional with an explicit `max_catch_up` cap
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    /// Drop missed fires (default for interval timers).
    ///
    /// Scheduler grace for skip is one period on interval timers (and zero on
    /// calendar kinds): within grace the due fire still runs once; beyond grace
    /// the backlog is advanced without firing.
    Skip,
    /// Coalesce missed backlog into a single recovery fire (default for calendar).
    Coalesce {
        /// Grace window in seconds; recovery only if lateness ≤ grace.
        grace_secs: u64,
    },
    /// Replay missed fires up to `max_catch_up`, each still within grace.
    CatchUp {
        /// Grace window in seconds measured from *now* back to each missed fire.
        grace_secs: u64,
        /// Hard cap on how many missed fires to deliver on recovery.
        max_catch_up: u32,
    },
}

impl MisfirePolicy {
    /// Default calendar grace (1 hour).
    pub const CALENDAR_GRACE_SECS: u64 = 3600;

    /// Product default for calendar (wall-clock) occurrence kinds.
    pub fn default_calendar() -> Self {
        Self::Coalesce {
            grace_secs: Self::CALENDAR_GRACE_SECS,
        }
    }

    /// Product default for interval (elapsed-time) occurrence kinds.
    pub fn default_interval() -> Self {
        Self::Skip
    }

    /// Choose the product default from the occurrence kind.
    pub fn for_occurrence(occ: &Occurrence) -> Self {
        if occ.kind().is_elapsed_time() {
            Self::default_interval()
        } else {
            Self::default_calendar()
        }
    }

    /// Explicit grace window in seconds, when the policy carries one.
    ///
    /// [`MisfirePolicy::Skip`] has no stored grace — the scheduler uses one
    /// interval period (or zero for non-interval kinds).
    pub fn grace_secs(&self) -> Option<u64> {
        match self {
            Self::Skip => None,
            Self::Coalesce { grace_secs } | Self::CatchUp { grace_secs, .. } => Some(*grace_secs),
        }
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
    /// Execution jitter amplitude in seconds (`±jitter_secs`). Applied to
    /// heap execution time only — displayed `next_fire_utc` stays clean.
    #[serde(default)]
    pub jitter_secs: u32,
    /// Optional per-timer accuracy slack (seconds) for high-frequency firing.
    /// `None` → use the global config default for high-freq timers.
    #[serde(default)]
    pub accuracy_slack_secs: Option<u32>,
    /// Participate in the single-next-wake election (RTC resume). Default false.
    /// Greyed in the GUI when platform capability is Disabled.
    #[serde(default)]
    pub wake_machine: bool,
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
    /// Execution jitter amplitude in seconds (default 0).
    pub jitter_secs: u32,
    /// Optional per-timer accuracy slack override.
    pub accuracy_slack_secs: Option<u32>,
    /// Participate in single-next-wake election (default false).
    pub wake_machine: bool,
}

impl NewTimer {
    /// Build a new timer with product-default policies for the occurrence kind
    /// (interval → misfire Skip; calendar → Coalesce 1 h).
    pub fn new(name: impl Into<String>, occurrence: Occurrence) -> Self {
        let misfire = MisfirePolicy::for_occurrence(&occurrence);
        Self {
            id: None,
            name: name.into(),
            enabled: true,
            occurrence,
            misfire,
            overlap: OverlapPolicy::default(),
            retry: RetryPolicy::default(),
            tags: Vec::new(),
            action: Action::default(),
            last_fired: None,
            jitter_secs: 0,
            accuracy_slack_secs: None,
            wake_machine: false,
        }
    }

    pub fn with_jitter(mut self, jitter_secs: u32) -> Self {
        self.jitter_secs = jitter_secs;
        self
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
    pub jitter_secs: Option<u32>,
    pub accuracy_slack_secs: Option<Option<u32>>,
    pub wake_machine: Option<bool>,
}

/// Optimistic update envelope.
#[derive(Debug, Clone)]
pub struct TimerUpdate {
    pub id: TimerId,
    pub expected_revision: i64,
    pub patch: TimerPatch,
}

/// Dispatch phase of a row in the at-least-once claim ledger (SCH1).
///
/// Internal delivery bookkeeping — NOT the R5 run-state vocabulary, and NOT
/// an outcome: `pending` / `active` only say where the claim sits in the
/// dispatcher; the outcome lives in [`RunClaim::outcome`] once the row is
/// [`ClaimStatus::Finished`]. Project onto R5 at the wire boundary
/// (`SlotRunEvent::from_claim`): an unfinished claim is an open `fired` run;
/// `finished` carries exactly one [`RunOutcome`]. The R5 states `completed` /
/// `failed` are reserved for app reports (IK3).
///
/// The pre-SCH1 ledger used `claimed` / `completed` / `wake_failed`; the
/// schema-v8 migration rewrites those rows (`claimed → pending`,
/// `completed → finished + wake_delivered`, `wake_failed → finished +
/// wake_failed`). [`FromStr`] still accepts the legacy strings so a
/// not-yet-migrated row can never crash a reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// Committed, admitted for execution, waiting for a worker lane.
    Pending,
    /// A worker holds the lane and is executing (or was, when its owner
    /// died — the dispatcher-lock holder returns these to `pending`).
    Active,
    /// Terminal; `outcome` carries the R5 delivery outcome.
    Finished,
}

impl ClaimStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Finished => "finished",
        }
    }
}

impl std::str::FromStr for ClaimStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "finished" => Ok(Self::Finished),
            // Legacy (pre-v8) strings, mapped into the split.
            "claimed" => Ok(Self::Pending),
            "completed" | "wake_failed" => Ok(Self::Finished),
            other => Err(format!("unknown claim status '{other}'")),
        }
    }
}

/// The one R5 delivery outcome a `finished` claim carries (SCH1).
///
/// Dispatch state (`pending` / `active`) has no outcome; a skip decided in
/// the fire transaction can never appear as `wake_delivered`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// Wake action delivered successfully.
    WakeDelivered,
    /// Wake action failed after retries, or was cancelled by `Replace`.
    WakeFailed,
    /// The overlap policy decided at fire commit not to run this action.
    SkippedMisfire,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WakeDelivered => "wake_delivered",
            Self::WakeFailed => "wake_failed",
            Self::SkippedMisfire => "skipped_misfire",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "wake_delivered" => Some(Self::WakeDelivered),
            "wake_failed" => Some(Self::WakeFailed),
            "skipped_misfire" => Some(Self::SkippedMisfire),
            _ => None,
        }
    }
}

/// Durable record of a processed slot request (`request_id` is the idempotency key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRequestRecord {
    pub request_id: String,
    pub slot_id: String,
    pub operation: String,
    pub app_name: Option<String>,
    pub timer_id: Option<TimerId>,
    /// `"ok"` or `"error"` (mirrors the output-slot status).
    pub status: String,
    /// Full serialized slot response JSON (`SlotResponse`).
    pub response_json: String,
    pub created_at: DateTime<Utc>,
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
    /// Durable monotonic sequence for this timer's run events (slot output feed).
    pub event_sequence: u64,
    /// The R5 delivery outcome; `Some` exactly when `status` is
    /// [`ClaimStatus::Finished`] (internal dispatch field, not a wire shape).
    #[serde(default)]
    pub outcome: Option<RunOutcome>,
    /// Short machine reason for the outcome (`overlap_skip`,
    /// `overlap_replace`, launch error summary, …).
    #[serde(default)]
    pub outcome_reason: Option<String>,
    /// Durable cancellation request set by a `Replace` fire transaction; the
    /// dispatcher observes it and signals the worker's cancellation token.
    #[serde(default)]
    pub cancel_requested: bool,
}

impl RunClaim {
    /// Unfinished means a worker lane still owes this claim an outcome.
    pub fn is_unfinished(&self) -> bool {
        self.status != ClaimStatus::Finished
    }
}

/// Retry/routing state for one run's fire notification (SCH1) — NOT another
/// event or history copy. Written by the fire transaction with the immutable
/// target path, the serialized notification payload and a database-wide
/// monotonic `publication_order`; pruned with the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportProjection {
    pub run_id: Uuid,
    pub timer_id: TimerId,
    /// Immutable absolute target path (`slots/fires/fire-<run_id>.json`, or a
    /// configured fixed name under `fires/` used as an at-least-once wake hint).
    pub target_path: String,
    /// Serialized `FireNotification` JSON (the exact bytes to publish).
    pub payload: String,
    /// Database-wide monotonic order assigned in the fire transaction.
    pub publication_order: u64,
    /// `pending` | `published` | `obsolete` | `picked_up`.
    pub state: String,
    /// Bounded-retry bookkeeping for the publication pump.
    pub attempts: u32,
    pub next_attempt_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

impl TransportProjection {
    pub const PENDING: &'static str = "pending";
    pub const PUBLISHED: &'static str = "published";
    pub const OBSOLETE: &'static str = "obsolete";
    pub const PICKED_UP: &'static str = "picked_up";
}

/// Who reported a `failed` run state (IK3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The app said it failed.
    Reported,
    /// The app opted into `error_detection` and went quiet past its own
    /// declared deadline (provisional — a late app reply may revise it).
    TimedOut,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::TimedOut => "timed_out",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "reported" => Some(Self::Reported),
            "timed_out" => Some(Self::TimedOut),
            _ => None,
        }
    }
}

/// The app-lifecycle state of one integration-owned run (IK3 `run_states`).
///
/// Created by the fire transaction with the integration owner snapshotted
/// onto the row — validation and status projection use this snapshot, never
/// the mutable timer configuration. App-reported fields ACCUMULATE here (a
/// later reply that omits a field never retracts it), which is what lets
/// `status.json` be re-projected from the database alone after a crash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStateRow {
    pub run_id: Uuid,
    pub timer_id: TimerId,
    /// Integration owner snapshotted at fire time.
    pub app_name: String,
    /// R5 wire state (`fired` / `acknowledged` / `running` / `completed` /
    /// `failed` / `no_ack` / `cancelled` / `superseded`).
    pub state: String,
    /// Bellman wall-clock at the fire commit (duration fallback anchor).
    pub fired_at: DateTime<Utc>,
    /// Persisted wall-clock pickup deadline (restart recovery only); `None`
    /// once pickup is satisfied or consumed by `no_ack`.
    pub pickup_deadline: Option<DateTime<Utc>>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub expected_secs: Option<u64>,
    /// Accumulated `error_detection` (`None` = never mentioned).
    pub error_detection: Option<bool>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub progress: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    pub failure_kind: Option<FailureKind>,
    /// App result, capped at 32 KB as stored (`result_truncated` flags it).
    pub result_json: Option<serde_json::Value>,
    pub result_truncated: bool,
    /// Persisted wall-clock watchdog deadline (restart recovery only).
    pub watchdog_deadline: Option<DateTime<Utc>>,
    pub no_ack_at: Option<DateTime<Utc>>,
    /// Digest of the last accepted reply — an exact duplicate is a no-op.
    pub reply_digest: Option<String>,
    /// Transition lines already in the event log (log records transitions
    /// only; repeated writes inside a state append nothing).
    pub acknowledged_logged: bool,
    pub running_logged: bool,
}

impl RunStateRow {
    /// Fresh row at fire time, before any pickup signal.
    pub fn fired(
        run_id: Uuid,
        timer_id: TimerId,
        app_name: &str,
        fire_state: &str,
        fired_at: DateTime<Utc>,
        pickup_deadline: DateTime<Utc>,
    ) -> Self {
        Self {
            run_id,
            timer_id,
            app_name: app_name.to_string(),
            state: fire_state.to_string(),
            fired_at,
            pickup_deadline: Some(pickup_deadline),
            acknowledged_at: None,
            expected_secs: None,
            error_detection: None,
            heartbeat_at: None,
            progress: None,
            completed_at: None,
            failed_at: None,
            reason: None,
            failure_kind: None,
            result_json: None,
            result_truncated: false,
            watchdog_deadline: None,
            no_ack_at: None,
            reply_digest: None,
            acknowledged_logged: false,
            running_logged: false,
        }
    }

    /// The R5 state of this row.
    pub fn run_state(&self) -> Option<crate::events::RunState> {
        crate::events::RunState::from_wire(&self.state)
    }

    /// Terminal for the app lifecycle (see [`crate::events::RunState::is_terminal`]).
    pub fn is_terminal(&self) -> bool {
        self.run_state().map(|s| s.is_terminal()).unwrap_or(false)
    }

    /// An app-authored closing verdict (`completed`, or `failed` the app
    /// reported itself) — as opposed to Bellman's provisional `no_ack` /
    /// watchdog `timed_out`, which a valid reply may still revise.
    pub fn is_app_authored_terminal(&self) -> bool {
        match self.run_state() {
            Some(crate::events::RunState::Completed) => true,
            Some(crate::events::RunState::Failed) => {
                self.failure_kind == Some(FailureKind::Reported)
            }
            _ => false,
        }
    }
}

/// Meta bookkeeping row (single-row table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub schema_version: i32,
    pub last_prune: Option<DateTime<Utc>>,
    pub last_recalibration: Option<DateTime<Utc>>,
    pub tzdata_version: Option<String>,
}

/// Rotation phase recorded in the R11 journal (`rotation_journal.phase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationPhase {
    /// Current renamed to the `.rotating` source.
    Renamed,
    /// Final archive durably in place.
    Finalized,
    /// `.rotating` source deleted after final-archive verification.
    SourceRemoved,
}

impl RotationPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Renamed => "renamed",
            Self::Finalized => "finalized",
            Self::SourceRemoved => "source_removed",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "renamed" => Some(Self::Renamed),
            "finalized" => Some(Self::Finalized),
            "source_removed" => Some(Self::SourceRemoved),
            _ => None,
        }
    }
}

/// The R11 rotation journal row: every artifact of an in-flight rotation,
/// named durably so a recovering publisher can roll any interrupted phase
/// forward before appending or rotating again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationJournal {
    /// The live current file that was renamed away.
    pub source: std::path::PathBuf,
    /// The plain `.rotating` working copy (readers include it while the
    /// journal is active; never a partial gzip temp).
    pub rotating: std::path::PathBuf,
    /// The gzip temporary (never parsed by readers).
    pub gz_tmp: std::path::PathBuf,
    /// The final compressed archive.
    pub final_path: std::path::PathBuf,
    pub phase: RotationPhase,
    pub started_at: DateTime<Utc>,
}
