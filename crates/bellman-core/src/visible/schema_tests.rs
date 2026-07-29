//! Schema-stability checks for `bellman scan --json` surface.

use crate::visible::cron::parse::{parse_crontab, CrontabMode};
use crate::visible::cron::tasks_from_crontab_text;
use crate::visible::types::{
    DiscoveredTask, LastResult, ScanResult, SourceKind,
};
use chrono::{TimeZone, Utc};
use serde_json::Value;

fn sample_scan() -> ScanResult {
    let text = "# hand\n\n0 6 * * MON-FRI /usr/bin/backup\n";
    let tasks = tasks_from_crontab_text(
        text,
        CrontabMode::User,
        SourceKind::CronUser,
        "crontab:tester",
        "tester",
        true,
        None,
    );
    ScanResult {
        scanned_at: Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap(),
        platform: "linux".into(),
        filter: "all".into(),
        count: tasks.len(),
        tasks,
        warnings: vec![],
    }
}

/// Required top-level keys for a scan JSON document (plus `ok`/`command` from CLI).
const SCAN_KEYS: &[&str] = &[
    "scanned_at",
    "platform",
    "filter",
    "count",
    "tasks",
    "warnings",
];

const TASK_KEYS: &[&str] = &[
    "id",
    "source_kind",
    "source",
    "owner",
    "command",
    "schedule_expr",
    "human_explanation",
    "next_run",
    "last_run",
    "last_result",
    "enabled",
    "writable",
    "write_block_reason",
    "timezone",
    "line_no",
    "raw_line",
    "disabled_original",
    "platform_note",
];

#[test]
fn scan_json_schema_stable() {
    let scan = sample_scan();
    let v = serde_json::to_value(&scan).expect("serialize");
    let obj = v.as_object().expect("object");
    for k in SCAN_KEYS {
        assert!(obj.contains_key(*k), "missing scan key {k}");
    }
    assert_eq!(obj["platform"], "linux");
    assert_eq!(obj["count"], 1);
    let tasks = obj["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1);
    let t = tasks[0].as_object().expect("task object");
    for k in TASK_KEYS {
        assert!(t.contains_key(*k), "missing task key {k}");
    }
    assert_eq!(t["source_kind"], "cron_user");
    assert_eq!(t["command"], "/usr/bin/backup");
    assert_eq!(t["schedule_expr"], "0 6 * * MON-FRI");
    assert_eq!(t["enabled"], true);
    assert_eq!(t["writable"], true);
    // last_result is tagged enum → { "status": "unknown" }
    assert_eq!(t["last_result"]["status"], "unknown");
    // Round-trip
    let back: ScanResult = serde_json::from_value(v).expect("deserialize");
    assert_eq!(back.count, 1);
    assert!(matches!(back.tasks[0].last_result, LastResult::Unknown));
}

#[test]
fn cron_last_result_never_ok_without_evidence() {
    let text = "*/5 * * * * /bin/true\n";
    let tasks = tasks_from_crontab_text(
        text,
        CrontabMode::User,
        SourceKind::CronUser,
        "crontab:t",
        "t",
        true,
        None,
    );
    assert!(matches!(tasks[0].last_result, LastResult::Unknown));
    let v = serde_json::to_value(&tasks[0].last_result).unwrap();
    assert_eq!(v["status"], "unknown");
    assert!(v.get("exit_code").is_none());
}

#[test]
fn system_crontab_six_field_in_scan_shape() {
    let text = "17 * * * * root cd / && run-parts --report /etc/cron.hourly\n";
    let tasks = tasks_from_crontab_text(
        text,
        CrontabMode::System,
        SourceKind::CronSystem,
        "/etc/crontab",
        "root",
        false,
        Some("readonly".into()),
    );
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].owner, "root");
    assert!(!tasks[0].writable);
    assert!(tasks[0].command.contains("run-parts"));
}

#[test]
fn parse_preserves_unknown_and_comments() {
    let text = "# c\n\nFOO=bar\nnot a job line !!!\n0 * * * * true\n";
    let lines = parse_crontab(text, CrontabMode::User);
    assert_eq!(lines.len(), 5);
    let rebuilt: String = lines
        .iter()
        .map(|l| l.raw().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_eq!(rebuilt, text);
}

#[test]
fn discovered_task_json_nulls_explicit() {
    let t = DiscoveredTask {
        id: "x".into(),
        source_kind: SourceKind::At,
        source: "at:1".into(),
        owner: "u".into(),
        command: "echo".into(),
        schedule_expr: "now".into(),
        human_explanation: "h".into(),
        next_run: None,
        last_run: None,
        last_result: LastResult::Never,
        enabled: true,
        writable: false,
        write_block_reason: None,
        timezone: None,
        line_no: None,
        raw_line: None,
        disabled_original: None,
        platform_note: None,
    };
    let v: Value = serde_json::to_value(&t).unwrap();
    // Explicit nulls for stable agent schema
    assert!(v["next_run"].is_null());
    assert!(v["write_block_reason"].is_null());
    assert_eq!(v["last_result"]["status"], "never");
}
