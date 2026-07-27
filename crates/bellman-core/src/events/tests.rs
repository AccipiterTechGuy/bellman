//! Event log acceptance tests: rotation atomicity, tolerant reader, retention.

use super::*;
use chrono::Utc;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn open_tmp() -> (tempfile::TempDir, EventLog) {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = EventLog::open(EventLogConfig::new(dir.path().join("logs"))).expect("open log");
    (dir, log)
}

#[test]
fn append_writes_self_contained_jsonl_lines() {
    let (_dir, mut log) = open_tmp();
    let tid = Uuid::new_v4();
    let rid = Uuid::new_v4();
    log.emit(
        EventRecord::new(EventKind::Registered)
            .with_timer(tid, "t1")
            .with_message("created"),
    )
    .unwrap();
    log.emit(
        EventRecord::new(EventKind::Fired)
            .with_timer(tid, "t1")
            .with_run(rid)
            .with_scheduled_for(Utc::now()),
    )
    .unwrap();

    let (recs, stats) = read_events(log.current_path()).unwrap();
    assert_eq!(stats.skipped, 0);
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0].kind, EventKind::Registered);
    assert_eq!(recs[1].kind, EventKind::Fired);
    assert_eq!(recs[1].run_id, Some(rid));
}

#[test]
fn rotate_is_atomic_rename_to_iso_week_archive() {
    let (_dir, mut log) = open_tmp();
    log.emit(EventRecord::new(EventKind::Fired).with_message("a"))
        .unwrap();
    log.emit(EventRecord::new(EventKind::Fired).with_message("b"))
        .unwrap();

    let current_before = fs::read_to_string(log.current_path()).unwrap();
    assert!(current_before.lines().count() >= 2);

    let archived = log.rotate().unwrap().expect("archive path");
    assert!(archived.exists(), "archive must exist after rotate");
    let name = archived.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("events-") && name.contains("-W") && name.ends_with(".jsonl"),
        "unexpected archive name: {name}"
    );
    // Content moved intact (no rewrite/filter).
    let archived_body = fs::read_to_string(&archived).unwrap();
    assert_eq!(archived_body, current_before);

    // Fresh current file exists and is empty (or only new writes).
    assert!(log.current_path().exists());
    let after = fs::read_to_string(log.current_path()).unwrap();
    assert!(after.is_empty(), "fresh current must be empty, got: {after:?}");

    // New appends go to the fresh file, not the archive.
    log.emit(EventRecord::new(EventKind::Pruned).with_message("new"))
        .unwrap();
    let (recs, _) = read_events(log.current_path()).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].kind, EventKind::Pruned);
    // Archive unchanged.
    assert_eq!(fs::read_to_string(&archived).unwrap(), current_before);
}

#[test]
fn rotate_empty_current_yields_none() {
    let (_dir, mut log) = open_tmp();
    assert!(log.rotate().unwrap().is_none());
}

#[test]
fn tolerant_reader_skips_torn_tail_and_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.current.jsonl");
    let good1 = serde_json::to_string(
        &EventRecord::new(EventKind::Fired).with_message("ok1"),
    )
    .unwrap();
    let good2 = serde_json::to_string(
        &EventRecord::new(EventKind::WakeDelivered).with_message("ok2"),
    )
    .unwrap();
    let mut f = fs::File::create(&path).unwrap();
    writeln!(f, "{good1}").unwrap();
    writeln!(f, "{{this is not json").unwrap(); // garbage line
    writeln!(f, "{good2}").unwrap();
    write!(f, "{{\"ts\":\"2026-01-01T00:00:00Z\",\"kind\":\"fired\"").unwrap(); // torn tail
    f.flush().unwrap();
    drop(f);

    let (recs, stats) = read_events(&path).unwrap();
    assert_eq!(recs.len(), 2, "only complete lines parse");
    assert_eq!(stats.records, 2);
    assert_eq!(stats.skipped, 2, "garbage + torn tail");
    assert_eq!(recs[0].kind, EventKind::Fired);
    assert_eq!(recs[1].kind, EventKind::WakeDelivered);
}

#[test]
fn retention_deletes_archives_past_window() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    let archive = logs.join("archive");
    fs::create_dir_all(&archive).unwrap();

    // Old archive (mtime forced into the past).
    let old = archive.join("events-2020-W01.jsonl");
    fs::write(&old, b"{\"kind\":\"fired\"}\n").unwrap();
    let old_time = SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60);
    filetime_set_mtime(&old, old_time);

    // Recent archive stays.
    let recent = archive.join("events-2026-W30.jsonl");
    fs::write(&recent, b"{\"kind\":\"fired\"}\n").unwrap();

    let log = EventLog::open(
        EventLogConfig::new(&logs).with_retention(Duration::from_secs(30 * 24 * 60 * 60)),
    )
    .unwrap();
    let removed = log.retain().unwrap();
    assert_eq!(removed, 1);
    assert!(!old.exists(), "old archive must be deleted");
    assert!(recent.exists(), "recent archive must be kept");
}

/// Set mtime without an extra crate (POSIX utimensat via filetime alternative:
/// rewrite isn't enough on all FS; use `filetime` if present — else touch via
/// a short retention window instead).
fn filetime_set_mtime(path: &Path, when: SystemTime) {
    // Prefer the `filetime` approach via libc when available; for tests we can
    // also use a tiny retention window + sleep, but setting mtime is cleaner.
    // std has no set_mtime — use a subprocess `touch -d` as a portable-enough
    // fallback on Linux (this worktree's CI target).
    let secs = when
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let status = std::process::Command::new("touch")
        .arg("-d")
        .arg(format!("@{secs}"))
        .arg(path)
        .status()
        .expect("touch");
    assert!(status.success(), "touch -d failed");
}

#[test]
fn all_event_kinds_round_trip_json() {
    let kinds = [
        EventKind::Registered,
        EventKind::Fired,
        EventKind::FiredLate,
        EventKind::SkippedMisfire,
        EventKind::Coalesced,
        EventKind::WakeDelivered,
        EventKind::WakeFailed,
        EventKind::NoAck,
        EventKind::Pruned,
        EventKind::YearRecalibrate,
    ];
    for k in kinds {
        let rec = EventRecord::new(k);
        let s = serde_json::to_string(&rec).unwrap();
        let back: EventRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back.kind, k);
        // Wire string form matches product vocabulary.
        assert!(s.contains(&format!("\"{}\"", k.as_str())), "{s}");
    }
}
