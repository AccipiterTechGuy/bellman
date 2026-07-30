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

fn test_engine(dir: &std::path::Path) -> crate::reply::ReplyEngine {
    crate::reply::ReplyEngine {
        tree: TimersTree::new(dir),
        data_dir: dir.to_path_buf(),
        pickup_grace: std::time::Duration::from_secs(60),
        watchdog_factor: 2.0,
        anchors: crate::reply::new_anchors(),
        deadlines: crate::reply::new_deadlines(),
    }
}

/// Drain the outbox through an elected publisher and read the appended events.
fn drain_events(dir: &std::path::Path, store: &Store) -> Vec<crate::events::EventRecord> {
    let mut publisher =
        crate::events::EventPublisher::with_config(crate::events::EventLogConfig::new(dir.join("logs")))
            .unwrap();
    publisher.publish_cycle(store);
    let (recs, _) = crate::events::read_events(publisher.current_path()).unwrap();
    recs
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
    let stub_a = tree.create_reply_stub(&folder, run_a, "lightbulb").unwrap();
    assert_eq!(stub_a.file_name().unwrap(), reply_file_name(run_a).as_str());
    // O_EXCL: an app-written reply at the path is never clobbered.
    std::fs::write(&stub_a, b"{\"state\":\"completed\"}").unwrap();
    let again = tree.create_reply_stub(&folder, run_a, "lightbulb").unwrap();
    assert_eq!(std::fs::read_to_string(&again).unwrap(), "{\"state\":\"completed\"}");

    // Next run: a fresh per-run file; the previous run's path is removed,
    // never overwritten.
    let run_b = Uuid::new_v4();
    tree.create_reply_stub(&folder, run_b, "lightbulb").unwrap();
    tree.remove_stale_replies(&folder, run_b).unwrap();
    assert!(!stub_a.exists());
    assert!(folder.join(reply_file_name(run_b)).exists());
}

#[test]
fn fire_projection_writes_firing_snapshot_only() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "bulb-test");
    tree.create_for_timer(&timer, None).unwrap();
    let engine = test_engine(dir.path());

    let claim = project_fire(&tree, &mut store, &timer, Utc::now(), &FireKind::OnTime, &engine, Utc::now()).unwrap();

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

    // The claim closing (wake delivered) does NOT fold app states into
    // status.json: R5 `completed`/`failed` are app reports (IK3). The
    // snapshot stays at fired.
    store.complete_run(claim.run_id).unwrap();
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tree.folder_for(timer.id).unwrap().join(STATUS_FILE_NAME))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(v["state"], "fired");
    assert!(v.get("completed_at").is_none());
}

#[test]
fn owned_timer_refire_supersedes_even_when_claim_completed() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "owned");
    store.set_timer_owner(timer.id, "lightbulb").unwrap();
    tree.create_for_timer(&timer, Some("lightbulb")).unwrap();
    let engine = test_engine(dir.path());

    // First fire, and its wake action WAS delivered (claim completed) — but
    // the app never replied, so the run is still unresolved in R5 terms.
    let claim1 = project_fire(&tree, &mut store, &timer, Utc::now(), &FireKind::OnTime, &engine, Utc::now()).unwrap();
    store.complete_run(claim1.run_id).unwrap();

    project_fire(
        &tree,
        &mut store,
        &timer,
        Utc::now() + chrono::Duration::days(1),
        &FireKind::OnTime,
        &engine,
        Utc::now(),
    )
    .unwrap();

    let recs = drain_events(dir.path(), &store);
    let sup: Vec<_> = recs.iter().filter(|r| r.kind == RunState::Superseded).collect();
    assert_eq!(
        sup.len(),
        1,
        "an owned run with no app terminal state is unresolved — claim status is delivery bookkeeping only"
    );
    assert_eq!(sup[0].run_id, Some(claim1.run_id));
}

#[test]
fn unowned_timer_refire_supersedes_only_an_unfinished_claim() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "plain");
    tree.create_for_timer(&timer, None).unwrap();
    let engine = test_engine(dir.path());

    // First fire completed its action claim → resolved → no superseded.
    let claim1 = project_fire(&tree, &mut store, &timer, Utc::now(), &FireKind::OnTime, &engine, Utc::now()).unwrap();
    store.complete_run(claim1.run_id).unwrap();
    let claim2 = project_fire(
        &tree,
        &mut store,
        &timer,
        Utc::now() + chrono::Duration::days(1),
        &FireKind::OnTime,
        &engine,
        Utc::now(),
    )
    .unwrap();
    let recs = drain_events(dir.path(), &store);
    assert!(
        !recs.iter().any(|r| r.kind == RunState::Superseded),
        "finished unowned claim is resolved"
    );

    // Third fire while the second claim is still open → superseded.
    project_fire(
        &tree,
        &mut store,
        &timer,
        Utc::now() + chrono::Duration::days(2),
        &FireKind::OnTime,
        &engine,
        Utc::now(),
    )
    .unwrap();
    let recs = drain_events(dir.path(), &store);
    let sup: Vec<_> = recs.iter().filter(|r| r.kind == RunState::Superseded).collect();
    assert_eq!(sup.len(), 1);
    assert_eq!(sup[0].run_id, Some(claim2.run_id));
}

#[test]
fn second_fire_supersedes_unresolved_first_run() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    let timer = daily_timer(&mut store, "owned");
    store.set_timer_owner(timer.id, "lightbulb").unwrap();
    tree.create_for_timer(&timer, Some("lightbulb")).unwrap();
    let engine = test_engine(dir.path());

    // First fire left unresolved (claimed, never completed).
    let claim1 = project_fire(&tree, &mut store, &timer, Utc::now(), &FireKind::OnTime, &engine, Utc::now()).unwrap();
    let reply1 = tree.folder_for(timer.id).unwrap().join(reply_file_name(claim1.run_id));
    assert!(reply1.exists(), "owned timer gets a per-run reply stub");

    // Second fire: superseded is logged, status rewritten, new stub created,
    // first stub removed (never overwritten).
    let claim2 = project_fire(
        &tree,
        &mut store,
        &timer,
        Utc::now() + chrono::Duration::days(1),
        &FireKind::OnTime,
        &engine,
        Utc::now(),
    )
    .unwrap();

    let recs = drain_events(dir.path(), &store);
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
    let claim = store.claim_run(timer.id, Utc::now()).unwrap();

    let n = log_cancelled_for_open_runs(&store, &timer).unwrap();
    assert_eq!(n, 1);
    store.delete_timer(timer.id).unwrap();
    assert!(tree.remove_for(timer.id).unwrap());

    let recs = drain_events(dir.path(), &store);
    let cancel: Vec<_> = recs.iter().filter(|r| r.kind == RunState::Cancelled).collect();
    assert_eq!(cancel.len(), 1);
    assert_eq!(cancel[0].run_id, Some(claim.run_id));
    assert!(tree.folder_for(timer.id).is_none());
}

#[test]
fn delete_cancels_owned_run_even_with_finished_claim() {
    let (dir, mut store) = test_store();
    let timer = daily_timer(&mut store, "owned-doomed");
    store.set_timer_owner(timer.id, "lightbulb").unwrap();
    let claim = store.claim_run(timer.id, Utc::now()).unwrap();
    store.complete_run(claim.run_id).unwrap();

    // The app never spoke: the owned run is still open in R5 terms.
    let n = log_cancelled_for_open_runs(&store, &timer).unwrap();
    assert_eq!(n, 1);
    let recs = drain_events(dir.path(), &store);
    let cancel: Vec<_> = recs.iter().filter(|r| r.kind == RunState::Cancelled).collect();
    assert_eq!(cancel.len(), 1);
    assert_eq!(cancel[0].run_id, Some(claim.run_id));
}

#[test]
fn colliding_four_hex_ids_get_distinct_folders() {
    let (dir, mut store) = test_store();
    let tree = TimersTree::new(dir.path());
    // Same name AND same first four hex digits — the worst-case collision.
    let id_a = Uuid::parse_str("a001b200-0000-4000-8000-00000000000a").unwrap();
    let id_b = Uuid::parse_str("a001f000-0000-4000-8000-00000000000b").unwrap();
    let occ = || {
        Occurrence::new(
            OccurrenceKind::Daily {
                at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
            },
            "UTC",
        )
        .unwrap()
    };
    let mut new_a = NewTimer::new("same-name", occ());
    new_a.id = Some(id_a);
    let mut new_b = NewTimer::new("same-name", occ());
    new_b.id = Some(id_b);
    let timer_a = store.create_timer(new_a).unwrap();
    let timer_b = store.create_timer(new_b).unwrap();

    let folder_a = tree.create_for_timer(&timer_a, None).unwrap();
    let folder_b = tree.create_for_timer(&timer_b, None).unwrap();

    assert_ne!(folder_a, folder_b, "collision must not share a folder");
    assert!(folder_a.is_dir() && folder_b.is_dir());
    // Each folder's timer.json names its own timer.
    let va: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder_a.join(TIMER_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let vb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder_b.join(TIMER_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(va["timer_id"], id_a.to_string());
    assert_eq!(vb["timer_id"], id_b.to_string());
    // Resolution finds each timer's own folder.
    assert_eq!(tree.folder_for(id_a).unwrap(), folder_a);
    assert_eq!(tree.folder_for(id_b).unwrap(), folder_b);
    // Idempotent re-create reuses the same folder (no suffix growth).
    assert_eq!(tree.create_for_timer(&timer_b, None).unwrap(), folder_b);
    // Renames keep both folders stable.
    let renamed = store
        .update_timer(crate::store::TimerUpdate {
            id: id_b,
            expected_revision: timer_b.revision,
            patch: crate::store::TimerPatch {
                name: Some("other-name".into()),
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(tree.sync_timer_json(&renamed, None).unwrap(), folder_b);
    // Orphan sweep keeps both (their suffixes are prefixes of live ids).
    let live_ids: HashSet<TimerId> = [id_a, id_b].into_iter().collect();
    assert!(tree.sweep_orphans(&live_ids).unwrap().is_empty());
    // Delete one timer: its own folder is swept, the prefix-sharing
    // survivor's folder stays.
    store.delete_timer(id_b).unwrap();
    let live_ids: HashSet<TimerId> = [id_a].into_iter().collect();
    let removed = tree.sweep_orphans(&live_ids).unwrap();
    assert_eq!(removed, vec![folder_b.clone()]);
    assert!(!folder_b.exists());
    assert_eq!(tree.folder_for(id_a).unwrap(), folder_a);
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
