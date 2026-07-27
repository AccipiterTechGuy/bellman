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

use bellman_core::{
    Action, EventRecord, MisfirePolicy, Occurrence, OccurrenceKind, OverlapPolicy, RetryPolicy,
    Timer,
};
use chrono::{TimeZone, Utc};

use crate::commands::{
    AppInfo, LogTailDto, PreviewFireDto, PreviewResponseDto, TimerDto, TimerPatchDto,
};
use crate::first_run::{WizardChoice, WizardStatus};
use crate::occurrence_input::{CreateTimerInput, OccurrenceInput, PreviewFire};
use crate::state::RunNowResponse;

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
    for needed in ["nextFireUtc", "lastFired"] {
        assert!(
            keys.contains(&needed.to_string()),
            "TimerDto missing camelCase key {needed}; got {keys:?}"
        );
    }
    for forbidden in ["next_fire_utc", "last_fired"] {
        assert!(
            !keys.contains(&forbidden.to_string()),
            "TimerDto leaked snake_case / wrong key {forbidden}; got {keys:?}"
        );
    }
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
            if p.file_name().map_or(false, |n| n == "tests") {
                continue;
            }
            visit_rs_files(&p, visit);
        } else if p.extension().map_or(false, |e| e == "rs")
                        // Skip the test file itself (it contains the bad-pattern
                        // strings as r#"…"# literals, which would self-match).
                        && p.file_name().map_or(true, |n| n != "dto_serde_tests.rs")
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
    let input = CreateTimerInput {
        name: "tick".into(),
        occurrence: OccurrenceInput {
            kind: "daily".into(),
            tz: Some("UTC".into()),
            time: Some("09:00:00".into()),
            once_at: None,
            every_secs: None,
            interval_anchor: None,
            days: None,
            day: None,
            month: None,
            cron_expr: None,
            dst_gap: None,
            dst_fold: None,
            invalid_monthday: None,
        },
        enabled: Some(true),
        action: Some(Action::None),
        misfire: None,
        overlap: None,
        retry: None,
        tags: None,
    };
    let json = serde_json::to_string(&input).unwrap();
    for needed in [
        "\"name\":\"tick\"",
        "\"occurrence\":{",
        "\"kind\":\"daily\"",
        "\"tz\":\"UTC\"",
        "\"time\":\"09:00:00\"",
        "\"enabled\":true",
        "\"action\":{\"type\":\"none\"}",
    ] {
        assert!(
            json.contains(needed),
            "CreateTimerInput missing {needed}; full json: {json}"
        );
    }
    // Negative: our helpers never emit placeholders like once_at / every_secs
    // from the JSON.
    assert!(!json.contains("once_at"));
    assert!(!json.contains("every_secs"));
    assert!(!json.contains("cron_expr"));
}

#[test]
fn timer_patch_dto_is_camel_case() {
    let patch = TimerPatchDto {
        name: Some("renamed".into()),
        enabled: Some(false),
        occurrence: None,
        action: Some(Action::None),
    };
    let s = serde_json::to_string(&patch).unwrap();
    // camelCase keys wire to Tauri: `name`/`enabled` are the user-facing
    // fields; `renamed` is the value. Negative: no snake_case leak.
    assert!(s.contains("\"name\":\"renamed\""));
    assert!(s.contains("\"enabled\":false"));
    assert!(!s.contains("\"name_\""));
    assert!(!s.contains("\"enable_d\""));
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
fn preview_fire_from_helper_keeps_fields() {
    let fire = PreviewFire {
        utc: chrono::DateTime::parse_from_rfc3339("2030-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        local_date: "2030-01-01".into(),
        local_time: "12:00:00".into(),
        offset: "UTC".into(),
        tz_name: "UTC".into(),
    };
    let dto: PreviewFireDto = fire.into();
    assert_eq!(dto.local_time, "12:00:00");
    assert_eq!(dto.tz_name, "UTC");
    assert_eq!(dto.offset, "UTC");
}
