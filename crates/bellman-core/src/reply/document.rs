//! The `bellman-reply/1` document — the only file an integrating app writes.
//!
//! The app reads the pre-filled stub, sets what changed, and writes it back
//! atomically. It never composes `schema`, `run_id` or `app_name` itself.
//! Only `state` is required from the app; every other field is optional and
//! accumulates on Bellman's side (a later write that omits a field never
//! retracts it).
//!
//! A reply is DATA, never a command (R9): parsing and validation here must
//! never launch, execute, schedule or modify anything.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

/// Wire schema identifier carried by every reply (R1).
pub const REPLY_SCHEMA_V1: &str = "bellman-reply/1";

/// Whole-file cap (R12): over it the body is never read — a bounded
/// metadata-only diagnostic goes to `bad/` and the file is left in place.
pub const MAX_REPLY_FILE_BYTES: u64 = 64 * 1024;
/// `result` cap as stored in `status.json` (R12): truncate, never reject.
pub const MAX_RESULT_STATUS_BYTES: usize = 32 * 1024;
/// `result` cap as carried on the log event (R12): truncate, never reject.
pub const MAX_RESULT_EVENT_BYTES: usize = 2 * 1024;
/// `reason` / `progress` free-text cap (R12): truncate, never reject.
pub const MAX_FREE_TEXT_BYTES: usize = 1024;

/// One parsed reply. All fields optional at the serde layer — semantic
/// requirements (schema/run_id/app_name/state) are validated on sight by the
/// ingest engine, never by debounce. Unknown fields are ignored (R6).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReplyDocument {
    pub schema: Option<String>,
    pub run_id: Option<Uuid>,
    pub app_name: Option<String>,
    pub state: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub expected_secs: Option<u64>,
    pub error_detection: Option<bool>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub progress: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<serde_json::Value>,
    pub failed_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    /// Present on the Bellman-written stub; how the transport tells
    /// "stub, untouched" (`state: null`) from an invalid stateless reply.
    pub hint: Option<String>,
}

/// Why a reply was refused. Semantic rejections are decidable on sight —
/// they are never debounced and never quarantined more than once per
/// distinct content (the transport owns that idempotence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyRejection {
    /// No `schema` field — not a reply at all.
    MissingSchema,
    /// A `schema` other than `bellman-reply/1`.
    BadSchema,
    /// No `run_id` — cannot be matched to a run.
    MissingRunId,
    /// No `app_name` — ownership cannot be verified.
    MissingAppName,
    /// `app_name` differs from the owner snapshotted on the run.
    WrongAppName,
    /// No `state` — the one field the app must set.
    MissingState,
    /// A state an app may never write (`fired`, `no_ack`, `cancelled`, …).
    ReservedState,
    /// `error_detection: true` without a positive accumulated `expected_secs`.
    ErrorDetectionWithoutEstimate,
    /// Moving an app-authored terminal verdict back to a non-terminal state.
    TerminalRegression,
    /// A `run_id` unknown to this timer entirely (garbage / tampering).
    UnknownRun,
}

impl ReplyRejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingSchema => "missing schema",
            Self::BadSchema => "unexpected schema",
            Self::MissingRunId => "missing run_id",
            Self::MissingAppName => "missing app_name",
            Self::WrongAppName => "app_name does not match the run's owner",
            Self::MissingState => "missing state",
            Self::ReservedState => "state is reserved to Bellman",
            Self::ErrorDetectionWithoutEstimate => {
                "error_detection requires a positive expected_secs"
            }
            Self::TerminalRegression => "terminal report cannot move back to non-terminal",
            Self::UnknownRun => "unknown run_id",
        }
    }
}

/// The pre-filled stub Bellman creates at fire time (T0). `state: null` is
/// how Bellman tells "stub, untouched" from "the app answered"; the app edits
/// this document and never reconstructs it.
pub fn stub_bytes(run_id: Uuid, app_name: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "schema": REPLY_SCHEMA_V1,
        "run_id": run_id,
        "app_name": app_name,
        "state": null,
        "hint": "set state to acknowledged | running | completed | failed",
    }))
    .unwrap_or_default()
}

/// Truncate a free-text field to its R12 cap (on char boundaries).
pub fn truncate_text(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut end = cap;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Cap a `result` value for storage / logging (R12). Returns the value
/// unchanged when it fits; otherwise the head of its serialization as a
/// plain string plus the truncation flag — bounded and honest, never a
/// rejection.
pub fn cap_result(value: &serde_json::Value, cap: usize) -> (serde_json::Value, bool) {
    let serialized = serde_json::to_string(value).unwrap_or_default();
    if serialized.len() <= cap {
        return (value.clone(), false);
    }
    (serde_json::Value::String(truncate_text(&serialized, cap)), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_is_prefilled_and_parseable() {
        let run_id = Uuid::new_v4();
        let bytes = stub_bytes(run_id, "lightbulb");
        let doc: ReplyDocument = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc.schema.as_deref(), Some(REPLY_SCHEMA_V1));
        assert_eq!(doc.run_id, Some(run_id));
        assert_eq!(doc.app_name.as_deref(), Some("lightbulb"));
        assert_eq!(doc.state, None);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.get("hint").is_some());
    }

    #[test]
    fn unknown_fields_ignored() {
        let doc: ReplyDocument = serde_json::from_str(
            r#"{"schema":"bellman-reply/1","state":"running","future_field":{"x":1}}"#,
        )
        .unwrap();
        assert_eq!(doc.state.as_deref(), Some("running"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "abécd";
        assert_eq!(truncate_text(s, 4), "abé");
        assert_eq!(truncate_text(s, 3), "ab");
        assert_eq!(truncate_text(s, 99), s);
    }

    #[test]
    fn cap_result_truncates_to_string_head() {
        let small = serde_json::json!({"ok": true});
        let (v, truncated) = cap_result(&small, 1024);
        assert!(!truncated);
        assert_eq!(v, small);

        let big = serde_json::json!({"blob": "x".repeat(10_000)});
        let (v, truncated) = cap_result(&big, 100);
        assert!(truncated);
        assert!(v.as_str().unwrap().len() <= 100);
    }
}
