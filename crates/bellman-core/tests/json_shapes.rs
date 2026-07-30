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

use bellman_core::actions::{WriteSlotPayload, FIRE_SCHEMA_V1};
use bellman_core::events::{EventKind, EventRecord, EVENT_SCHEMA_V1};
use bellman_core::slots::{
    SlotErrSidecar, SlotRequest, SlotResponse, SlotRunEvent, SCHEMA_V1,
};
use bellman_core::store::{ClaimStatus, RunClaim};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use uuid::Uuid;

fn fixed() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 30, 12, 0, 0).unwrap()
}

fn sample_event_record() -> EventRecord {
    EventRecord::new(EventKind::Fired)
        .with_logged_at(fixed())
        .with_timer(Uuid::nil(), "tick")
        .with_run(Uuid::nil())
        .with_scheduled_for(fixed())
        .with_duration_ms(3)
        .with_count(1)
        .with_message("m")
        .with_detail(serde_json::json!({"offset": 1}))
}

fn sample_fire_payload() -> WriteSlotPayload {
    WriteSlotPayload {
        schema: FIRE_SCHEMA_V1.to_string(),
        kind: "fired".into(),
        timer_id: Uuid::nil(),
        timer_name: "tick".into(),
        run_id: Uuid::nil(),
        scheduled_for: fixed(),
        fired_at: fixed(),
        occurrence_kind: "on_time".into(),
    }
}

fn sample_run_claim(status: ClaimStatus) -> RunClaim {
    RunClaim {
        run_id: Uuid::nil(),
        timer_id: Uuid::nil(),
        scheduled_for: fixed(),
        status,
        claimed_at: fixed(),
        completed_at: match status {
            ClaimStatus::Claimed => None,
            ClaimStatus::Completed => Some(fixed()),
        },
        event_sequence: 7,
    }
}

fn sample_slot_response() -> SlotResponse {
    SlotResponse::ok(
        "0001",
        "550e8400-e29b-41d4-a716-446655440000",
        Some(Uuid::nil()),
        Some(fixed()),
        vec![SlotRunEvent::from_claim(&sample_run_claim(ClaimStatus::Claimed))],
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
            "WriteSlotPayload (fire notification)",
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
    assert_eq!(FIRE_SCHEMA_V1, "bellman-fire/1");
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
    assert_eq!(v["occurrence_kind"], "on_time");

    // R5 run-state vocabulary on the run-event feed.
    assert_eq!(
        SlotRunEvent::from_claim(&sample_run_claim(ClaimStatus::Claimed)).status,
        "fired",
        "an open claimed run is `fired` in the R5 vocabulary"
    );
    assert_eq!(
        SlotRunEvent::from_claim(&sample_run_claim(ClaimStatus::Completed)).status,
        "completed"
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
    assert_eq!(back.kind, EventKind::Fired);
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
    assert_eq!(back.events[0].status, "fired");

    // SlotErrSidecar.
    let sidecar = SlotErrSidecar::new("bad json", Some("0001".into()), None);
    let mut v = serde_json::to_value(&sidecar).unwrap();
    v["future_field"] = serde_json::json!(false);
    let back: SlotErrSidecar = serde_json::from_value(v).unwrap();
    assert_eq!(back.reason, "bad json");

    // WriteSlotPayload (fire notification).
    let payload = sample_fire_payload();
    let mut v = serde_json::to_value(&payload).unwrap();
    v["future_field"] = serde_json::json!("ignored");
    let back: WriteSlotPayload = serde_json::from_value(v).unwrap();
    assert_eq!(back.schema, FIRE_SCHEMA_V1);
    assert_eq!(back.kind, "fired");
    assert_eq!(back.occurrence_kind, "on_time");
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
        "run_id",
        "scheduled_for",
        "fired_at",
        "occurrence_kind",
    ] {
        assert!(obj.contains_key(key), "fire notification missing `{key}`: {v}");
    }
    assert_eq!(obj.len(), 8, "unexpected extra keys in fire notification: {v}");
}
