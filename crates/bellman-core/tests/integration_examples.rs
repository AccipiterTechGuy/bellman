//! Run the INTEGRATION.md client examples against a live slots dir.
//!
//! Acceptance: python3 + bash (minimum) each land a timer. The Python snippet
//! exercised here matches docs/INTEGRATION.md (optional BELLMAN_DB, check_call).

use bellman_core::slots::{SlotConfig, SlotService, MIN_FREE_SLOTS};
use bellman_core::store::{OpenOptions, Store};
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn bellman_bin() -> PathBuf {
    let root = workspace_root();
    let status = Command::new("cargo")
        .args(["build", "-p", "bellman-cli", "-q"])
        .current_dir(&root)
        .status()
        .expect("cargo build");
    assert!(status.success(), "cargo build -p bellman-cli failed");
    let debug = root.join("target/debug/bellman");
    assert!(debug.exists(), "missing binary at {}", debug.display());
    debug
}

fn open_live_slots() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("timers.db");
    let slots = dir.path().join("slots");
    let _store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .expect("store");
    let service = SlotService::open(&slots, SlotConfig::default()).expect("slots");
    assert!(
        service.free_count().unwrap() >= MIN_FREE_SLOTS,
        "need free stubs"
    );
    (dir, db, slots)
}

fn timer_count(db: &Path) -> usize {
    let store = Store::open_with(
        db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )
    .expect("reopen");
    store.list_timers().expect("list").len()
}

/// PATH that prefers our just-built `bellman` binary.
fn path_with_bellman(bin: &Path) -> String {
    let dir = bin.parent().unwrap();
    let old = std::env::var("PATH").unwrap_or_default();
    format!("{}:{}", dir.display(), old)
}

#[test]
fn integration_bash_example_lands_timer() {
    let bin = bellman_bin();
    let (_dir, db, slots) = open_live_slots();
    let before = timer_count(&db);

    // Mirrors docs/INTEGRATION.md bash snippet (optional BELLMAN_DB).
    let script = r#"
set -euo pipefail
REQ=$(mktemp)
cat >"$REQ" <<EOF
{"schema":"bellman-slot/1","request_id":"$(uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid)","operation":"add",
 "payload":{"app_name":"demo-app","timer_name":"demo-wake-bash","tz":"UTC",
 "occurrence":{"kind":"interval","every_secs":60}}}
EOF
args=(slot-submit "$REQ" --slots "${BELLMAN_SLOTS}")
[[ -n "${BELLMAN_DB:-}" ]] && args+=(--db "$BELLMAN_DB")
bellman "${args[@]}" --json
"#;

    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("BELLMAN_SLOTS", &slots)
        .env("BELLMAN_DB", &db)
        .env("PATH", path_with_bellman(&bin))
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "bash example failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(timer_count(&db), before + 1, "bash must land a timer");
}

#[test]
fn integration_python3_example_lands_timer() {
    let bin = bellman_bin();
    let (_dir, db, slots) = open_live_slots();
    let before = timer_count(&db);

    // Exact control flow from docs/INTEGRATION.md Python snippet:
    // optional BELLMAN_DB, subprocess.check_call (not os.system).
    let py = r#"
import json, os, uuid, pathlib, subprocess
root, db = os.environ["BELLMAN_SLOTS"], os.environ.get("BELLMAN_DB")
req = {"schema":"bellman-slot/1","request_id":str(uuid.uuid4()),"operation":"add",
  "payload":{"app_name":"demo-app","timer_name":"demo-wake-py","tz":"UTC",
  "occurrence":{"kind":"interval","every_secs":60}}}
pathlib.Path("/tmp/bellman-req.json").write_text(json.dumps(req))
cmd = ["bellman","slot-submit","/tmp/bellman-req.json","--slots",root]
if db: cmd += ["--db", db]
subprocess.check_call(cmd)
"#;

    let out = Command::new("python3")
        .arg("-c")
        .arg(py)
        .env("BELLMAN_SLOTS", &slots)
        .env("BELLMAN_DB", &db)
        .env("PATH", path_with_bellman(&bin))
        .output()
        .expect("python3");
    assert!(
        out.status.success(),
        "python3 example failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(timer_count(&db), before + 1, "python3 must land a timer");
}

#[test]
fn integration_python3_works_without_bellman_db_env() {
    // Auditor REPRO: BELLMAN_SLOTS only; omit BELLMAN_DB. Doc uses CLI default
    // db path (~/.bellman/timers.db) — here we point HOME at the tempdir so
    // the default lands under our harness without polluting the real home.
    let bin = bellman_bin();
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".bellman")).unwrap();
    let slots = dir.path().join("slots");
    let _ = SlotService::open(&slots, SlotConfig::default()).expect("slots");

    let py = r#"
import json, os, uuid, pathlib, subprocess
root, db = os.environ["BELLMAN_SLOTS"], os.environ.get("BELLMAN_DB")
req = {"schema":"bellman-slot/1","request_id":str(uuid.uuid4()),"operation":"add",
  "payload":{"app_name":"demo-app","timer_name":"demo-wake-py-default-db","tz":"UTC",
  "occurrence":{"kind":"interval","every_secs":60}}}
pathlib.Path("/tmp/bellman-req-def.json").write_text(json.dumps(req))
cmd = ["bellman","slot-submit","/tmp/bellman-req-def.json","--slots",root]
if db: cmd += ["--db", db]
subprocess.check_call(cmd)
"#;

    let out = Command::new("python3")
        .arg("-c")
        .arg(py)
        .env("BELLMAN_SLOTS", &slots)
        .env_remove("BELLMAN_DB")
        .env("HOME", &home)
        .env("PATH", path_with_bellman(&bin))
        .output()
        .expect("python3");
    assert!(
        out.status.success(),
        "python without BELLMAN_DB failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let db = home.join(".bellman").join("timers.db");
    assert!(db.exists(), "default db must be created");
    assert_eq!(timer_count(&db), 1, "timer must land in default db");
}

#[test]
fn integration_node_example_lands_timer_when_available() {
    let node_ok = Command::new("node")
        .arg("-v")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !node_ok {
        eprintln!("skip: node not on PATH");
        return;
    }
    let bin = bellman_bin();
    let (_dir, db, slots) = open_live_slots();
    let before = timer_count(&db);

    let js = r#"
const fs = require("fs"), {execFileSync} = require("child_process"), {randomUUID} = require("crypto");
const f = "/tmp/bellman-req-node.json";
fs.writeFileSync(f, JSON.stringify({schema:"bellman-slot/1", request_id:randomUUID(),
  operation:"add", payload:{app_name:"demo-app", timer_name:"demo-wake-node", tz:"UTC",
  occurrence:{kind:"interval", every_secs:60}}}));
const args = ["slot-submit", f, "--slots", process.env.BELLMAN_SLOTS];
if (process.env.BELLMAN_DB) args.push("--db", process.env.BELLMAN_DB);
execFileSync("bellman", args, {stdio:"inherit"});
"#;

    let out = Command::new("node")
        .arg("-e")
        .arg(js)
        .env("BELLMAN_SLOTS", &slots)
        .env("BELLMAN_DB", &db)
        .env("PATH", path_with_bellman(&bin))
        .output()
        .expect("node");
    assert!(
        out.status.success(),
        "node example failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(timer_count(&db), before + 1, "node must land a timer");
}
