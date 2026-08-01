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
    Parallel {
        /// Maximum runs in flight at once.
        cap: u32,
    },
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
        /// Program to run. No shell, so this is an executable, not a line.
        command: String,
        /// Arguments, one per element — never split from a string.
        #[serde(default)]
        args: Vec<String>,
        /// Working directory; the launcher's own when absent.
        #[serde(default)]
        workdir: Option<String>,
    },
    /// Desktop notification.
    Notify {
        /// Notification title; required, and the only part some desktops show.
        title: String,
        /// Notification body; empty is legal and renders as title-only.
        #[serde(default)]
        body: String,
    },
    /// No-op placeholder (useful in tests / disabled actions).
    #[default]
    None,
}

/// Per-timer delivery transport for fire notifications (IK6).
///
/// Both transports carry the same logical messages and hit the same ingest;
/// the choice is made per firing, recorded on the run, and never changes
/// mid-firing. `Auto` uses IPC when a client holding the timer is connected
/// at fire time, files otherwise. `Json` is the default: today's behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportMode {
    /// Files only (`slots/fires/` + reply stub) — the default.
    #[default]
    Json,
    /// Socket only; no client at fire time ⇒ the run ages to `no_ack`
    /// exactly like an unwatched folder. Never falls back to files.
    Ipc,
    /// IPC when a client holding this timer is connected at fire time,
    /// files otherwise; an unconfirmed IPC failure may fall back to files
    /// with the same `run_id` (`ipc_fallback` on the run).
    Auto,
}

impl TransportMode {
    /// The wire spelling stored on timers and runs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Ipc => "ipc",
            Self::Auto => "auto",
        }
    }

    /// Parse a stored/wire spelling; `None` for anything unrecognised so a
    /// future mode read by an old build degrades rather than panics.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "json" => Some(Self::Json),
            "ipc" => Some(Self::Ipc),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// Run-recorded transport values (IK6): `selected_transport` is the mode
/// chosen at fire (immutable mid-firing); `transport` is the effective
/// delivery, which only ever diverges as `ipc_fallback` (an `auto` run whose
/// unconfirmed IPC delivery failed over to files with the same `run_id`).
/// Delivered over the file adapter.
pub const TRANSPORT_JSON: &str = "json";
/// Delivered over the local socket.
pub const TRANSPORT_IPC: &str = "ipc";
/// Selected IPC, delivered over files after an unconfirmed socket failure.
pub const TRANSPORT_IPC_FALLBACK: &str = "ipc_fallback";

/// Fully loaded timer row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timer {
    /// Stable primary key; survives renames.
    pub id: TimerId,
    /// Display name. Not unique — resolve by id when it matters.
    pub name: String,
    /// `false` means paused: kept, listed, never scheduled.
    pub enabled: bool,
    /// The recurrence rule and its timezone.
    pub occurrence: Occurrence,
    /// Denormalized IANA tz name (mirrors `occurrence.tz`).
    pub tz: String,
    /// Next scheduled instant, recomputed lazily; `None` when exhausted.
    pub next_fire_utc: Option<DateTime<Utc>>,
    /// Last instant actually delivered — the ledger that stops re-firing.
    pub last_fired: Option<DateTime<Utc>>,
    /// What to do about fires missed while nothing was running.
    pub misfire: MisfirePolicy,
    /// What to do when a previous run is still in flight.
    pub overlap: OverlapPolicy,
    /// Retry schedule for a failed wake action.
    pub retry: RetryPolicy,
    /// Do not fire before this instant, if set.
    pub valid_from: Option<DateTime<Utc>>,
    /// Do not fire after this instant, if set.
    pub valid_until: Option<DateTime<Utc>>,
    /// Stop after this many delivered runs, if set.
    pub max_runs: Option<u64>,
    /// Free-form labels; Bellman never interprets them.
    pub tags: Vec<String>,
    /// What happens when this timer fires.
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
    /// Delivery transport for fire notifications (IK6). Default `json`.
    #[serde(default)]
    pub transport: TransportMode,
}

/// Input for [`super::Store::create_timer`].
#[derive(Debug, Clone)]
pub struct NewTimer {
    /// Optional fixed id (tests); random UUID when `None`.
    pub id: Option<TimerId>,
    /// Display name.
    pub name: String,
    /// Start scheduled, or start paused.
    pub enabled: bool,
    /// The recurrence rule and its timezone.
    pub occurrence: Occurrence,
    /// Missed-fire policy; [`NewTimer::new`] picks the product default.
    pub misfire: MisfirePolicy,
    /// Behaviour when a previous run is still in flight.
    pub overlap: OverlapPolicy,
    /// Retry schedule for a failed wake action.
    pub retry: RetryPolicy,
    /// Free-form labels.
    pub tags: Vec<String>,
    /// What happens when it fires.
    pub action: Action,
    /// Seed last_fired (usually `None` for new timers).
    pub last_fired: Option<DateTime<Utc>>,
    /// Execution jitter amplitude in seconds (default 0).
    pub jitter_secs: u32,
    /// Optional per-timer accuracy slack override.
    pub accuracy_slack_secs: Option<u32>,
    /// Participate in single-next-wake election (default false).
    pub wake_machine: bool,
    /// Delivery transport for fire notifications (default `json`).
    pub transport: TransportMode,
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
            transport: TransportMode::default(),
        }
    }

    /// Set the execution-jitter amplitude, in seconds either way. Spreads a
    /// fleet of identical timers so they do not all fire on the same second.
    pub fn with_jitter(mut self, jitter_secs: u32) -> Self {
        self.jitter_secs = jitter_secs;
        self
    }
}

/// Partial update applied under optimistic revision check.
#[derive(Debug, Clone, Default)]
pub struct TimerPatch {
    /// Rename.
    pub name: Option<String>,
    /// Pause or resume.
    pub enabled: Option<bool>,
    /// Replace the recurrence rule.
    pub occurrence: Option<Occurrence>,
    /// Replace the missed-fire policy.
    pub misfire: Option<MisfirePolicy>,
    /// Replace the overlap policy.
    pub overlap: Option<OverlapPolicy>,
    /// Replace the retry schedule.
    pub retry: Option<RetryPolicy>,
    /// Replace the whole tag list.
    pub tags: Option<Vec<String>>,
    /// Replace the action outright — `Action::None` clears it.
    pub action: Option<Action>,
    /// Rewrite the fire ledger. Doubly wrapped on purpose: the outer
    /// `Some` means "change it", the inner `None` means "clear it".
    pub last_fired: Option<Option<DateTime<Utc>>>,
    /// Replace the jitter amplitude.
    pub jitter_secs: Option<u32>,
    /// Replace the per-timer accuracy slack; inner `None` restores the
    /// global default.
    pub accuracy_slack_secs: Option<Option<u32>>,
    /// Include or exclude this timer from the wake-from-sleep election.
    pub wake_machine: Option<bool>,
    /// Change the delivery transport; applies from the next firing.
    pub transport: Option<TransportMode>,
}

/// Optimistic update envelope.
#[derive(Debug, Clone)]
pub struct TimerUpdate {
    /// Timer to patch.
    pub id: TimerId,
    /// The revision the caller last read; a mismatch is `StaleRevision`
    /// rather than a lost update.
    pub expected_revision: i64,
    /// The fields to change.
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
    /// The stored spelling of this phase.
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
    /// The stored spelling, shared with the R5 event-log vocabulary.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WakeDelivered => "wake_delivered",
            Self::WakeFailed => "wake_failed",
            Self::SkippedMisfire => "skipped_misfire",
        }
    }

    /// Parse a stored spelling; `None` rather than a panic for a value a
    /// newer build wrote.
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
    /// The producer's idempotency key. A retry with the same value returns
    /// this record instead of creating a second timer.
    pub request_id: String,
    /// The reserved slot the request was published under.
    pub slot_id: String,
    /// `add` | `modify` | `delete`.
    pub operation: String,
    /// The requesting app, when it named itself.
    pub app_name: Option<String>,
    /// The timer created or addressed, when there was one.
    pub timer_id: Option<TimerId>,
    /// `"ok"` or `"error"` (mirrors the output-slot status).
    pub status: String,
    /// Full serialized slot response JSON (`SlotResponse`).
    pub response_json: String,
    /// When the request was applied.
    pub created_at: DateTime<Utc>,
}

/// One row of the runs claim ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunClaim {
    /// Identity of this firing; the key everything else joins on.
    pub run_id: Uuid,
    /// The timer that fired.
    pub timer_id: TimerId,
    /// The instant it was *meant* to fire — an intent, not an occurrence.
    pub scheduled_for: DateTime<Utc>,
    /// Where the claim sits in the dispatcher.
    pub status: ClaimStatus,
    /// When the claim row was committed, which is the fire instant.
    pub claimed_at: DateTime<Utc>,
    /// When it reached a terminal status.
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
    /// The run whose notification this publishes.
    pub run_id: Uuid,
    /// Its timer — the routing key for the IPC adapter.
    pub timer_id: TimerId,
    /// Delivery adapter for this projection (IK6): `file` (the JSON folder
    /// adapter) or `ipc` (the socket adapter). The payload is the deterministic
    /// per-adapter encoding of the same logical fire message; an `auto`
    /// fallback rewrites `kind`/`target_path`/`payload` in place — the run and
    /// its identity fields never change.
    #[serde(default = "TransportProjection::default_kind")]
    pub kind: String,
    /// Immutable absolute target path (`slots/fires/fire-<run_id>.json`, or a
    /// configured fixed name under `fires/` used as an at-least-once wake hint;
    /// `ipc://<timer_id>` for an IPC-delivered run).
    pub target_path: String,
    /// Serialized `FireNotification` JSON (the exact bytes to publish).
    pub payload: String,
    /// Database-wide monotonic order assigned in the fire transaction.
    pub publication_order: u64,
    /// `pending` | `published` | `obsolete` | `picked_up`.
    pub state: String,
    /// Bounded-retry bookkeeping for the publication pump.
    pub attempts: u32,
    /// Earliest instant the pump may try again.
    pub next_attempt_at: DateTime<Utc>,
    /// When the fire transaction wrote this row.
    pub created_at: DateTime<Utc>,
    /// When delivery succeeded.
    pub published_at: Option<DateTime<Utc>>,
}

impl TransportProjection {
    /// Not delivered yet; the pump owns it.
    pub const PENDING: &'static str = "pending";
    /// Handed to the adapter successfully.
    pub const PUBLISHED: &'static str = "published";
    /// A newer firing superseded it; never publish.
    pub const OBSOLETE: &'static str = "obsolete";
    /// The app confirmed — the transport is settled, retries stop.
    pub const PICKED_UP: &'static str = "picked_up";
    /// Adapter kinds (IK6).
    pub const KIND_FILE: &'static str = "file";
    /// The socket adapter.
    pub const KIND_IPC: &'static str = "ipc";

    fn default_kind() -> String {
        Self::KIND_FILE.to_string()
    }
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
    /// The stored spelling, shown in the GUI as `failed · reported` or
    /// `failed · timed out`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::TimedOut => "timed_out",
        }
    }

    /// Parse a stored spelling; `None` for anything unrecognised.
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
    /// The firing this row describes.
    pub run_id: Uuid,
    /// Its timer.
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
    /// When the app said it had picked the run up.
    pub acknowledged_at: Option<DateTime<Utc>>,
    /// The app's own estimate of how long it will take. Drives the GUI's
    /// overdue label and, with `error_detection`, the watchdog deadline.
    pub expected_secs: Option<u64>,
    /// Accumulated `error_detection` (`None` = never mentioned).
    pub error_detection: Option<bool>,
    /// Last liveness ping. Never logged — the live view is the only place
    /// heartbeats exist.
    pub heartbeat_at: Option<DateTime<Utc>>,
    /// Free-text progress the app is showing; also never logged.
    pub progress: Option<String>,
    /// When the app reported success.
    pub completed_at: Option<DateTime<Utc>>,
    /// When the run was recorded as failed.
    pub failed_at: Option<DateTime<Utc>>,
    /// The app's failure text, capped at 1 KB.
    pub reason: Option<String>,
    /// Who decided it failed — the app, or the watchdog.
    pub failure_kind: Option<FailureKind>,
    /// App result, capped at 32 KB as stored (`result_truncated` flags it).
    pub result_json: Option<serde_json::Value>,
    /// Set when `result_json` was trimmed to fit the cap.
    pub result_truncated: bool,
    /// Persisted wall-clock watchdog deadline (restart recovery only).
    pub watchdog_deadline: Option<DateTime<Utc>>,
    /// When the pickup grace lapsed with nothing heard. Retained even after
    /// a late reply revises the state, so the whole story stays readable.
    pub no_ack_at: Option<DateTime<Utc>>,
    /// Digest of the last accepted reply — an exact duplicate is a no-op.
    pub reply_digest: Option<String>,
    /// Transition lines already in the event log (log records transitions
    /// only; repeated writes inside a state append nothing).
    pub acknowledged_logged: bool,
    /// Whether the `running` transition has already been logged.
    pub running_logged: bool,
    /// The transport mode selected at fire (IK6: `json` | `ipc`) — fixed for
    /// the firing, never changes mid-firing. `None` on rows predating IK6.
    #[serde(default)]
    pub selected_transport: Option<String>,
    /// The effective delivery transport (`json` | `ipc` | `ipc_fallback`).
    /// Starts equal to `selected_transport`; an `auto` run whose unconfirmed
    /// IPC delivery fell back to files records `ipc_fallback` here.
    #[serde(default)]
    pub transport: Option<String>,
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
            selected_transport: None,
            transport: None,
        }
    }

    /// Stamp the firing's selected transport (IK6): both the immutable
    /// selection and the effective delivery start at the same value; only an
    /// `auto` fallback later moves `transport` to `ipc_fallback`.
    pub fn with_transport(mut self, selected: &str) -> Self {
        self.selected_transport = Some(selected.to_string());
        self.transport = Some(selected.to_string());
        self
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
    /// Store schema version; drives migrations.
    pub schema_version: i32,
    /// When the pruner last ran — the startup catch-up test.
    pub last_prune: Option<DateTime<Utc>>,
    /// When the Jan-1 consistency pass last ran.
    pub last_recalibration: Option<DateTime<Utc>>,
    /// The tzdata version in force when the store was last written, so a
    /// zone-rule update is detectable rather than silent.
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
    /// The spelling stored in the journal row.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Renamed => "renamed",
            Self::Finalized => "finalized",
            Self::SourceRemoved => "source_removed",
        }
    }

    /// Parse a stored phase; `None` for anything unrecognised.
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
    /// How far the interrupted rotation got.
    pub phase: RotationPhase,
    /// When the rotation began.
    pub started_at: DateTime<Utc>,
}
