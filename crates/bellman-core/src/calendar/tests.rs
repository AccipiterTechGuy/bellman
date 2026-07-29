//! Integration tests for calendar snapshot: grids, DST, overflow, determinism,
//! headless PNG, JSON/SVG parity, privacy of commands.

use super::*;
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::store::{NewTimer, OpenOptions, Store};
use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
use tempfile::TempDir;

fn open_tmp() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("timers.db");
    let store = Store::open_with(&path, OpenOptions::default()).unwrap();
    (dir, store)
}

fn daily(name: &str, hour: u32, min: u32, tz: &str) -> ExpandableTask {
    let at = NaiveTime::from_hms_opt(hour, min, 0).unwrap();
    let occ = Occurrence::new(OccurrenceKind::Daily { at }, tz).unwrap();
    ExpandableTask {
        id: format!("id-{name}"),
        name: name.into(),
        enabled: true,
        occurrence: occ,
        past_status: CalendarStatus::Unknown,
        command: Some(format!("/usr/bin/{name} --token=SECRET")),
        source_kind: "bellman".into(),
    }
}

#[test]
fn empty_month_valid_grid() {
    let (from, to) = month_bounds(2026, 8).unwrap();
    let snap = build_snapshot(
        &[],
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps::default(),
        },
    )
    .unwrap();
    assert!(snap.entries.is_empty());
    assert_eq!(snap.days.len(), 42); // 6 weeks
    assert!(snap.days.iter().any(|d| d.date == "2026-08-01" && d.in_month));
    assert!(snap.days.iter().any(|d| d.date == "2026-08-31" && d.in_month));
    let svg = render_svg(&snap);
    assert!(svg.contains("August 2026 · UTC"));
    assert!(svg.contains("data-date=\"2026-08-01\""));
    // Adjacent greyed
    assert!(snap.days.iter().any(|d| !d.in_month));
}

#[test]
fn determinism_svg_byte_identical() {
    let task = daily("tick", 9, 0, "UTC");
    let (from, to) = month_bounds(2026, 8).unwrap();
    let opts = CalendarBuildOptions {
        from,
        to,
        month: Some((2026, 8)),
        timezone: "UTC".into(),
        week_start: WeekStart::Mon,
        show_commands: false,
        now_utc: Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        caps: CalendarCaps::default(),
    };
    let a = build_snapshot(&[task.clone()], &opts).unwrap();
    let b = build_snapshot(&[task], &opts).unwrap();
    assert_eq!(render_svg(&a), render_svg(&b));
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}

#[test]
fn display_unset_svg_and_png() {
    std::env::remove_var("DISPLAY");
    let (from, to) = month_bounds(2026, 8).unwrap();
    let snap = build_snapshot(
        &[daily("a", 8, 30, "UTC")],
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps::default(),
        },
    )
    .unwrap();
    let svg = render_svg(&snap);
    let png = svg_to_png(&svg).expect("png without DISPLAY");
    assert!(png.starts_with(b"\x89PNG"));
    assert!(std::env::var_os("DISPLAY").is_none());
}

#[test]
fn overflow_twenty_tasks_same_day() {
    let mut tasks = Vec::new();
    for i in 0..20 {
        tasks.push(daily(&format!("job{i:02}"), 10, i as u32 % 60, "UTC"));
    }
    // Force all onto same minute so they land on every day — still 20 per day.
    // Actually different minutes; each daily fires once per day → 20 entries/day.
    let (from, to) = month_bounds(2026, 8).unwrap();
    let snap = build_snapshot(
        &tasks,
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps {
                max_per_cell: 5,
                ..CalendarCaps::default()
            },
        },
    )
    .unwrap();
    let day = snap
        .days
        .iter()
        .find(|d| d.date == "2026-08-15" && d.in_month)
        .unwrap();
    assert_eq!(day.total, 20);
    assert_eq!(day.entries.len(), 5);
    assert_eq!(day.overflow, 15);
    let svg = render_svg(&snap);
    assert!(svg.contains("+15 more"));
    // No command secrets
    assert!(!svg.contains("SECRET"));
}

#[test]
fn commands_absent_unless_opt_in() {
    let task = daily("secret-job", 12, 0, "UTC");
    let (from, to) = month_bounds(2026, 8).unwrap();
    let hidden = build_snapshot(
        &[task.clone()],
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps::default(),
        },
    )
    .unwrap();
    assert!(hidden.entries.iter().all(|e| e.command.is_none()));
    let json = serde_json::to_string(&hidden).unwrap();
    assert!(!json.contains("SECRET"));
    assert!(!render_svg(&hidden).contains("SECRET"));

    let shown = build_snapshot(
        &[task],
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: true,
            now_utc: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps::default(),
        },
    )
    .unwrap();
    assert!(shown.entries.iter().any(|e| {
        e.command
            .as_ref()
            .is_some_and(|c| c.contains("SECRET"))
    }));
}

#[test]
fn json_and_svg_same_task_set() {
    let tasks = vec![
        daily("alpha", 9, 0, "UTC"),
        daily("beta", 15, 30, "UTC"),
    ];
    let (from, to) = month_bounds(2026, 8).unwrap();
    let snap = build_snapshot(
        &tasks,
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps::default(),
        },
    )
    .unwrap();
    let svg = render_svg(&snap);
    // Every entry's task id appears in SVG data attributes.
    for e in &snap.entries {
        assert!(
            svg.contains(&format!("data-task-id=\"{}\"", e.task_id)),
            "missing {} in svg",
            e.task_id
        );
        assert!(
            svg.contains(&format!("data-time=\"{}\"", e.time)),
            "missing time {} in svg",
            e.time
        );
    }
    // JSON contract lists the same ids.
    let ids_json: std::collections::BTreeSet<_> =
        snap.entries.iter().map(|e| e.task_id.as_str()).collect();
    assert!(ids_json.contains("id-alpha"));
    assert!(ids_json.contains("id-beta"));
    // Daily × 31 days
    assert_eq!(snap.entries.len(), 62);
}

#[test]
fn dst_transition_europe_helsinki_march_2026() {
    // EU DST spring-forward 2026-03-29 03:00 → 04:00.
    // Daily at 02:30 should still resolve; daily at 12:00 both sides stay 12:00.
    let before = daily("noon", 12, 0, "Europe/Helsinki");
    let (from, to) = month_bounds(2026, 3).unwrap();
    let snap = build_snapshot(
        &[before],
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 3)),
            timezone: "Europe/Helsinki".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps::default(),
        },
    )
    .unwrap();
    let mar28 = snap
        .entries
        .iter()
        .find(|e| e.date == "2026-03-28")
        .expect("mar28");
    let mar30 = snap
        .entries
        .iter()
        .find(|e| e.date == "2026-03-30")
        .expect("mar30");
    assert_eq!(mar28.time, "12:00");
    assert_eq!(mar30.time, "12:00");
    assert!(snap.timezone.contains("Helsinki"));
}

#[test]
fn store_snapshot_integration() {
    let (_dir, mut store) = open_tmp();
    let at = NaiveTime::from_hms_opt(7, 15, 0).unwrap();
    let occ = Occurrence::new(OccurrenceKind::Daily { at }, "UTC").unwrap();
    store
        .create_timer(NewTimer::new("morning", occ))
        .unwrap();
    let snap = snapshot_month_from_store(
        &store,
        2026,
        8,
        "UTC",
        WeekStart::Mon,
        false,
        Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
        CalendarCaps::default(),
    )
    .unwrap();
    assert_eq!(snap.entries.len(), 31);
    assert!(snap.entries.iter().all(|e| e.time == "07:15"));
    assert!(snap.entries.iter().all(|e| e.command.is_none()));
}

#[test]
fn six_week_31_day_month() {
    // Aug 2026: starts Saturday → 6 weeks with Mon start.
    let (from, to) = month_bounds(2026, 8).unwrap();
    let snap = build_snapshot(
        &[],
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc::now(),
            caps: CalendarCaps::default(),
        },
    )
    .unwrap();
    assert_eq!(snap.days.len(), 42);
}

#[test]
fn high_frequency_interval_capped() {
    let occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 1,
            anchor: Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
        },
        "UTC",
    )
    .unwrap();
    let task = ExpandableTask {
        id: "hot".into(),
        name: "hot".into(),
        enabled: true,
        occurrence: occ,
        past_status: CalendarStatus::Unknown,
        command: None,
        source_kind: "bellman".into(),
    };
    let (from, to) = month_bounds(2026, 8).unwrap();
    let snap = build_snapshot(
        &[task],
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps {
                max_fires_per_task: 50,
                max_total_entries: 100,
                ..CalendarCaps::default()
            },
        },
    )
    .unwrap();
    assert!(snap.entries.len() <= 100);
    assert!(!snap.warnings.is_empty());
}

#[test]
fn disabled_status() {
    let mut t = daily("off", 9, 0, "UTC");
    t.enabled = false;
    let (from, to) = month_bounds(2026, 8).unwrap();
    let snap = build_snapshot(
        &[t],
        &CalendarBuildOptions {
            from,
            to,
            month: Some((2026, 8)),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            now_utc: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            caps: CalendarCaps::default(),
        },
    )
    .unwrap();
    assert!(snap
        .entries
        .iter()
        .all(|e| e.status == CalendarStatus::Disabled));
}

#[test]
fn resolve_month_phrases() {
    let now = NaiveDate::from_ymd_opt(2026, 7, 29).unwrap();
    assert_eq!(resolve_month("2026-08", now).unwrap(), (2026, 8));
    assert_eq!(resolve_month("next", now).unwrap(), (2026, 8));
}
