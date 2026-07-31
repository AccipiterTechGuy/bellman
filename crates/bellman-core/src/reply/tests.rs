//! IK3 reply-channel tests: the transport-agnostic engine and the file
//! transport against a real store + folder tree in a temp dir.
//!
//! Time discipline: `at(&h, secs)` is the WALL clock (timestamps only);
//! `mono(&h, secs)` is Bellman's MONOTONIC clock — the only clock deadlines
//! count on. Deadline entries in the shared book are set explicitly against
//! the harness `mono0` base so expiry is deterministic.

use super::engine::ReplyEngine;
use super::watcher::{poll_once, reconcile, startup_scan, InvalidTracker, PollStats};
use super::*;
use crate::events::{read_events, EventLogConfig, EventPublisher, EventRecord, RunState};
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::scheduler::FireKind;
use crate::store::{NewTimer, OpenOptions, RunClaim, RunStateRow, Store, Timer};
use crate::tree::{reply_file_name, TimersTree, STATUS_FILE_NAME};
use chrono::{DateTime, NaiveTime, Utc};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

// ── Harness ─────────────────────────────────────────────────────────────

struct Harness {
    dir: tempfile::TempDir,
    store: Store,
    engine: ReplyEngine,
    tracker: InvalidTracker,
    t0: DateTime<Utc>,
    mono0: Instant,
}

impl Harness {
    fn new() -> Self {
        Self::with_grace(60, 2.0)
    }

    fn with_grace(pickup_grace_secs: u64, watchdog_factor: f64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("slots")).unwrap();
        let store =
            Store::open_with(dir.path().join("timers.db"), OpenOptions::default()).unwrap();
        let engine = ReplyEngine {
            tree: TimersTree::new(dir.path()),
            data_dir: dir.path().to_path_buf(),
            pickup_grace: Duration::from_secs(pickup_grace_secs),
            watchdog_factor,
            anchors: new_anchors(),
            deadlines: new_deadlines(),
            fire_slot_file: None,
            status_listener: None,
            ipc: None,
        };
        Self {
            dir,
            store,
            engine,
            tracker: InvalidTracker::default(),
            t0: Utc::now(),
            mono0: Instant::now(),
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

    /// The R10 fire transaction (gate → barrier → one commit → projections).
    /// The pickup countdown is DISARMED afterwards (row + book) so polls at
    /// large time offsets don't `no_ack` — deadline tests use `fire_armed`
    /// or `arm_pickup` explicitly.
    fn fire(&mut self, timer: &Timer, day_offset: i64, now: DateTime<Utc>) -> RunClaim {
        let claim = self.fire_armed(timer, day_offset, now);
        self.disarm(claim.run_id);
        claim
    }

    /// Fire with the pickup deadline left armed (deadline tests).
    fn fire_armed(&mut self, timer: &Timer, day_offset: i64, now: DateTime<Utc>) -> RunClaim {
        let scheduled_for =
            self.t0 + chrono::Duration::days(day_offset) + chrono::Duration::seconds(1);
        let tree = self.engine.tree.clone();
        let engine = self.engine.clone();
        crate::tree::project_fire(
            &tree,
            &mut self.store,
            timer,
            scheduled_for,
            &FireKind::OnTime,
            &engine,
            now,
        )
        .unwrap()
    }

    /// Disarm the pickup countdown for a run (persisted row AND book).
    fn disarm(&mut self, run_id: Uuid) {
        if let Ok(Some(mut row)) = self.store.get_run_state(run_id) {
            row.pickup_deadline = None;
            let _ = self.store.update_run_state(&row);
        }
        self.engine.clear_deadlines(run_id);
    }

    /// Deterministically arm a pickup countdown for `run_id` at `secs` after
    /// the harness monotonic base (the book is THE deadline source).
    fn arm_pickup(&self, run_id: Uuid, secs: u64) {
        self.engine.deadlines.lock().unwrap().entries.insert(
            run_id,
            MonoDeadline {
                kind: DeadlineKind::Pickup,
                at: self.mono0 + Duration::from_secs(secs),
            },
        );
    }

    fn folder(&self, timer: &Timer) -> PathBuf {
        self.engine.tree.folder_for(timer.id).unwrap()
    }

    fn reply_path(&self, timer: &Timer, run_id: Uuid) -> PathBuf {
        self.folder(timer).join(reply_file_name(run_id))
    }

    fn write_reply(&self, timer: &Timer, run_id: Uuid, body: serde_json::Value) {
        std::fs::write(
            self.reply_path(timer, run_id),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
    }

    fn write_reply_named(&self, timer: &Timer, run_id: Uuid, body: serde_json::Value) {
        self.write_reply(timer, run_id, body)
    }

    fn reply_json(&self, run_id: Uuid, app: &str, state: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": REPLY_SCHEMA_V1,
            "run_id": run_id,
            "app_name": app,
            "state": state,
        })
    }

    fn poll(&mut self, secs: i64) -> PollStats {
        poll_once(
            &self.engine,
            &self.store,
            at(self, secs),
            mono(self, secs),
            &mut self.tracker,
        )
    }

    fn expire_pickups(&mut self, secs: i64) -> usize {
        let runs = self
            .engine
            .expire_pickups(&self.store, at(self, secs), mono(self, secs))
            .unwrap();
        for run_id in &runs {
            if let Ok(Some(row)) = self.store.get_run_state(*run_id) {
                if let Ok(Some(timer)) = self.store.get_timer(row.timer_id) {
                    let _ = self.engine.project_status(&self.store, &timer, run_id);
                }
            }
        }
        runs.len()
    }

    fn expire_watchdogs(&mut self, secs: i64) -> usize {
        let runs = self
            .engine
            .expire_watchdogs(&self.store, at(self, secs), mono(self, secs))
            .unwrap();
        for run_id in &runs {
            if let Ok(Some(row)) = self.store.get_run_state(*run_id) {
                if let Ok(Some(timer)) = self.store.get_timer(row.timer_id) {
                    let _ = self.engine.project_status(&self.store, &timer, run_id);
                }
            }
        }
        runs.len()
    }

    fn startup(&mut self, secs: i64) {
        startup_scan(&self.engine, &self.store, at(self, secs));
    }

    fn reconcile(&mut self) -> usize {
        reconcile(&self.engine, &self.store)
    }

    fn status(&self, timer: &Timer) -> serde_json::Value {
        let raw = std::fs::read_to_string(self.folder(timer).join(STATUS_FILE_NAME)).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn reply_bytes(&self, timer: &Timer, run_id: Uuid) -> Vec<u8> {
        std::fs::read(self.reply_path(timer, run_id)).unwrap()
    }

    /// Drain the outbox through the elected publisher, then read the log.
    fn events(&mut self) -> Vec<EventRecord> {
        let mut publisher =
            EventPublisher::with_config(EventLogConfig::new(self.dir.path().join("logs")))
                .unwrap();
        publisher.publish_cycle(&self.store);
        let (recs, _) = read_events(publisher.current_path()).unwrap();
        recs
    }

    fn events_for(&mut self, run_id: Uuid) -> Vec<EventRecord> {
        self.events()
            .into_iter()
            .filter(|r| r.run_id == Some(run_id))
            .collect()
    }

    fn kinds_for(&mut self, run_id: Uuid) -> Vec<RunState> {
        self.events_for(run_id).iter().map(|r| r.kind).collect()
    }

    fn row(&self, run_id: Uuid) -> RunStateRow {
        self.store.get_run_state(run_id).unwrap().unwrap()
    }
}

fn at(h: &Harness, secs: i64) -> DateTime<Utc> {
    h.t0 + chrono::Duration::seconds(secs)
}

fn mono(h: &Harness, secs: i64) -> Instant {
    if secs >= 0 {
        h.mono0 + Duration::from_secs(secs as u64)
    } else {
        h.mono0 - Duration::from_secs((-secs) as u64)
    }
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
    let stats = h.poll(1);
    assert_eq!(stats.applied, 0, "the untouched stub is not a reply");
    assert_eq!(h.kinds_for(claim.run_id), vec![RunState::Fired]);

    // T1 — acknowledged (stub edited, expected_secs set).
    let mut doc: serde_json::Value = serde_json::from_slice(&stub).unwrap();
    doc["state"] = serde_json::json!("acknowledged");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 2));
    doc["expected_secs"] = serde_json::json!(15);
    std::fs::write(
        h.reply_path(&timer, claim.run_id),
        serde_json::to_vec(&doc).unwrap(),
    )
    .unwrap();
    assert_eq!(h.poll(3).applied, 1);
    let s = h.status(&timer);
    assert_eq!(s["state"], "acknowledged");
    assert_eq!(s["expected_secs"], 15);
    assert_eq!(s["app_name"], "lightbulb");
    assert_eq!(s["acknowledged_at"], serde_json::json!(at(&h, 2)));

    // T2 — running with heartbeat + progress.
    doc["state"] = serde_json::json!("running");
    doc["heartbeat_at"] = serde_json::json!(at(&h, 7));
    doc["progress"] = serde_json::json!("bulb on, 5s elapsed");
    std::fs::write(
        h.reply_path(&timer, claim.run_id),
        serde_json::to_vec(&doc).unwrap(),
    )
    .unwrap();
    assert_eq!(h.poll(8).applied, 1);
    let s = h.status(&timer);
    assert_eq!(s["state"], "running");
    assert_eq!(s["progress"], "bulb on, 5s elapsed");
    assert_eq!(s["expected_secs"], 15, "earlier fields survive");

    // T3 — completed with a result.
    doc["state"] = serde_json::json!("completed");
    doc["completed_at"] = serde_json::json!(at(&h, 15));
    doc["result"] = serde_json::json!({ "on_duration_secs": 13.0 });
    std::fs::write(
        h.reply_path(&timer, claim.run_id),
        serde_json::to_vec(&doc).unwrap(),
    )
    .unwrap();
    assert_eq!(h.poll(16).applied, 1);
    let s = h.status(&timer);
    assert_eq!(s["state"], "completed");
    assert_eq!(s["result"]["on_duration_secs"], 13.0);
    assert_eq!(s["expected_secs"], 15, "accumulated fields are never retracted");

    // The log: the app transitions, one run_id, app's own timestamps.
    let evs: Vec<_> = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind != RunState::Fired)
        .collect();
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
    assert_eq!(h.poll(16).applied, 1);

    let kinds = h
        .kinds_for(claim.run_id)
        .into_iter()
        .filter(|k| *k != RunState::Fired)
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec![RunState::Acknowledged, RunState::Completed]);
    let evs: Vec<_> = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind != RunState::Fired)
        .collect();
    assert_eq!(evs[0].logged_at, at(&h, 2), "reconstructed from the file, not the tick");
    assert_eq!(evs[1].logged_at, at(&h, 15));
    // Bellman never invents `running`.
    assert!(!kinds.contains(&RunState::Running));
}

#[test]
fn direct_terminal_without_acknowledged_at_logs_no_invented_acknowledged() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // fired → completed directly, no acknowledged_at: a normal short path.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 9));
    h.write_reply(&timer, claim.run_id, doc);
    assert_eq!(h.poll(9).applied, 1);
    assert_eq!(
        h.kinds_for(claim.run_id)
            .into_iter()
            .filter(|k| *k != RunState::Fired)
            .collect::<Vec<_>>(),
        vec![RunState::Completed],
        "Bellman never invents an acknowledged transition"
    );

    // Same for a direct running with no acknowledged_at: running only.
    let claim2 = h.fire(&timer, 1, at(&h, 20));
    h.write_reply(&timer, claim2.run_id, h.reply_json(claim2.run_id, "lightbulb", "running"));
    assert_eq!(h.poll(21).applied, 1);
    assert_eq!(
        h.kinds_for(claim2.run_id)
            .into_iter()
            .filter(|k| *k != RunState::Fired)
            .collect::<Vec<_>>(),
        // SCH1: the first claim is still unfinished, so this firing's ACTION
        // is an overlap skip (Skip is the product default) — the firing's
        // record and reply lifecycle are unaffected.
        vec![RunState::SkippedMisfire, RunState::Running]
    );

    // But a direct failed — also terminal — logs failed only.
    let claim3 = h.fire(&timer, 2, at(&h, 40));
    let mut doc = h.reply_json(claim3.run_id, "lightbulb", "failed");
    doc["reason"] = serde_json::json!("boom");
    h.write_reply(&timer, claim3.run_id, doc);
    assert_eq!(h.poll(41).applied, 1);
    assert_eq!(
        h.kinds_for(claim3.run_id)
            .into_iter()
            .filter(|k| *k != RunState::Fired)
            .collect::<Vec<_>>(),
        // Leading SCH1 overlap skip (previous claim unfinished at commit).
        vec![RunState::SkippedMisfire, RunState::Failed]
    );
}

#[test]
fn heartbeats_and_progress_never_reach_the_log() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 1));
    h.write_reply(&timer, claim.run_id, doc.clone());
    h.poll(1);
    let baseline = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind != RunState::Fired)
        .count();
    assert_eq!(baseline, 2, "acknowledged + running");

    // A long run with many distinct heartbeats: zero new lines, and the live
    // view still tracks progress.
    for i in 0..20 {
        doc["heartbeat_at"] = serde_json::json!(at(&h, 10 + i));
        doc["progress"] = serde_json::json!(format!("{}s elapsed", 10 + i));
        h.write_reply(&timer, claim.run_id, doc.clone());
        h.poll(10 + i);
    }
    let after = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind != RunState::Fired)
        .count();
    assert_eq!(after, baseline, "heartbeats add exactly zero log lines");
    assert_eq!(h.status(&timer)["progress"], "29s elapsed");
}

#[test]
fn minimal_from_scratch_reply_works_like_an_edited_stub() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // Identity fields + state, nothing else — no stub edit.
    h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "lightbulb", "completed"));
    assert_eq!(h.poll(5).applied, 1);
    assert_eq!(h.status(&timer)["state"], "completed");
    assert_eq!(
        h.kinds_for(claim.run_id)
            .into_iter()
            .filter(|k| *k != RunState::Fired)
            .collect::<Vec<_>>(),
        vec![RunState::Completed]
    );

    // And {"state": "completed"} alone is NOT a reply — no run_id.
    let claim2 = h.fire(&timer, 1, at(&h, 10));
    h.write_reply(&timer, claim2.run_id, serde_json::json!({ "state": "completed" }));
    let stats = h.poll(11);
    assert_eq!(stats.rejected, 1);
    assert!(h.kinds_for(claim2.run_id).contains(&RunState::ReplyRejected));
    assert_ne!(h.status(&timer)["state"], "completed");
}

// ── The timing asymmetry: pickup deadline, no completion timeout ────────

#[test]
fn no_ack_after_pickup_grace_and_the_reply_file_stays_the_stub() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire_armed(&timer, 0, at(&h, 0));
    let stub_at_fire = h.reply_bytes(&timer, claim.run_id);
    h.arm_pickup(claim.run_id, 60);

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
    let evs: Vec<_> = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind != RunState::Fired)
        .collect();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, RunState::NoAck);
}

#[test]
fn deadlines_run_on_the_monotonic_clock_never_the_wall() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire_armed(&timer, 0, at(&h, 0));
    h.arm_pickup(claim.run_id, 60);

    // Wall jumps +2h (NTP correction, DST, suspend): the monotonic countdown
    // does not care — no early no_ack.
    let jumped_wall = at(&h, 2 * 3600);
    let transitioned = h
        .engine
        .expire_pickups(&h.store, jumped_wall, mono(&h, 30))
        .unwrap();
    assert!(transitioned.is_empty(), "a wall jump must never fire a deadline early");
    assert_eq!(h.status(&timer)["state"], "fired");

    // The monotonic deadline is what expires it.
    assert_eq!(h.expire_pickups(61), 1);
    assert_eq!(h.status(&timer)["state"], "no_ack");
}

#[test]
fn an_unfinished_run_ages_forever_without_auto_complete_or_auto_fail() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["acknowledged_at"] = serde_json::json!(at(&h, 1));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(1);

    // Three days pass: no watchdog opt-in, so nothing moves.
    assert_eq!(h.expire_pickups(3 * 86_400), 0);
    assert_eq!(h.expire_watchdogs(3 * 86_400), 0);
    assert_eq!(h.status(&timer)["state"], "running");
    assert_eq!(
        h.events_for(claim.run_id)
            .into_iter()
            .filter(|e| e.kind != RunState::Fired)
            .count(),
        2
    );
}

#[test]
fn late_reply_revises_no_ack_while_the_run_is_current() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire_armed(&timer, 0, at(&h, 0));
    h.arm_pickup(claim.run_id, 60);
    h.expire_pickups(61);
    assert_eq!(h.status(&timer)["state"], "no_ack");

    // The app finally answers: the provisional no_ack is revised, the log
    // keeps both facts. No acknowledged_at in the file → no invented line.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 90));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(90);
    assert_eq!(h.status(&timer)["state"], "completed");
    let kinds: Vec<_> = h
        .kinds_for(claim.run_id)
        .into_iter()
        .filter(|k| *k != RunState::Fired)
        .collect();
    assert_eq!(
        kinds,
        vec![RunState::NoAck, RunState::Completed],
        "append-only log keeps the no_ack and the revision"
    );
}

#[test]
fn ack_through_counts_as_pickup_and_revises_no_ack_symmetrically() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire_armed(&timer, 0, at(&h, 0));

    // Cursor advances past this run's event — no reply file at all.
    h.store
        .ack_run_events(timer.id, claim.event_sequence)
        .unwrap();
    let transitioned = h
        .engine
        .on_ack_through(&h.store, &timer, claim.event_sequence, at(&h, 5))
        .unwrap();
    assert!(transitioned);
    h.engine.project_status(&h.store, &timer, &claim.run_id).unwrap();
    assert_eq!(h.status(&timer)["state"], "acknowledged");
    assert_eq!(
        h.kinds_for(claim.run_id)
            .into_iter()
            .filter(|k| *k != RunState::Fired)
            .collect::<Vec<_>>(),
        vec![RunState::Acknowledged]
    );
    // The pickup deadline was consumed: expiry must not declare no_ack.
    assert_eq!(h.expire_pickups(120), 0);

    // Symmetric late-cursor case: no_ack first, then the cursor arrives.
    let claim2 = h.fire_armed(&timer, 1, at(&h, 200));
    h.arm_pickup(claim2.run_id, 260);
    h.expire_pickups(261);
    assert_eq!(h.status(&timer)["state"], "no_ack");
    h.store
        .ack_run_events(timer.id, claim2.event_sequence)
        .unwrap();
    let transitioned = h
        .engine
        .on_ack_through(&h.store, &timer, claim2.event_sequence, at(&h, 300))
        .unwrap();
    assert!(transitioned);
    h.engine
        .project_status(&h.store, &timer, &claim2.run_id)
        .unwrap();
    assert_eq!(h.status(&timer)["state"], "acknowledged");
    assert_eq!(
        h.kinds_for(claim2.run_id)
            .into_iter()
            .filter(|k| *k != RunState::Fired)
            .collect::<Vec<_>>(),
        // Leading SCH1 overlap skip (the previous claim was unfinished at
        // this fire's commit) — the lifecycle transitions are unaffected.
        vec![RunState::SkippedMisfire, RunState::NoAck, RunState::Acknowledged],
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
    h.poll(1); // armed at receipt mono0+1 → deadline mono0+21
    let reply_at_arm = h.reply_bytes(&timer, claim.run_id);

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
    h.poll(40);
    assert_eq!(h.status(&timer)["state"], "completed");
    assert!(h.status(&timer).get("failure_kind").is_none());
    let kinds: Vec<_> = h
        .kinds_for(claim.run_id)
        .into_iter()
        .filter(|k| *k != RunState::Fired)
        .collect();
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
    h.poll(0); // armed at mono0 → deadline mono0+20

    // Exact duplicate rescans: the deadline does not move.
    h.poll(5);
    h.poll(10);
    h.poll(18);
    assert_eq!(h.expire_watchdogs(19), 0);
    assert_eq!(h.expire_watchdogs(21), 1, "duplicates never extended the original deadline");

    // Rearm, then a distinct heartbeat at t0+15 rearms from THAT receipt.
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    let mut doc = h.reply_json(claim2.run_id, "lightbulb", "running");
    doc["expected_secs"] = serde_json::json!(10);
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim2.run_id, doc.clone());
    h.poll(100); // deadline mono0+120
    doc["progress"] = serde_json::json!("still alive");
    h.write_reply(&timer, claim2.run_id, doc);
    h.poll(115); // distinct → deadline mono0+135
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
    assert_eq!(h.poll(1).rejected, 1);
    assert!(h.kinds_for(claim.run_id).contains(&RunState::ReplyRejected));
    assert_eq!(h.row(claim.run_id).state, "fired");

    // Estimate first, opt-in later works (accumulated estimate counts).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "acknowledged");
    doc["expected_secs"] = serde_json::json!(10);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(2);
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(3); // armed: deadline mono0+23

    // An explicit false cancels the pending watchdog; the estimate stays.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["error_detection"] = serde_json::json!(false);
    doc["progress"] = serde_json::json!("nearly there");
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(4);
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
    h.poll(0); // deadline mono0+20

    // Mid-run correction: 900s now — re-anchored at THIS receipt (mono0+5).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["expected_secs"] = serde_json::json!(900);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(5); // deadline mono0+5+1800
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
    h.poll(5);
    assert_eq!(h.status(&timer)["failure_kind"], "reported");

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 9));
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(9);
    let s = h.status(&timer);
    assert_eq!(s["state"], "completed");
    assert!(s.get("reason").is_none(), "the new verdict wins wholesale");
    assert_eq!(
        h.kinds_for(claim.run_id)
            .into_iter()
            .filter(|k| *k != RunState::Fired)
            .collect::<Vec<_>>(),
        vec![RunState::Failed, RunState::Completed]
    );

    // …and the reverse: completed, then failed.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "failed");
    doc["failed_at"] = serde_json::json!(at(&h, 12));
    doc["reason"] = serde_json::json!("actually it broke");
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(12);
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
    h.poll(3);

    // running after an app-authored completed → rejected.
    h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "lightbulb", "running"));
    assert_eq!(h.poll(4).rejected, 1);
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
    h.poll(100); // deadline mono0+110
    assert_eq!(h.expire_watchdogs(111), 1);
    assert_eq!(h.status(&timer)["state"], "failed");

    let mut doc = h.reply_json(claim2.run_id, "lightbulb", "running");
    doc["progress"] = serde_json::json!("recovered, still working");
    h.write_reply(&timer, claim2.run_id, doc);
    assert_eq!(h.poll(120).applied, 1);
    assert_eq!(h.status(&timer)["state"], "running");
    // Rearmed from the mono0+120 receipt (5 × 2 = 10s): not at +129, yes at +131.
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
    h.poll(0);
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
    assert_eq!(h.poll(1).rejected, 1);
    assert_eq!(h.status(&timer)["state"], "fired");

    // Reserved states an app may never write.
    for reserved in ["fired", "no_ack", "cancelled"] {
        h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "lightbulb", reserved));
        assert_eq!(h.poll(2).rejected, 1, "{reserved} must be rejected");
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
    h.poll(3);
    h.poll(4);
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
    h.write_reply_named(&timer, fabricated, h.reply_json(fabricated, "lightbulb", "completed"));
    assert_eq!(h.poll(1).rejected, 1);
    let kinds = h.kinds_for(fabricated);
    assert_eq!(kinds, vec![RunState::ReplyRejected]);
    assert!(
        !h.events().iter().any(|e| e.kind == RunState::Superseded),
        "fabricated ids are tamper/garbage, not slow apps"
    );
    let _ = claim;
}

#[test]
fn a_document_naming_a_different_run_than_its_filename_is_rejected() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim_a = h.fire(&timer, 0, at(&h, 0));
    let claim_b = h.fire(&timer, 1, at(&h, 100));

    // Hand edit: reply-B.json carries a valid document for previous run A.
    // The filename is the channel identity — the document must match it.
    let mut doc = h.reply_json(claim_a.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 150));
    h.write_reply(&timer, claim_b.run_id, doc.clone());
    let mismatched = h.reply_bytes(&timer, claim_b.run_id);

    let stats = h.poll(150);
    assert_eq!(stats.rejected, 1, "filename/document identity mismatch is a rejection");
    assert_eq!(stats.superseded, 0);
    assert!(
        h.reply_path(&timer, claim_b.run_id).exists(),
        "the LIVE current channel is never deleted by a mismatched document"
    );
    assert_eq!(h.reply_bytes(&timer, claim_b.run_id), mismatched);
    assert_eq!(h.status(&timer)["state"], "fired", "run B untouched");
    let evs = h.events();
    let superseded_for_a: Vec<_> = evs
        .iter()
        .filter(|e| e.kind == RunState::Superseded && e.run_id == Some(claim_a.run_id))
        .collect();
    assert_eq!(
        superseded_for_a.len(),
        1,
        "only the fire-time supersede — the mismatched document never reached run A"
    );
    assert_eq!(
        superseded_for_a[0].message.as_deref(),
        Some("superseded by a new firing while still unresolved")
    );
    assert!(evs.iter().any(|e| e.kind == RunState::ReplyRejected));

    // The legitimate owner then answers its own channel properly.
    let mut doc = h.reply_json(claim_b.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 160));
    h.write_reply(&timer, claim_b.run_id, doc);
    assert_eq!(h.poll(160).applied, 1);
    assert_eq!(h.status(&timer)["state"], "completed");
}

#[test]
fn a_stub_shaped_forgery_is_rejected_not_ignored() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // Merely carrying a hint and state:null is NOT the stub Bellman wrote.
    h.write_reply(
        &timer,
        claim.run_id,
        serde_json::json!({ "hint": "x", "state": null }),
    );
    let stats = h.poll(1);
    assert_eq!(stats.rejected, 1, "forged stub is semantically rejected");
    let stats = h.poll(2);
    assert_eq!(stats.rejected, 0, "…once per distinct content");
    let evs = h.events_for(claim.run_id);
    assert_eq!(
        evs.iter().filter(|e| e.kind == RunState::ReplyRejected).count(),
        1
    );

    // The genuine pre-filled stub is still ignored (never a rejection).
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    let stats = h.poll(101);
    assert_eq!(stats.rejected, 0);
    assert_eq!(stats.applied, 0, "the untouched stub is not a reply");
    let _ = claim2;
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
    let stats = h.poll(150);
    assert_eq!(stats.superseded, 1);

    let evs_a = h.events_for(claim_a.run_id);
    assert!(
        evs_a
            .iter()
            .all(|e| matches!(e.kind, RunState::Fired | RunState::Superseded)),
        "only superseded lines for the late reply (plus the fire)"
    );
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
    let stats = h.poll(160);
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

    let kinds_a: Vec<_> = h
        .kinds_for(claim_a.run_id)
        .into_iter()
        .filter(|k| *k != RunState::Fired)
        .collect();
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
fn a_crash_between_commit_and_projection_is_repaired_by_the_reconciler() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    // Simulate the crash window: the fire committed, then the machine died
    // before the projections — status.json and the stub are gone.
    let folder = h.folder(&timer);
    std::fs::remove_file(folder.join(STATUS_FILE_NAME)).unwrap();
    std::fs::remove_file(h.reply_path(&timer, claim.run_id)).unwrap();
    let fire_path = super::notification::fires_dir(&h.dir.path().join("slots"))
        .join(super::notification::fire_notification_name(claim.run_id));
    let _ = std::fs::remove_file(&fire_path);

    // The database is the truth; the reconciler re-projects everything.
    let repaired = h.reconcile();
    assert!(repaired >= 2, "status + stub (+ notification) repaired, got {repaired}");
    let s = h.status(&timer);
    assert_eq!(s["state"], "fired");
    assert_eq!(s["run_id"], claim.run_id.to_string());
    let stub = h.reply_bytes(&timer, claim.run_id);
    let stub_json: serde_json::Value = serde_json::from_slice(&stub).unwrap();
    assert!(stub_json["state"].is_null());
    assert!(fire_path.exists(), "notification re-projected last");
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
        h.kinds_for(claim.run_id)
            .into_iter()
            .filter(|k| *k != RunState::Fired)
            .collect::<Vec<_>>(),
        vec![RunState::Completed],
        "the reply is folded in and logged before the next fire"
    );
    assert_eq!(h.status(&timer)["state"], "completed");
}

#[test]
fn a_restart_reconstructs_the_pickup_deadline_from_the_persisted_value() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire_armed(&timer, 0, at(&h, 0));
    let data_dir = h.dir.path().to_path_buf();
    let t0 = h.t0;

    // "Restart": every in-memory structure is replaced by fresh handles on
    // the same data dir — fresh anchors, fresh EMPTY deadline book, fresh
    // store connection. What survives is only what was persisted.
    let store = Store::open_with(data_dir.join("timers.db"), OpenOptions::default()).unwrap();
    let engine = ReplyEngine {
        tree: TimersTree::new(&data_dir),
        data_dir: data_dir.clone(),
        pickup_grace: Duration::from_secs(60),
        watchdog_factor: 2.0,
        anchors: new_anchors(),
        deadlines: new_deadlines(),
        fire_slot_file: None,
        status_listener: None,
        ipc: None,
    };

    // The persisted wall-clock deadline (t0+60) rebuilds the countdown —
    // a restart does not grant a fresh grace period. Reconstruction at
    // wall t0+61: the deadline is already past → fires on the next pass.
    let mono_now = Instant::now();
    engine
        .sync_deadline_book(&store, t0 + chrono::Duration::seconds(61), mono_now)
        .unwrap();
    let transitioned = engine
        .expire_pickups(
            &store,
            t0 + chrono::Duration::seconds(61),
            mono_now + Duration::from_millis(1),
        )
        .unwrap();
    assert_eq!(transitioned.len(), 1);
    let row = store.get_run_state(claim.run_id).unwrap().unwrap();
    assert_eq!(row.state, "no_ack");
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
    let stats = h.poll(1);
    assert_eq!(stats.rejected, 0, "first sight of partial bytes only starts the debounce");
    std::fs::write(&path, &full).unwrap();
    assert_eq!(h.poll(2).applied, 1);
    assert_eq!(h.status(&timer)["state"], "completed");

    // Invalid bytes left in place: rejected after the debounce, file stays.
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    let path2 = h.reply_path(&timer, claim2.run_id);
    std::fs::write(&path2, b"{ not json").unwrap();
    h.poll(101);
    let stats = h.poll(101);
    assert_eq!(stats.rejected, 0, "still within the debounce window");
    let stats = h.poll(102);
    assert_eq!(stats.rejected, 1, "stable invalid bytes past the debounce");
    assert!(path2.exists(), "quarantine COPIES — the live file is left in place");
    assert_eq!(std::fs::read(&path2).unwrap(), b"{ not json");

    // The app may still overwrite with a valid reply — and it is ingested.
    h.write_reply(&timer, claim2.run_id, h.reply_json(claim2.run_id, "lightbulb", "completed"));
    assert_eq!(h.poll(103).applied, 1);
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

    let stats = h.poll(1);
    assert_eq!(stats.rejected, 1);
    assert!(path.exists(), "the oversize file is left untouched");
    let evs: Vec<_> = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind != RunState::Fired)
        .collect();
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
    let before = h.events().len();
    h.poll(2);
    h.poll(3);
    assert_eq!(h.events().len(), before, "one rejection, then idempotent");
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

    let stats = h.poll(1);
    assert_eq!(stats.rejected, 1);
    let bad = super::quarantine::quarantine_dir(h.engine.tree.root());
    for entry in std::fs::read_dir(&bad).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".payload"),
            "a symlink is never followed — no payload may be copied"
        );
    }
    assert!(h.poll(2).rejected == 0, "one bounded rejection, not a stream");
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
    h.poll(5);
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
    h.poll(130);
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
    h.poll(1);
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
    let evs: Vec<_> = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind != RunState::Fired)
        .collect();
    assert!(evs.is_empty());
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
    assert_eq!(h.poll(1).rejected, 1);
    h.write_reply(&timer, claim.run_id, h.reply_json(claim.run_id, "lightbulb", "completed"));
    assert_eq!(h.poll(2).applied, 1);

    // The next firing's stub carries only the new owner.
    let claim2 = h.fire(&timer, 1, at(&h, 100));
    let stub: serde_json::Value =
        serde_json::from_slice(&h.reply_bytes(&timer, claim2.run_id)).unwrap();
    assert_eq!(stub["app_name"], "new-owner");
}

#[test]
fn duplicate_reply_is_a_no_op() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));

    let mut doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    doc["completed_at"] = serde_json::json!(at(&h, 3));
    h.write_reply(&timer, claim.run_id, doc.clone());
    assert_eq!(h.poll(3).applied, 1);
    let stats = h.poll(4);
    assert_eq!(stats.duplicates, 1);
    assert_eq!(stats.applied, 0);
    let count = h
        .events_for(claim.run_id)
        .into_iter()
        .filter(|e| e.kind != RunState::Fired)
        .count();
    assert_eq!(count, 1, "no second terminal line");
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
    h.poll(3);
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "failed");
    doc["failed_at"] = serde_json::json!(at(&h, 5));
    doc["reason"] = serde_json::json!("post-completion check failed");
    h.write_reply(&timer, claim.run_id, doc);
    assert_eq!(h.poll(5).applied, 1);
    assert_eq!(h.status(&timer)["state"], "failed");
    assert_eq!(h.status(&timer)["reason"], "post-completion check failed");
}

#[test]
fn a_failed_transition_commits_nothing_atomically() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire(&timer, 0, at(&h, 0));
    let _ = h.events(); // drain the fire's own outbox rows first

    // A second connection holds the write lock: the transition (outbox row +
    // lifecycle update) cannot commit, and must leave NO partial state.
    let blocker = Store::open_with(h.dir.path().join("timers.db"), OpenOptions::default())
        .unwrap();
    let _hold = blocker.immediate_tx().unwrap();

    let doc: ReplyDocument =
        serde_json::from_value(h.reply_json(claim.run_id, "lightbulb", "completed")).unwrap();
    let res = h
        .engine
        .ingest(&h.store, &timer, &doc, "digest-a", at(&h, 5), mono(&h, 5));
    assert!(res.is_err(), "a busy store rejects the transition");
    drop(_hold);

    assert_eq!(
        h.store.count_pending_events().unwrap(),
        0,
        "no orphaned outbox row — the rollback was total"
    );
    assert_eq!(h.row(claim.run_id).state, "fired", "no partial lifecycle update");

    // After the blocker: the same transition succeeds and logs exactly once.
    let outcome = h
        .engine
        .ingest(&h.store, &timer, &doc, "digest-a", at(&h, 6), mono(&h, 6))
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Applied);
    assert_eq!(h.row(claim.run_id).state, "completed");
    let completed = h
        .events_for(claim.run_id)
        .iter()
        .filter(|e| e.kind == RunState::Completed)
        .count();
    assert_eq!(completed, 1, "the retried transition logs exactly once");
}

#[test]
fn the_barrier_quarantines_a_forged_run_id_instead_of_ingesting_it() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim_b = h.fire(&timer, 0, at(&h, 0));

    // B current: reply-B.json carries a fabricated run id, and the next fire
    // arrives BEFORE any watcher tick — the barrier must apply the same
    // filename/document identity rule as the ordinary watcher.
    let fabricated = Uuid::new_v4();
    let doc = h.reply_json(fabricated, "lightbulb", "completed");
    h.write_reply(&timer, claim_b.run_id, doc);
    h.fire(&timer, 1, at(&h, 100));

    let evs = h.events();
    assert!(
        evs.iter().any(|e| e.kind == RunState::ReplyRejected),
        "the forged document was rejected"
    );
    assert!(
        !evs.iter().any(|e| e.kind == RunState::Completed && e.run_id == Some(fabricated)),
        "never ingested"
    );
    let bad = super::quarantine::quarantine_dir(h.engine.tree.root());
    let has_payload = std::fs::read_dir(&bad)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().ends_with(".payload"));
    assert!(has_payload, "the forged bytes were quarantined (copy)");
    assert_eq!(h.status(&timer)["state"], "fired", "the new run proceeds untouched");
}

#[test]
fn armed_deadlines_produce_heap_hints_for_the_scheduler() {
    let mut h = Harness::new();
    let timer = h.add_timer("bulb-test", Some("lightbulb"));
    let claim = h.fire_armed(&timer, 0, at(&h, 0));

    // The fire's pickup deadline queued exactly one hint, drained once.
    let hints = h.engine.take_deadline_hints();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].0, claim.run_id);
    assert_eq!(hints[0].1, DeadlineKind::Pickup);
    assert!(h.engine.take_deadline_hints().is_empty());

    // A watchdog rearm queues a watchdog hint with a wall estimate.
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["expected_secs"] = serde_json::json!(10);
    doc["error_detection"] = serde_json::json!(true);
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(1);
    let hints = h.engine.take_deadline_hints();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].1, DeadlineKind::Watchdog);
    let expected_wall = at(&h, 1) + chrono::Duration::seconds(20);
    let delta = (hints[0].2 - expected_wall).num_seconds().abs();
    assert!(delta <= 1, "wall estimate matches receipt + expected × factor");
}

/// IK5: every accepted status projection fires exactly one invalidation with
/// the timer id — on the fire itself, on an accepted reply, on a pickup
/// expiry (`no_ack`) and on a watchdog expiry. The GUI refetches on this
/// signal; a miss here is a stale row.
#[test]
fn status_listener_fires_on_every_projection() {
    let mut h = Harness::new();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Uuid>::new()));
    let seen2 = seen.clone();
    h.engine.status_listener = Some(StatusListener(std::sync::Arc::new(move |timer_id| {
        seen2.lock().unwrap().push(timer_id);
    })));
    let timer = h.add_timer("bulb-test", Some("lightbulb"));

    // 1. The fire's initial projection (state `fired`).
    let claim = h.fire_armed(&timer, 0, at(&h, 0));
    assert_eq!(seen.lock().unwrap().as_slice(), &[timer.id]);

    // 2. An accepted reply (state → running, progress folded in).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "running");
    doc["expected_secs"] = serde_json::json!(10);
    doc["error_detection"] = serde_json::json!(true);
    doc["progress"] = serde_json::json!("half-way");
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(1);
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(h.status(&timer)["state"], "running");
    assert_eq!(h.status(&timer)["progress"], "half-way");

    // 3. The opt-in watchdog expiring (state → failed/timed_out).
    h.engine.deadlines.lock().unwrap().entries.insert(
        claim.run_id,
        MonoDeadline {
            kind: DeadlineKind::Watchdog,
            at: mono(&h, 2),
        },
    );
    h.expire_watchdogs(3);
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert_eq!(h.status(&timer)["state"], "failed");
    assert_eq!(h.status(&timer)["failure_kind"], "timed_out");

    // 4. A late terminal revision on the still-current run (completed wins).
    let doc = h.reply_json(claim.run_id, "lightbulb", "completed");
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(4);
    assert_eq!(seen.lock().unwrap().len(), 4);
    assert_eq!(h.status(&timer)["state"], "completed");

    // 5. The app revises its OWN verdict completed → failed on the same
    // still-current run — a terminal-but-current revision notifies too
    // (the GUI must refresh without any polling).
    let mut doc = h.reply_json(claim.run_id, "lightbulb", "failed");
    doc["reason"] = serde_json::json!("changed my mind");
    h.write_reply(&timer, claim.run_id, doc);
    h.poll(5);
    assert_eq!(seen.lock().unwrap().len(), 5, "terminal revision notifies");
    assert_eq!(h.status(&timer)["state"], "failed");
    assert_eq!(h.status(&timer)["failure_kind"], "reported");

    // Every notification carried ONLY this timer's id.
    assert!(seen.lock().unwrap().iter().all(|id| *id == timer.id));
}

/// IK5: a pickup deadline expiring to `no_ack` also notifies.
#[test]
fn status_listener_fires_on_no_ack() {
    let mut h = Harness::new();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Uuid>::new()));
    let seen2 = seen.clone();
    h.engine.status_listener = Some(StatusListener(std::sync::Arc::new(move |timer_id| {
        seen2.lock().unwrap().push(timer_id);
    })));
    let timer = h.add_timer("quiet-app", Some("quiet"));
    let claim = h.fire_armed(&timer, 0, at(&h, 0));
    assert_eq!(seen.lock().unwrap().len(), 1);

    h.arm_pickup(claim.run_id, 60);
    h.expire_pickups(61);
    assert_eq!(seen.lock().unwrap().len(), 2, "no_ack projection notified");
    assert_eq!(h.status(&timer)["state"], "no_ack");
}
