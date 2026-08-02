//! Slot request / response JSON envelopes (`bellman-slot/1`).
//!
//! Tolerant reader: unknown fields are ignored. **Never** use
//! `deny_unknown_fields` on these types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Wire schema identifier.
pub const SCHEMA_V1: &str = "bellman-slot/1";

/// Slot operation (add / modify / delete a timer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotOperation {
    /// Create a timer owned by the requesting `app_name`.
    Add,
    /// Change a timer the same `app_name` created.
    Modify,
    /// Remove a timer the same `app_name` created.
    Delete,
}

impl SlotOperation {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Modify => "modify",
            Self::Delete => "delete",
        }
    }
}

impl std::fmt::Display for SlotOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Input envelope written by an integrating app (or free-slot stub).
///
/// Free stubs leave `request_id` / `operation` / `payload` as `None`.
/// A filled request always carries `request_id` + `operation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotRequest {
    /// Schema id; must start with `bellman-slot/` for major-version match.
    #[serde(default = "default_schema")]
    pub schema: String,
    /// Reserved free-slot id (e.g. `"0007"`). Filled by publish from the free stub.
    #[serde(default)]
    pub slot_id: String,
    /// Idempotency key (UUID string). `None` ⇒ empty free stub.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Producer timestamp (optional; ignored by processing).
    #[serde(default)]
    pub logged_at: Option<DateTime<Utc>>,
    /// `add` | `modify` | `delete`. `None` ⇒ empty free stub.
    #[serde(default)]
    pub operation: Option<SlotOperation>,
    /// Operation payload (app_name, timer fields, …). Tolerant: extra keys ok.
    #[serde(default)]
    pub payload: Option<Value>,
}

fn default_schema() -> String {
    SCHEMA_V1.to_string()
}

impl SlotRequest {
    /// Empty free-slot stub showing the v1 schema shape.
    pub fn free_stub(slot_id: impl Into<String>) -> Self {
        Self {
            schema: SCHEMA_V1.to_string(),
            slot_id: slot_id.into(),
            request_id: None,
            logged_at: None,
            operation: None,
            payload: Some(serde_json::json!({
                "app_name": null,
                "timer_name": null,
                "timer_id": null,
                "occurrence": null,
                "tz": null,
                "action": null
            })),
        }
    }

    /// True when this file is an empty pre-generated free stub (not a request).
    pub fn is_free_stub(&self) -> bool {
        self.request_id.is_none() || self.operation.is_none()
    }

    /// True when major schema is supported (`bellman-slot/1` and same major).
    pub fn schema_supported(&self) -> bool {
        self.schema == SCHEMA_V1
            || self.schema.starts_with("bellman-slot/1")
            || self.schema.starts_with("bellman-slot/")
                && self
                    .schema
                    .split('/')
                    .nth(1)
                    .and_then(|v| v.split('.').next())
                    .is_some_and(|maj| maj == "1")
    }
}

/// Typed view of the request payload (tolerant extraction from JSON object).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlotPayload {
    /// The requesting app. Becomes the timer's integration owner on `add`,
    /// and the ownership check on `modify` / `delete`.
    #[serde(default)]
    pub app_name: Option<String>,
    /// Display name; required for `add`.
    #[serde(default)]
    pub timer_name: Option<String>,
    /// Timer id for modify/delete (and optional fixed id for add).
    #[serde(default)]
    pub timer_id: Option<Uuid>,
    /// Alias accepted by the product brief (`id` from earlier output slot).
    #[serde(default)]
    pub id: Option<Uuid>,
    /// Full [`crate::occurrence::OccurrenceKind`] JSON or a simplified object.
    #[serde(default)]
    pub occurrence: Option<Value>,
    /// IANA timezone the occurrence's wall-clock times are in; UTC default.
    #[serde(default)]
    pub tz: Option<String>,
    /// Full [`crate::store::Action`] JSON.
    #[serde(default)]
    pub action: Option<Value>,
    /// Convenience fields for launch actions (PLAN input schema).
    #[serde(default)]
    pub launch_command: Option<String>,
    /// Arguments for `launch_command`, one per element. Never shell-split.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Working directory for `launch_command`.
    #[serde(default)]
    pub workdir: Option<String>,
    /// `"skip"` / `"coalesce"`, or the full policy object.
    #[serde(default)]
    pub misfire_policy: Option<Value>,
    /// Wall-clock time for the simplified occurrence forms.
    #[serde(default)]
    pub time: Option<String>,
    /// Period for the simplified `interval` form.
    #[serde(default)]
    pub every_secs: Option<u64>,
    /// Weekdays for the simplified `weekly` form.
    #[serde(default)]
    pub days: Option<Value>,
    /// Day of month for `monthly` / `yearly`.
    #[serde(default)]
    pub day: Option<u8>,
    /// Month for the simplified `yearly` form.
    #[serde(default)]
    pub month: Option<u8>,
    /// Expression for the simplified `cron` form.
    #[serde(default)]
    pub cron: Option<String>,
    /// Advance the durable un-acked run-event cursor through this sequence
    /// (inclusive). Only moves forward; requires ownership of the timer.
    #[serde(default)]
    pub ack_through: Option<u64>,
    /// IK6 delivery transport: `{ "mode": "auto" | "json" | "ipc" }`
    /// (a bare `"auto"`-style string is also accepted).
    #[serde(default)]
    pub transport: Option<Value>,
}

impl SlotPayload {
    /// Resolve the timer id from `timer_id` or `id`.
    pub fn resolved_timer_id(&self) -> Option<Uuid> {
        self.timer_id.or(self.id)
    }

    /// Parse payload from a free-form JSON value (ignores unknown keys).
    pub fn from_value(v: &Value) -> Result<Self, String> {
        serde_json::from_value(v.clone()).map_err(|e| e.to_string())
    }
}

/// Status written into the done/ output slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    /// The request was applied.
    Ok,
    /// It was rejected; `error` says why.
    Error,
}

impl SlotStatus {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// One un-acked run event in the output feed (monotonic `event_sequence`).
///
/// Until the JSONL event log lands, these are projected from the `runs` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlotRunEvent {
    /// Per-timer monotonic cursor; what `ack_through` advances past.
    pub event_sequence: u64,
    /// The firing this event belongs to.
    pub run_id: Uuid,
    /// Its timer.
    pub timer_id: Uuid,
    /// The instant it was meant to fire.
    pub scheduled_for: DateTime<Utc>,
    /// Run state from the one R5 vocabulary ([`crate::events::RunState`]) —
    /// the same strings the event log uses in `kind`.
    pub status: crate::events::RunState,
    /// When it actually fired.
    pub claimed_at: DateTime<Utc>,
    /// When it reached a terminal state, if it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

impl SlotRunEvent {
    /// Project a runs-ledger row onto the wire shape.
    ///
    /// The ledger's internal phase is delivery bookkeeping; the wire speaks
    /// R5: an unfinished claim is an open `fired` run, and a `finished` claim
    /// projects its recorded outcome — `wake_delivered`, `wake_failed`, or
    /// `skipped_misfire` (an overlap skip can never appear as delivered).
    /// The R5 states `completed` / `failed` are reserved for app reports
    /// (IK3) and are never invented from scheduler bookkeeping.
    pub fn from_claim(run: &crate::store::RunClaim) -> Self {
        use crate::store::{ClaimStatus, RunOutcome};
        let status = match (run.status, run.outcome) {
            (ClaimStatus::Pending | ClaimStatus::Active, _) => crate::events::RunState::Fired,
            (ClaimStatus::Finished, Some(RunOutcome::WakeDelivered)) => {
                crate::events::RunState::WakeDelivered
            }
            (ClaimStatus::Finished, Some(RunOutcome::SkippedMisfire)) => {
                crate::events::RunState::SkippedMisfire
            }
            // A finished row without a recorded outcome is never success.
            (ClaimStatus::Finished, Some(RunOutcome::WakeFailed) | None) => {
                crate::events::RunState::WakeFailed
            }
        };
        Self {
            event_sequence: run.event_sequence,
            run_id: run.run_id,
            timer_id: run.timer_id,
            scheduled_for: run.scheduled_for,
            status,
            claimed_at: run.claimed_at,
            completed_at: run.completed_at,
        }
    }
}

/// Output envelope written by Bellman into `done/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotResponse {
    /// Always [`SCHEMA_V1`].
    pub schema: String,
    /// The slot this answers, matching the request's filename.
    pub slot_id: String,
    /// Echo of the request's idempotency key.
    pub request_id: String,
    /// Whether the request was applied.
    pub status: SlotStatus,
    /// The timer created or addressed. This is the id to keep for later
    /// `modify` / `delete`.
    ///
    /// Absent — not `null` — on a rejection. `bellman-slot/1` is frozen, and
    /// the golden test below is what keeps this attribute attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_id: Option<Uuid>,
    /// When that timer next fires, so a producer need not compute it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_fire_at: Option<DateTime<Utc>>,
    /// Why it was rejected, when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Un-acknowledged run events with monotonic sequence (bounded).
    #[serde(default)]
    pub events: Vec<SlotRunEvent>,
}

impl SlotResponse {
    /// A successful response for an applied request.
    pub fn ok(
        slot_id: impl Into<String>,
        request_id: impl Into<String>,
        timer_id: Option<Uuid>,
        next_fire_at: Option<DateTime<Utc>>,
        events: Vec<SlotRunEvent>,
    ) -> Self {
        Self {
            schema: SCHEMA_V1.to_string(),
            slot_id: slot_id.into(),
            request_id: request_id.into(),
            status: SlotStatus::Ok,
            timer_id,
            next_fire_at,
            error: None,
            events,
        }
    }

    /// A rejection carrying the reason. Note this is a *response*: garbage
    /// that could not be parsed at all is quarantined instead, with a
    /// [`SlotErrSidecar`].
    pub fn err(
        slot_id: impl Into<String>,
        request_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            schema: SCHEMA_V1.to_string(),
            slot_id: slot_id.into(),
            request_id: request_id.into(),
            status: SlotStatus::Error,
            timer_id: None,
            next_fire_at: None,
            error: Some(error.into()),
            events: Vec::new(),
        }
    }
}

/// Quarantine sidecar written next to a bad input: `<name>.err.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotErrSidecar {
    /// Always [`SCHEMA_V1`].
    pub schema: String,
    /// The slot id, when the bad input carried a usable one.
    pub slot_id: Option<String>,
    /// Why the input was quarantined.
    pub reason: String,
    /// When it was quarantined.
    pub logged_at: DateTime<Utc>,
    /// The original filename, so the copy can be traced back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
}

impl SlotErrSidecar {
    /// Build a sidecar for a quarantined input.
    pub fn new(
        reason: impl Into<String>,
        slot_id: Option<String>,
        source_name: Option<String>,
    ) -> Self {
        Self {
            schema: SCHEMA_V1.to_string(),
            slot_id,
            reason: reason.into(),
            logged_at: Utc::now(),
            source_name,
        }
    }
}
