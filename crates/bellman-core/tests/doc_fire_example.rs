//! SHIP1-A golden fixture: the fire-notification example in
//! `docs/INTEGRATION.md` (§ *Step 1 — notice a fire*) must match, field for
//! field, what the real producer serialises. This is the anti-drift pin for
//! the two documentation defects SHIP1 fixed:
//!
//! - A1: the example used the removed `bellman-fire/1` schema tag — the
//!   producer writes `bellman-slot/1` (`FIRE_SCHEMA_V1`);
//! - A2: the example branched on an `occurrence_kind` vocabulary
//!   (`on_time` / `late` / …) that never existed on the wire — the producer
//!   passes the timer's **recurrence type** (`occurrence.kind().kind_label()`).
//!
//! If either the doc example or the serialised shape drifts, this test fails.

use bellman_core::reply::{FireNotification, FIRE_SCHEMA_V1};
use chrono::{TimeZone, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

fn integration_doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/INTEGRATION.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Extract the first ```json fenced block inside the named section.
fn json_block_in_section(doc: &str, section_heading: &str) -> String {
    let start = doc
        .find(section_heading)
        .unwrap_or_else(|| panic!("section `{section_heading}` not found in INTEGRATION.md"));
    let rest = &doc[start..];
    let fence_start = rest
        .find("```json\n")
        .unwrap_or_else(|| panic!("no ```json block in section `{section_heading}`"))
        + "```json\n".len();
    let after = &rest[fence_start..];
    let fence_end = after
        .find("```")
        .expect("unterminated ```json block in INTEGRATION.md");
    after[..fence_end].to_string()
}

/// The documented example, rebuilt through the real producer type with the
/// same concrete values the doc shows.
fn documented_notification() -> FireNotification {
    FireNotification::new(
        "fired",
        "interval",
        Uuid::parse_str("3f1a2b9c-5e7d-4c1a-9f2e-8b6d4a2c1e09").unwrap(),
        "demo-wake",
        "demo-app",
        Uuid::parse_str("9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08").unwrap(),
        Utc.with_ymd_and_hms(2026, 7, 28, 8, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 7, 28, 8, 0, 1).unwrap(),
        PathBuf::from("/home/you/.bellman/timers/demo-wake-3f1a/status.json"),
        Some(PathBuf::from(
            "/home/you/.bellman/timers/demo-wake-3f1a/reply-9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08.json",
        )),
        None,
    )
}

#[test]
fn documented_fire_example_matches_producer_field_for_field() {
    let doc = integration_doc();
    let block = json_block_in_section(&doc, "### Step 1 — notice a fire");
    let documented: Value = serde_json::from_str(&block)
        .unwrap_or_else(|e| panic!("Step 1 fire example is not valid JSON: {e}\n{block}"));
    let produced = serde_json::to_value(documented_notification()).unwrap();
    assert_eq!(
        documented, produced,
        "docs/INTEGRATION.md Step 1 fire example has drifted from the wire shape"
    );
}

#[test]
fn documented_vocabularies_are_the_wire_vocabularies() {
    let n = serde_json::to_value(documented_notification()).unwrap();
    // A1: the schema tag is the slot schema; the legacy `bellman-fire/1`
    // WriteSlotPayload duplicate was removed by SCH1 and must stay gone.
    assert_eq!(n["schema"], FIRE_SCHEMA_V1);
    assert_eq!(FIRE_SCHEMA_V1, "bellman-slot/1");
    // A2: `occurrence_kind` is the recurrence type…
    assert!(
        matches!(
            n["occurrence_kind"].as_str(),
            Some("once" | "interval" | "daily" | "weekly" | "monthly" | "yearly" | "cron")
        ),
        "occurrence_kind must be a recurrence type: {n}"
    );
    // …and the firing's timing rides in `kind` (the event vocabulary).
    assert!(
        matches!(n["kind"].as_str(), Some("fired" | "fired_late" | "coalesced")),
        "kind must be an event kind: {n}"
    );
}

#[test]
fn integration_doc_contains_no_legacy_fire_schema() {
    let doc = integration_doc();
    assert!(
        !doc.contains("bellman-fire"),
        "docs/INTEGRATION.md still references the removed bellman-fire/1 schema"
    );
    // And the dead vocabulary from A2 must not come back either.
    for dead in ["on_time", "catch_up_"] {
        assert!(
            !doc.contains(dead),
            "docs/INTEGRATION.md references the dead occurrence_kind vocabulary `{dead}`"
        );
    }
}
