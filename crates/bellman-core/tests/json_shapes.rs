//! IK1 exit-gate tests: the normalised JSON shapes
//! (docs/todo/json_normalization.md, rules R1–R3 + R5).
//!
//! These tests enumerate every JSON document Bellman emits onto a wire an
//! integrator reads — the event log, the slot channel (request stubs,
//! responses, quarantine sidecars), and the fire notification — and prove:
//!
//! 1. every document carries `schema` (R1);
//! 2. top-level `kind` always means the event kind (R2);
//! 3. every timestamp field ends `_at`, except `scheduled_for` (R3);
//! 4. run states come from the one R5 vocabulary;
//! 5. shapes round-trip and unknown fields are still ignored on read (R6).

use bellman_core::reply::{FireNotification, FIRE_SCHEMA_V1};
use bellman_core::events::{RunState, EventRecord, EVENT_SCHEMA_V1};
use bellman_core::slots::{
    SlotErrSidecar, SlotRequest, SlotResponse, SlotRunEvent, SCHEMA_V1,
};
use bellman_core::store::{ClaimStatus, RunClaim, RunOutcome};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use uuid::Uuid;

fn fixed() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 30, 12, 0, 0).unwrap()
}

fn sample_event_record() -> EventRecord {
    EventRecord::new(RunState::Fired)
        .with_logged_at(fixed())
        .with_timer(Uuid::nil(), "tick")
        .with_run(Uuid::nil())
        .with_scheduled_for(fixed())
        .with_duration_ms(3)
        .with_count(1)
        .with_message("m")
        .with_detail(serde_json::json!({"offset": 1}))
}

fn sample_fire_payload() -> FireNotification {
    FireNotification::new(
        "fired",
        "daily",
        Uuid::nil(),
        "tick",
        "app",
        Uuid::nil(),
        fixed(),
        fixed(),
        std::path::PathBuf::from("/data/timers/tick/status.json"),
        Some(std::path::PathBuf::from("/data/timers/tick/reply-run.json")),
        None,
    )
}

fn sample_run_claim(status: ClaimStatus, outcome: Option<RunOutcome>) -> RunClaim {
    RunClaim {
        run_id: Uuid::nil(),
        timer_id: Uuid::nil(),
        scheduled_for: fixed(),
        status,
        claimed_at: fixed(),
        completed_at: match status {
            ClaimStatus::Pending | ClaimStatus::Active => None,
            ClaimStatus::Finished => Some(fixed()),
        },
        event_sequence: 7,
        outcome,
        outcome_reason: None,
        cancel_requested: false,
    }
}

fn sample_slot_response() -> SlotResponse {
    SlotResponse::ok(
        "0001",
        "550e8400-e29b-41d4-a716-446655440000",
        Some(Uuid::nil()),
        Some(fixed()),
        vec![SlotRunEvent::from_claim(&sample_run_claim(ClaimStatus::Pending, None))],
    )
}

/// One serialized sample per JSON emitter Bellman owns.
fn emitted_shapes() -> Vec<(&'static str, Value)> {
    vec![
        (
            "EventRecord (event log line)",
            serde_json::to_value(sample_event_record()).unwrap(),
        ),
        (
            "FireNotification (fire notification)",
            serde_json::to_value(sample_fire_payload()).unwrap(),
        ),
        (
            "SlotRequest (free stub)",
            serde_json::to_value(SlotRequest::free_stub("0001")).unwrap(),
        ),
        (
            "SlotResponse (done/ output)",
            serde_json::to_value(sample_slot_response()).unwrap(),
        ),
        (
            "SlotErrSidecar (quarantine)",
            serde_json::to_value(SlotErrSidecar::new("bad json", Some("0001".into()), None))
                .unwrap(),
        ),
    ]
}

#[test]
fn every_emitted_json_carries_schema() {
    for (name, v) in emitted_shapes() {
        let schema = v
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: missing top-level `schema`: {v}"));
        assert!(
            schema.starts_with("bellman-"),
            "{name}: schema must be a bellman wire id, got {schema:?}"
        );
    }
    // Exact ids per channel (R1).
    assert_eq!(sample_event_record().schema, EVENT_SCHEMA_V1);
    assert_eq!(EVENT_SCHEMA_V1, "bellman-event/1");
    // The fire notification under slots/fires/ is the IK3 FireNotification
    // (the legacy bellman-fire/1 WriteSlotPayload duplicate was removed by SCH1).
    assert_eq!(FIRE_SCHEMA_V1, "bellman-slot/1");
    assert_eq!(SCHEMA_V1, "bellman-slot/1");
}

#[test]
fn top_level_kind_is_always_the_event_kind() {
    // Event log: kind is the lifecycle event kind.
    let v = serde_json::to_value(sample_event_record()).unwrap();
    assert_eq!(v["kind"], "fired");

    // Fire notification: kind is the event kind, occurrence type moved out.
    let v = serde_json::to_value(sample_fire_payload()).unwrap();
    assert!(
        matches!(v["kind"].as_str(), Some("fired" | "fired_late" | "coalesced")),
        "fire-notification top-level kind must be an event kind: {v}"
    );
    assert_eq!(v["occurrence_kind"], "daily");
    assert_eq!(v["schema"], FIRE_SCHEMA_V1);

    // R5 run-state vocabulary on the run-event feed: the ledger's internal
    // bookkeeping projects onto the same `RunState` the event log uses, and
    // never invents the app-reported `completed` / `failed`.
    assert_eq!(
        SlotRunEvent::from_claim(&sample_run_claim(ClaimStatus::Pending, None)).status,
        RunState::Fired,
        "an unfinished (pending) run is `fired` in the R5 vocabulary"
    );
    assert_eq!(
        SlotRunEvent::from_claim(&sample_run_claim(ClaimStatus::Active, None)).status,
        RunState::Fired,
        "an executing (active) run is still an open `fired` run"
    );
    assert_eq!(
        SlotRunEvent::from_claim(&sample_run_claim(
            ClaimStatus::Finished,
            Some(RunOutcome::WakeDelivered)
        ))
        .status,
        RunState::WakeDelivered,
        "finished+wake_delivered means Bellman delivered the wake action, not an app outcome"
    );
    assert_eq!(
        SlotRunEvent::from_claim(&sample_run_claim(
            ClaimStatus::Finished,
            Some(RunOutcome::WakeFailed)
        ))
        .status,
        RunState::WakeFailed,
        "a failed wake action projects as wake_failed, never as success"
    );
    assert_eq!(
        SlotRunEvent::from_claim(&sample_run_claim(
            ClaimStatus::Finished,
            Some(RunOutcome::SkippedMisfire)
        ))
        .status,
        RunState::SkippedMisfire,
        "an overlap skip projects as skipped_misfire, never as wake_delivered"
    );
}

/// Recursively collect (path, key, value) for every object member.
fn walk<'a>(v: &'a Value, path: &str, out: &mut Vec<(String, String, &'a Value)>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                let p = format!("{path}.{k}");
                out.push((p.clone(), k.clone(), val));
                walk(val, &p, out);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk(item, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_timestamp_field_ends_with_at() {
    for (name, v) in emitted_shapes() {
        let mut members = Vec::new();
        walk(&v, "$", &mut members);
        for (path, key, val) in &members {
            assert_ne!(key, "ts", "{name}: legacy `ts` key at {path}");
            assert_ne!(key, "next_fire", "{name}: legacy `next_fire` key at {path}");
            let Some(s) = val.as_str() else { continue };
            if DateTime::parse_from_rfc3339(s).is_ok() {
                assert!(
                    key.ends_with("_at") || key == "scheduled_for",
                    "{name}: timestamp field `{key}` at {path} must end `_at` \
                     (only `scheduled_for` is exempt — it is an intent)"
                );
            }
        }
    }
}

#[test]
fn shapes_round_trip_and_ignore_unknown_fields() {
    // EventRecord.
    let rec = sample_event_record();
    let mut v = serde_json::to_value(&rec).unwrap();
    v["future_field"] = serde_json::json!({"nested": true});
    let back: EventRecord = serde_json::from_value(v).unwrap();
    assert_eq!(back.schema, EVENT_SCHEMA_V1);
    assert_eq!(back.kind, RunState::Fired);
    assert_eq!(back.logged_at, fixed());

    // SlotRequest (filled request, not just the stub).
    let mut v = serde_json::to_value(SlotRequest::free_stub("0002")).unwrap();
    v["future_field"] = serde_json::json!(123);
    let back: SlotRequest = serde_json::from_value(v).unwrap();
    assert_eq!(back.slot_id, "0002");
    assert!(back.is_free_stub());

    // SlotResponse including the nested run events.
    let resp = sample_slot_response();
    let mut v = serde_json::to_value(&resp).unwrap();
    v["future_field"] = serde_json::json!("ignored");
    let back: SlotResponse = serde_json::from_value(v).unwrap();
    assert_eq!(back.schema, SCHEMA_V1);
    assert_eq!(back.next_fire_at, Some(fixed()));
    assert_eq!(back.events.len(), 1);
    assert_eq!(back.events[0].status, RunState::Fired);

    // SlotErrSidecar.
    let sidecar = SlotErrSidecar::new("bad json", Some("0001".into()), None);
    let mut v = serde_json::to_value(&sidecar).unwrap();
    v["future_field"] = serde_json::json!(false);
    let back: SlotErrSidecar = serde_json::from_value(v).unwrap();
    assert_eq!(back.reason, "bad json");

    // FireNotification (fire notification).
    let payload = sample_fire_payload();
    let mut v = serde_json::to_value(&payload).unwrap();
    v["future_field"] = serde_json::json!("ignored");
    let back: FireNotification = serde_json::from_value(v).unwrap();
    assert_eq!(back.schema, FIRE_SCHEMA_V1);
    assert_eq!(back.kind, "fired");
    assert_eq!(back.occurrence_kind, "daily");
    assert_eq!(back.fired_at, fixed());
}

#[test]
fn fire_notification_wire_keys() {
    let v = serde_json::to_value(sample_fire_payload()).unwrap();
    let obj = v.as_object().unwrap();
    for key in [
        "schema",
        "kind",
        "timer_id",
        "timer_name",
        "app_name",
        "run_id",
        "scheduled_for",
        "fired_at",
        "occurrence_kind",
        "status_path",
        "reply_path",
    ] {
        assert!(obj.contains_key(key), "fire notification missing `{key}`: {v}");
    }
    assert_eq!(obj.len(), 11, "unexpected extra keys in fire notification: {v}");
}

#[test]
fn event_record_requires_schema() {
    // R1 clean break, no shim: a schema-less line is rejected (the tolerant
    // reader then skips it) — never silently relabelled as the current
    // version, so consumers can version-check.
    let mut v = serde_json::to_value(sample_event_record()).unwrap();
    v.as_object_mut().unwrap().remove("schema");
    assert!(
        serde_json::from_value::<EventRecord>(v).is_err(),
        "EventRecord without `schema` must not deserialize"
    );
}

#[test]
fn run_state_is_the_one_r5_vocabulary() {
    // Every R5 state, Bellman-written and app-written, lives in the single
    // `RunState` enum shared by the event log and the slot run-event feed.
    let cases = [
        (RunState::Registered, "registered"),
        (RunState::Fired, "fired"),
        (RunState::FiredLate, "fired_late"),
        (RunState::SkippedMisfire, "skipped_misfire"),
        (RunState::Coalesced, "coalesced"),
        (RunState::WakeDelivered, "wake_delivered"),
        (RunState::WakeFailed, "wake_failed"),
        (RunState::NoAck, "no_ack"),
        (RunState::Pruned, "pruned"),
        (RunState::YearRecalibrate, "year_recalibrate"),
        (RunState::WakeCapability, "wake_capability"),
        (RunState::Acknowledged, "acknowledged"),
        (RunState::Running, "running"),
        (RunState::Completed, "completed"),
        (RunState::Failed, "failed"),
    ];
    for (state, name) in cases {
        assert_eq!(state.as_str(), name);
        let v = serde_json::to_value(state).unwrap();
        assert_eq!(v, name);
        let back: RunState = serde_json::from_value(v).unwrap();
        assert_eq!(back, state);
    }
}
