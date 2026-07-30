//! IK2 slug rules (unit-level, OS-independent — real Windows validation is
//! M9) and folder-tree lifecycle tests.

use super::*;
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::store::{NewTimer, OpenOptions, Store};
use chrono::NaiveTime;

fn daily_timer(store: &mut Store, name: &str) -> Timer {
    let occ = Occurrence::new(
        OccurrenceKind::Daily {
            at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
        },
        "UTC",
    )
    .unwrap();
    store.create_timer(NewTimer::new(name, occ)).unwrap()
}

fn test_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_with(dir.path().join("timers.db"), OpenOptions::default()).unwrap();
    (dir, store)
}

// ── Slug rules (IK2 exit gate) ──────────────────────────────────────────

#[test]
fn reserved_device_names_are_escaped() {
    for name in ["CON", "con", "COM1", "COM¹", "LPT3", "COM0", "LPT0", "PRN", "AUX", "NUL"] {
        let slug = slugify(name);
        assert!(
            slug.starts_with('_'),
            "reserved name {name:?} must be escaped, got {slug:?}"
        );
        assert!(
            !is_reserved_stem(&slug),
            "escaped slug {slug:?} must no longer be reserved"
        );
    }
    // Full folder names are safe too.
    let id = Uuid::new_v4();
    for name in ["CON", "con", "COM1", "COM¹", "LPT3", "COM0"] {
        let folder = folder_name(name, id);
        assert!(folder.starts_with('_'), "{name:?} → {folder:?}");
    }
}

#[test]
fn conx_is_left_alone() {
    // CONX was never reserved; over-escaping is its own bug.
    assert_eq!(slugify("CONX"), "CONX");
    assert_eq!(slugify("conx"), "conx");
    assert_eq!(slugify("backup"), "backup");
}

#[test]
fn trailing_dot_and_space_are_stripped() {
    // Windows silently strips trailing dots/spaces; we must do it ourselves
    // so `backup.` and `backup` cannot collide into the same folder there.
    assert_eq!(slugify("backup."), "backup");
    assert_eq!(slugify("backup "), "backup");
    assert_eq!(slugify("backup...  "), "backup");
    assert_eq!(slugify("backup"), "backup");
    // The trailing-dot collision: both names map to the same slug, so the
    // folders differ only by the id suffix — and different timers always
    // have different ids.
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();
    assert_ne!(folder_name("backup.", id_a), folder_name("backup", id_b));
    assert_eq!(slugify("backup."), slugify("backup"));
}

#[test]
fn illegal_characters_are_removed() {
    assert_eq!(slugify("a:b"), "ab");
    assert_eq!(slugify("a/b"), "ab");
    assert_eq!(slugify("a<b"), "ab");
    assert_eq!(slugify("a>b"), "ab");
    assert_eq!(slugify("a?b"), "ab");
    assert_eq!(slugify("a*b"), "ab");
    assert_eq!(slugify("a|b"), "ab");
    assert_eq!(slugify("a\\b"), "ab");
    assert_eq!(slugify("a\"b"), "ab");
    // ASCII control characters (0x00-0x1F), e.g. BEL 0x07.
    assert_eq!(slugify("a\u{7}b"), "ab");
    assert_eq!(slugify("a\u{0}b"), "ab");
}

#[test]
fn empty_and_dot_only_names_fall_back() {
    assert_eq!(slugify(""), "timer");
    assert_eq!(slugify("..."), "timer");
    assert_eq!(slugify("   "), "timer");
}

#[test]
fn reserved_with_extension_is_escaped() {
    // `CON.txt` matches the reserved rule (stem + optional extension).
    assert!(slugify("CON.txt").starts_with('_'));
}

#[test]
fn folder_name_shape() {
    let id = Uuid::parse_str("3f1a8c2e-6b41-4d9e-8a17-0c2f5d7e9b33").unwrap();
    assert_eq!(short_id(id), "3f1a");
    assert_eq!(folder_name("bulb-test", id), "bulb-test-3f1a");
}

// ── Folder lifecycle ────────────────────────────────────────────────────

#[test]
fn create_writes_readme_and_timer_json() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "bulb-test");

    let folder = tree.create_for_timer(&timer, None).unwrap();

    assert!(dir.path().join("timers").join(README_FILE_NAME).is_file());
    assert_eq!(folder, dir.path().join("timers").join(folder_name("bulb-test", timer.id)));
    let raw = std::fs::read_to_string(folder.join(TIMER_FILE_NAME)).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["schema"], TIMER_SCHEMA_V1);
    assert_eq!(v["name"], "bulb-test");
    assert_eq!(v["timer_id"], timer.id.to_string());
    assert_eq!(v["occurrence"]["kind"], "daily");
    assert_eq!(v["occurrence"]["time"], "08:00:00");
    assert_eq!(v["action"]["type"], "none");
    assert!(v["note"].as_str().unwrap().contains("editing this file has no effect"));
}

#[test]
fn rename_keeps_folder_but_updates_timer_json() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "old-name");
    let folder = tree.create_for_timer(&timer, None).unwrap();

    let updated = store
        .update_timer(crate::store::TimerUpdate {
            id: timer.id,
            expected_revision: timer.revision,
            patch: crate::store::TimerPatch {
                name: Some("new-name".into()),
                ..Default::default()
            },
        })
        .unwrap();
    let folder_after = tree.sync_timer_json(&updated, None).unwrap();

    assert_eq!(folder, folder_after, "rename must not rename the folder");
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join(TIMER_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(v["name"], "new-name");
}

#[test]
fn remove_for_deletes_folder() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "gone");
    let folder = tree.create_for_timer(&timer, None).unwrap();
    assert!(folder.is_dir());
    assert!(tree.remove_for(timer.id).unwrap());
    assert!(!folder.exists());
    assert!(!tree.remove_for(timer.id).unwrap());
}

#[test]
fn orphan_sweep_removes_folders_without_a_timer() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let live = daily_timer(&mut store, "live");
    let orphan = daily_timer(&mut store, "orphan");
    tree.create_for_timer(&live, None).unwrap();
    let orphan_folder = tree.create_for_timer(&orphan, None).unwrap();
    // Simulate crash between database delete and folder delete.
    store.delete_timer(orphan.id).unwrap();

    let live_ids: HashSet<TimerId> = store.list_timers().unwrap().iter().map(|t| t.id).collect();
    let removed = tree.sweep_orphans(&live_ids).unwrap();

    assert_eq!(removed, vec![orphan_folder.clone()]);
    assert!(!orphan_folder.exists());
    assert!(tree.folder_for(live.id).is_some());
    // README and non-timer files are untouched.
    assert!(tree.root().join(README_FILE_NAME).is_file());
}

#[test]
fn reply_stub_is_create_only_and_stale_replies_are_removed() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "owned");
    let folder = tree.create_for_timer(&timer, Some("lightbulb")).unwrap();

    let run_a = Uuid::new_v4();
    let stub_a = tree.create_reply_stub(&folder, run_a).unwrap();
    assert_eq!(stub_a.file_name().unwrap(), reply_file_name(run_a).as_str());
    // O_EXCL: an app-written reply at the path is never clobbered.
    std::fs::write(&stub_a, b"{\"state\":\"completed\"}").unwrap();
    let again = tree.create_reply_stub(&folder, run_a).unwrap();
    assert_eq!(std::fs::read_to_string(&again).unwrap(), "{\"state\":\"completed\"}");

    // Next run: a fresh per-run file; the previous run's path is removed,
    // never overwritten.
    let run_b = Uuid::new_v4();
    tree.create_reply_stub(&folder, run_b).unwrap();
    tree.remove_stale_replies(&folder, run_b).unwrap();
    assert!(!stub_a.exists());
    assert!(folder.join(reply_file_name(run_b)).exists());
}

#[test]
fn fire_projection_writes_status_and_completed_fold() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "bulb-test");
    tree.create_for_timer(&timer, None).unwrap();
    let mut log = EventLog::open(crate::events::EventLogConfig::new(dir.path().join("logs"))).unwrap();

    let scheduled_for = Utc::now();
    let claim = store.claim_run(timer.id, scheduled_for).unwrap();
    project_run_started(&tree, &store, &timer, &claim, &FireKind::OnTime, &mut log).unwrap();

    let raw = std::fs::read_to_string(
        tree.folder_for(timer.id).unwrap().join(STATUS_FILE_NAME),
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["schema"], RUN_SCHEMA_V1);
    assert_eq!(v["state"], "fired");
    assert_eq!(v["run_id"], claim.run_id.to_string());
    assert_eq!(v["timer_id"], timer.id.to_string());
    assert_eq!(v["occurrence_kind"], "daily");
    assert!(v.get("completed_at").is_none(), "absent, never empty");
    assert!(v.get("duration_ms").is_none(), "duration_ms lives on the log event only");
    assert!(v.get("app_name").is_none());

    // The run completes: state folds to completed with completed_at.
    project_run_finished(&tree, &store, &timer, &claim, &FireKind::OnTime, None).unwrap();
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tree.folder_for(timer.id).unwrap().join(STATUS_FILE_NAME))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(v["state"], "completed");
    assert!(v["completed_at"].is_string());
    assert!(v.get("duration_ms").is_none());

    // Failure path: failed with failed_at + reason.
    let claim2 = store
        .claim_run(timer.id, scheduled_for + chrono::Duration::days(1))
        .unwrap();
    project_run_started(&tree, &store, &timer, &claim2, &FireKind::OnTime, &mut log).unwrap();
    project_run_finished(&tree, &store, &timer, &claim2, &FireKind::OnTime, Some("boom")).unwrap();
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tree.folder_for(timer.id).unwrap().join(STATUS_FILE_NAME))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(v["state"], "failed");
    assert!(v["failed_at"].is_string());
    assert_eq!(v["reason"], "boom");
}

#[test]
fn second_fire_supersedes_unresolved_first_run() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "owned");
    store.set_timer_owner(timer.id, "lightbulb").unwrap();
    tree.create_for_timer(&timer, Some("lightbulb")).unwrap();
    let mut log = EventLog::open(crate::events::EventLogConfig::new(dir.path().join("logs"))).unwrap();

    // First fire left unresolved (claimed, never completed).
    let claim1 = store.claim_run(timer.id, Utc::now()).unwrap();
    project_run_started(&tree, &store, &timer, &claim1, &FireKind::OnTime, &mut log).unwrap();
    let reply1 = tree.folder_for(timer.id).unwrap().join(reply_file_name(claim1.run_id));
    assert!(reply1.exists(), "owned timer gets a per-run reply stub");

    // Second fire: superseded is logged, status rewritten, new stub created,
    // first stub removed (never overwritten).
    let claim2 = store
        .claim_run(timer.id, Utc::now() + chrono::Duration::days(1))
        .unwrap();
    project_run_started(&tree, &store, &timer, &claim2, &FireKind::OnTime, &mut log).unwrap();

    let (recs, _) = crate::events::read_events(log.current_path()).unwrap();
    let sup: Vec<_> = recs.iter().filter(|r| r.kind == RunState::Superseded).collect();
    assert_eq!(sup.len(), 1);
    assert_eq!(sup[0].run_id, Some(claim1.run_id));

    let folder = tree.folder_for(timer.id).unwrap();
    assert!(!reply1.exists());
    assert!(folder.join(reply_file_name(claim2.run_id)).exists());
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join(STATUS_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(v["run_id"], claim2.run_id.to_string());
    assert_eq!(v["app_name"], "lightbulb");
}

#[test]
fn delete_logs_cancelled_for_open_run_before_folder_removal() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "doomed");
    tree.create_for_timer(&timer, None).unwrap();
    let mut log = EventLog::open(crate::events::EventLogConfig::new(dir.path().join("logs"))).unwrap();
    let claim = store.claim_run(timer.id, Utc::now()).unwrap();

    let n = log_cancelled_for_open_runs(&store, &timer, &mut log).unwrap();
    assert_eq!(n, 1);
    store.delete_timer(timer.id).unwrap();
    assert!(tree.remove_for(timer.id).unwrap());

    let (recs, _) = crate::events::read_events(log.current_path()).unwrap();
    let cancel: Vec<_> = recs.iter().filter(|r| r.kind == RunState::Cancelled).collect();
    assert_eq!(cancel.len(), 1);
    assert_eq!(cancel[0].run_id, Some(claim.run_id));
    assert!(tree.folder_for(timer.id).is_none());
}

#[test]
fn reconcile_creates_missing_folders() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let t1 = daily_timer(&mut store, "one");
    let t2 = daily_timer(&mut store, "two");
    // No folders yet (pre-IK2 timers).
    let n = reconcile_folders(&tree, &store).unwrap();
    assert_eq!(n, 2);
    assert!(tree.folder_for(t1.id).is_some());
    assert!(tree.folder_for(t2.id).is_some());
}
