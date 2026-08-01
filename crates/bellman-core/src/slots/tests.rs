//! Slot IPC acceptance / torture tests.
//!
//! Covers: concurrent producers, duplicate request_ids (idempotent),
//! malformed + oversized + symlinked input quarantine, mid-publish tear-safety,
//! free-count invariant ≥ 5, modify/delete ownership.

use super::*;
use crate::store::{OpenOptions, Store};
use chrono::Utc;
use std::fs;
use std::io::Write;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

fn open_harness() -> (tempfile::TempDir, Store, SlotService) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("timers.db");
    let store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .expect("store");
    let slots_root = dir.path().join("slots");
    let service = SlotService::open(&slots_root, SlotConfig::default()).expect("slots");
    (dir, store, service)
}

fn free_stub_count(service: &SlotService) -> usize {
    let mut n = 0;
    for path in service.layout().list_free_files().unwrap() {
        let bytes = read_capped(&path, DEFAULT_MAX_READ_BYTES).unwrap();
        let req: SlotRequest = serde_json::from_slice(&bytes).unwrap();
        if req.is_free_stub() {
            n += 1;
        }
    }
    n
}

#[test]
fn free_replenish_starts_at_min_and_holds_after_claim() {
    let (_dir, mut store, service) = open_harness();
    assert!(free_stub_count(&service) >= MIN_FREE_SLOTS);

    let req = make_add_request("app-a", "t1", "interval", None, Some(60));
    service.publish(req).expect("publish");
    // Filled request occupies a free file; stubs must still be ≥ MIN.
    assert!(
        free_stub_count(&service) >= MIN_FREE_SLOTS,
        "stubs after publish: {}",
        free_stub_count(&service)
    );

    let n = service.poll(&mut store).expect("poll");
    assert_eq!(n, 1);
    assert!(
        free_stub_count(&service) >= MIN_FREE_SLOTS,
        "stubs after claim: {}",
        free_stub_count(&service)
    );
}

#[test]
fn free_stubs_are_valid_schema_json() {
    let (_dir, _store, service) = open_harness();
    let files = service.layout().list_free_files().unwrap();
    assert!(!files.is_empty());
    for path in files {
        let bytes = read_capped(&path, DEFAULT_MAX_READ_BYTES).unwrap();
        let req: SlotRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(req.schema, SCHEMA_V1);
        assert!(req.is_free_stub());
        assert!(!req.slot_id.is_empty());
    }
}

#[test]
fn add_modify_delete_round_trip() {
    let (_dir, mut store, service) = open_harness();

    // ADD
    let add = make_add_request("app-a", "wake", "daily", Some("08:00:00"), None);
    let rid = add.request_id.clone().unwrap();
    service.publish(add).unwrap();
    assert_eq!(service.poll(&mut store).unwrap(), 1);

    let prior = store.get_slot_request(&rid).unwrap().expect("ledger");
    let resp: SlotResponse = serde_json::from_str(&prior.response_json).unwrap();
    assert!(response_is_ok(&resp), "add failed: {:?}", resp.error);
    let timer_id = resp.timer_id.expect("timer id");
    assert!(store.get_timer(timer_id).unwrap().is_some());
    assert_eq!(
        store.get_timer_owner(timer_id).unwrap().as_deref(),
        Some("app-a")
    );

    // MODIFY by owner
    let mod_req = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: String::new(),
        request_id: Some(Uuid::new_v4().to_string()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Modify),
        payload: Some(serde_json::json!({
            "app_name": "app-a",
            "timer_id": timer_id,
            "timer_name": "wake-renamed",
            "time": "09:30:00",
            "occurrence": { "kind": "daily", "time": "09:30:00" },
            "tz": "UTC"
        })),
    };
    service.publish(mod_req).unwrap();
    service.poll(&mut store).unwrap();
    let t = store.get_timer(timer_id).unwrap().unwrap();
    assert_eq!(t.name, "wake-renamed");

    // DELETE by wrong owner → error response
    let bad_del = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: String::new(),
        request_id: Some(Uuid::new_v4().to_string()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Delete),
        payload: Some(serde_json::json!({
            "app_name": "app-b",
            "timer_id": timer_id
        })),
    };
    let bad_rid = bad_del.request_id.clone().unwrap();
    service.publish(bad_del).unwrap();
    service.poll(&mut store).unwrap();
    let prior = store.get_slot_request(&bad_rid).unwrap().unwrap();
    let resp: SlotResponse = serde_json::from_str(&prior.response_json).unwrap();
    assert_eq!(resp.status, SlotStatus::Error);
    assert!(
        resp.error.as_deref().unwrap_or("").contains("ownership"),
        "expected ownership error, got {:?}",
        resp.error
    );
    assert!(store.get_timer(timer_id).unwrap().is_some());

    // DELETE by owner
    let del = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: String::new(),
        request_id: Some(Uuid::new_v4().to_string()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Delete),
        payload: Some(serde_json::json!({
            "app_name": "app-a",
            "timer_id": timer_id
        })),
    };
    service.publish(del).unwrap();
    service.poll(&mut store).unwrap();
    assert!(store.get_timer(timer_id).unwrap().is_none());
}

#[test]
fn duplicate_request_id_is_idempotent() {
    let (_dir, mut store, service) = open_harness();
    let rid = Uuid::new_v4().to_string();
    let mut req = make_add_request("app-a", "once-a", "interval", None, Some(30));
    req.request_id = Some(rid.clone());
    service.publish(req).unwrap();
    service.poll(&mut store).unwrap();
    let first = store.get_slot_request(&rid).unwrap().unwrap();
    let first_resp: SlotResponse = serde_json::from_str(&first.response_json).unwrap();
    let timer_id = first_resp.timer_id.unwrap();

    // Re-publish same request_id with different name — must not create a second timer.
    let mut req2 = make_add_request("app-a", "once-a-DUPLICATE", "interval", None, Some(30));
    req2.request_id = Some(rid.clone());
    service.publish(req2).unwrap();
    service.poll(&mut store).unwrap();
    let second = store.get_slot_request(&rid).unwrap().unwrap();
    assert_eq!(first.response_json, second.response_json);
    // Only one timer with the original name.
    let timers = store.list_timers().unwrap();
    let ours: Vec<_> = timers.iter().filter(|t| t.id == timer_id).collect();
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].name, "once-a");
    assert_eq!(
        timers
            .iter()
            .filter(|t| t.name == "once-a-DUPLICATE")
            .count(),
        0
    );
}

#[test]
fn malformed_input_quarantined_to_bad() {
    let (_dir, mut store, service) = open_harness();
    // Overwrite a free stub with garbage via atomic write.
    let free = service.layout().list_free_files().unwrap();
    let target = free.first().unwrap();
    let name = target.file_name().unwrap().to_string_lossy().to_string();
    atomic_write_bytes(&service.layout().free_dir(), target, b"{not-valid-json!!!").unwrap();
    // Re-check the file is garbage.
    let _ = name;
    service.poll(&mut store).unwrap();
    let bad = service.layout().bad_dir();
    let entries: Vec<_> = fs::read_dir(&bad)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .collect();
    assert!(
        !entries.is_empty(),
        "expected quarantine files in bad/, got none"
    );
    // Sidecar present.
    assert!(
        entries
            .iter()
            .any(|e| { e.file_name().to_string_lossy().ends_with(".err.json") }),
        "expected .err.json sidecar"
    );
    assert!(free_stub_count(&service) >= MIN_FREE_SLOTS);
}

#[test]
fn oversized_input_quarantined() {
    let (_dir, mut store, service) = open_harness();
    let free = service.layout().list_free_files().unwrap();
    let target = free.first().unwrap().clone();
    let big = vec![b'x'; (DEFAULT_MAX_READ_BYTES as usize) + 64];
    // Wrap as almost-json so it's a file; size cap triggers before full parse.
    let mut payload = b"{".to_vec();
    payload.extend_from_slice(&big);
    atomic_write_bytes(&service.layout().free_dir(), &target, &payload).unwrap();
    service.poll(&mut store).unwrap();
    let bad_count = fs::read_dir(service.layout().bad_dir())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .count();
    assert!(bad_count > 0, "oversized must quarantine");
    assert!(free_stub_count(&service) >= MIN_FREE_SLOTS);
}

#[test]
fn symlink_input_quarantined() {
    let (_dir, mut store, service) = open_harness();
    let free_dir = service.layout().free_dir();
    // Live-target symlink.
    let outside = service.layout().root().join("outside.json");
    fs::write(
        &outside,
        serde_json::to_vec(&make_add_request("app", "t", "interval", None, Some(10))).unwrap(),
    )
    .unwrap();
    let link = free_dir.join("slot-symlink.json");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
    }
    #[cfg(not(unix))]
    {
        return;
    }
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    service.poll(&mut store).unwrap();

    // Use symlink_metadata — Path::exists follows the target and is wrong for links.
    assert!(
        fs::symlink_metadata(&link).is_err(),
        "live-target symlink must leave free/"
    );
    assert!(
        outside.exists(),
        "quarantine must not delete the link target"
    );
    let bad_entries: Vec<_> = fs::read_dir(service.layout().bad_dir())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        bad_entries.iter().any(|n| n.contains("slot-symlink")),
        "expected quarantined symlink in bad/, got {bad_entries:?}"
    );
    assert!(
        bad_entries.iter().any(|n| n.ends_with(".err.json")),
        "expected .err.json sidecar in bad/, got {bad_entries:?}"
    );
    assert!(free_stub_count(&service) >= MIN_FREE_SLOTS);
}

#[test]
fn dangling_symlink_input_quarantined() {
    // Acceptance: symlinked input is quarantined even when the target is missing.
    let (_dir, mut store, service) = open_harness();
    let free_dir = service.layout().free_dir();
    let missing = free_dir.join("missing-target.json");
    let link = free_dir.join("slot-dangle.json");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&missing, &link).expect("dangling symlink");
    }
    #[cfg(not(unix))]
    {
        return;
    }
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(
        !link.exists(),
        "precondition: dangling (exists follows target)"
    );

    service.poll(&mut store).unwrap();

    assert!(
        fs::symlink_metadata(&link).is_err(),
        "dangling symlink must be removed from free/, not left for rediscovery"
    );
    let bad_entries: Vec<_> = fs::read_dir(service.layout().bad_dir())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        bad_entries.iter().any(|n| n.contains("slot-dangle")),
        "expected quarantined dangling link in bad/, got {bad_entries:?}"
    );
    assert!(
        bad_entries.iter().any(|n| n.ends_with(".err.json")),
        "expected .err.json sidecar in bad/, got {bad_entries:?}"
    );
    assert!(free_stub_count(&service) >= MIN_FREE_SLOTS);
}

#[test]
fn kill_mid_publish_leaves_no_torn_final() {
    // Simulate a crashed producer: a temp file left behind, final path still a stub.
    let (_dir, mut store, service) = open_harness();
    let free = service.layout().list_free_files().unwrap();
    let stub_path = free.first().unwrap().clone();
    let before: SlotRequest =
        serde_json::from_slice(&read_capped(&stub_path, DEFAULT_MAX_READ_BYTES).unwrap()).unwrap();
    assert!(before.is_free_stub());

    // Write a temp file in free/ that is NOT persisted (kill mid-publish).
    let tmp_name = format!(".tmp-kill-{}.json", Uuid::new_v4());
    let tmp_path = service.layout().free_dir().join(&tmp_name);
    let mut f = fs::File::create(&tmp_path).unwrap();
    f.write_all(br#"{"schema":"bellman-slot/1","slot_id":"x","request_id":"partial"#)
        .unwrap();
    // Intentionally do not rename over the stub.

    service.poll(&mut store).unwrap();
    // Stub still intact (not torn).
    let after: SlotRequest =
        serde_json::from_slice(&read_capped(&stub_path, DEFAULT_MAX_READ_BYTES).unwrap()).unwrap();
    assert!(after.is_free_stub());
    assert_eq!(after.slot_id, before.slot_id);
    // Temp leftover is not treated as a free stub / request (not matching slot-*.json
    // or starts with .). Our list skips dotfiles.
    assert!(free_stub_count(&service) >= MIN_FREE_SLOTS);
    // Cleanup temp for harness.
    let _ = fs::remove_file(tmp_path);
}

#[test]
fn concurrent_producers_all_get_unique_slots() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timers.db");
    // Shared store path; each thread opens its own connection.
    {
        let _ = Store::open_with(
            &db,
            OpenOptions {
                refuse_network_fs: false,
                ..Default::default()
            },
        )
        .unwrap();
    }
    let slots_root = dir.path().join("slots");
    let service = SlotService::open(&slots_root, SlotConfig::default()).unwrap();
    // Publish 8 concurrent adds — create extra free stubs for concurrency headroom.
    let n_prod = 8usize;
    {
        let layout = service.layout();
        for i in 100..120 {
            let id = format!("{i:04}");
            let name = format!("slot-{id}.json");
            let path = layout.free_dir().join(&name);
            if !path.exists() {
                atomic_write_json(&layout.free_dir(), &name, &SlotRequest::free_stub(&id)).unwrap();
            }
        }
    }

    let barrier = Arc::new(Barrier::new(n_prod));
    let slots_root = Arc::new(slots_root);
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for i in 0..n_prod {
        let barrier = Arc::clone(&barrier);
        let slots_root = Arc::clone(&slots_root);
        let results = Arc::clone(&results);
        handles.push(thread::spawn(move || {
            let svc = SlotService::open(slots_root.as_path(), SlotConfig::default()).unwrap();
            barrier.wait();
            let req = make_add_request(
                &format!("app-{i}"),
                &format!("timer-{i}"),
                "interval",
                None,
                Some(60),
            );
            let rid = req.request_id.clone().unwrap();
            let path = svc.publish(req);
            results.lock().unwrap().push((rid, path.is_ok()));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let results = results.lock().unwrap();
    let ok_count = results.iter().filter(|(_, ok)| *ok).count();
    assert_eq!(
        ok_count, n_prod,
        "all producers should publish: {results:?}"
    );

    // Single consumer processes all.
    let mut store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .unwrap();
    let service = SlotService::open(slots_root.as_path(), SlotConfig::default()).unwrap();
    // Poll until drained (multiple passes if needed).
    for _ in 0..16 {
        let n = service.poll(&mut store).unwrap();
        if n == 0 && service.layout().list_work_files().unwrap().is_empty() {
            // Check free has no filled left.
            let filled = service
                .layout()
                .list_free_files()
                .unwrap()
                .into_iter()
                .filter(|p| {
                    let b = read_capped(p, DEFAULT_MAX_READ_BYTES).unwrap();
                    let r: SlotRequest =
                        serde_json::from_slice(&b).unwrap_or_else(|_| SlotRequest::free_stub("x"));
                    !r.is_free_stub()
                })
                .count();
            if filled == 0 {
                break;
            }
        }
    }
    let timers = store.list_timers().unwrap();
    assert_eq!(
        timers.len(),
        n_prod,
        "expected {n_prod} timers, got {}",
        timers.len()
    );
    assert!(free_stub_count(&service) >= MIN_FREE_SLOTS);
}

#[test]
fn unknown_fields_are_tolerated() {
    let (_dir, mut store, service) = open_harness();
    let req = make_add_request("app-a", "tol", "interval", None, Some(45));
    // Inject unknown fields at envelope + payload level via raw JSON publish.
    let rid = req.request_id.clone().unwrap();
    // Grab a free stub id.
    let free = service.layout().list_free_files().unwrap();
    let path = free.first().unwrap();
    let stub: SlotRequest =
        serde_json::from_slice(&read_capped(path, DEFAULT_MAX_READ_BYTES).unwrap()).unwrap();
    let raw = serde_json::json!({
        "schema": SCHEMA_V1,
        "slot_id": stub.slot_id,
        "request_id": rid,
        "logged_at": Utc::now(),
        "operation": "add",
        "future_field": 123,
        "payload": {
            "app_name": "app-a",
            "timer_name": "tol",
            "every_secs": 45,
            "extra_ignore_me": true,
            "occurrence": { "kind": "interval", "every_secs": 45, "bonus": 1 }
        }
    });
    let name = path.file_name().unwrap().to_str().unwrap();
    atomic_write_json(&service.layout().free_dir(), name, &raw).unwrap();
    service.poll(&mut store).unwrap();
    let prior = store.get_slot_request(&rid).unwrap().unwrap();
    let resp: SlotResponse = serde_json::from_str(&prior.response_json).unwrap();
    assert!(response_is_ok(&resp), "{:?}", resp.error);
}

#[test]
fn output_includes_run_events_from_runs_table() {
    let (_dir, mut store, service) = open_harness();
    let req = make_add_request("app-a", "ev", "interval", None, Some(60));
    let rid = req.request_id.clone().unwrap();
    service.publish(req).unwrap();
    service.poll(&mut store).unwrap();
    let prior = store.get_slot_request(&rid).unwrap().unwrap();
    let resp: SlotResponse = serde_json::from_str(&prior.response_json).unwrap();
    let timer_id = resp.timer_id.unwrap();
    // Claim a run so events is non-empty on a subsequent modify.
    let scheduled = Utc::now();
    let claim = store.claim_run(timer_id, scheduled).unwrap();
    assert_eq!(claim.event_sequence, 1);

    let mod_req = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: String::new(),
        request_id: Some(Uuid::new_v4().to_string()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Modify),
        payload: Some(serde_json::json!({
            "app_name": "app-a",
            "timer_id": timer_id,
            "timer_name": "ev2",
            "every_secs": 90,
            "occurrence": { "kind": "interval", "every_secs": 90 }
        })),
    };
    let mrid = mod_req.request_id.clone().unwrap();
    service.publish(mod_req).unwrap();
    service.poll(&mut store).unwrap();
    let prior = store.get_slot_request(&mrid).unwrap().unwrap();
    let resp: SlotResponse = serde_json::from_str(&prior.response_json).unwrap();
    assert!(response_is_ok(&resp), "{:?}", resp.error);
    assert!(!resp.events.is_empty(), "expected run events in output");
    assert_eq!(resp.events[0].event_sequence, 1);
}

#[test]
fn concurrent_duplicate_request_id_single_side_effect() {
    // Two free slots carry the same request_id; concurrent pollers must not
    // create two timers — ledger + mutations are one Immediate transaction.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timers.db");
    {
        let _ = Store::open_with(
            &db,
            OpenOptions {
                refuse_network_fs: false,
                ..Default::default()
            },
        )
        .unwrap();
    }
    let slots_root = dir.path().join("slots");
    let service = SlotService::open(&slots_root, SlotConfig::default()).unwrap();
    for i in 200..210 {
        let id = format!("{i:04}");
        let name = format!("slot-{id}.json");
        atomic_write_json(
            &service.layout().free_dir(),
            &name,
            &SlotRequest::free_stub(&id),
        )
        .unwrap();
    }

    let rid = Uuid::new_v4().to_string();
    let fixed_timer = Uuid::new_v4();
    for i in 0..2 {
        let free = service.layout().list_free_files().unwrap();
        let stub_path = free
            .iter()
            .find(|p| {
                let b = read_capped(p, DEFAULT_MAX_READ_BYTES).unwrap();
                serde_json::from_slice::<SlotRequest>(&b).is_ok_and(|r| r.is_free_stub())
            })
            .unwrap()
            .clone();
        let stub: SlotRequest =
            serde_json::from_slice(&read_capped(&stub_path, DEFAULT_MAX_READ_BYTES).unwrap())
                .unwrap();
        let req = SlotRequest {
            schema: SCHEMA_V1.to_string(),
            slot_id: stub.slot_id,
            request_id: Some(rid.clone()),
            logged_at: Some(Utc::now()),
            operation: Some(SlotOperation::Add),
            payload: Some(serde_json::json!({
                "app_name": "app-dup",
                "timer_name": format!("dup-{i}"),
                "timer_id": fixed_timer,
                "every_secs": 30,
                "occurrence": { "kind": "interval", "every_secs": 30 },
                "tz": "UTC"
            })),
        };
        let name = stub_path.file_name().unwrap().to_str().unwrap();
        atomic_write_json(&service.layout().free_dir(), name, &req).unwrap();
    }

    let barrier = Arc::new(Barrier::new(2));
    let db = Arc::new(db);
    let slots_root = Arc::new(slots_root);
    let mut handles = Vec::new();
    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        let db = Arc::clone(&db);
        let slots_root = Arc::clone(&slots_root);
        handles.push(thread::spawn(move || {
            let mut store = Store::open_with(
                db.as_path(),
                OpenOptions {
                    refuse_network_fs: false,
                    ..Default::default()
                },
            )
            .unwrap();
            let svc = SlotService::open(slots_root.as_path(), SlotConfig::default()).unwrap();
            barrier.wait();
            svc.poll(&mut store).unwrap()
        }));
    }
    let mut total_processed = 0usize;
    for h in handles {
        total_processed += h.join().unwrap();
    }
    assert!(total_processed >= 1);

    let store = Store::open_with(
        db.as_path(),
        OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .unwrap();
    let timers = store.list_timers().unwrap();
    assert_eq!(
        timers.len(),
        1,
        "duplicate request_id must create exactly one timer, got {}",
        timers.len()
    );
    assert_eq!(timers[0].id, fixed_timer);
    let prior = store.get_slot_request(&rid).unwrap().unwrap();
    assert_eq!(prior.status, "ok");
}

#[test]
fn unacked_events_drain_via_ack_through() {
    // max_events=2; create 5 runs; first response gets 1..2, ack 2, next gets 3..4.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timers.db");
    let mut store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .unwrap();
    let slots_root = dir.path().join("slots");
    let service = SlotService::open(
        &slots_root,
        SlotConfig {
            max_events: 2,
            ..SlotConfig::default()
        },
    )
    .unwrap();

    let req = make_add_request("app-a", "ack-t", "interval", None, Some(60));
    let rid = req.request_id.clone().unwrap();
    service.publish(req).unwrap();
    service.poll(&mut store).unwrap();
    let prior = store.get_slot_request(&rid).unwrap().unwrap();
    let resp: SlotResponse = serde_json::from_str(&prior.response_json).unwrap();
    let timer_id = resp.timer_id.unwrap();

    for i in 0..5 {
        store
            .claim_run(timer_id, Utc::now() + chrono::Duration::seconds(i))
            .unwrap();
    }
    assert_eq!(store.runs_for_timer(timer_id).unwrap().len(), 5);

    let m1 = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: String::new(),
        request_id: Some(Uuid::new_v4().to_string()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Modify),
        payload: Some(serde_json::json!({
            "app_name": "app-a",
            "timer_id": timer_id,
            "timer_name": "ack-t-1",
            "every_secs": 60,
            "occurrence": { "kind": "interval", "every_secs": 60 }
        })),
    };
    let m1id = m1.request_id.clone().unwrap();
    service.publish(m1).unwrap();
    service.poll(&mut store).unwrap();
    let r1: SlotResponse = serde_json::from_str(
        &store
            .get_slot_request(&m1id)
            .unwrap()
            .unwrap()
            .response_json,
    )
    .unwrap();
    assert_eq!(r1.events.len(), 2);
    assert_eq!(r1.events[0].event_sequence, 1);
    assert_eq!(r1.events[1].event_sequence, 2);

    let m2 = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: String::new(),
        request_id: Some(Uuid::new_v4().to_string()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Modify),
        payload: Some(serde_json::json!({
            "app_name": "app-a",
            "timer_id": timer_id,
            "timer_name": "ack-t-2",
            "ack_through": 2,
            "every_secs": 60,
            "occurrence": { "kind": "interval", "every_secs": 60 }
        })),
    };
    let m2id = m2.request_id.clone().unwrap();
    service.publish(m2).unwrap();
    service.poll(&mut store).unwrap();
    let r2: SlotResponse = serde_json::from_str(
        &store
            .get_slot_request(&m2id)
            .unwrap()
            .unwrap()
            .response_json,
    )
    .unwrap();
    assert_eq!(r2.events.len(), 2);
    assert_eq!(r2.events[0].event_sequence, 3);
    assert_eq!(r2.events[1].event_sequence, 4);
    assert_eq!(store.last_acked_sequence(timer_id).unwrap(), 2);

    store.ack_run_events(timer_id, 5).unwrap();
    let leftover = store.unacked_runs_for_timer(timer_id, 64).unwrap();
    assert!(leftover.is_empty());
}

#[test]
fn done_gc_with_zero_retention_clears() {
    let (_dir, _store, service) = open_harness();
    let done = service.layout().done_dir();
    let path = done.join("slot-fresh.json");
    fs::write(&path, b"{}").unwrap();
    // Zero retention: anything with mtime <= now is removed.
    let removed = service.layout().gc_done(Duration::from_secs(0)).unwrap();
    assert!(removed >= 1);
    assert!(!path.exists());
}

#[test]
fn poll_once_helper_works() {
    let (_dir, mut store, service) = open_harness();
    let req = make_add_request("app-a", "p", "interval", None, Some(12));
    service.publish(req).unwrap();
    let n = poll_once(&service, &mut store).unwrap();
    assert_eq!(n, 1);
}

#[test]
fn atomic_write_never_leaves_partial_final() {
    let dir = tempfile::tempdir().unwrap();
    let final_path = dir.path().join("out.json");
    atomic_write_json(dir.path(), "out.json", &serde_json::json!({"a":1})).unwrap();
    let s = fs::read_to_string(&final_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["a"], 1);
}

#[test]
fn atomic_write_rejects_path_traversal_names() {
    let dir = tempfile::tempdir().unwrap();
    let err = atomic_write_json(
        dir.path(),
        "slot-x/../../../escaped.json",
        &serde_json::json!({"x": 1}),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("unsafe") || err.to_string().contains("forbidden"),
        "got {err}"
    );
    assert!(!dir.path().join("escaped.json").exists());
    // Parent of dir must not gain escaped.json either.
    assert!(!dir.path().parent().unwrap().join("escaped.json").exists());
}

#[test]
fn slot_id_path_traversal_quarantined_no_escape_write() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timers.db");
    let mut store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .unwrap();
    let slots_root = dir.path().join("slots");
    let service = SlotService::open(&slots_root, SlotConfig::default()).unwrap();

    // Claim free/slot-0001.json shape — use first free stub.
    let free = service.layout().list_free_files().unwrap();
    let stub_path = free.first().unwrap().clone();
    let stub: SlotRequest =
        serde_json::from_slice(&read_capped(&stub_path, DEFAULT_MAX_READ_BYTES).unwrap()).unwrap();
    let reserved = stub.slot_id.clone();
    let name = stub_path.file_name().unwrap().to_str().unwrap().to_string();

    let rid = Uuid::new_v4().to_string();
    let evil = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: "slot-x/../../../escaped".into(),
        request_id: Some(rid.clone()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Add),
        payload: Some(serde_json::json!({
            "app_name": "evil",
            "timer_name": "escape",
            "every_secs": 30,
            "occurrence": { "kind": "interval", "every_secs": 30 },
            "tz": "UTC"
        })),
    };
    atomic_write_json(&service.layout().free_dir(), &name, &evil).unwrap();
    service.poll(&mut store).unwrap();

    // No timer created (quarantined before apply).
    assert!(
        store.list_timers().unwrap().is_empty(),
        "traversal slot_id must not apply"
    );
    assert!(
        store.get_slot_request(&rid).unwrap().is_none(),
        "must not ledger a traversal request"
    );

    // No escape file outside done/.
    let escaped = dir.path().join("escaped.json");
    assert!(!escaped.exists(), "escaped.json must not appear at root");
    let escaped_done = service
        .layout()
        .done_dir()
        .join("slot-x")
        .join("../../../escaped.json");
    // Even if joined, the realpath outside done should not exist as written content.
    assert!(
        !dir.path().join("escaped.json").exists(),
        "no write outside done"
    );
    let _ = escaped_done;

    // Work/free must not retain the evil filled request as processable forever —
    // it should be in bad/.
    let bad: Vec<_> = fs::read_dir(service.layout().bad_dir())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        bad.iter().any(|n| n.contains(&format!("slot-{reserved}"))
            || n.contains(&name)
            || n.ends_with(".err.json")),
        "expected quarantine in bad/, got {bad:?}"
    );
}

#[test]
fn cross_slot_id_forge_cannot_overwrite_foreign_done() {
    let (_dir, mut store, service) = open_harness();

    // Legitimate request in slot A.
    let req_a = make_add_request("app-a", "legit", "interval", None, Some(40));
    let rid_a = req_a.request_id.clone().unwrap();
    service.publish(req_a).unwrap();
    service.poll(&mut store).unwrap();
    let prior = store.get_slot_request(&rid_a).unwrap().unwrap();
    let resp_a: SlotResponse = serde_json::from_str(&prior.response_json).unwrap();
    assert!(response_is_ok(&resp_a));
    let done_a = service
        .layout()
        .done_dir()
        .join(format!("slot-{}.json", prior.slot_id));
    let before = fs::read(&done_a).expect("done A exists");
    let victim_id = prior.slot_id.clone();

    // Forge: free stub B with envelope slot_id = victim A's id.
    let free = service.layout().list_free_files().unwrap();
    let stub_b = free
        .iter()
        .find(|p| {
            let b = read_capped(p, DEFAULT_MAX_READ_BYTES).unwrap();
            serde_json::from_slice::<SlotRequest>(&b)
                .is_ok_and(|r| r.is_free_stub() && r.slot_id != victim_id)
        })
        .expect("another free stub")
        .clone();
    let stub_req: SlotRequest =
        serde_json::from_slice(&read_capped(&stub_b, DEFAULT_MAX_READ_BYTES).unwrap()).unwrap();
    let name_b = stub_b.file_name().unwrap().to_str().unwrap().to_string();
    let rid_b = Uuid::new_v4().to_string();
    let forge = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: victim_id.clone(), // forge foreign slot
        request_id: Some(rid_b),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Add),
        payload: Some(serde_json::json!({
            "app_name": "forger",
            "timer_name": "overwrite-me",
            "every_secs": 99,
            "occurrence": { "kind": "interval", "every_secs": 99 },
            "tz": "UTC"
        })),
    };
    atomic_write_json(&service.layout().free_dir(), &name_b, &forge).unwrap();
    service.poll(&mut store).unwrap();

    // Victim done/ response unchanged.
    let after = fs::read(&done_a).unwrap();
    assert_eq!(before, after, "foreign done/slot must not be overwritten");

    // Forger must not create a second timer either (quarantined).
    let timers = store.list_timers().unwrap();
    assert_eq!(timers.len(), 1, "forge must not apply add");
    assert_eq!(timers[0].name, "legit");
    let _ = stub_req;
}

#[test]
fn slot_delete_cancels_an_open_app_run_even_with_a_finished_claim() {
    let (dir, mut store, service) = open_harness();
    let service = service.with_timers_tree(crate::tree::TimersTree::new(dir.path()));

    // Owned timer via the slot path.
    let req = make_add_request("app-a", "owned-run", "interval", None, Some(60));
    service.publish(req).unwrap();
    service.poll(&mut store).unwrap();
    let timer = store.list_timers().unwrap()[0].clone();

    // A run whose ACTION claim is finished (wake delivered) but whose app
    // lifecycle is still open (state running). This is exactly the case the
    // old post-commit ordering lost: owner cleared before the cancel check.
    let claim = store.claim_run(timer.id, Utc::now()).unwrap();
    store.complete_run(claim.run_id).unwrap();
    let mut row = crate::store::RunStateRow::fired(
        claim.run_id,
        timer.id,
        "app-a",
        "fired",
        Utc::now(),
        Utc::now() + chrono::Duration::seconds(60),
    );
    row.state = "running".to_string();
    store.insert_run_state(&row).unwrap();

    // DELETE by the owner.
    let del = SlotRequest {
        schema: SCHEMA_V1.to_string(),
        slot_id: String::new(),
        request_id: Some(Uuid::new_v4().to_string()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Delete),
        payload: Some(serde_json::json!({
            "app_name": "app-a",
            "timer_id": timer.id
        })),
    };
    service.publish(del).unwrap();
    service.poll(&mut store).unwrap();
    assert!(store.get_timer(timer.id).unwrap().is_none());

    // The cancelled event committed WITH the delete: find it in the outbox.
    let pending = store.pending_events(100).unwrap();
    let cancelled: Vec<_> = pending
        .iter()
        .filter_map(|(_, payload)| serde_json::from_str::<crate::events::EventRecord>(payload).ok())
        .filter(|e| e.kind == crate::events::RunState::Cancelled && e.run_id == Some(claim.run_id))
        .collect();
    assert_eq!(cancelled.len(), 1, "cancelled committed with the delete");

    // And the lifecycle row is closed — its deadlines must not fire later.
    let row = store.get_run_state(claim.run_id).unwrap().unwrap();
    assert_eq!(row.state, "cancelled");
    assert!(row.pickup_deadline.is_none());
    assert!(row.watchdog_deadline.is_none());
}

// --- SCH2 Path A: watcher-processed slot requests refill the scheduler ---
//
// An external app publishes to free/ and the RUNNING Bellman's watcher
// claims and applies the request. The watcher knows a mutation happened, so
// it must refill the scheduler's horizon heap — before this fix nothing in
// slots/ ever signalled the scheduler and the timer never fired.

/// SCH2 regression (Path A): publish to free/, let the running watcher claim
/// it, and the timer must fire PROMPTLY on a running scheduler — no restart,
/// no GUI edit, no manual refill.
///
/// Probe-proof on purpose: `max_sleep` is 60 s, so the scheduler's tick (and
/// with it the data_version probe the watcher's own commit would trip) does
/// not run again within the test window, and the rebuild floor is disabled.
/// The ONLY thing that can wake the loop in time is the watcher's Refill
/// control message — reverting `refill_if_mutated` turns this test red.
#[test]
fn watcher_claimed_slot_add_fires_running_scheduler() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    let db = data_dir.join("timers.db");
    let slots_root = data_dir.join("slots");

    // The running Bellman: a scheduler thread with its own connection.
    let sched_store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let mut sched = crate::scheduler::Scheduler::new(
        sched_store,
        crate::scheduler::SystemClock::new(),
        crate::scheduler::RecordingAction::new(),
        crate::scheduler::SchedulerConfig::default()
            // 60 s between ticks: without the watcher's Refill interrupting
            // the sleep, nothing fires inside the test window.
            .with_max_sleep(Duration::from_secs(60))
            // Floor disabled too — no periodic rebuild can rescue it either.
            .with_external_rebuild_interval(Duration::from_secs(3600)),
    );
    let handle = sched.control_handle();
    let sched_thread = thread::spawn(move || sched.run_until_shutdown());

    // The running app's watcher (Path A): claims the request itself.
    let watch = spawn_watch_thread(WatchConfig {
        slots_root: slots_root.clone(),
        data_dir: data_dir.clone(),
        db_path: db.clone(),
        reply_engine: None,
        scheduler: Some(handle.clone()),
        poll_interval: Duration::from_millis(50),
    })
    .expect("watch thread");

    // Let the scheduler finish booting (empty-heap rebuild) before the
    // publish, so the timer cannot slip in via the boot snapshot.
    thread::sleep(Duration::from_millis(500));

    // External app publishes to free/ and waits — no slot-submit, no poll of
    // its own. The running Bellman's watcher must claim it.
    let ext_service = SlotService::open(&slots_root, SlotConfig::default()).unwrap();
    let t_publish = Utc::now();
    ext_service
        .publish(make_add_request(
            "app-a",
            "watched",
            "interval",
            None,
            Some(1),
        ))
        .unwrap();

    // Wait past two intervals, then shut everything down.
    thread::sleep(Duration::from_millis(6000));
    handle.shutdown();
    let result = sched_thread.join().unwrap().unwrap();
    watch.stop();

    assert!(
        result.refilled,
        "the watcher must refill the scheduler after claiming a slot request"
    );
    assert!(
        result.fires.len() >= 2,
        "slot-created timer must fire on its own interval, got {}",
        result.fires.len()
    );
    // Promptness (INTEGRATION.md rule 5: watcher-applied requests are live
    // immediately): the first fire lands on its own scheduled second, a
    // second or two after publishing — not one max_sleep tick (60 s) later.
    let first = &result.fires[0];
    assert!(
        first.scheduled_for <= t_publish + chrono::Duration::seconds(5),
        "first fire must land within seconds of publishing (prompt refill), got {:?} (published {t_publish:?})",
        first.scheduled_for
    );
}

/// C11 regression: the ONE background watcher must survive a data directory
/// whose `timers/` root does not exist yet.
///
/// `timers/` is created lazily by the first timer folder, but the watcher
/// watches it **recursively from startup** — and `notify`'s `watch()` on a
/// missing path is a hard error that ends the thread. When that happened the
/// app kept running and looked healthy while reply ingest, the slot channel
/// and the event publisher were all dead for the life of the process.
///
/// Seen for real: a container whose `/etc/localtime` pointed at
/// `/usr/share/zoneinfo//UTC` made `startup_maintenance` abort, so nothing
/// had created `timers/` by the time the watcher started; the packaged demo
/// then answered its fire correctly and the run still went `no_ack`.
#[test]
fn watcher_starts_on_a_data_dir_with_no_timers_root_yet() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    let db = data_dir.join("timers.db");
    // Only what the app creates up front — deliberately no `timers/`.
    fs::create_dir_all(data_dir.join("logs")).unwrap();
    fs::create_dir_all(data_dir.join("slots")).unwrap();
    let _store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    assert!(
        !data_dir.join("timers").exists(),
        "precondition: the tree root must be missing"
    );

    let engine = crate::reply::ReplyEngine {
        tree: crate::tree::TimersTree::new(&data_dir),
        data_dir: data_dir.clone(),
        pickup_grace: Duration::from_secs(60),
        watchdog_factor: 2.0,
        anchors: crate::reply::new_anchors(),
        deadlines: crate::reply::new_deadlines(),
        fire_slot_file: None,
        status_listener: None,
        ipc: None,
    };
    let stop = crate::slots::watcher::spawn_watch_thread(crate::slots::watcher::WatchConfig {
        slots_root: data_dir.join("slots"),
        data_dir: data_dir.clone(),
        db_path: db.clone(),
        reply_engine: Some(engine),
        scheduler: None,
        poll_interval: Duration::from_millis(100),
    })
    .expect("watcher spawns");

    // Give the thread time to reach its watch calls and its first poll.
    thread::sleep(Duration::from_millis(600));
    assert!(
        data_dir.join("timers").is_dir(),
        "the watcher must create the tree root rather than die on it"
    );

    // Still alive and doing its job: publish a slot request and see it applied.
    let service = SlotService::open(data_dir.join("slots"), SlotConfig::default()).unwrap();
    let free = service.layout().free_dir();
    let stub = fs::read_dir(&free)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "json"))
        .expect("a free stub is pre-generated");
    let slot_id = stub.file_stem().unwrap().to_string_lossy().to_string();
    let slot_id = slot_id.trim_start_matches("slot-").to_string();
    let req = serde_json::json!({
        "schema": "bellman-slot/1",
        "slot_id": slot_id,
        "request_id": Uuid::new_v4().to_string(),
        "operation": "add",
        "payload": {
            "app_name": "watcher-liveness",
            "timer_name": "watcher-liveness-timer",
            "tz": "UTC",
            "occurrence": {"kind": "interval", "every_secs": 3600}
        }
    });
    let tmp = free.join("publish.tmp");
    fs::write(&tmp, serde_json::to_vec(&req).unwrap()).unwrap();
    fs::rename(&tmp, &stub).unwrap();

    let store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let mut seen = false;
    for _ in 0..100 {
        thread::sleep(Duration::from_millis(100));
        if store
            .list_timers()
            .unwrap()
            .iter()
            .any(|t| t.name == "watcher-liveness-timer")
        {
            seen = true;
            break;
        }
    }
    stop.stop();
    assert!(
        seen,
        "a live watcher applies the request; a dead one silently never does"
    );
}
