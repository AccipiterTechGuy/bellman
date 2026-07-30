//! IK3 end-to-end: the real fire path (`run_now`) plus the real reply
//! watcher thread, over a real temp data dir — no fakes.

use bellman_core::events::{read_events, EventRecord};
use bellman_core::occurrence::{Occurrence, OccurrenceKind};
use bellman_core::reply::{
    self, new_anchors, new_deadlines, ReplyEngine, SharedAnchors, SharedDeadlines,
    REPLY_SCHEMA_V1,
};
use bellman_core::slots::{spawn_watch_thread, WatchConfig};
use bellman_core::store::{NewTimer, OpenOptions, Store};
use bellman_core::tree::{reply_file_name, TimersTree, STATUS_FILE_NAME};
use bellman_core::{run_now, RunNowOptions};
use chrono::NaiveTime;
use std::path::Path;
use std::time::{Duration, Instant};

struct E2e {
    dir: tempfile::TempDir,
    store: Store,
}

impl E2e {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("slots")).unwrap();
        let store = Store::open_with(dir.path().join("timers.db"), OpenOptions::default())
            .unwrap();
        Self { dir, store }
    }

    fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    fn engine(
        &self,
        pickup_grace: Duration,
        anchors: SharedAnchors,
        deadlines: SharedDeadlines,
    ) -> ReplyEngine {
        ReplyEngine {
            tree: TimersTree::new(self.data_dir()),
            data_dir: self.data_dir().to_path_buf(),
            pickup_grace,
            watchdog_factor: 2.0,
            anchors,
            deadlines,
        }
    }

    fn add_owned_timer(&mut self, name: &str, app: &str) -> bellman_core::Timer {
        let occ = Occurrence::new(
            OccurrenceKind::Daily {
                at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
            },
            "UTC",
        )
        .unwrap();
        let timer = self.store.create_timer(NewTimer::new(name, occ)).unwrap();
        self.store.set_timer_owner(timer.id, app).unwrap();
        TimersTree::new(self.data_dir())
            .create_for_timer(&timer, Some(app))
            .unwrap();
        timer
    }

    fn status(&self, timer_id: uuid::Uuid) -> serde_json::Value {
        let folder = TimersTree::new(self.data_dir()).folder_for(timer_id).unwrap();
        let raw = std::fs::read_to_string(folder.join(STATUS_FILE_NAME)).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    /// Poll status.json until `pred` holds or the timeout expires.
    fn wait_status(
        &self,
        timer_id: uuid::Uuid,
        timeout: Duration,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.status(timer_id);
            if pred(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "timed out waiting for status.json; last: {s}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The reply-lifecycle kinds logged for `run_id` so far (R11: the
    /// watcher's publisher tick drains the outbox, so they can lag the
    /// status.json projection by a tick).
    fn log_kinds(&self, run_id: uuid::Uuid) -> (Vec<EventRecord>, Vec<String>) {
        let (recs, _) = read_events(self.data_dir().join("logs/events.current.jsonl"))
            .unwrap_or_default();
        let kinds: Vec<String> = recs
            .iter()
            .filter(|r| r.run_id == Some(run_id))
            .map(|r| r.kind.as_str().to_string())
            .filter(|k| ["acknowledged", "running", "completed", "failed", "no_ack"].contains(&k.as_str()))
            .collect();
        (recs, kinds)
    }

    /// Poll the drained event log until the reply-lifecycle kinds for
    /// `run_id` equal `expected`; returns the full records for field checks.
    fn wait_log_kinds(
        &self,
        run_id: uuid::Uuid,
        timeout: Duration,
        expected: &[&str],
    ) -> Vec<EventRecord> {
        let deadline = Instant::now() + timeout;
        loop {
            let (recs, kinds) = self.log_kinds(run_id);
            if kinds == expected {
                return recs;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for log kinds {expected:?}; last: {kinds:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn end_to_end_run_now_reply_mirror_and_fire_notification() {
    let mut e = E2e::new();
    let timer = e.add_owned_timer("bulb-e2e", "lightbulb");
    let anchors = new_anchors();
    let deadlines = new_deadlines();
    let engine = e.engine(Duration::from_secs(60), anchors.clone(), deadlines.clone());

    // Fire for real through the production run_now path. The watch thread is
    // started AFTER the fire: its startup (store open + migrate writes, first
    // publisher/slot commits) would otherwise race the R10 fire transaction,
    // and a deferred-tx upgrade conflict surfaces as "database is locked"
    // (SQLITE_BUSY_SNAPSHOT does not honor busy_timeout).
    let opts = RunNowOptions {
        skip_retry_sleep: true,
        anchors: Some(anchors),
        deadlines: Some(deadlines),
        ..Default::default()
    };
    let db = e.data_dir().join("timers.db");
    let outcome = run_now(&mut e.store, &db, timer.id, &opts)
        .expect("run_now");
    let run_id = outcome.run_id;

    let stop = spawn_watch_thread(WatchConfig {
        slots_root: e.data_dir().join("slots"),
        data_dir: e.data_dir().to_path_buf(),
        db_path: e.data_dir().join("timers.db"),
        reply_engine: Some(engine),
        scheduler: None,
        poll_interval: Duration::from_millis(100),
    })
    .unwrap();

    // T0: pre-filled stub, firing snapshot, fire notification with reply_path.
    let folder = TimersTree::new(e.data_dir()).folder_for(timer.id).unwrap();
    let stub_raw = std::fs::read(folder.join(reply_file_name(run_id))).unwrap();
    let stub: serde_json::Value = serde_json::from_slice(&stub_raw).unwrap();
    assert_eq!(stub["schema"], REPLY_SCHEMA_V1);
    assert_eq!(stub["app_name"], "lightbulb");
    assert!(stub["state"].is_null());

    let fire_path = e
        .data_dir()
        .join("slots/fires")
        .join(reply::fire_notification_name(run_id));
    let fire: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fire_path).unwrap()).unwrap();
    assert_eq!(fire["schema"], "bellman-slot/1");
    assert_eq!(fire["kind"], "fired");
    assert_eq!(fire["app_name"], "lightbulb");
    let reply_path = fire["reply_path"].as_str().unwrap().to_string();
    assert!(reply_path.ends_with(&reply_file_name(run_id)));
    assert!(Path::new(&reply_path).exists(), "reply_path is projected before the notification");

    // The app answers through the file named by the notification.
    let reply = serde_json::json!({
        "schema": REPLY_SCHEMA_V1,
        "run_id": run_id,
        "app_name": "lightbulb",
        "state": "completed",
        "expected_secs": 5,
        "result": { "ok": true },
    });
    std::fs::write(&reply_path, serde_json::to_vec(&reply).unwrap()).unwrap();

    // The watcher folds it in: status.json mirrors completed.
    let s = e.wait_status(timer.id, Duration::from_secs(5), |s| s["state"] == "completed");
    assert_eq!(s["expected_secs"], 5);
    assert_eq!(s["result"]["ok"], true);

    // And the app-lifecycle transition lines are in the log under one run_id
    // (the run_now fire lines — fired/wake_delivered — are separate kinds).
    // A direct `completed` with no acknowledged_at logs ONLY `completed` —
    // Bellman never invents `acknowledged`.
    let recs = e.wait_log_kinds(run_id, Duration::from_secs(5), &["completed"]);
    let (_, kinds) = e.log_kinds(run_id);
    assert_eq!(kinds, vec!["completed"]);
    let completed = recs
        .iter()
        .find(|r| r.run_id == Some(run_id) && r.kind.as_str() == "completed")
        .unwrap();
    assert!(completed.duration_ms.unwrap() >= 0);

    stop.stop();
}

#[test]
fn end_to_end_no_ack_when_the_app_never_answers() {
    let mut e = E2e::new();
    let timer = e.add_owned_timer("silent-app", "lightbulb");
    // The pickup deadline is its own config knob — 1s here so the test is fast.
    let cfg = bellman_core::app_config::AppConfig {
        pickup_grace_secs: 1,
        ..Default::default()
    };
    cfg.save(e.data_dir()).unwrap();
    let anchors = new_anchors();
    let deadlines = new_deadlines();
    let engine = e.engine(Duration::from_secs(1), anchors.clone(), deadlines.clone());

    // Fire first, then start the watcher (see the first e2e for why the
    // watcher startup must not overlap the R10 fire transaction).
    let opts = RunNowOptions {
        skip_retry_sleep: true,
        anchors: Some(anchors),
        deadlines: Some(deadlines),
        ..Default::default()
    };
    let db = e.data_dir().join("timers.db");
    let outcome = run_now(&mut e.store, &db, timer.id, &opts)
        .expect("run_now");
    let stop = spawn_watch_thread(WatchConfig {
        slots_root: e.data_dir().join("slots"),
        data_dir: e.data_dir().to_path_buf(),
        db_path: e.data_dir().join("timers.db"),
        reply_engine: Some(engine),
        scheduler: None,
        poll_interval: Duration::from_millis(100),
    })
    .unwrap();
    let folder = TimersTree::new(e.data_dir()).folder_for(timer.id).unwrap();
    let stub_at_fire = std::fs::read(folder.join(reply_file_name(outcome.run_id))).unwrap();

    // ~1s pickup grace: the run becomes no_ack and the stub is untouched.
    let s = e.wait_status(timer.id, Duration::from_secs(5), |s| s["state"] == "no_ack");
    assert!(s["no_ack_at"].is_string());
    assert_eq!(
        std::fs::read(folder.join(reply_file_name(outcome.run_id))).unwrap(),
        stub_at_fire,
        "no_ack never touches the reply file"
    );

    // A late reply while the run is still current revises it.
    let reply = serde_json::json!({
        "schema": REPLY_SCHEMA_V1,
        "run_id": outcome.run_id,
        "app_name": "lightbulb",
        "state": "completed",
    });
    std::fs::write(
        folder.join(reply_file_name(outcome.run_id)),
        serde_json::to_vec(&reply).unwrap(),
    )
    .unwrap();
    let s = e.wait_status(timer.id, Duration::from_secs(5), |s| s["state"] == "completed");
    assert_eq!(s["state"], "completed");

    e.wait_log_kinds(outcome.run_id, Duration::from_secs(5), &["no_ack", "completed"]);
    let (_, kinds) = e.log_kinds(outcome.run_id);
    assert_eq!(kinds, vec!["no_ack", "completed"]);

    stop.stop();
}
