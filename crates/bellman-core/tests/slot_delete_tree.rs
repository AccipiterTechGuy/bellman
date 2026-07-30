//! Slot add/delete drives the per-timer folder tree (IK2): an open run must
//! be logged `cancelled` before the folder is removed.
use bellman_core::events::{read_events, EventPublisher, RunState};
use bellman_core::slots::{make_add_request, SlotConfig, SlotService};
use bellman_core::store::{OpenOptions, Store};
use bellman_core::tree::TimersTree;
use bellman_core::{SlotOperation, SlotRequest};
use chrono::Utc;

#[test]
fn slot_delete_logs_cancelled_for_open_run() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut store = Store::open_with(data.join("timers.db"), OpenOptions {
        refuse_network_fs: false,
        ..OpenOptions::default()
    })
    .unwrap();
    let slots_root = data.join("slots");
    let service = SlotService::open(&slots_root, SlotConfig::default())
        .unwrap()
        .with_timers_tree(TimersTree::new(data));

    // Add via slot (owner = app-a).
    let mut add = make_add_request("app-a", "owned", "daily", Some("08:00:00"), None);
    add.request_id = Some("repro-add".into());
    service.publish(add).unwrap();
    service.poll(&mut store).unwrap();
    let timer = store.list_timers().unwrap().into_iter().find(|t| t.name == "owned").unwrap();
    assert!(TimersTree::new(data).folder_for(timer.id).is_some());

    // Open run (claimed, never completed).
    let claim = store.claim_run(timer.id, Utc::now()).unwrap();

    // Delete via slot.
    let del = SlotRequest {
        schema: bellman_core::slots::SCHEMA_V1.into(),
        slot_id: String::new(),
        operation: Some(SlotOperation::Delete),
        request_id: Some("repro-del".into()),
        payload: Some(serde_json::json!({"app_name": "app-a", "timer_id": timer.id.to_string()})),
        logged_at: Some(Utc::now()),
    };
    service.publish(del).unwrap();
    service.poll(&mut store).unwrap();

    assert!(store.get_timer(timer.id).unwrap().is_none());
    assert!(TimersTree::new(data).folder_for(timer.id).is_none(), "folder removed");
    // The cancelled event is enqueued in the outbox (R11); drain it with the
    // elected publisher, then read the log.
    let mut publisher = EventPublisher::open(data).unwrap();
    publisher.publish_cycle(&store);
    let (recs, _) = read_events(publisher.current_path()).unwrap();
    let cancel: Vec<_> = recs.iter().filter(|r| r.kind == RunState::Cancelled).collect();
    assert_eq!(cancel.len(), 1, "cancelled event must be logged");
    assert_eq!(cancel[0].run_id, Some(claim.run_id));
}
