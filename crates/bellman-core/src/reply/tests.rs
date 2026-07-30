//! IK3 reply-channel tests: the transport-agnostic engine and the file
//! transport against a real store + folder tree in a temp dir.

use super::engine::ReplyEngine;
use super::watcher::{poll_once, startup_scan, InvalidTracker, PollStats};
use super::*;
use crate::events::{read_events, EventLog, EventLogConfig, EventRecord, RunState};
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::scheduler::FireKind;
use crate::store::{NewTimer, OpenOptions, RunClaim, RunStateRow, Store, Timer};
use crate::tree::{reply_file_name, TimersTree, STATUS_FILE_NAME};
use chrono::{DateTime, NaiveTime, Utc};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

// ── Harness ─────────────────────────────────────────────────────────────

struct Harness {
    dir: tempfile::TempDir,
    store: Store,
    engine: ReplyEngine,
    log: EventLog,
    tracker: InvalidTracker,
    t0: DateTime<Utc>,
}

impl Harness {
    fn new() -> Self {
        Self::with_grace(60, 2.0)
    }

    fn with_grace(pickup_grace_secs: u64, watchdog_factor: f64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open_with(dir.path().join("timers.db"), OpenOptions::default()).unwrap();
        let engine = ReplyEngine {
            tree: TimersTree::new(dir.path()),
            data_dir: dir.path().to_path_buf(),
            pickup_grace: Duration::from_secs(pickup_grace_secs),
            watchdog_factor,
            anchors: new_anchors(),
        };
        let log = EventLog::open(EventLogConfig::new(dir.path().join("logs"))).unwrap();
        Self {
            dir,
            store,
            engine,
            log,
            tracker: InvalidTracker::default(),
            t0: Utc::now(),
        }
    }

    fn add_timer(&mut self, name: &str, owner: Option<&str>) -> Timer {
        let occ = Occurrence::new(
            OccurrenceKind::Daily {
                at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
            },
            "UTC",
        )
        .unwrap();
        let timer = self.store.create_timer(NewTimer::new(name, occ)).unwrap();
        if let Some(app) = owner {
            self.store.set_timer_owner(timer.id, app).unwrap();
        }
        self.engine.tree.create_for_timer(&timer, owner).unwrap();
        timer
    }

    /// Claim + full fire projection (the engine path, with barrier).
    fn fire(&mut self, timer: &Timer, day_offset: i64, now: DateTime<Utc>) -> RunClaim {
        let claim = self
            .store
            .claim_run(
                timer.id,
                self.t0 + chrono::Duration::days(day_offset) + chrono::Duration::seconds(1),
            )
            .unwrap();
        crate::tree::project_run_started(
            &self.engine.tree,
            &self.store,
            timer,
            &claim,
            &FireKind::OnTime,
            &mut self.log,
            Some(&self.engine),
            now,
        )
        .unwrap();
        claim
    }

    fn folder(&self, timer: &Timer) -> PathBuf {
        self.engine.tree.folder_for(timer.id).unwrap()
    }

    fn reply_path(&self, timer: &Timer, run_id: Uuid) -> PathBuf {
        self.folder(timer).join(reply_file_name(run_id))
    }

    /// Write a reply file the way a well-behaved app would (the stub edited
    /// and atomically replaced — direct write is fine for the harness).
    fn write_reply(&self, timer: &Timer, run_id: Uuid, body: serde_json::Value) {
        std::fs::write(self.reply_path(timer, run_id), serde_json::to_vec(&body).unwrap())
            .unwrap();
    }

    fn reply_json(&self, run_id: Uuid, app: &str, state: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": REPLY_SCHEMA_V1,
            "run_id": run_id,
            "app_name": app,
            "state": state,
        })
    }

    fn poll(&mut self, now: DateTime<Utc>) -> PollStats {
        poll_once(
            &self.engine,
            &self.store,
            &mut self.log,
            now,
            &mut self.tracker,
        )
    }

    fn status(&self, timer: &Timer) -> serde_json::Value {
        let raw = std::fs::read_to_string(self.folder(timer).join(STATUS_FILE_NAME)).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn reply_bytes(&self, timer: &Timer, run_id: Uuid) -> Vec<u8> {
        std::fs::read(self.reply_path(timer, run_id)).unwrap()
    }

    fn events(&self) -> Vec<EventRecord> {
        let (recs, _) = read_events(self.log.current_path()).unwrap();
        recs
    }

    fn events_for(&self, run_id: Uuid) -> Vec<EventRecord> {
        self.events()
            .into_iter()
            .filter(|r| r.run_id == Some(run_id))
            .collect()
    }

    fn kinds_for(&self, run_id: Uuid) -> Vec<RunState> {
        self.events_for(run_id).iter().map(|r| r.kind).collect()
    }

    fn row(&self, run_id: Uuid) -> RunStateRow {
        self.store.get_run_state(run_id).unwrap().unwrap()
    }

    fn expire_pickups(&mut self, secs: i64) -> usize {
        let t = at(self, secs);
        self.engine
            .expire_pickups(&self.store, &mut self.log, t)
            .unwrap()
    }

    fn expire_watchdogs(&mut self, secs: i64) -> usize {
        let t = at(self, secs);
        self.engine
            .expire_watchdogs(&self.store, &mut self.log, t)
            .unwrap()
    }

    fn startup(&mut self, secs: i64) {
        let t = at(self, secs);
        startup_scan(&self.engine, &self.store, &mut self.log, t);
    }
}

fn at(h: &Harness, secs: i64) -> DateTime<Utc> {
    h.t0 + chrono::Duration::seconds(secs)
}

// ── The full chain, and the mirror at every step ────────────────────────

#[test]
fn full_chain_logged_with_app_timestamps_and_mirrored_at_every_step() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // T0: the stub exists, pre-filled, state null — Bellman does not act.
    let stub = h.reply_bytes(&timer, claim.run_id);
    let stub_json: serde_json::Value = serde_json::from_slice(&stub).unwrap();
    assert_eq!(stub_json["schema"], REPLY_SCHEMA_V1);
    assert_eq!(stub_json["run_id"], claim.run_id.to_string());
    assert_eq!(stub_json["app_name"], "lightbulb");
    assert!(stub_json["state"].is_null());
    assert!(stub_json["hint"].is_string());
    let stats = h.poll(at(&h, 1));
    assert_eq!(stats.applied, 0, "the untouched stub is not a reply");
    assert_eq!(h.kinds_for(claim.run_id), vec![]);

    // T1 — acknowledged (stub edited, expected_secs set).
    let mut doc: serde_json::Value = serde_json::from_slice(&stub).unwrap();
    doc["state"] = serde_json::json!("acknowledged");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 2));
    doc["expected_secs"] = serde_json::json!(15);
    std::fs::write(h.reply_path(&timer, claim.run_id), serde_json::to_vec(&doc).unwrap())
        .unwrap();
    assert_eq!(h.poll(at(&h, 3)).applied, 1);
    let s = h.status(&timer);
    assert_eq!(s["state"], "acknowledged");
    assert_eq!(s["expected_secs"], 15);
    assert_eq!(s["app_name"], "lightbulb");
    assert_eq!(s["acknowledged_at"], serde_json::json!(at(&h, 2)));

    // T2 — running with heartbeat + progress.
    doc["state"] = serde_json::json!("running");
    doc["heartbeat_at"] = serde_json::json!(at(&h, 7));
    doc["progress"] = serde_json::json!("bulb on, 5s elapsed");
    std::fs::write(h.reply_path(&timer, claim.run_id), serde_json::to_vec(&doc).unwrap())
        .unwrap();
    assert_eq!(h.poll(at(&h, 8)).applied, 1);
    let s = h.status(&timer);
    assert_eq!(s["state"], "running");
    assert_eq!(s["progress"], "bulb on, 5s elapsed");
    assert_eq!(s["expected_secs"], 15, "earlier fields survive");

    // T3 — completed with a result.
    doc["state"] = serde_json::json!("completed");
    doc["completed_at"] = serde_json::json!(at(&h, 15));
    doc["result"] = serde_json::json!({ "on_duration_secs": 13.0 });
    std::fs::write(h.reply_path(&timer, claim.run_id), serde_json::to_vec(&doc).unwrap())
        .unwrap();
    assert_eq!(h.poll(at(&h, 16)).applied, 1);
    let s = h.status(&timer);
    assert_eq!(s["state"], "completed");
    assert_eq!(s["result"]["on_duration_secs"], 13.0);
    assert_eq!(s["expected_secs"], 15, "accumulated fields are never retracted");

    // The log: exactly the three transitions, one run_id, app's timestamps.
    let evs = h.events_for(claim.run_id);
    let kinds: Vec<RunState> = evs.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![RunState::Acknowledged, RunState::Running, RunState::Completed]
    );
    assert_eq!(evs[0].logged_at, at(&h, 2), "acknowledged uses the app ts");
    assert_eq!(evs[1].logged_at, at(&h, 7), "running uses the app ts");
    assert_eq!(evs[2].logged_at, at(&h, 15), "completed uses the app ts");
    assert_eq!(evs[0].detail.as_ref().unwrap()["app_name"], "lightbulb");
    assert_eq!(evs[0].detail.as_ref().unwrap()["expected_secs"], 15);
    let dur = evs[2].duration_ms.expect("terminal event carries duration_ms");
    assert!((0..60_000).contains(&dur), "sane monotonic duration: {dur}");
}

#[test]
fn missed_transition_is_reconstructed_from_accumulated_timestamps() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // The app moved acknowledged → completed between two watcher ticks: the
    // watcher only ever sees `completed`, but acknowledged_at is in the file.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 2));
    doc["completed_at"] = serde_json::json!(at(&h, 15));
    h.write_reply(&timer, claim.run_id, doc);
    assert_eq!(h.poll(at(&h, 16)).applied, 1);

    let evs = h.events_for(claim.run_id);
    let kinds: Vec<RunState> = evs.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![RunState::Acknowledged, RunState::Completed]);
    assert_eq!(evs[0].logged_at, at(&h, 2), "reconstructed from the file, not the tick");
    assert_eq!(evs[1].logged_at, at(&h, 15));
    // Bellman never invents `running`.
    assert!(!kinds.contains(&RunState::Running));
}

#[test]
fn heartbeats_and_progress_never_reach_the_log() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 1));
    h.write_reply(&timer, claim.run_id, doc.clone());
    h.poll(at(&h, 1));
    let baseline = h.events_for(claim.run_id).len();
    assert_eq!(baseline, 2, "acknowledged + running");

    // A long run with many distinct heartbeats: zero new lines, and the live
    // view still tracks progress.
    for i in 0..20 {
        doc["heartbeat_at"] = serde_json::json!(at(&h, 10 + i));
        doc["progress"] = serde_json::json!(format!("{}s elapsed", 10 + i));
        h.write_reply(&timer, claim.run_id, doc.clone());
        h.poll(at(&h, 10 + i));
    }
    assert_eq!(
        h.events_for(claim.run_id).len(),
        baseline,
        "heartbeats add exactly zero log lines"
    );
    assert_eq!(h.status(&timer)["progress"], "29s elapsed");
}

#[test]
fn minimal_from_scratch_reply_works_like_an_edited_stub() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // Identity fields + state, nothing else — no stub edit.
    h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "lightbulb", "completed"));
    assert_eq!(h.poll(at(&h, 5)).applied, 1);
    assert_eq!(h.status(&timer)["state"], "completed");

    // And {"state": "completed"} alone is NOT a reply — no run_id.
    let claim2 = h.fire(&timer, 1, at(&h, 10));
    h.write_reply(&timer, claim2.run_id, serde_json::json!({ "state": "completed" }));
    let stats = h.poll(at(&h, 11));
    assert_eq!(stats.rejected, 1);
    assert!(h.kinds_for(claim2.run_id).contains(&RunState::ReplyRejected));
    assert_ne!(h.status(&timer)["state"], "completed");
}

// ── The timing asymmetry: pickup deadline, no completion timeout ────────

#[test]
fn no_ack_after_pickup_grace_and_the_reply_file_stays_the_stub() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));
    let stub_at_fire = h.reply_bytes(&timer, claim.run_id);

    // 59s: nothing. 61s: no_ack.
    assert_eq!(h.expire_pickups(59), 0);
    assert_eq!(h.expire_pickups(61), 1);

    let s = h.status(&timer);
    assert_eq!(s["state"], "no_ack");
    assert!(s["no_ack_at"].is_string());
    assert_eq!(
        h.reply_bytes(&timer, claim.run_id),
        stub_at_fire,
        "no_ack never touches the reply file — the stub is untouched"
    );
    let evs = h.events_for(claim.run_id);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, RunState::NoAck);
}

#[test]
fn an_unfinished_run_ages_forever_without_auto_complete_or_auto_fail() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 1));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 1));

    // Three days pass: no watchdog opt-in, so nothing moves.
    let later = at(&h, 3 * 86_400);
    assert_eq!(h.engine.expire_pickups(&h.store, &mut h.log, later).unwrap(), 0);
    assert_eq!(h.engine.expire_watchdogs(&h.store, &mut h.log, later).unwrap(), 0);
    assert_eq!(h.status(&timer)["state"], "running");
    assert_eq!(h.events_for(claim.run_id).len(), 2);
}

#[test]
fn late_reply_revises_no_ack_while_the_run_is_current() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));
    h.expire_pickups(61);
    assert_eq!(h.status(&timer)["state"], "no_ack");

    // The app finally answers: the provisional no_ack is revised, the log
    // keeps both facts.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 90));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 90));
    assert_eq!(h.status(&timer)["state"], "completed");
    let kinds = h.kinds_for(claim.run_id);
    assert_eq!(
        kinds,
        vec![RunState::NoAck, RunState::Acknowledged, RunState::Completed],
        "append-only log keeps the no_ack and the revision"
    );
}

#[test]
fn ack_through_counts_as_pickup_and_revises_no_ack_symmetrically() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // Cursor advances past this run's event — no reply file at all.
    h.store
        .ack_run_events(timer.id, claim.event_sequence)
        .unwrap();
    let t = at(&h, 5);
    assert!(h
        .engine
        .on_ack_through(&h.store, &mut h.log, &timer, claim.event_sequence, t)
        .unwrap());
    assert_eq!(h.status(&timer)["state"], "acknowledged");
    assert_eq!(h.kinds_for(claim.run_id), vec![RunState::Acknowledged]);
    // The pickup deadline was consumed: expiry must not declare no_ack.
    assert_eq!(h.expire_pickups(120), 0);

    // Symmetric late-cursor case: no_ack first, then the cursor arrives.
    let claim2 = h.fire(&timer, 1, at(&h, 200));
    h.expire_pickups(261);
    assert_eq!(h.status(&timer)["state"], "no_ack");
    h.store
        .ack_run_events(timer.id, claim2.event_sequence)
        .unwrap();
    let t = at(&h, 300);
    assert!(h
        .engine
        .on_ack_through(&h.store, &mut h.log, &timer, claim2.event_sequence, t)
        .unwrap());
    assert_eq!(h.status(&timer)["state"], "acknowledged");
    assert_eq!(
        h.kinds_for(claim2.run_id),
        vec![RunState::NoAck, RunState::Acknowledged],
        "late cursor revises no_ack to acknowledged — never running/completed"
    );
}

// ── The opt-in watchdog ─────────────────────────────────────────────────

#[test]
fn watchdog_marks_timed_out_and_never_touches_the_reply_file() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 1));
    doc["expected_secs"] = serde_json::json!(10);
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 1));
    let reply_at_arm = h.reply_bytes(&timer, claim.run_id);

    // Deadline = receipt (t0+1) + 10 × 2.0 → t0+21.
    assert_eq!(h.expire_watchdogs(20), 0);
    assert_eq!(h.expire_watchdogs(22), 1);

    let s = h.status(&timer);
    assert_eq!(s["state"], "failed");
    assert_eq!(s["failure_kind"], "timed_out");
    assert_eq!(
        h.reply_bytes(&timer, claim.run_id),
        reply_at_arm,
        "watchdog expiry writes status.json and the log ONLY — reply.json is byte-identical"
    );
    let evs = h.events_for(claim.run_id);
    let failed = evs.iter().find(|e| e.kind == RunState::Failed).unwrap();
    assert_eq!(failed.detail.as_ref().unwrap()["failure_kind"], "timed_out");

    // A late completed revises the provisional inference; the log keeps both.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 40));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 40));
    assert_eq!(h.status(&timer)["state"], "completed");
    assert!(h.status(&timer).get("failure_kind").is_none());
    let kinds = h.kinds_for(claim.run_id);
    assert_eq!(kinds.last(), Some(&RunState::Completed));
    assert!(kinds.contains(&RunState::Failed));
}

#[test]
fn a_distinct_heartbeat_rearms_and_a_duplicate_never_extends() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["expected_secs"] = serde_json::json!(10);
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim.run_id, doc.clone());
    h.poll(at(&h, 0)); // armed at t0 → deadline t0+20

    // Exact duplicate rescans: the deadline does not move.
    h.poll(at(&h, 5));
    h.poll(at(&h, 10));
    h.poll(at(&h, 18));
    assert_eq!(h.expire_watchdogs(19), 0);
    assert_eq!(
        h.expire_watchdogs(21),
        1,
        "duplicates never extended the original deadline"
    );

    // Rearm, then a distinct heartbeat at t0+15 rearms from THAT receipt.
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    let mut doc = h.reply_json(claim2.run_id, "lightbulb", "running");
    doc["expected_secs"] = serde_json::json!(10);
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim2.run_id, doc.clone());
    h.poll(at(&h, 100)); // deadline t0+120
    doc["progress"] = serde_json::json!("still alive");
    h.write_reply(&timer, claim2.run_id, doc);
    h.poll(at(&h, 115)); // distinct → deadline t0+135
    assert_eq!(h.expire_watchdogs(121), 0);
    assert_eq!(h.expire_watchdogs(134), 0);
    assert_eq!(h.expire_watchdogs(136), 1);
}

#[test]
fn watchdog_opt_in_requires_an_estimate_and_false_cancels() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // error_detection: true with no expected_secs anywhere → rejected.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim.run_id, doc);
    assert_eq!(h.poll(at(&h, 1)).rejected, 1);
    assert!(h.kinds_for(claim.run_id).contains(&RunState::ReplyRejected));
    assert_eq!(h.row(claim.run_id).state, "fired");

    // Estimate first, opt-in later works (accumulated estimate counts).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "acknowledged");
    doc["expected_secs"] = serde_json::json!(10);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 2));
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 3)); // armed: deadline t0+23

    // An explicit false cancels the pending watchdog; the estimate stays.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["error_detection"] = serde_json::json!(false);
    doc["progress"] = serde_json::json!("nearly there");
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 4));
    assert_eq!(h.expire_watchdogs(500), 0);
    assert_eq!(h.status(&timer)["expected_secs"], 10, "advisory estimate retained");
}

#[test]
fn a_new_estimate_replaces_the_old_one_from_bellmans_receipt() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["expected_secs"] = serde_json::json!(10);
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 0)); // deadline t0+20

    // Mid-run correction: 900s now — re-anchored at THIS receipt (t0+5).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["expected_secs"] = serde_json::json!(900);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 5)); // deadline t0+5+1800
    assert_eq!(h.expire_watchdogs(21), 0);
    assert_eq!(h.expire_watchdogs(1804), 0);
    assert_eq!(h.expire_watchdogs(1806), 1);
    assert_eq!(h.status(&timer)["expected_secs"], 900);
}

#[test]
fn the_apps_latest_terminal_verdict_wins_from_either_source() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // The app's OWN failed, then its completed — accepted, same rule as
    // revising a watchdog verdict (no failure_kind check).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "failed");
    doc["failed_at"] = serde_json::json!(at(&h, 5));
    doc["reason"] = serde_json::json!("GPIO write refused");
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 5));
    assert_eq!(h.status(&timer)["failure_kind"], "reported");

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 9));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 9));
    let s = h.status(&timer);
    assert_eq!(s["state"], "completed");
    assert!(s.get("reason").is_none(), "the new verdict wins wholesale");
    assert_eq!(
        h.kinds_for(claim.run_id),
        vec![RunState::Acknowledged, RunState::Failed, RunState::Completed]
    );

    // …and the reverse: completed, then failed.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "failed");
    doc["failed_at"] = serde_json::json!(at(&h, 12));
    doc["reason"] = serde_json::json!("actually it broke");
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 12));
    assert_eq!(h.status(&timer)["state"], "failed");
}

#[test]
fn terminal_report_never_moves_backwards_but_provisional_states_may() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 3));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 3));

    // running after an app-authored completed → rejected.
    h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "lightbulb", "running"));
    assert_eq!(h.poll(at(&h, 4)).rejected, 1);
    let evs = h.events_for(claim.run_id);
    assert!(evs.iter().any(|e| e.kind == RunState::ReplyRejected));
    assert_eq!(h.status(&timer)["state"], "completed", "the verdict stands");

    // A distinct valid `running` after a watchdog timed_out DOES revise and
    // rearms from receipt.
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    let mut doc = h.reply_json(claim2.run_id, "lightbulb", "acknowledged");
    doc["expected_secs"] = serde_json::json!(5);
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim2.run_id, doc);
    h.poll(at(&h, 100)); // deadline t0+110
    assert_eq!(h.expire_watchdogs(111), 1);
    assert_eq!(h.status(&timer)["state"], "failed");

    let mut doc = h.reply_json(claim2.run_id, "lightbulb", "running");
    doc["progress"] = serde_json::json!("recovered, still working");
    h.write_reply(&timer, claim2.run_id, doc);
    assert_eq!(h.poll(at(&h, 120)).applied, 1);
    assert_eq!(h.status(&timer)["state"], "running");
    // Rearmed from the t0+120 receipt (5 × 2 = 10s): not at +129, yes at +131.
    assert_eq!(h.expire_watchdogs(129), 0);
    assert_eq!(h.expire_watchdogs(131), 1);
}

#[test]
fn bellman_never_flips_an_app_completed_back_to_failed() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["expected_secs"] = serde_json::json!(1);
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 0));
    // The terminal reply disarmed the watchdog; nothing can fail this run.
    assert_eq!(h.expire_watchdogs(10_000), 0);
    assert_eq!(h.status(&timer)["state"], "completed");
}

// ── Validation, quarantine, per-run channels ────────────────────────────

#[test]
fn wrong_app_name_and_reserved_states_are_rejected_and_quarantined() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // A different app is rejected (no first-responder claim on a shared file).
    h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "intruder", "running"));
    assert_eq!(h.poll(at(&h, 1)).rejected, 1);
    assert_eq!(h.status(&timer)["state"], "fired");

    // Reserved states an app may never write.
    for reserved in ["fired", "no_ack", "cancelled"] {
        h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "lightbulb", reserved));
        assert_eq!(h.poll(at(&h, 2)).rejected, 1, "{reserved} must be rejected");
    }
    let rejected: Vec<_> = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind == RunState::ReplyRejected)
        .collect();
    assert_eq!(rejected.len(), 4);

    // Re-scans of the same unchanged bytes produce neither more events nor
    // more artifacts.
    let before = h.events().len();
    h.poll(at(&h, 3));
    h.poll(at(&h, 4));
    assert_eq!(h.events().len(), before, "idempotent rejection");
    let bad = super::quarantine::quarantine_dir(h.engine.tree.root());
    let artifacts = std::fs::read_dir(&bad).unwrap().count();
    assert_eq!(artifacts, 8, "four distinct rejected contents → four pairs");
}

#[test]
fn an_unknown_run_id_is_rejected_never_confused_with_superseded() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let fabricated = Uuid::new_v4();
    h.write_reply(&timer, fabricated, h.reply_json(fabricated, "lightbulb", "completed"));
    assert_eq!(h.poll(at(&h, 1)).rejected, 1);
    let kinds = h.kinds_for(fabricated);
    assert_eq!(kinds, vec![RunState::ReplyRejected]);
    assert!(
        !h.events().iter().any(|e| e.kind == RunState::Superseded),
        "fabricated ids are tamper/garbage, not slow apps"
    );
    let _ = claim;
}

#[test]
fn previous_run_reply_is_superseded_stale_file_deleted_current_untouched() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim_a = h.fire(&timer, 0, at(&h, 0));
    let claim_b = h.fire(&timer, 1, at(&h, 100));
    let stub_b = h.reply_bytes(&timer, claim_b.run_id);

    // Run A's slow app finishes into its OWN file after run B is current.
    let mut doc = h.reply_json(claim_a.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 150));
    h.write_reply(&timer, claim_a.run_id, doc);
    let stats = h.poll(at(&h, 150));
    assert_eq!(stats.superseded, 1);

    let evs_a = h.events_for(claim_a.run_id);
    // fire-time superseded (unresolved) + the late-reply superseded.
    assert!(evs_a.iter().all(|e| e.kind == RunState::Superseded));
    assert!(
        !h.reply_path(&timer, claim_a.run_id).exists(),
        "the stale file is deleted after ingest"
    );
    assert_eq!(
        h.reply_bytes(&timer, claim_b.run_id),
        stub_b,
        "run B's channel is byte-identical throughout — Bellman never writes over a reply file"
    );
    assert_eq!(h.status(&timer)["state"], "fired", "run B untouched");

    // The slow app writes again (its process still alive): superseded again,
    // deleted again — never applied.
    h.write_reply(&timer, claim_a.run_id, h.reply_json(claim_a.run_id, "lightbulb", "failed"));
    let stats = h.poll(at(&h, 160));
    assert_eq!(stats.superseded, 1);
    assert!(!h.reply_path(&timer, claim_a.run_id).exists());
}

#[test]
fn the_pre_fire_barrier_ingests_a_completed_the_watcher_never_saw() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim_a = h.fire(&timer, 0, at(&h, 0));

    // The app completes — and the next fire arrives BEFORE any watcher tick.
    let mut doc = h.reply_json(claim_a.run_id, "lightbulb", "completed");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 5));
    doc["completed_at"] = serde_json::json!(at(&h, 15));
    doc["result"] = serde_json::json!({ "ok": true });
    h.write_reply(&timer, claim_a.run_id, doc);
    let claim_b = h.fire(&timer, 1, at(&h, 100));

    let kinds_a = h.kinds_for(claim_a.run_id);
    assert_eq!(
        kinds_a,
        vec![RunState::Acknowledged, RunState::Completed],
        "the barrier folded the outcome in — NOT superseded"
    );
    assert_eq!(h.row(claim_a.run_id).state, "completed");
    // Run B proceeds normally with its own fresh channel.
    assert!(h.reply_path(&timer, claim_b.run_id).exists());
    assert_eq!(h.status(&timer)["state"], "fired");
}

#[test]
fn a_reply_written_while_stopped_survives_the_restart() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // Bellman "stopped": no watcher runs. The app answers anyway.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 30));
    h.write_reply(&timer, claim.run_id, doc);

    // Startup: replies are scanned before anything may fire.
    h.startup(31);
    assert_eq!(
        h.kinds_for(claim.run_id),
        vec![RunState::Acknowledged, RunState::Completed],
        "the reply is folded in and logged before the next fire"
    );
    assert_eq!(h.status(&timer)["state"], "completed");
}

// ── Robustness: mid-write, oversize, duplicates, skewed clocks ──────────

#[test]
fn a_mid_write_file_is_not_quarantined_but_stable_garbage_is() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // Half a JSON document, then the rest: accepted, never rejected.
    let full = serde_json::to_vec(&h.reply_json(claim.run_id, "lightbulb", "completed")).unwrap();
    let path = h.reply_path(&timer, claim.run_id);
    std::fs::write(&path, &full[..full.len() / 2]).unwrap();
    let stats = h.poll(at(&h, 1));
    assert_eq!(stats.rejected, 0, "first sight of partial bytes only starts the debounce");
    std::fs::write(&path, &full).unwrap();
    assert_eq!(h.poll(at(&h, 2)).applied, 1);
    assert_eq!(h.status(&timer)["state"], "completed");

    // Invalid bytes left in place: rejected after the debounce, file stays.
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    let path2 = h.reply_path(&timer, claim2.run_id);
    std::fs::write(&path2, b"{ not json").unwrap();
    h.poll(at(&h, 101));
    let stats = h.poll(at(&h, 101));
    assert_eq!(stats.rejected, 0, "still within the debounce window");
    let stats = h.poll(at(&h, 102));
    assert_eq!(stats.rejected, 1, "stable invalid bytes past the debounce");
    assert!(path2.exists(), "quarantine COPIES — the live file is left in place");
    assert_eq!(std::fs::read(&path2).unwrap(), b"{ not json");

    // The app may still overwrite with a valid reply — and it is ingested.
    h.write_reply(&timer, claim2.run_id, h.reply_json(claim2.run_id, "lightbulb", "completed"));
    assert_eq!(h.poll(at(&h, 103)).applied, 1);
    assert_eq!(h.status(&timer)["state"], "completed");
}

#[test]
fn an_oversize_reply_is_bounded_and_never_read() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // A sparse file far over the 64 KB cap.
    let path = h.reply_path(&timer, claim.run_id);
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(1024 * 1024).unwrap();
    drop(f);

    let stats = h.poll(at(&h, 1));
    assert_eq!(stats.rejected, 1);
    assert!(path.exists(), "the oversize file is left untouched");
    let evs = h.events_for(claim.run_id);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, RunState::ReplyRejected);

    // Only a metadata-only sidecar lands in bad/ (content_copied: false).
    let bad = super::quarantine::quarantine_dir(h.engine.tree.root());
    let sidecar = std::fs::read_dir(&bad)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".sidecar.json"))
        .unwrap();
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sidecar.path()).unwrap()).unwrap();
    assert_eq!(meta["content_copied"], false);
    assert_eq!(meta["reason"], "oversize");

    // Repeated rescans: no new event, no new artifact.
    h.poll(at(&h, 2));
    h.poll(at(&h, 3));
    assert_eq!(h.events().len(), 1, "one rejection, then idempotent");
    assert_eq!(std::fs::read_dir(&bad).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn a_symlinked_reply_path_is_rejected_without_following() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let path = h.reply_path(&timer, claim.run_id);
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", &path).unwrap();

    let stats = h.poll(at(&h, 1));
    assert_eq!(stats.rejected, 1);
    let bad = super::quarantine::quarantine_dir(h.engine.tree.root());
    for entry in std::fs::read_dir(&bad).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".payload"),
            "a symlink is never followed — no payload may be copied"
        );
    }
    assert!(h.poll(at(&h, 2)).rejected == 0, "one bounded rejection, not a stream");
}

#[test]
fn duration_uses_bellmans_clock_never_the_apps() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // The app stamps its completion an hour in the past (skewed clock).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, -3600));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 5));
    let evs = h.events_for(claim.run_id);
    let completed = evs.iter().find(|e| e.kind == RunState::Completed).unwrap();
    let dur = completed.duration_ms.unwrap();
    assert!((0..60_000).contains(&dur), "no negative or absurd duration: {dur}");

    // After a restart the monotonic anchor is gone: wall-clock fallback,
    // marked as the estimate it is.
    h.engine.anchors.lock().unwrap().clear();
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    h.engine.anchors.lock().unwrap().remove(&claim2.run_id);
    let mut doc = h.reply_json(claim2.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 130));
    h.write_reply(&timer, claim2.run_id, doc);
    h.poll(at(&h, 130));
    let evs = h.events_for(claim2.run_id);
    let completed = evs.iter().find(|e| e.kind == RunState::Completed).unwrap();
    assert_eq!(
        completed.detail.as_ref().unwrap()["duration_source"], "wall_clock",
        "the fallback is identifiable"
    );
    assert!(completed.duration_ms.unwrap() >= 0);
}

#[test]
fn a_reply_is_data_never_a_command() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));
    let marker = h.dir.path().join("reply-must-not-execute-this");

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["action"] = serde_json::json!({ "type": "launch", "command": "touch", "args": [marker] });
    doc["result"] = serde_json::json!({ "path": marker, "summary": "stored elsewhere" });
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 1));
    assert!(!marker.exists(), "R9: nothing a reply carries is ever executed");
    assert_eq!(h.status(&timer)["state"], "completed");
}

// ── Ownership ───────────────────────────────────────────────────────────

#[test]
fn an_unowned_timer_has_no_reply_channel_and_never_goes_no_ack() {
    let mut h = Harness::new();
    let timer = h.add_timer("plain", None);
    let claim = h.fire(&timer, 0, at(&h, 0));

    assert!(
        !h.reply_path(&timer, claim.run_id).exists(),
        "no stub without an integration owner"
    );
    assert!(h.store.get_run_state(claim.run_id).unwrap().is_none());
    assert_eq!(h.expire_pickups(500), 0);
    assert_eq!(h.status(&timer)["state"], "fired");
    assert!(h.events_for(claim.run_id).is_empty());
}

#[test]
fn an_owner_change_applies_at_the_next_firing() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // The owner changes mid-run: the run snapshot still names the original.
    h.store.set_timer_owner(timer.id, "new-owner").unwrap();

    // The new owner cannot answer this run; the original can.
    h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "new-owner", "completed"));
    assert_eq!(h.poll(at(&h, 1)).rejected, 1);
    h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "lightbulb", "completed"));
    assert_eq!(h.poll(at(&h, 2)).applied, 1);

    // The next firing's stub carries only the new owner.
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    let stub: serde_json::Value =
        serde_json::from_slice(&h.reply_bytes(&timer, claim2.run_id)).unwrap();
    assert_eq!(stub["app_name"], "new-owner");
}

#[test]
fn a_restart_reconstructs_the_pickup_deadline_from_the_persisted_value() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // "Restart": every in-memory structure is replaced by fresh handles on
    // the same data dir — fresh anchors, fresh store connection, fresh log.
    // What survives is only what was persisted.
    let data_dir = h.dir.path().to_path_buf();
    let t0 = h.t0;
    let store = Store::open_with(data_dir.join("timers.db"), OpenOptions::default()).unwrap();
    let engine = ReplyEngine {
        tree: TimersTree::new(&data_dir),
        data_dir: data_dir.clone(),
        pickup_grace: Duration::from_secs(60),
        watchdog_factor: 2.0,
        anchors: new_anchors(),
    };
    let mut log = EventLog::open(EventLogConfig::new(data_dir.join("logs"))).unwrap();

    // The persisted wall-clock deadline (t0+60) still governs — a restart
    // does not grant a fresh grace period.
    let before = t0 + chrono::Duration::seconds(59);
    let after = t0 + chrono::Duration::seconds(61);
    assert_eq!(engine.expire_pickups(&store, &mut log, before).unwrap(), 0);
    assert_eq!(engine.expire_pickups(&store, &mut log, after).unwrap(), 1);
    let row = store.get_run_state(claim.run_id).unwrap().unwrap();
    assert_eq!(row.state, "no_ack");
}

#[test]
fn duplicate_reply_is_a_no_op() {    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 3));
    h.write_reply(&timer, claim.run_id, doc.clone());
    assert_eq!(h.poll(at(&h, 3)).applied, 1);
    let stats = h.poll(at(&h, 4));
    assert_eq!(stats.duplicates, 1);
    assert_eq!(stats.applied, 0);
    assert_eq!(h.events_for(claim.run_id).len(), 2, "no second terminal line");
}

#[test]
fn status_json_mirror_survives_terminal_but_current_watching() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // completed, then failed, with no fire in between: the second report is
    // ingested, not missed (watching stops at current-ness, never at terminal).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 3));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(at(&h, 3));
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "failed");
    doc["failed_at"] = serde_json::json!(at(&h, 5));
    doc["reason"] = serde_json::json!("post-completion check failed");
    h.write_reply(&timer, claim.run_id, doc);
    assert_eq!(h.poll(at(&h, 5)).applied, 1);
    assert_eq!(h.status(&timer)["state"], "failed");
    assert_eq!(h.status(&timer)["reason"], "post-completion check failed");
}
