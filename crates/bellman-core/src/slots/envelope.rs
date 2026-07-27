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
    Add,
    Modify,
    Delete,
}

impl SlotOperation {
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
    pub ts: Option<DateTime<Utc>>,
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
            ts: None,
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
    #[serde(default)]
    pub app_name: Option<String>,
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
    #[serde(default)]
    pub tz: Option<String>,
    /// Full [`crate::store::Action`] JSON.
    #[serde(default)]
    pub action: Option<Value>,
    /// Convenience fields for launch actions (PLAN input schema).
    #[serde(default)]
    pub launch_command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub misfire_policy: Option<Value>,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub every_secs: Option<u64>,
    #[serde(default)]
    pub days: Option<Value>,
    #[serde(default)]
    pub day: Option<u8>,
    #[serde(default)]
    pub month: Option<u8>,
    #[serde(default)]
    pub cron: Option<String>,
    /// Advance the durable un-acked run-event cursor through this sequence
    /// (inclusive). Only moves forward; requires ownership of the timer.
    #[serde(default)]
    pub ack_through: Option<u64>,
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
    Ok,
    Error,
}

impl SlotStatus {
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
    pub event_sequence: u64,
    pub run_id: Uuid,
    pub timer_id: Uuid,
    pub scheduled_for: DateTime<Utc>,
    pub status: String,
    pub claimed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// Output envelope written by Bellman into `done/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotResponse {
    pub schema: String,
    pub slot_id: String,
    pub request_id: String,
    pub status: SlotStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_fire: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Un-acknowledged run events with monotonic sequence (bounded).
    #[serde(default)]
    pub events: Vec<SlotRunEvent>,
}

impl SlotResponse {
    pub fn ok(
        slot_id: impl Into<String>,
        request_id: impl Into<String>,
        timer_id: Option<Uuid>,
        next_fire: Option<DateTime<Utc>>,
        events: Vec<SlotRunEvent>,
    ) -> Self {
        Self {
            schema: SCHEMA_V1.to_string(),
            slot_id: slot_id.into(),
            request_id: request_id.into(),
            status: SlotStatus::Ok,
            timer_id,
            next_fire,
            error: None,
            events,
        }
    }

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
            next_fire: None,
            error: Some(error.into()),
            events: Vec::new(),
        }
    }
}

/// Quarantine sidecar written next to a bad input: `<name>.err.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotErrSidecar {
    pub schema: String,
    pub slot_id: Option<String>,
    pub reason: String,
    pub ts: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
}

impl SlotErrSidecar {
    pub fn new(
        reason: impl Into<String>,
        slot_id: Option<String>,
        source_name: Option<String>,
    ) -> Self {
        Self {
            schema: SCHEMA_V1.to_string(),
            slot_id,
            reason: reason.into(),
            ts: Utc::now(),
            source_name,
        }
    }
}
