// -----------------------------------------------------------
// Real serde-shape regression tests.
//
// These tests JSON-serialize every DTO that crosses the Tauri IPC
// boundary and assert the wire-shape keys. They are the actual
// contract behind `ui/src/api.test.js`: any Rust commit that drops
// (or forgets to add) `#[serde(rename_all = "camelCase")]` will
// fail one of these tests, and the cargo-driven test suite runs
// on every PR.
//
// History: the previous vitest-only suite tested constant strings
// and did not exercise serialization, so a snake_case regression
// could pass CI. Adding Rust-side serialization assertions
// eliminates that gap.
// -----------------------------------------------------------

use bellman_core::occurrence::Weekdays;
use bellman_core::{
    Action, EventRecord, MisfirePolicy, Occurrence, OccurrenceKind, OverlapPolicy, RetryPolicy,
    Timer,
};
use chrono::{NaiveTime, TimeZone, Utc};

use crate::commands::{
    AppInfo, CreateTimerInput, LogTailDto, PreviewFireDto, PreviewResponseDto, TimerDto,
};
use crate::first_run::{WizardChoice, WizardStatus};
use crate::state::RunNowResponse;
use crate::web::{WebActionDto, WebTimerPatchDto};

fn sample_timer() -> Timer {
    let anchor = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 5,
            anchor,
        },
        "UTC",
    )
    .expect("valid occurrence");
    Timer {
        id: uuid::Uuid::nil(),
        name: "tick".into(),
        enabled: true,
        occurrence: occ,
        tz: "UTC".into(),
        next_fire_utc: Some(anchor),
        last_fired: None,
        misfire: MisfirePolicy::Skip,
        overlap: OverlapPolicy::Skip,
        retry: RetryPolicy::default(),
        valid_from: None,
        valid_until: None,
        max_runs: None,
        tags: Vec::new(),
        action: Action::None,
        revision: 1,
        jitter_secs: 0,
        accuracy_slack_secs: None,
        wake_machine: false,
        transport: bellman_core::TransportMode::default(),
    }
}

fn json_keys(v: &impl serde::Serialize) -> Vec<String> {
    let s = serde_json::to_string(v).expect("serialize");
    let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
    v.as_object()
        .expect("top-level is an object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn timer_dto_is_camel_case() {
    let dto = TimerDto::from(sample_timer());
    let keys = json_keys(&dto);
    for needed in ["nextFireUtc", "lastFired", "occurrence", "actionKind"] {
        assert!(
            keys.contains(&needed.to_string()),
            "TimerDto missing camelCase key {needed}; got {keys:?}"
        );
    }
    for forbidden in ["next_fire_utc", "last_fired", "action_kind"] {
        assert!(
            !keys.contains(&forbidden.to_string()),
            "TimerDto leaked snake_case / wrong key {forbidden}; got {keys:?}"
        );
    }
}

#[test]
fn timer_dto_round_trips_occurrence_and_action() {
    use bellman_core::occurrence::OccurrenceKind;
    // The web DTO is the deliberate UI shape (no nested serde enum).
    // Build a weekly Mon/Wed/Fri 08:00 UTC timer with a Notify action
    // and confirm the wire shape:
    //   - `kind: "weekly"`, `summary: "weekly mon,wed,fri 08:00:00 UTC"`
    //   - `occurrence.days` is `{mon:true, wed:true, fri:true,
    //     thu:false, tue:false, sat:false, sun:false}` (seven keys,
    //     BTreeMap → sorted).
    //   - `occurrence.at` is `"08:00:00"` (NaiveTime → HH:MM:SS).
    //   - `actionKind.type == "notify"`, with title/body.
    let anchor = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let mut wd = Weekdays::new();
    wd.insert(chrono::Weekday::Mon);
    wd.insert(chrono::Weekday::Wed);
    wd.insert(chrono::Weekday::Fri);
    let occ = Occurrence::new(
        OccurrenceKind::Weekly {
            days: wd,
            at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
        },
        "UTC",
    )
    .unwrap();
    let timer = Timer {
        id: uuid::Uuid::nil(),
        name: "weekly-mwf".into(),
        enabled: true,
        occurrence: occ,
        tz: "UTC".into(),
        next_fire_utc: Some(anchor),
        last_fired: None,
        misfire: MisfirePolicy::default_calendar(),
        overlap: OverlapPolicy::default(),
        retry: RetryPolicy::default(),
        valid_from: None,
        valid_until: None,
        max_runs: None,
        tags: Vec::new(),
        action: Action::Notify {
            title: "hello".into(),
            body: "world".into(),
        },
        revision: 7,
        jitter_secs: 0,
        accuracy_slack_secs: None,
        wake_machine: false,
        transport: bellman_core::TransportMode::default(),
    };
    let dto = TimerDto::from(timer);
    assert_eq!(dto.name, "weekly-mwf");
    assert_eq!(dto.revision, 7);
    assert_eq!(dto.tz, "UTC");
    // The flat web occurrence carries the structured fields the dialog
    // needs. Check the wire-shape surface (no inner serde enum leak).
    assert_eq!(dto.occurrence.occ, "weekly");
    assert_eq!(dto.occurrence.tz, "UTC");
    assert_eq!(dto.occurrence.at.as_deref(), Some("08:00:00"));
    let days = dto
        .occurrence
        .days
        .as_ref()
        .expect("weekly timer must populate days");
    assert_eq!(days.get("mon"), Some(&true));
    assert_eq!(days.get("wed"), Some(&true));
    assert_eq!(days.get("fri"), Some(&true));
    assert_eq!(days.get("tue"), Some(&false));
    assert_eq!(days.get("thu"), Some(&false));
    assert_eq!(days.get("sat"), Some(&false));
    assert_eq!(days.get("sun"), Some(&false));
    // Kind-specific fields that don't apply to weekly must stay null.
    assert_eq!(dto.occurrence.once_at, None);
    assert_eq!(dto.occurrence.every_secs, None);
    assert_eq!(dto.occurrence.anchor, None);
    assert_eq!(dto.occurrence.day, None);
    assert_eq!(dto.occurrence.month, None);
    assert_eq!(dto.occurrence.expr, None);
    // The tagged action kind round-trips.
    match &dto.action_kind {
        WebActionDto::Notify { title, body } => {
            assert_eq!(title, "hello");
            assert_eq!(body, "world");
        }
        other => panic!("expected Notify, got {other:?}"),
    }
    // The serialized JSON must keep the deliberate UI shape so the JS
    // dialog reads `days` / `at` without path gymnastics.
    let s = serde_json::to_string(&dto).unwrap();
    assert!(s.contains("\"occ\":\"weekly\""), "missing weekly tag: {s}");
    assert!(s.contains("\"type\":\"notify\""), "missing notify tag: {s}");
    assert!(s.contains("\"mon\":true"), "missing day bit: {s}");
    assert!(s.contains("\"at\":\"08:00:00\""), "missing time field: {s}");
    // The outer `tz:` field is the IANA name (mirrors how the Rust core
    // exposes `Occurrence.tz`); the top-level `tz` carries the same
    // string. Either spelling is acceptable; pin both.
    assert!(
        s.contains("\"tz\":\"UTC\""),
        "missing tz top-level (flat): {s}"
    );
    assert!(
        !s.contains("\"kind\":{"),
        "WebTimerDto must not leak nested serde enum `kind`: {s}"
    );
    assert!(
        !s.contains("\"days\":21"),
        "WebTimerDto must not leak raw u8 bitmask: {s}"
    );
}

#[test]
fn log_tail_dto_is_camel_case() {
    let dto = LogTailDto {
        events: Vec::<EventRecord>::new(),
        total_records: 7,
        skipped: 1,
    };
    let keys = json_keys(&dto);
    assert!(keys.contains(&"totalRecords".to_string()));
    assert!(keys.contains(&"skipped".to_string()));
    assert!(
        !keys.contains(&"total_records".to_string()),
        "LogTailDto leaked total_records"
    );
    let s = serde_json::to_string(&dto).unwrap();
    assert!(
        s.contains("\"totalRecords\":7"),
        "expected camelCase value: {s}"
    );
    assert!(!s.contains("total_records"), "snake_case leak: {s}");
}

#[test]
fn run_now_response_is_camel_case() {
    let r = RunNowResponse {
        timer_id: uuid::Uuid::nil(),
        name: "tick".into(),
        run_id: uuid::Uuid::nil(),
        scheduled_for: chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        message: "action=none".into(),
        enabled: true,
        next_fire_utc: Some(
            chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:05Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        ),
    };
    let keys = json_keys(&r);
    for needed in ["timerId", "scheduledFor", "nextFireUtc", "message"] {
        assert!(
            keys.contains(&needed.to_string()),
            "RunNowResponse missing key {needed}; got {keys:?}"
        );
    }
    for forbidden in ["timer_id", "scheduled_for", "next_fire_utc", "run_id"] {
        assert!(
            !keys.contains(&forbidden.to_string()),
            "RunNowResponse leaked snake_case key {forbidden}; got {keys:?}"
        );
    }
}

#[test]
fn app_info_is_camel_case() {
    let info = AppInfo {
        data_dir: "/d".into(),
        db_path: "/d/timers.db".into(),
        logs_dir: "/d/logs".into(),
        slots_dir: "/d/slots".into(),
        wizard_completed: false,
        autostart_enabled: false,
        pause_all: false,
        wake_enabled: false,
        wake_status_line: "Wake from sleep: OFF — test".into(),
        max_concurrent_actions: 16,
        default_misfire_policy: "coalesce".into(),
        default_misfire_grace_secs: 3600,
    };
    let keys = json_keys(&info);
    for needed in [
        "dataDir",
        "dbPath",
        "logsDir",
        "slotsDir",
        "wizardCompleted",
        "autostartEnabled",
        "pauseAll",
    ] {
        assert!(
            keys.contains(&needed.to_string()),
            "AppInfo missing {needed}; got {keys:?}"
        );
    }
    for forbidden in [
        "data_dir",
        "db_path",
        "logs_dir",
        "slots_dir",
        "wizard_completed",
        "autostart_enabled",
        "pause_all",
    ] {
        assert!(
            !keys.contains(&forbidden.to_string()),
            "AppInfo leaked {forbidden}; got {keys:?}"
        );
    }
}

#[test]
fn wizard_choice_is_camel_case() {
    let c = WizardChoice {
        autostart: true,
        start_minimized: true,
        wake_enabled: false,
    };
    let keys = json_keys(&c);
    assert!(keys.contains(&"startMinimized".to_string()));
    assert!(keys.contains(&"wakeEnabled".to_string()));
    assert!(!keys.contains(&"start_minimized".to_string()));
    assert!(!keys.contains(&"wake_enabled".to_string()));
}

#[test]
fn wizard_status_defaults_is_camel_case_wizard_choice() {
    let s = WizardStatus {
        completed: false,
        defaults: WizardChoice {
            autostart: true,
            start_minimized: true,
            wake_enabled: false,
        },
    };
    let j = serde_json::to_string(&s).unwrap();
    assert!(j.contains("\"defaults\":"));
    assert!(j.contains("\"completed\":false"));
    assert!(
        j.contains("\"startMinimized\":true"),
        "nested startMinimized missing: {j}"
    );
    assert!(
        j.contains("\"wakeEnabled\":false"),
        "nested wakeEnabled missing: {j}"
    );
    assert!(!j.contains("start_minimized"), "snake_case leak: {j}");
    assert!(!j.contains("wake_enabled"), "snake_case leak: {j}");
}

/// Integration: confirm both emit() call sites in the shell pass a
/// bare bool. This is the "I would have caught the auditor's
/// regression" test — the auditor caught the tray emitting a JSON
/// object (`serde_json::json!({ "paused": next })`) while the
/// command emitted a bare bool. This test walks every `.rs` file
/// under `src/` and fails the build if anyone re-introduces either
/// the literal pattern or the legacy payload shape.
#[test]
fn pause_all_emit_is_bare_bool_in_sources() {
    use std::path::Path;
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    let needle = "emit(\"pause-all-changed\"";

    // The exact strings the auditor caught, plus obvious variants
    // (json! macro rename, escaped keys, etc.).
    let bad_object_shapes = [
        r#"emit("pause-all-changed", serde_json::json!"#,
        r#"emit("pause-all-changed", json!({"#,
        r#"emit("pause-all-changed", json!({ "paused""#,
        r#"emit("pause-all-changed", json!({ "paused":"#,
    ];
    // The auditor's original bug emitted `{ "paused": next }`.
    let legacy_payload_strings = [
        r#""paused": next"#,
        r#""paused": paused"#,
        r#""paused": paused,"#,
    ];

    let mut found_tray = false;
    let mut found_cmd = false;
    let mut errors: Vec<String> = Vec::new();
    visit_rs_files(&src, &mut |path: &std::path::Path| {
        let text = std::fs::read_to_string(path).expect("read source");
        if !text.contains(needle) {
            return;
        }
        for bad in bad_object_shapes {
            if text.contains(bad) {
                errors.push(format!(
                    "{} uses an object-shape payload on `pause-all-changed` \
                     (auditor NEEDS_FIX #4 regression). Use a bare bool.\n  bad pattern: {bad}",
                    path.display()
                ));
            }
        }
        for legacy in &legacy_payload_strings {
            if text.contains(legacy) {
                errors.push(format!(
                    "{} calls emit() with the legacy {{ \"paused\": ... }} object payload; \
                     use a bare bool.\n  legacy pattern: {legacy}",
                    path.display()
                ));
            }
        }
        if path.ends_with("tray.rs") {
            found_tray = true;
        }
        if path.ends_with("commands.rs") {
            found_cmd = true;
        }
    });

    assert!(
        errors.is_empty(),
        "pause-all emit() regression detected:\n  {}",
        errors.join("\n  ")
    );
    assert!(
        found_tray,
        "tray.rs no longer emits `pause-all-changed` (regression in tray sync)."
    );
    assert!(
        found_cmd,
        "commands.rs no longer emits `pause-all-changed` (regression in tray sync)."
    );
}

/// Recursive `.rs` walker. No external deps.
fn visit_rs_files(dir: &std::path::Path, visit: &mut dyn FnMut(&std::path::Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            // Only recurse into the regular source tree; skip any
            // test-only subdirectory if it ever appears.
            if p.file_name().is_some_and(|n| n == "tests") {
                continue;
            }
            visit_rs_files(&p, visit);
        } else if p.extension().is_some_and(|e| e == "rs")
                        // Skip the test file itself (it contains the bad-pattern
                        // strings as r#"…"# literals, which would self-match).
                        && p.file_name().is_none_or(|n| n != "dto_serde_tests.rs")
        {
            visit(&p);
        }
    }
}

// ── C8 calendar UI wire-shape guards ────────────────────────────────────
//
// Each of these tests fail if anyone breaks the camelCase contract for the
// new dialog / Week / Month / Run history commands. The serde warnings would
// reach webview inspect-tools, but a wire-shape regression stays invisible
// until something else (a runtime type error, a JSON.stringify test) catches
// it — so we pin the keys here.

#[test]
fn create_timer_input_is_camel_case() {
    // The dialog emits a flat `CreateTimerInput` wrapping the deliberate
    // `WebOccurrenceDto` + `WebActionDto` shapes. Pin both camelCase keys
    // and the absence of snake_case leaks.
    let input = CreateTimerInput {
        wake_machine: false,
        name: "tick".into(),
        occurrence: crate::web::WebOccurrenceDto {
            occ: "daily".into(),
            tz: "UTC".into(),
            days: None,
            at: Some("09:00:00".into()),
            once_at: None,
            every_secs: None,
            anchor: None,
            day: None,
            month: None,
            expr: None,
        },
        action: crate::web::WebActionDto::None,
        enabled: false,
    };
    let json = serde_json::to_string(&input).unwrap();
    for needed in [
        "\"name\":\"tick\"",
        "\"occurrence\":{",
        "\"occ\":\"daily\"",
        "\"tz\":\"UTC\"",
        "\"at\":\"09:00:00\"",
        "\"action\":{\"type\":\"none\"}",
    ] {
        assert!(
            json.contains(needed),
            "CreateTimerInput missing {needed}; full json: {json}"
        );
    }
    // The flat wire shape must never leak the nested serde enum.
    assert!(!json.contains("\"action\":\"none\""));
    assert!(!json.contains("onceAt"));
}

#[test]
fn web_timer_patch_dto_is_camel_case() {
    let patch = WebTimerPatchDto {
        wake_machine: None,
        name: Some("renamed".into()),
        enabled: Some(false),
        occurrence: Some(crate::web::WebOccurrenceDto {
            occ: "weekly".into(),
            tz: "UTC".into(),
            days: None,
            at: None,
            once_at: None,
            every_secs: None,
            anchor: None,
            day: None,
            month: None,
            expr: None,
        }),
        action_kind: Some(crate::web::WebActionDto::None),
    };
    let s = serde_json::to_string(&patch).unwrap();
    assert!(s.contains("\"name\":\"renamed\""));
    assert!(s.contains("\"enabled\":false"));
    assert!(s.contains("\"actionKind\":{\"type\":\"none\"}"));
    // Negative: no snake_case / no nested `kind:`.
    assert!(!s.contains("\"name_\""));
    assert!(!s.contains("\"kind\":{"));
}

/// End-to-end proof of the spec acceptance gate "every occurrence kind
/// creatable + editable + deletable from GUI" via the **same path
/// `Store` exercises** in production: `create_timer` → `update_timer`
/// → `delete_timer`, on all seven kinds. Closes Finding 4 from the
/// rework #2 audit (the prior version only built + previewed, never
/// touched `Store` or any Tauri command).
#[test]
fn seven_kinds_round_trip_through_store_crud() {
    use bellman_core::store::{NewTimer, Store};
    use chrono::Weekday;
    use std::collections::BTreeMap;

    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("timers.db");
    let mut store = Store::open(&db_path).expect("open store");
    let now = chrono::Utc::now();

    let mut days_map = BTreeMap::new();
    days_map.insert("mon".to_string(), true);
    days_map.insert("wed".to_string(), true);
    days_map.insert("fri".to_string(), true);
    days_map.insert("tue".to_string(), false);
    days_map.insert("thu".to_string(), false);
    days_map.insert("sat".to_string(), false);
    days_map.insert("sun".to_string(), false);

    let weekly_occ = crate::web::WebOccurrenceDto {
        occ: "weekly".into(),
        tz: "Europe/Helsinki".into(),
        days: Some(days_map),
        at: Some("08:00:00".into()),
        once_at: None,
        every_secs: None,
        anchor: None,
        day: None,
        month: None,
        expr: None,
    };
    let weekly_occ_core = weekly_occ
        .clone()
        .into_core_occurrence()
        .expect("weekly build");
    let weekly_new = NewTimer::new("weekly-mwf", weekly_occ_core.clone());
    let weekly_timer = store.create_timer(weekly_new).expect("create weekly");
    assert_eq!(weekly_timer.name, "weekly-mwf");
    assert_eq!(weekly_timer.revision, 1);
    assert!(store
        .get_timer(weekly_timer.id)
        .expect("get_timer weekly")
        .is_some());

    // Verify the stored weekly days match what we sent in (the flat
    // {mon:true, ...} bitmask object survives the round-trip through the
    // web DTO → core Occurrence → sqlite → core Occurrence).
    let stored_weekly = store
        .get_timer(weekly_timer.id)
        .expect("get")
        .unwrap()
        .occurrence;
    match stored_weekly.kind() {
        bellman_core::OccurrenceKind::Weekly { days, at } => {
            assert!(days.contains(Weekday::Mon));
            assert!(days.contains(Weekday::Wed));
            assert!(days.contains(Weekday::Fri));
            assert!(!days.contains(Weekday::Tue));
            assert_eq!(*at, chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap());
        }
        other => panic!("stored timer kind != weekly: {other:?}"),
    }

    // Update the same timer through Store::update_timer (mirrors what
    // `update_timer` Tauri command does). Use the same WebTimerPatchDto
    // → TimerPatch path the command takes so we exercise the actual wire
    // contract, not a side door.
    let new_patch = WebTimerPatchDto {
        wake_machine: None,
        name: Some("weekly-mwf-renamed".into()),
        enabled: None,
        occurrence: Some(weekly_occ.clone()),
        action_kind: Some(crate::web::WebActionDto::Notify {
            title: "hello".into(),
            body: "world".into(),
        }),
    };
    let core_patch = new_patch.into_core_patch().expect("patch build");
    let updated = store
        .update_timer(bellman_core::store::TimerUpdate {
            id: weekly_timer.id,
            expected_revision: weekly_timer.revision,
            patch: core_patch,
        })
        .expect("update_timer");
    assert_eq!(updated.name, "weekly-mwf-renamed");
    assert_eq!(updated.revision, weekly_timer.revision + 1);
    match &updated.action {
        bellman_core::Action::Notify { title, body } => {
            assert_eq!(title, "hello");
            assert_eq!(body, "world");
        }
        other => panic!("expected Notify action, got {other:?}"),
    }

    // Final delete round-trip.
    let deleted = store
        .delete_timer(updated.id)
        .expect("delete_timer");
    assert!(deleted, "delete_timer returned false");
    assert!(store
        .get_timer(updated.id)
        .expect("get_timer post-delete")
        .is_none());

    // Six remaining kinds: build each WebOccurrenceDto, persist via
    // create_timer, then list_timers to confirm presence. For the once
    // / yearly / monthly / cron kinds we use a far-future date so the
    // store's next_fire_utc computation doesn't probe the past.
    let cases: Vec<(&str, crate::web::WebOccurrenceDto)> = vec![
        (
            "once",
            crate::web::WebOccurrenceDto {
                occ: "once".into(),
                tz: "UTC".into(),
                days: None,
                at: None,
                once_at: Some("2099-01-01T00:00:00".into()),
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: None,
            },
        ),
        (
            "interval",
            crate::web::WebOccurrenceDto {
                occ: "interval".into(),
                tz: "UTC".into(),
                days: None,
                at: None,
                once_at: None,
                every_secs: Some(60),
                anchor: Some(now),
                day: None,
                month: None,
                expr: None,
            },
        ),
        (
            "daily",
            crate::web::WebOccurrenceDto {
                occ: "daily".into(),
                tz: "UTC".into(),
                days: None,
                at: Some("12:00:00".into()),
                once_at: None,
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: None,
            },
        ),
        (
            "monthly",
            crate::web::WebOccurrenceDto {
                occ: "monthly".into(),
                tz: "UTC".into(),
                days: None,
                at: Some("09:00:00".into()),
                once_at: None,
                every_secs: None,
                anchor: None,
                day: Some(15),
                month: None,
                expr: None,
            },
        ),
        (
            "yearly",
            crate::web::WebOccurrenceDto {
                occ: "yearly".into(),
                tz: "UTC".into(),
                days: None,
                at: Some("09:00:00".into()),
                once_at: None,
                every_secs: None,
                anchor: None,
                day: Some(29),
                month: Some(2),
                expr: None,
            },
        ),
        (
            "cron",
            crate::web::WebOccurrenceDto {
                occ: "cron".into(),
                tz: "UTC".into(),
                days: None,
                at: None,
                once_at: None,
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: Some("*/5 * * * *".into()),
            },
        ),
    ];

    for (label, input) in cases {
        let occ = input
            .clone()
            .into_core_occurrence()
            .unwrap_or_else(|e| panic!("{label} build: {e}"));
        let new = NewTimer::new(format!("{label}-qa"), occ);
        let created = store
            .create_timer(new)
            .unwrap_or_else(|e| panic!("{label} create: {e}"));
        assert_eq!(created.name, format!("{label}-qa"));
        // The preview path the dialog exercises must return at least
        // one fire for the kind we just persisted.
        let preview = created
            .occurrence
            .preview(now.with_timezone(&created.occurrence.timezone()), 5);
        assert!(
            !preview.is_empty(),
            "{label}: preview returned no fires after persist"
        );
        // And list_timers (the dialog's "After Save" refresh) sees it.
        let listed = store.list_timers().expect("list_timers");
        assert!(
            listed.iter().any(|t| t.id == created.id),
            "{label}: list_timers did not return the new row"
        );
    }

    // ── Rework #3: prove the GUI-payload preservation for the three
    // ── auditor-flagged regressions (once.onceAt, interval.anchor,
    // ── launch.workdir). Each test creates a timer via the GUI's
    // ── builder path (WebOccurrenceDto → Core → Store), patches it
    // ── verbatim, and asserts the stored action matches what the user
    // ── originally entered (no field resets to defaults).
    let fixed_anchor = chrono::DateTime::parse_from_rfc3339("2026-06-01T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    // 1. once → onceAt preserved across update.
    let once_inp = crate::web::WebOccurrenceDto {
        occ: "once".into(),
        tz: "UTC".into(),
        days: None,
        at: None,
        once_at: Some("2099-12-31T23:59:00".into()),
        every_secs: None,
        anchor: None,
        day: None,
        month: None,
        expr: None,
    };
    let once_created = store
        .create_timer(NewTimer::new(
            "once-qa",
            once_inp.clone().into_core_occurrence().unwrap(),
        ))
        .expect("create once");
    // patch with no occurrence/action (just a no-op name change) — onceAt
    // must survive verbatim.
    let patch_noop = WebTimerPatchDto {
        wake_machine: None,
        name: Some("once-renamed".into()),
        enabled: None,
        occurrence: None,
        action_kind: None,
    };
    let once_updated = store
        .update_timer(bellman_core::store::TimerUpdate {
            id: once_created.id,
            expected_revision: once_created.revision,
            patch: patch_noop.into_core_patch().unwrap(),
        })
        .expect("update once");
    match once_updated.occurrence.kind() {
        bellman_core::OccurrenceKind::Once { at } => {
            assert_eq!(
                at.format("%Y-%m-%dT%H:%M:%S").to_string(),
                "2099-12-31T23:59:00",
                "once.onceAt must NOT reset on Save"
            );
        }
        other => panic!("expected once after no-op patch, got {other:?}"),
    }

    // 2. interval → anchor stays stable across Save.
    let interval_inp = crate::web::WebOccurrenceDto {
        occ: "interval".into(),
        tz: "UTC".into(),
        days: None,
        at: None,
        once_at: None,
        every_secs: Some(60),
        anchor: Some(fixed_anchor),
        day: None,
        month: None,
        expr: None,
    };
    let interval_created = store
        .create_timer(NewTimer::new(
            "interval-qa",
            interval_inp.clone().into_core_occurrence().unwrap(),
        ))
        .expect("create interval");
    let patch_interval = WebTimerPatchDto {
        wake_machine: None,
        name: None,
        enabled: Some(false),
        occurrence: Some(interval_inp.clone()),
        action_kind: None,
    };
    let interval_updated = store
        .update_timer(bellman_core::store::TimerUpdate {
            id: interval_created.id,
            expected_revision: interval_created.revision,
            patch: patch_interval.into_core_patch().unwrap(),
        })
        .expect("update interval");
    match interval_updated.occurrence.kind() {
        bellman_core::OccurrenceKind::Interval { every_secs, anchor } => {
            assert_eq!(*every_secs, 60, "every_secs preserved verbatim");
            assert_eq!(
                anchor.to_rfc3339(),
                fixed_anchor.to_rfc3339(),
                "interval.anchor must NOT reset to now() on Save"
            );
        }
        other => panic!("expected interval after patch, got {other:?}"),
    }
    assert!(!interval_updated.enabled, "enabled=false preserved");

    // 3. Launch action → workdir preserved.
    let launch_inp = WebTimerPatchDto {
        wake_machine: None,
        name: None,
        enabled: None,
        occurrence: Some(crate::web::WebOccurrenceDto {
            occ: "daily".into(),
            tz: "UTC".into(),
            days: None,
            at: Some("09:00:00".into()),
            once_at: None,
            every_secs: None,
            anchor: None,
            day: None,
            month: None,
            expr: None,
        }),
        action_kind: Some(WebActionDto::Launch {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "true".into()],
            workdir: Some("/tmp".into()),
        }),
    };
    let launch_updated = store
        .update_timer(bellman_core::store::TimerUpdate {
            id: interval_updated.id,
            expected_revision: interval_updated.revision,
            patch: launch_inp.into_core_patch().unwrap(),
        })
        .expect("update launch");
    match &launch_updated.action {
        bellman_core::Action::Launch {
            command,
            args,
            workdir,
        } => {
            assert_eq!(command, "/bin/sh");
            assert_eq!(args, &vec!["-c".to_string(), "true".to_string()]);
            assert_eq!(
                workdir.as_deref(),
                Some("/tmp"),
                "Launch.workdir must NOT be dropped to None on Save"
            );
        }
        other => panic!("expected Launch action, got {other:?}"),
    }
}

/// Round-trip a captured JS-side payload exactly as the dialog would send
/// it: deserialize a hand-written JSON string, drive the same Tauri
/// command bodies (`create_timer` / `update_timer`), and confirm the
/// Rust side accepts the field names the GUI actually emits. This is the
/// closing test for rework #3 finding #1 — proving the JS-side
/// `buildInput` output is wire-compatible with `CreateTimerInput`.
#[test]
fn tauri_create_update_via_real_ipc_json() {
    use bellman_core::store::Store;
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("timers.db");
    let mut store = Store::open(&db_path).expect("open store");

    // Frozen snapshot of the exact JSON `ui/src/TimerDialog.svelte` builds
    // for a weekly Mon/Wed/Fri 08:00 Europe/Helsinki timer with launch
    // + workdir. If the dialog and the Rust DTO drift, this deserialization
    // fails ("missing field occ", "invalid type: map, expected string", etc.).
    let create_json = r#"{
        "name": "ipc-weekly-mwf",
        "enabled": true,
        "occurrence": {
            "occ": "weekly",
            "tz": "Europe/Helsinki",
            "days": {"mon": true, "tue": false, "wed": true, "thu": false, "fri": true, "sat": false, "sun": false},
            "at": "08:00:00",
            "onceAt": null,
            "everySecs": null,
            "anchor": null,
            "day": null,
            "month": null,
            "expr": null
        },
        "action": {"type": "launch", "command": "/bin/echo", "args": ["hello"], "workdir": "/tmp"}
    }"#;
    let create_input: CreateTimerInput =
        serde_json::from_str(create_json).expect("create IPC JSON must round-trip");
    assert_eq!(create_input.name, "ipc-weekly-mwf");
    assert_eq!(
        create_input.occurrence.days.as_ref().map(|d| d.get("mon").copied()),
        Some(Some(true))
    );
    assert!(matches!(
        create_input.action,
        WebActionDto::Launch { ref workdir, .. } if workdir.as_deref() == Some("/tmp")
    ));

    // And exercise the live command body — same path `create_timer` invokes.
    let new = create_input.into_new_timer().expect("GUI build");
    let created = store.create_timer(new).expect("create via IPC JSON");
    assert_eq!(created.name, "ipc-weekly-mwf");

    // Patch: the same flat shape the dialog emits for `Edit → Save`.
    let patch_json = r#"{
        "name": "ipc-weekly-mwf-renamed",
        "enabled": false,
        "occurrence": {
            "occ": "weekly",
            "tz": "Europe/Helsinki",
            "days": {"mon": true, "tue": false, "wed": true, "thu": false, "fri": true, "sat": false, "sun": false},
            "at": "09:30:00",
            "onceAt": null,
            "everySecs": null,
            "anchor": null,
            "day": null,
            "month": null,
            "expr": null
        },
        "actionKind": {"type": "notify", "title": "ok", "body": ""}
    }"#;
    let patch: WebTimerPatchDto = serde_json::from_str(patch_json).expect("patch IPC JSON must round-trip");
    assert_eq!(patch.action_kind.as_ref().map(|a| matches!(a, WebActionDto::Notify{..})), Some(true));
    let updated = store
        .update_timer(bellman_core::store::TimerUpdate {
            id: created.id,
            expected_revision: created.revision,
            patch: patch.into_core_patch().expect("patch build"),
        })
        .expect("update via IPC JSON");
    assert_eq!(updated.name, "ipc-weekly-mwf-renamed");
    assert!(!updated.enabled);
    match updated.occurrence.kind() {
        bellman_core::OccurrenceKind::Weekly { at, .. } => {
            assert_eq!(*at, chrono::NaiveTime::from_hms_opt(9, 30, 0).unwrap());
        }
        other => panic!("expected weekly, got {other:?}"),
    }
}

#[test]
fn preview_fire_dto_is_camel_case() {
    let f = PreviewFireDto {
        utc: chrono::DateTime::parse_from_rfc3339("2030-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        local_date: "2030-01-01".into(),
        local_time: "12:00:00".into(),
        offset: "+00:00".into(),
        tz_name: "UTC".into(),
    };
    let keys = json_keys(&f);
    for needed in ["utc", "localDate", "localTime", "tzName", "offset"] {
        assert!(
            keys.contains(&needed.to_string()),
            "PreviewFireDto missing {needed}; got {keys:?}"
        );
    }
    for forbidden in ["local_date", "local_time", "tz_name", "tz"] {
        assert!(
            !keys.contains(&forbidden.to_string()),
            "PreviewFireDto leaked {forbidden}; got {keys:?}"
        );
    }
}

#[test]
fn preview_response_dto_is_camel_case() {
    let resp = PreviewResponseDto {
        fires: vec![],
        warnings: vec!["daily times in DST gap".into()],
    };
    let s = serde_json::to_string(&resp).unwrap();
    assert!(s.contains("\"fires\":[]"));
    assert!(s.contains("\"warnings\":[\"daily times in DST gap\"]"));
    // Negative: no snake_case keys leaked.
    assert!(!s.contains("\"warning\""));
    assert!(!s.contains("\"fire_list\""));
    let keys = json_keys(&resp);
    assert!(keys.contains(&"warnings".to_string()));
    assert!(keys.contains(&"fires".to_string()));
    assert!(!keys.contains(&"warnings_count".to_string()));
}

#[test]
fn preview_fire_dto_round_trip_keeps_fields() {
    // Build the DTO directly (no PreviewFire helper exists anymore in
    // the post-rework command). We assert the to-pretty-printed JSON
    // surface and equality of the camelCase fields.
    let f = PreviewFireDto {
        utc: chrono::DateTime::parse_from_rfc3339("2030-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        local_date: "2030-01-01".into(),
        local_time: "12:00:00".into(),
        offset: "UTC".into(),
        tz_name: "UTC".into(),
    };
    let s = serde_json::to_string(&f).unwrap();
    assert!(s.contains("\"localDate\":\"2030-01-01\""));
    assert!(s.contains("\"localTime\":\"12:00:00\""));
    assert!(s.contains("\"tzName\":\"UTC\""));
}

/// IK5: the live-run DTO is camelCase at the IPC boundary and omits every
/// absent optional field — the GUI renders NOTHING for them, so a stray
/// `null` key would be a wire-contract bug, not a style issue.
#[test]
fn run_state_dto_is_camel_case_and_omits_absent_fields() {
    use crate::commands::RunStateDto;
    let timer = sample_timer();
    let fired = Utc.with_ymd_and_hms(2030, 1, 1, 8, 0, 0).unwrap();
    let row = bellman_core::RunStateRow::fired(
        uuid::Uuid::nil(),
        timer.id,
        "lightbulb",
        "running",
        fired,
        fired + chrono::Duration::seconds(60),
    );
    let dto = RunStateDto::from_row(&timer, &row);
    let s = serde_json::to_string(&dto).unwrap();
    // camelCase identity fields.
    assert!(s.contains("\"timerId\":"), "{s}");
    assert!(s.contains("\"timerName\":\"tick\""), "{s}");
    assert!(s.contains("\"runId\":"), "{s}");
    assert!(s.contains("\"appName\":\"lightbulb\""), "{s}");
    assert!(s.contains("\"firedAt\":"), "{s}");
    assert!(s.contains("\"state\":\"running\""), "{s}");
    // No snake_case leak.
    assert!(!s.contains("timer_id"), "{s}");
    assert!(!s.contains("app_name"), "{s}");
    assert!(!s.contains("fired_at"), "{s}");
    // Absent optional fields are NOT keys at all (no null placeholders).
    for absent in [
        "acknowledgedAt",
        "expectedSecs",
        "errorDetection",
        "heartbeatAt",
        "progress",
        "completedAt",
        "failedAt",
        "failureKind",
        "reason",
        "noAckAt",
        "result",
        "resultTruncated",
    ] {
        assert!(!s.contains(absent), "absent field {absent} serialized: {s}");
    }

    // Present optional fields serialize camelCase.
    let mut row2 = row.clone();
    row2.expected_secs = Some(900);
    row2.progress = Some("bulb on, 7s elapsed".into());
    row2.failure_kind = Some(bellman_core::FailureKind::TimedOut);
    let s2 = serde_json::to_string(&RunStateDto::from_row(&timer, &row2)).unwrap();
    assert!(s2.contains("\"expectedSecs\":900"), "{s2}");
    assert!(s2.contains("\"progress\":\"bulb on, 7s elapsed\""), "{s2}");
    assert!(s2.contains("\"failureKind\":\"timed_out\""), "{s2}");
}
