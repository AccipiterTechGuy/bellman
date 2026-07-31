//! IK6 exit gates — the dual transport (local IPC socket + JSON files) over
//! a real socket, a real temp data dir and the real fire path. No fakes.
//!
//! The structural claim of the card: there is exactly ONE ingest function
//! and both adapters call it — asserted here by driving the same reply
//! bytes through the file transport and the socket transport and comparing
//! state, log lines and `status.json`.
#![cfg(unix)]

use bellman_core::events::{read_events, EventPublisher, EventLogConfig};
use bellman_core::occurrence::{Occurrence, OccurrenceKind};
use bellman_core::reply::{
    self, new_anchors, new_deadlines, publication, ReplyEngine, REPLY_SCHEMA_V1,
};
use bellman_core::scheduler::FireKind;
use bellman_core::store::{NewTimer, OpenOptions, Store, Timer, TransportMode};
use bellman_core::tree::{project_fire, reply_file_name, TimersTree, STATUS_FILE_NAME};
use bellman_core::ipc::{IpcConfig, IpcHandle, IpcServer, CLAIM_SCHEMA_V1};
use chrono::{NaiveTime, Utc};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

const APP: &str = "testapp";

// ── Harness ───────────────────────────────────────────────────────────────

struct Env {
    _dir: tempfile::TempDir,
    data: PathBuf,
    db: PathBuf,
    sock: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_path_buf();
    std::fs::create_dir_all(data.join("slots")).unwrap();
    let sock = data.join("ipc").join("bellman.sock");
    Env {
        _dir: dir,
        db: data.join("timers.db"),
        data,
        sock,
    }
}

fn store(e: &Env) -> Store {
    Store::open_with(
        &e.db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .unwrap()
}

fn engine(e: &Env, ipc: Option<&IpcHandle>, pickup_grace: Duration) -> ReplyEngine {
    ReplyEngine {
        tree: TimersTree::new(&e.data),
        data_dir: e.data.clone(),
        pickup_grace,
        watchdog_factor: 2.0,
        anchors: new_anchors(),
        deadlines: new_deadlines(),
        fire_slot_file: None,
        status_listener: None,
        ipc: ipc.cloned(),
    }
}

fn spawn_server(e: &Env, eng: ReplyEngine, handle: &IpcHandle) -> IpcServer {
    IpcServer::spawn(IpcConfig {
        socket_path: e.sock.clone(),
        data_dir: e.data.clone(),
        db_path: e.db.clone(),
        engine: eng,
        handle: handle.clone(),
    })
    .expect("ipc server spawn")
}

fn owned_timer(e: &Env, store: &mut Store, name: &str, transport: TransportMode) -> Timer {
    let occ = Occurrence::new(
        OccurrenceKind::Daily {
            at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
        },
        "UTC",
    )
    .unwrap();
    let mut new = NewTimer::new(name, occ);
    new.transport = transport;
    let timer = store.create_timer(new).unwrap();
    store.set_timer_owner(timer.id, APP).unwrap();
    TimersTree::new(&e.data).create_for_timer(&timer, Some(APP)).ok();
    timer
}

fn folder(e: &Env, timer_id: Uuid) -> PathBuf {
    TimersTree::new(&e.data).folder_for(timer_id).unwrap()
}

fn fire(e: &Env, st: &mut Store, eng: &ReplyEngine, timer: &Timer) -> Uuid {
    let claim = project_fire(
        &TimersTree::new(&e.data),
        st,
        timer,
        Utc::now(),
        &FireKind::OnTime,
        eng,
        Utc::now(),
    )
    .expect("project_fire");
    claim.run_id
}

fn reply_json(run_id: Uuid) -> String {
    // Fixed timestamps: the byte string is identical for any run, so the
    // file transport and the socket transport ingest the SAME reply bytes.
    json!({
        "schema": REPLY_SCHEMA_V1,
        "run_id": run_id,
        "app_name": APP,
        "state": "acknowledged",
        "acknowledged_at": "2026-07-31T00:00:01Z",
        "expected_secs": 30,
        "error_detection": true,
        "heartbeat_at": "2026-07-31T00:00:02Z",
    })
    .to_string()
}

fn run_row(st: &Store, run_id: Uuid) -> bellman_core::RunStateRow {
    st.get_run_state(run_id).unwrap().expect("run_states row")
}

fn wait_row(st: &Store, run_id: Uuid, timeout: Duration, pred: impl Fn(&bellman_core::RunStateRow) -> bool) -> bellman_core::RunStateRow {
    let deadline = Instant::now() + timeout;
    loop {
        let row = run_row(st, run_id);
        if pred(&row) {
            return row;
        }
        assert!(Instant::now() < deadline, "timed out waiting for run row; last state: {}", row.state);
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn drain_log(e: &Env, st: &Store) -> Vec<bellman_core::events::EventRecord> {
    let mut publisher = EventPublisher::with_config(EventLogConfig::new(e.data.join("logs"))).unwrap();
    publisher.publish_cycle(st);
    let (recs, _) = read_events(e.data.join("logs/events.current.jsonl")).unwrap_or_default();
    recs
}

// ── Client ────────────────────────────────────────────────────────────────

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

fn connect(e: &Env) -> Client {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(&e.sock) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let reader = BufReader::new(stream.try_clone().unwrap());
                return Client { stream, reader };
            }
            Err(_) => {
                assert!(Instant::now() < deadline, "cannot connect to ipc socket");
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

impl Client {
    fn line(&mut self) -> String {
        let mut s = String::new();
        let n = self.reader.read_line(&mut s).expect("read line");
        assert!(n > 0, "connection closed by server");
        s.trim_end().to_string()
    }

    fn claim(&mut self, timer_id: Uuid, app_name: &str) -> Value {
        let claim = json!({
            "schema": CLAIM_SCHEMA_V1,
            "app_name": app_name,
            "timer_id": timer_id,
        });
        writeln!(self.stream, "{claim}").unwrap();
        serde_json::from_str(&self.line()).unwrap()
    }

    fn send(&mut self, payload: &str) {
        writeln!(self.stream, "{payload}").unwrap();
    }

    /// Read lines until a fire notification for `run_id` arrives.
    fn fire_for(&mut self, run_id: Uuid) -> Value {
        for _ in 0..4 {
            let v: Value = serde_json::from_str(&self.line()).unwrap();
            if v["run_id"].as_str() == Some(run_id.to_string().as_str()) {
                return v;
            }
        }
        panic!("no fire frame for {run_id}");
    }
}

// ── The exit gates ────────────────────────────────────────────────────────

/// THE structural gate: the same reply bytes produce identical state, log
/// lines and status.json whether they arrive by socket or by file — because
/// both adapters call the one ingest function.
#[test]
fn same_reply_bytes_identical_records_via_socket_and_file() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle);

    // ── Run 1: the FILE transport ──
    let t1 = owned_timer(&e, &mut st, "parity-file", TransportMode::Json);
    let run1 = fire(&e, &mut st, &eng, &t1);
    let stub1 = folder(&e, t1.id).join(reply_file_name(run1));
    assert!(stub1.exists(), "file firing pre-creates the reply stub");
    let bytes1 = reply_json(run1);
    std::fs::write(&stub1, &bytes1).unwrap();
    let mut tracker = reply::InvalidTracker::default();
    reply::poll_once(&eng, &st, Utc::now(), Instant::now(), &mut tracker);
    let row1 = wait_row(&st, run1, Duration::from_secs(2), |r| r.state == "acknowledged");

    // ── Run 2: the SOCKET transport, same reply content ──
    let t2 = owned_timer(&e, &mut st, "parity-ipc", TransportMode::Ipc);
    let mut client = connect(&e);
    let ok = client.claim(t2.id, APP);
    assert_eq!(ok["ok"], json!(true), "claim accepted: {ok}");
    let run2 = fire(&e, &mut st, &eng, &t2);
    let frame = client.fire_for(run2);
    assert!(frame.get("reply_path").is_none(), "IPC fire omits reply_path");
    let bytes2 = reply_json(run2);
    assert_eq!(
        bytes1.replace(&run1.to_string(), "RUN"),
        bytes2.replace(&run2.to_string(), "RUN"),
        "the reply bytes are identical modulo the run id"
    );
    client.send(&bytes2);
    let row2 = wait_row(&st, run2, Duration::from_secs(5), |r| r.state == "acknowledged");

    // ── Identical accumulated state ──
    for (name, a, b) in [
        ("state", json!(row1.state), json!(row2.state)),
        ("expected_secs", json!(row1.expected_secs), json!(row2.expected_secs)),
        ("error_detection", json!(row1.error_detection), json!(row2.error_detection)),
        ("has acknowledged_at", json!(row1.acknowledged_at.is_some()), json!(row2.acknowledged_at.is_some())),
        ("has heartbeat_at", json!(row1.heartbeat_at.is_some()), json!(row2.heartbeat_at.is_some())),
        ("watchdog armed", json!(row1.watchdog_deadline.is_some()), json!(row2.watchdog_deadline.is_some())),
        ("pickup consumed", json!(row1.pickup_deadline.is_none()), json!(row2.pickup_deadline.is_none())),
        ("ack logged once", json!(row1.acknowledged_logged), json!(row2.acknowledged_logged)),
    ] {
        assert_eq!(a, b, "state mismatch: {name}");
    }

    // ── Identical status.json modulo identity/timing/transport ──
    let norm = |mut v: Value, run: Uuid, t: &Timer| {
        for k in ["run_id", "fired_at", "acknowledged_at", "heartbeat_at", "timer_id", "timer_name", "transport", "scheduled_for"] {
            v.as_object_mut().unwrap().remove(k);
        }
        let _ = (run, t);
        v
    };
    let s1: Value = serde_json::from_str(&std::fs::read_to_string(folder(&e, t1.id).join(STATUS_FILE_NAME)).unwrap()).unwrap();
    let s2: Value = serde_json::from_str(&std::fs::read_to_string(folder(&e, t2.id).join(STATUS_FILE_NAME)).unwrap()).unwrap();
    assert_eq!(norm(s1, run1, &t1), norm(s2, run2, &t2), "status.json identical modulo identity");

    // ── Identical log lines modulo run identity/timestamps ──
    let recs = drain_log(&e, &st);
    let norm_events = |run: Uuid| {
        let mut v: Vec<(String, Option<String>)> = recs
            .iter()
            .filter(|r| r.run_id == Some(run))
            .map(|r| (r.kind.as_str().to_string(), r.message.clone()))
            .collect();
        v.dedup();
        v
    };
    assert_eq!(
        norm_events(run1),
        norm_events(run2),
        "identical log lines for identical replies on both transports"
    );
}

/// A handle without a LIVE server (failed bind, stopped server) must never
/// strand a firing: every mode resolves to the file transport — the
/// fallback that already works. (Supervisor repro: an `ipc` timer firing
/// against a dead handle dead-ended at `no_ack` with no channel any app
/// could have answered on.)
#[test]
fn no_live_server_degrades_every_firing_to_files() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone()); // NO server is ever spawned
    assert!(!handle.is_live());
    let eng = engine(&e, Some(&handle), Duration::from_secs(0));

    for (name, mode) in [("t-dead-ipc", TransportMode::Ipc), ("t-dead-auto", TransportMode::Auto)] {
        let t = owned_timer(&e, &mut st, name, mode);
        let run = fire(&e, &mut st, &eng, &t);
        let row = run_row(&st, run);
        assert_eq!(
            row.selected_transport.as_deref(),
            Some("json"),
            "{name}: no live server ⇒ files"
        );
        assert_eq!(row.transport.as_deref(), Some("json"));
        assert!(
            folder(&e, t.id).join(reply_file_name(run)).exists(),
            "{name}: the stub an app can answer on exists"
        );
        assert!(
            e.data.join("slots/fires").join(reply::fire_notification_name(run)).exists(),
            "{name}: the fire file exists"
        );
    }
}

/// A live server that later STOPS also degrades: the next firing after
/// `stop()` resolves to files again.
#[test]
fn stopped_server_degrades_later_firings_to_files() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let server = spawn_server(&e, eng.clone(), &handle);
    assert!(handle.is_live());

    let t = owned_timer(&e, &mut st, "t-bounce", TransportMode::Ipc);
    let mut client = connect(&e);
    assert_eq!(client.claim(t.id, APP)["ok"], json!(true));
    let run1 = fire(&e, &mut st, &eng, &t);
    let _ = client.fire_for(run1);
    assert_eq!(run_row(&st, run1).selected_transport.as_deref(), Some("ipc"));
    client.send(&reply_json(run1));
    wait_row(&st, run1, Duration::from_secs(5), |r| r.state == "acknowledged");
    drop(client);

    server.stop();
    assert!(!handle.is_live(), "stop clears the live flag");
    let run2 = fire(&e, &mut st, &eng, &t);
    let row2 = run_row(&st, run2);
    assert_eq!(row2.selected_transport.as_deref(), Some("json"), "post-stop firing uses files");
    assert!(folder(&e, t.id).join(reply_file_name(run2)).exists());
}

/// `json` mode (the default) and `auto` with no connected client both use
/// the file transport: stub + fire file, `transport: json` on the run.
#[test]
fn json_default_and_auto_without_client_use_files() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle);

    for (name, mode) in [("t-json", TransportMode::Json), ("t-auto", TransportMode::Auto)] {
        let t = owned_timer(&e, &mut st, name, mode);
        let run = fire(&e, &mut st, &eng, &t);
        let row = run_row(&st, run);
        assert_eq!(row.selected_transport.as_deref(), Some("json"), "{name} selected");
        assert_eq!(row.transport.as_deref(), Some("json"), "{name} effective");
        assert!(folder(&e, t.id).join(reply_file_name(run)).exists(), "{name} stub");
        assert!(
            e.data.join("slots/fires").join(reply::fire_notification_name(run)).exists(),
            "{name} fire file"
        );
        let proj = st.transport_projection(run).unwrap().unwrap();
        assert_eq!(proj.kind, "file");
        let status: Value = serde_json::from_str(
            &std::fs::read_to_string(folder(&e, t.id).join(STATUS_FILE_NAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(status["transport"], json!("json"));
    }
}

/// An `ipc` firing delivers the fire message over the socket (no
/// `reply_path`, the `ipc.socket` endpoint present), writes NO reply stub,
/// always writes status.json, and a socket reply confirms the run.
#[test]
fn ipc_firing_delivers_over_socket_and_confirms() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle);

    let t = owned_timer(&e, &mut st, "t-ipc", TransportMode::Ipc);
    let mut client = connect(&e);
    assert_eq!(client.claim(t.id, APP)["ok"], json!(true));

    let run = fire(&e, &mut st, &eng, &t);
    let frame = client.fire_for(run);
    assert_eq!(frame["schema"], json!("bellman-slot/1"));
    assert_eq!(frame["run_id"], json!(run.to_string()));
    assert!(frame.get("reply_path").is_none(), "IPC fire message has no reply_path");
    assert!(
        frame["ipc"]["socket"].as_str().unwrap().ends_with("bellman.sock"),
        "fire message carries the socket endpoint: {frame}"
    );
    assert!(frame["status_path"].as_str().unwrap().ends_with("status.json"));

    // No stub for an IPC firing; status.json always written.
    assert!(!folder(&e, t.id).join(reply_file_name(run)).exists(), "no stub for ipc run");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(folder(&e, t.id).join(STATUS_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(status["run_id"], json!(run.to_string()));
    assert_eq!(status["transport"], json!("ipc"));

    let row = run_row(&st, run);
    assert_eq!(row.selected_transport.as_deref(), Some("ipc"));
    assert_eq!(row.transport.as_deref(), Some("ipc"));

    // Confirm over the socket; the projection records pickup and sends stop.
    client.send(&reply_json(run));
    wait_row(&st, run, Duration::from_secs(5), |r| r.state == "acknowledged");
    publication::pump(&e.data, &st, 8, Some(&handle));
    let proj = st.transport_projection(run).unwrap().unwrap();
    assert_eq!(proj.kind, "ipc");
    assert_eq!(proj.state, "picked_up", "confirmation settles the transport");

    // A client that confirms and then disconnects is NOT no_ack.
    drop(client);
    assert!(
        !eng.expire_pickup_one(&st, &t, run, Utc::now()).unwrap(),
        "nothing left to expire after confirmation"
    );
    assert_eq!(run_row(&st, run).state, "acknowledged");
}

/// Explicit `ipc`, client killed BEFORE confirmation: the run goes `no_ack`
/// when grace lapses — exactly like an unwatched folder. A later client
/// claim triggers ONE replay for the still-current run, and confirmation
/// revises `no_ack` to `acknowledged`, exactly like late file pickup.
#[test]
fn ipc_kill_before_confirmation_no_ack_then_late_claim_replay() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle);

    let t = owned_timer(&e, &mut st, "t-ipc-kill", TransportMode::Ipc);
    let run = {
        let mut client = connect(&e);
        assert_eq!(client.claim(t.id, APP)["ok"], json!(true));
        let run = fire(&e, &mut st, &eng, &t);
        let _frame = client.fire_for(run);
        drop(client); // killed before any reply
        run
    };
    // The server notices the disconnect (reader EOF → unregister).
    let deadline = Instant::now() + Duration::from_secs(5);
    while handle.has_client(&t.id) {
        assert!(Instant::now() < deadline, "client still registered");
        std::thread::sleep(Duration::from_millis(25));
    }

    // Grace lapses → no_ack, never a fallback in explicit ipc mode.
    assert!(eng.expire_pickup_one(&st, &t, run, Utc::now()).unwrap());
    let row = run_row(&st, run);
    assert_eq!(row.state, "no_ack");
    assert_eq!(row.transport.as_deref(), Some("ipc"), "no fallback in explicit ipc");
    assert!(!folder(&e, t.id).join(reply_file_name(run)).exists(), "no stub minted");
    assert!(!e.data.join("slots/fires").join(reply::fire_notification_name(run)).exists());

    // A later claim triggers one replay of the still-current run…
    let mut client = connect(&e);
    assert_eq!(client.claim(t.id, APP)["ok"], json!(true));
    let replay = client.fire_for(run);
    assert_eq!(replay["run_id"], json!(run.to_string()), "same run_id replayed, never a second run");
    // …and confirmation revises no_ack exactly like late file pickup.
    client.send(&reply_json(run));
    wait_row(&st, run, Duration::from_secs(5), |r| r.state == "acknowledged");
    assert_eq!(run_row(&st, run).state, "acknowledged");
}

/// `auto` with a client at fire selects IPC; an unconfirmed IPC failure
/// (client gone) falls back to files with the SAME `run_id`, `ipc_fallback`
/// recorded, the stub created BEFORE the fallback message, and no duplicate
/// run anywhere. The app then answers through the file channel.
#[test]
fn auto_unconfirmed_ipc_failure_falls_back_to_files_same_run() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle);

    let t = owned_timer(&e, &mut st, "t-auto-fb", TransportMode::Auto);
    let run = {
        let mut client = connect(&e);
        assert_eq!(client.claim(t.id, APP)["ok"], json!(true));
        let run = fire(&e, &mut st, &eng, &t);
        let _frame = client.fire_for(run); // IPC selected at fire
        drop(client); // gone before confirmation
        run
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while handle.has_client(&t.id) {
        assert!(Instant::now() < deadline, "client still registered");
        std::thread::sleep(Duration::from_millis(25));
    }
    let row = run_row(&st, run);
    assert_eq!(row.selected_transport.as_deref(), Some("ipc"), "auto selected ipc at fire");

    // The pump observes the unconfirmed IPC failure and falls back.
    publication::pump(&e.data, &st, 8, Some(&handle));

    let row = run_row(&st, run);
    assert_eq!(row.transport.as_deref(), Some("ipc_fallback"), "fallback recorded on the run");
    assert_eq!(row.selected_transport.as_deref(), Some("ipc"), "selected mode never changed");
    let runs = st.runs_for_timer(t.id).unwrap();
    assert_eq!(runs.len(), 1, "no duplicate run minted");
    assert_eq!(runs[0].run_id, run);

    // The stub exists and the SAME run_id is published through the file
    // adapter with its reply_path (added only after the stub exists).
    let stub = folder(&e, t.id).join(reply_file_name(run));
    assert!(stub.exists(), "fallback creates the stub");
    let fire_file = e.data.join("slots/fires").join(reply::fire_notification_name(run));
    let msg: Value = serde_json::from_str(&std::fs::read_to_string(&fire_file).unwrap()).unwrap();
    assert_eq!(msg["run_id"], json!(run.to_string()));
    assert_eq!(msg["reply_path"], json!(stub.to_string_lossy().to_string()));
    let proj = st.transport_projection(run).unwrap().unwrap();
    assert_eq!(proj.kind, "file", "same projection row, converted");

    // The mirror is re-projected at fallback time, not left stale.
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(folder(&e, t.id).join(STATUS_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(status["transport"], json!("ipc_fallback"));

    // The app answers through the file channel — one ingest path again.
    std::fs::write(&stub, reply_json(run)).unwrap();
    let mut tracker = reply::InvalidTracker::default();
    reply::poll_once(&eng, &st, Utc::now(), Instant::now(), &mut tracker);
    wait_row(&st, run, Duration::from_secs(2), |r| r.state == "acknowledged");
}

/// A process claiming an owned timer's `app_name` over the socket is
/// rejected by the same rule as a wrong-name file reply — one rule, both
/// paths.
#[test]
fn wrong_app_name_rejected_on_socket_and_file_alike() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle);

    let t = owned_timer(&e, &mut st, "t-claim", TransportMode::Auto);

    // Socket path: a wrong-name CLAIM is rejected with the file path's rule.
    let mut evil = connect(&e);
    let res = evil.claim(t.id, "not-the-owner");
    assert_eq!(res["ok"], json!(false));
    assert_eq!(
        res["error"],
        json!("app_name does not match the run's owner"),
        "same rejection as the file path: {res}"
    );
    // The connection is closed by the server.
    let mut buf = [0u8; 1];
    let n = evil.reader.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "rejected claim closes the connection");

    // File path: a wrong-name reply gets the identical rejection reason.
    let run = fire(&e, &mut st, &eng, &t);
    let stub = folder(&e, t.id).join(reply_file_name(run));
    std::fs::write(
        &stub,
        json!({
            "schema": REPLY_SCHEMA_V1,
            "run_id": run,
            "app_name": "not-the-owner",
            "state": "acknowledged",
        })
        .to_string(),
    )
    .unwrap();
    let mut tracker = reply::InvalidTracker::default();
    reply::poll_once(&eng, &st, Utc::now(), Instant::now(), &mut tracker);

    let recs = drain_log(&e, &st);
    let rejections: Vec<Option<String>> = recs
        .iter()
        .filter(|r| r.kind.as_str() == "reply_rejected")
        .map(|r| r.message.clone())
        .collect();
    assert!(
        rejections
            .iter()
            .all(|m| m.as_deref() == Some("app_name does not match the run's owner")),
        "one rejection rule on both paths: {rejections:?}"
    );
    assert!(rejections.len() >= 2, "both the claim and the file reply were rejected: {rejections:?}");
}

/// R12 at the socket: a stream exceeding 64 KB without a newline is
/// disconnected with bounded memory, and another client is still serviced.
#[test]
fn oversize_frame_disconnects_peer_others_unaffected() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle);

    let t = owned_timer(&e, &mut st, "t-r12", TransportMode::Ipc);
    let mut noisy = connect(&e);
    assert_eq!(noisy.claim(t.id, APP)["ok"], json!(true));
    let mut quiet_client = connect(&e);
    assert_eq!(quiet_client.claim(t.id, APP)["ok"], json!(true));

    let run = fire(&e, &mut st, &eng, &t);
    let _ = quiet_client.fire_for(run);
    let _ = noisy.fire_for(run);

    // 256 KB without a newline — the peer is disconnected; Bellman retains
    // at most the 64 KB cap, never the unbounded prefix. (The write itself
    // may EPIPE once the server hangs up — that is the point.)
    let junk = vec![b'x'; 256 * 1024];
    for chunk in junk.chunks(16 * 1024) {
        if noisy.stream.write_all(chunk).is_err() {
            break;
        }
    }
    let mut buf = [0u8; 16];
    let deadline = Instant::now() + Duration::from_secs(5);
    let closed = loop {
        match noisy.reader.read(&mut buf) {
            Ok(0) => break true,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "oversize peer not disconnected");
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break true,
        }
    };
    assert!(closed, "oversize peer disconnected");

    // Another client on the same socket is still fully serviceable.
    quiet_client.send(&reply_json(run));
    wait_row(&st, run, Duration::from_secs(5), |r| r.state == "acknowledged");
}

/// Platform rules: private directory/socket permissions, refusal to unlink
/// a non-socket or symlink, safe recovery of a genuinely stale owned
/// socket, and the server-instance lock.
#[test]
fn socket_permissions_stale_recovery_and_instance_lock() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let e = env();
    let st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));

    // Refuse a non-socket at the configured path — never unlink it.
    std::fs::create_dir_all(e.sock.parent().unwrap()).unwrap();
    std::fs::write(&e.sock, b"not a socket").unwrap();
    let err = spawn_server_expect_err(&e, eng.clone(), &handle);
    assert!(std::fs::read(&e.sock).unwrap() == b"not a socket", "file untouched: {err}");

    // Refuse a symlink at the configured path — never follow/unlink it.
    std::fs::remove_file(&e.sock).unwrap();
    let target = e.data.join("elsewhere");
    std::fs::write(&target, b"x").unwrap();
    std::os::unix::fs::symlink(&target, &e.sock).unwrap();
    let _ = spawn_server_expect_err(&e, eng.clone(), &handle);
    assert!(std::fs::symlink_metadata(&e.sock).unwrap().file_type().is_symlink(), "symlink untouched");
    std::fs::remove_file(&e.sock).unwrap();

    // A genuinely stale owned socket (server died without cleanup) is
    // recovered: bind + drop leaves the file; the next spawn replaces it.
    {
        let _listener = std::os::unix::net::UnixListener::bind(&e.sock).unwrap();
    }
    assert!(std::fs::symlink_metadata(&e.sock).unwrap().file_type().is_socket());
    let server = spawn_server(&e, eng.clone(), &handle);

    // Private permissions: directory 0700, socket 0600.
    let dir_mode = std::fs::metadata(e.sock.parent().unwrap()).unwrap().permissions().mode() & 0o777;
    let sock_mode = std::fs::metadata(&e.sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "private socket dir");
    assert_eq!(sock_mode, 0o600, "owner-only socket");

    // The server-instance lock refuses a second live server on the path.
    let err = spawn_server_expect_err(&e, eng.clone(), &handle);
    assert!(
        err.kind() == std::io::ErrorKind::AlreadyExists || err.to_string().contains("another Bellman IPC server"),
        "second server refused: {err}"
    );
    server.stop();
    drop(st);
}

fn spawn_server_expect_err(e: &Env, eng: ReplyEngine, handle: &IpcHandle) -> std::io::Error {
    IpcServer::spawn(IpcConfig {
        socket_path: e.sock.clone(),
        data_dir: e.data.clone(),
        db_path: e.db.clone(),
        engine: eng,
        handle: handle.clone(),
    })
    .map(|_| ())
    .expect_err("spawn must fail")
}

/// timer.json carries the transport mode and — while the server is up —
/// the `"ipc": { "socket": … }` data field. Connection info is data, never
/// code: no generated adapter exists in any timer folder, asserted.
#[test]
fn timer_json_advertises_socket_and_no_adapter_py_exists() {
    use std::os::unix::fs::PermissionsExt;

    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle);

    let t = owned_timer(&e, &mut st, "t-advertised", TransportMode::Auto);
    let run = fire(&e, &mut st, &eng, &t);
    let timer_json: Value = serde_json::from_str(
        &std::fs::read_to_string(folder(&e, t.id).join("timer.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(timer_json["transport"]["mode"], json!("auto"));
    assert!(
        timer_json["ipc"]["socket"].as_str().unwrap().ends_with("bellman.sock"),
        "timer.json advertises the socket as data: {timer_json}"
    );

    // No adapter.py — and no generated executable of any kind — exists in
    // any timer folder, so it cannot quietly return.
    let timers_root = e.data.join("timers");
    for entry in std::fs::read_dir(&timers_root).unwrap().flatten() {
        let name = entry.file_name();
        assert_ne!(name.to_string_lossy(), "adapter.py");
        if entry.path().is_dir() {
            for f in std::fs::read_dir(entry.path()).unwrap().flatten() {
                let fname = f.file_name();
                let fname = fname.to_string_lossy();
                assert!(!fname.ends_with(".py"), "no generated python in timer folders: {fname}");
                assert_ne!(fname, "adapter.py", "no generated adapter: {fname}");
                let mode = f.metadata().unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0, "nothing executable in timer folders: {fname} ({mode:o})");
            }
        }
    }
    let _ = run;
}

/// Regression: a JSON-only app speaking the IK4 file protocol (the
/// lightbulb flow, unmodified) passes its full cycle with IPC enabled
/// globally — it never touches the socket and nothing about its world
/// changes.
#[test]
fn json_only_app_flow_unaffected_with_ipc_enabled() {
    let e = env();
    let mut st = store(&e);
    let handle = IpcHandle::new(e.sock.clone());
    let eng = engine(&e, Some(&handle), Duration::from_secs(60));
    let _server = spawn_server(&e, eng.clone(), &handle); // IPC "enabled globally"

    let t = owned_timer(&e, &mut st, "lightbulb", TransportMode::Json);
    let run = fire(&e, &mut st, &eng, &t);

    // The lightbulb protocol: scan slots/fires, dedupe by run_id, take
    // reply_path verbatim from the notification, write acknowledged then
    // completed with a result — never constructs a path.
    let fire_file = e.data.join("slots/fires").join(reply::fire_notification_name(run));
    let notif: Value = serde_json::from_str(&std::fs::read_to_string(&fire_file).unwrap()).unwrap();
    let reply_path = PathBuf::from(notif["reply_path"].as_str().expect("reply_path present"));
    assert!(reply_path.exists(), "the stub the notification names exists");

    let mut reply_doc: Value = serde_json::from_str(&std::fs::read_to_string(&reply_path).unwrap()).unwrap();
    reply_doc["state"] = json!("acknowledged");
    reply_doc["acknowledged_at"] = json!(Utc::now());
    reply_doc["expected_secs"] = json!(15);
    let tmp = reply_path.with_extension("json.tmp");
    std::fs::write(&tmp, reply_doc.to_string()).unwrap();
    std::fs::rename(&tmp, &reply_path).unwrap();

    let mut tracker = reply::InvalidTracker::default();
    reply::poll_once(&eng, &st, Utc::now(), Instant::now(), &mut tracker);
    wait_row(&st, run, Duration::from_secs(2), |r| r.state == "acknowledged");

    let mut reply_doc: Value = serde_json::from_str(&std::fs::read_to_string(&reply_path).unwrap()).unwrap();
    reply_doc["state"] = json!("completed");
    reply_doc["completed_at"] = json!(Utc::now());
    reply_doc["result"] = json!({"on_duration_secs": 15});
    let tmp = reply_path.with_extension("json.tmp");
    std::fs::write(&tmp, reply_doc.to_string()).unwrap();
    std::fs::rename(&tmp, &reply_path).unwrap();
    reply::poll_once(&eng, &st, Utc::now(), Instant::now(), &mut tracker);

    let row = wait_row(&st, run, Duration::from_secs(2), |r| r.state == "completed");
    assert_eq!(row.result_json, Some(json!({"on_duration_secs": 15})));
    assert_eq!(row.transport.as_deref(), Some("json"));
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(folder(&e, t.id).join(STATUS_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(status["state"], json!("completed"));
    assert_eq!(status["result"], json!({"on_duration_secs": 15}));
}
