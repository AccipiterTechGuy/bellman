//! Event kinds and the self-contained JSONL line shape.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle event kinds written to the JSONL log.
///
/// String form matches the product vocabulary (`registered`, `fired`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Timer created (CLI / slot / GUI).
    Registered,
    /// Fire delivered on time (within ~1 s).
    Fired,
    /// Fire delivered late but still within grace.
    FiredLate,
    /// Missed fire dropped by misfire policy.
    SkippedMisfire,
    /// Multiple missed fires coalesced into one recovery delivery.
    Coalesced,
    /// Wake action (launch / notify / write-slot) succeeded.
    WakeDelivered,
    /// Wake action failed after retries (`FAILED` path).
    WakeFailed,
    /// Woken app did not ack within the grace window.
    NoAck,
    /// Pruner tombstone (elapsed one-shot removed / archive GC).
    Pruned,
    /// Jan-1 year consistency pass completed.
    YearRecalibrate,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Fired => "fired",
            Self::FiredLate => "fired_late",
            Self::SkippedMisfire => "skipped_misfire",
            Self::Coalesced => "coalesced",
            Self::WakeDelivered => "wake_delivered",
            Self::WakeFailed => "wake_failed",
            Self::NoAck => "no_ack",
            Self::Pruned => "pruned",
            Self::YearRecalibrate => "year_recalibrate",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One self-contained JSONL line.
///
/// Tolerant: unknown fields are ignored on read. Never use
/// `deny_unknown_fields` (BUILD_PLAN rule 7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    /// Wall-clock timestamp of the event (UTC).
    pub ts: DateTime<Utc>,
    /// Lifecycle kind.
    pub kind: EventKind,
    /// Stable event id (dedupe / correlation).
    pub event_id: Uuid,
    /// Related timer, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_id: Option<Uuid>,
    /// Related run claim, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
    /// Timer display name (denormalized for filtered tails).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_name: Option<String>,
    /// Scheduled fire time (UTC), when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_for: Option<DateTime<Utc>>,
    /// Lateness / duration in milliseconds (kind-dependent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Missed-count for coalesced fires, retry attempt, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Human / machine message (redacted — never secrets or full env).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Structured error string (redacted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Free-form extra fields (offset, status, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl EventRecord {
    /// Build a minimal event with a fresh `event_id` and `ts = now`.
    pub fn new(kind: EventKind) -> Self {
        Self {
            ts: Utc::now(),
            kind,
            event_id: Uuid::new_v4(),
            timer_id: None,
            run_id: None,
            timer_name: None,
            scheduled_for: None,
            duration_ms: None,
            count: None,
            message: None,
            error: None,
            detail: None,
        }
    }

    pub fn with_timer(mut self, id: Uuid, name: impl Into<String>) -> Self {
        self.timer_id = Some(id);
        self.timer_name = Some(name.into());
        self
    }

    pub fn with_run(mut self, run_id: Uuid) -> Self {
        self.run_id = Some(run_id);
        self
    }

    pub fn with_scheduled_for(mut self, when: DateTime<Utc>) -> Self {
        self.scheduled_for = Some(when);
        self
    }

    pub fn with_message(mut self, msg: impl Into<String>) -> Self {
        self.message = Some(msg.into());
        self
    }

    pub fn with_error(mut self, err: impl Into<String>) -> Self {
        self.error = Some(err.into());
        self
    }

    pub fn with_count(mut self, n: u32) -> Self {
        self.count = Some(n);
        self
    }

    pub fn with_duration_ms(mut self, ms: i64) -> Self {
        self.duration_ms = Some(ms);
        self
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }

    pub fn with_ts(mut self, ts: DateTime<Utc>) -> Self {
        self.ts = ts;
        self
    }
}
