//! Run the INTEGRATION.md client examples against a live slots dir.
//!
//! Acceptance: python3 + bash (minimum) each land a timer. Node is exercised
//! when `node` is on PATH; PowerShell is documented but not required on Linux.

use bellman_core::slots::{SlotConfig, SlotService, MIN_FREE_SLOTS};
use bellman_core::store::{OpenOptions, Store};
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/bellman-core
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn bellman_bin() -> PathBuf {
    // Always rebuild so the CLI matches this worktree's sources (slot_id
    // defaults, etc.). Quiet + incremental keeps this cheap.
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

#[test]
fn integration_bash_example_lands_timer() {
    let bin = bellman_bin();
    let (_dir, db, slots) = open_live_slots();
    let before = timer_count(&db);

    let script = format!(
        r#"
set -euo pipefail
export BELLMAN_SLOTS="{slots}"
export BELLMAN_DB="{db}"
REQ=$(mktemp)
# uuidgen may be missing — fall back to /proc
RID=$(uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid)
cat >"$REQ" <<EOF
{{"schema":"bellman-slot/1","request_id":"$RID","operation":"add",
 "payload":{{"app_name":"demo-app","timer_name":"demo-wake-bash","tz":"UTC",
 "occurrence":{{"kind":"interval","every_secs":60}}}}}}
EOF
"{bin}" slot-submit "$REQ" --slots "$BELLMAN_SLOTS" --db "$BELLMAN_DB" --json
"#,
        slots = slots.display(),
        db = db.display(),
        bin = bin.display(),
    );

    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "bash example failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"ok\":true") || stdout.contains("status=ok") || stdout.contains("\"status\":\"ok\""),
        "unexpected stdout: {stdout}"
    );
    assert_eq!(timer_count(&db), before + 1, "bash must land a timer");
}

#[test]
fn integration_python3_example_lands_timer() {
    let bin = bellman_bin();
    let (_dir, db, slots) = open_live_slots();
    let before = timer_count(&db);

    // Mirrors docs/INTEGRATION.md Python snippet (CLI helper path).
    let py = format!(
        r#"
import json, os, uuid, pathlib, subprocess, sys
root = pathlib.Path({slots:?})
db = {db:?}
bin = {bin:?}
req = {{"schema":"bellman-slot/1","slot_id":"","request_id":str(uuid.uuid4()),
       "operation":"add","payload":{{"app_name":"demo-app","timer_name":"demo-wake-py",
       "tz":"UTC","occurrence":{{"kind":"interval","every_secs":60}}}}}}
path = pathlib.Path("/tmp/bellman-req-py.json")
path.write_text(json.dumps(req))
r = subprocess.run([bin, "slot-submit", str(path), "--slots", str(root), "--db", db, "--json"],
                   capture_output=True, text=True)
print(r.stdout)
print(r.stderr, file=sys.stderr)
sys.exit(r.returncode)
"#,
        slots = slots.to_string_lossy(),
        db = db.to_string_lossy(),
        bin = bin.to_string_lossy(),
    );

    let out = Command::new("python3")
        .arg("-c")
        .arg(&py)
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

    let js = format!(
        r#"
const fs = require("fs"), {{execFileSync}} = require("child_process"), {{randomUUID}} = require("crypto");
const f = "/tmp/bellman-req-node.json";
fs.writeFileSync(f, JSON.stringify({{schema:"bellman-slot/1", request_id:randomUUID(),
  operation:"add", payload:{{app_name:"demo-app", timer_name:"demo-wake-node", tz:"UTC",
  occurrence:{{kind:"interval", every_secs:60}}}}}}));
const out = execFileSync({bin:?}, ["slot-submit", f, "--slots", {slots:?},
  "--db", {db:?}, "--json"], {{encoding:"utf8"}});
process.stdout.write(out);
"#,
        bin = bin.to_string_lossy(),
        slots = slots.to_string_lossy(),
        db = db.to_string_lossy(),
    );

    let out = Command::new("node")
        .arg("-e")
        .arg(&js)
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
