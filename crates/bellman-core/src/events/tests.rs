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
        EventRecord::new(RunState::Registered)
            .with_timer(tid, "t1")
            .with_message("created"),
    )
    .unwrap();
    log.emit(
        EventRecord::new(RunState::Fired)
            .with_timer(tid, "t1")
            .with_run(rid)
            .with_scheduled_for(Utc::now()),
    )
    .unwrap();

    let (recs, stats) = read_events(log.current_path()).unwrap();
    assert_eq!(stats.skipped, 0);
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0].kind, RunState::Registered);
    assert_eq!(recs[1].kind, RunState::Fired);
    assert_eq!(recs[1].run_id, Some(rid));
}

#[test]
fn rotate_is_atomic_rename_to_iso_week_archive() {
    let (_dir, mut log) = open_tmp();
    log.emit(EventRecord::new(RunState::Fired).with_message("a"))
        .unwrap();
    log.emit(EventRecord::new(RunState::Fired).with_message("b"))
        .unwrap();

    let current_before = fs::read_to_string(log.current_path()).unwrap();
    assert!(current_before.lines().count() >= 2);

    let archived = log.rotate().unwrap().expect("archive path");
    assert!(archived.exists(), "archive must exist after rotate");
    let name = archived.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("events-") && name.contains("-W") && name.ends_with(".jsonl.gz"),
        "archives are gzip-compressed on rotation: {name}"
    );
    // Content moved intact (no rewrite/filter), just compressed.
    let archived_body = gunzip(&archived);
    assert_eq!(archived_body, current_before);

    // Fresh current file exists and is empty (or only new writes).
    assert!(log.current_path().exists());
    let after = fs::read_to_string(log.current_path()).unwrap();
    assert!(after.is_empty(), "fresh current must be empty, got: {after:?}");

    // New appends go to the fresh file, not the archive.
    log.emit(EventRecord::new(RunState::Pruned).with_message("new"))
        .unwrap();
    let (recs, _) = read_events(log.current_path()).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].kind, RunState::Pruned);
    // Archive unchanged; readers decompress transparently.
    assert_eq!(gunzip(&archived), current_before);
    let (recs, _) = read_events(&archived).unwrap();
    assert_eq!(recs.len(), 2);
}

/// Decompress a `.jsonl.gz` archive to its plain JSONL text.
fn gunzip(path: &Path) -> String {
    use std::io::Read;
    let f = fs::File::open(path).unwrap();
    let mut s = String::new();
    flate2::read::GzDecoder::new(f)
        .read_to_string(&mut s)
        .unwrap();
    s
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
        &EventRecord::new(RunState::Fired).with_message("ok1"),
    )
    .unwrap();
    let good2 = serde_json::to_string(
        &EventRecord::new(RunState::WakeDelivered).with_message("ok2"),
    )
    .unwrap();
    let mut f = fs::File::create(&path).unwrap();
    writeln!(f, "{good1}").unwrap();
    writeln!(f, "{{this is not json").unwrap(); // garbage line
    writeln!(f, "{good2}").unwrap();
    write!(f, "{{\"logged_at\":\"2026-01-01T00:00:00Z\",\"kind\":\"fired\"").unwrap(); // torn tail
    f.flush().unwrap();
    drop(f);

    let (recs, stats) = read_events(&path).unwrap();
    assert_eq!(recs.len(), 2, "only complete lines parse");
    assert_eq!(stats.records, 2);
    assert_eq!(stats.skipped, 2, "garbage + torn tail");
    assert_eq!(recs[0].kind, RunState::Fired);
    assert_eq!(recs[1].kind, RunState::WakeDelivered);
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
    assert_eq!(removed.removed_count(), 1);
    assert_eq!(removed.aged, vec![old.clone()]);
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
        RunState::Registered,
        RunState::Fired,
        RunState::FiredLate,
        RunState::SkippedMisfire,
        RunState::Coalesced,
        RunState::WakeDelivered,
        RunState::WakeFailed,
        RunState::NoAck,
        RunState::Pruned,
        RunState::YearRecalibrate,
        RunState::WakeCapability,
        RunState::Acknowledged,
        RunState::Running,
        RunState::Completed,
        RunState::Failed,
        RunState::Cancelled,
        RunState::Superseded,
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

#[test]
fn size_threshold_rotates_and_compresses_before_next_line() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    // Tiny cap so a handful of lines crosses it.
    let mut log = EventLog::open(EventLogConfig::new(&logs).with_max_current_bytes(600)).unwrap();

    // Fill current close to the cap.
    let mut appended = 0;
    for i in 0..20 {
        log.emit(
            EventRecord::new(RunState::Fired).with_message(format!("line-{i:03}-{}", "x".repeat(40))),
        )
        .unwrap();
        appended += 1;
    }
    assert!(appended > 0);

    // The live file never crossed the cap, and a compressed archive exists.
    let current_len = fs::metadata(log.current_path()).unwrap().len();
    assert!(
        current_len <= 600,
        "current must rotate before crossing the cap, got {current_len}"
    );
    let archives: Vec<_> = fs::read_dir(log.archive_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".jsonl.gz"))
        .collect();
    assert!(!archives.is_empty(), "rotation must produce a .jsonl.gz archive");
    // All 20 fired events remain readable across archive(s) + current, and
    // every rotation was logged (never silent).
    let mut fired = 0;
    let mut rotation_notes = 0;
    for a in &archives {
        for r in read_events(a).unwrap().0 {
            match r.kind {
                RunState::Fired => fired += 1,
                RunState::Pruned => rotation_notes += 1,
                _ => {}
            }
        }
    }
    for r in read_events(log.current_path()).unwrap().0 {
        match r.kind {
            RunState::Fired => fired += 1,
            RunState::Pruned => rotation_notes += 1,
            _ => {}
        }
    }
    assert_eq!(fired, 20, "every fired line survives rotation");
    assert!(rotation_notes >= 1, "each rotation is logged, never silent");
}

#[test]
fn budget_prunes_oldest_archives_but_never_current() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    let archive = logs.join("archive");
    fs::create_dir_all(&archive).unwrap();

    // Three gz archives with increasing mtime and high-entropy bodies so the
    // compressed sizes are meaningful.
    let mut paths = Vec::new();
    for (i, week) in ["events-2026-W28.jsonl.gz", "events-2026-W29.jsonl.gz", "events-2026-W30.jsonl.gz"]
        .iter()
        .enumerate()
    {
        let p = archive.join(week);
        let body: String = (0..100)
            .map(|_| Uuid::new_v4().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let f = fs::File::create(&p).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        enc.write_all(body.as_bytes()).unwrap();
        enc.finish().unwrap();
        let t = SystemTime::now() - Duration::from_secs((3 - i as u64) * 3600);
        filetime_set_mtime(&p, t);
        paths.push(p);
    }
    let sizes: Vec<u64> = paths.iter().map(|p| fs::metadata(p).unwrap().len()).collect();

    // Budget fits current (empty) + the two newest archives, not all three.
    let budget = sizes[1] + sizes[2] + 8;
    let log = EventLog::open(
        EventLogConfig::new(&logs)
            .with_retention(Duration::from_secs(30 * 24 * 60 * 60))
            .with_budget_bytes(budget),
    )
    .unwrap();
    let report = log.retain().unwrap();

    assert_eq!(report.budget.len(), 1, "only the oldest archive is budget-pruned");
    assert_eq!(report.budget[0], paths[0]);
    assert!(!paths[0].exists());
    assert!(paths[1].exists() && paths[2].exists());
    assert!(log.current_path().exists(), "the live file is never deleted");

    // A budget smaller than the archives alone prunes them all but still
    // never touches the live current file.
    let log2 = EventLog::open(EventLogConfig::new(&logs).with_budget_bytes(1)).unwrap();
    let report2 = log2.retain().unwrap();
    assert_eq!(report2.budget.len(), 2);
    assert!(log2.current_path().exists());
}

#[test]
fn stale_handle_reanchors_and_never_loses_events() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    let cfg = EventLogConfig::new(&logs).with_max_current_bytes(4096);
    let mut a = EventLog::open(cfg.clone()).unwrap();
    let mut b = EventLog::open(cfg).unwrap();

    // A fills current past the cap and rotates.
    for i in 0..4 {
        a.emit(EventRecord::new(RunState::Fired).with_message(format!("a-{i}-{}", "x".repeat(120))))
            .unwrap();
    }
    let archived = a.rotate().unwrap().expect("rotation produced an archive");

    // B's handle still points at the renamed-away inode. Its next append must
    // re-anchor on the live file — the event must not vanish.
    let lost_id = Uuid::new_v4();
    b.emit(
        EventRecord::new(RunState::WakeDelivered)
            .with_run(lost_id)
            .with_message("b-after-rotate"),
    )
    .unwrap();

    let in_current = read_events(a.current_path()).unwrap().0;
    assert!(
        in_current.iter().any(|r| r.run_id == Some(lost_id)),
        "stale writer's event must land in the live current file"
    );
    // And nothing from A's rotation was lost either.
    let in_archive = read_events(&archived).unwrap().0;
    assert_eq!(in_archive.len(), 4);

    // A stale writer whose handle reports a large file must NOT rotate the
    // fresh (small) live file: the size check reads the path, not the handle.
    let archives_before = fs::read_dir(a.archive_dir()).unwrap().count();
    b.emit(EventRecord::new(RunState::Fired).with_message("small"))
        .unwrap();
    let archives_after = fs::read_dir(a.archive_dir()).unwrap().count();
    assert_eq!(archives_before, archives_after, "no bogus rotation from a stale size view");
}

#[test]
fn reader_reads_plain_and_gz_archives() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("events-2026-W30.jsonl");
    let rec = EventRecord::new(RunState::Fired).with_message("plain");
    fs::write(&plain, format!("{}\n", serde_json::to_string(&rec).unwrap())).unwrap();

    let gz = dir.path().join("events-2026-W31.jsonl.gz");
    let rec2 = EventRecord::new(RunState::Completed).with_message("gz");
    let body = format!("{}\n", serde_json::to_string(&rec2).unwrap());
    let f = fs::File::create(&gz).unwrap();
    let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    enc.write_all(body.as_bytes()).unwrap();
    enc.finish().unwrap();

    let (a, _) = read_events(&plain).unwrap();
    let (b, _) = read_events(&gz).unwrap();
    assert_eq!(a[0].kind, RunState::Fired);
    assert_eq!(b[0].kind, RunState::Completed);
}
