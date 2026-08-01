#!/usr/bin/env python3
"""C11 §3 — the reply channel under a live watcher, all from scheduled fires.

Six scenarios share one running desktop app and one wall clock:

  A  happy      a listening app answers -> acknowledged -> completed
  B  no_ack     nobody listens -> no_ack after the pickup grace; a late reply
                still revises the run to completed
  C  watchdog   error_detection + expected_secs -> failed/timed_out at x factor,
                and the app's reply file is left byte-identical
  D  heartbeat  the same opt-in watchdog, kept alive by heartbeats, completes
  E  rejects    malformed bytes / wrong app_name / unknown run_id / oversize
                -> reply_rejected + a copy in timers/bad/ + the live file intact
  F  stale      a reply to a run the timer has already superseded

Nothing here calls run-now.
"""
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path
import time
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

# The harness lives next to the evidence it produced; importing the shared
# helper from beside this file keeps every script runnable from anywhere.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_lib import Run, say, stamp, utcnow  # noqa: E402

TZ = "Europe/Helsinki"
EV = {}          # scenario -> evidence


def write_reply(path, _compose=None, **fields):
    """Exactly what INTEGRATION.md Step 2 says: read the stub, set what
    changed, temp-write, rename onto the same path.

    `_compose` covers the documented alternative: "If you compose instead of
    editing the stub, the minimal valid reply is schema + run_id + app_name +
    state" — needed when the stub is no longer on disk."""
    try:
        r = json.load(open(path))
    except (OSError, ValueError):
        if _compose is None:
            raise
        r = dict(_compose)
    r.update(fields)
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(r, fh, indent=2)
    os.replace(tmp, path)
    return r


def sha(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def main():
    run = Run("reply").fresh()
    run.start_app()
    run.seed_config()
    say("data dir:", run.appdata)
    say("config: pickup_grace_secs=60 watchdog_factor=2.0 (shipped defaults, untouched)")

    local = ZoneInfo(TZ)
    base = datetime.now(local)
    fire_at = (base + timedelta(seconds=100)).strftime("%Y-%m-%dT%H:%M:%S")

    timers = {
        "happy":     ("happy-app",  {"kind": "once", "time": fire_at}),
        "noack":     ("ghost-app",  {"kind": "once", "time": fire_at}),
        "watchdog":  ("wd-app",     {"kind": "once", "time": fire_at}),
        "heartbeat": ("hb-app",     {"kind": "once", "time": fire_at}),
        "rejects":   ("rej-app",    {"kind": "once", "time": fire_at}),
        # 60 s interval: run 1 gets superseded by run 2, then we answer run 1.
        "stale":     ("stale-app",  {"kind": "interval", "every_secs": 60}),
    }
    created = {}
    for key, (app_name, occ) in timers.items():
        resp = run.submit({"app_name": app_name, "timer_name": f"rc-{key}",
                           "tz": TZ, "occurrence": occ})
        r = resp.get("response", resp)
        created[key] = {"app_name": app_name, "occurrence": occ,
                        "timer_id": r["timer_id"], "next_fire_at": r["next_fire_at"]}
        say(f"submitted rc-{key:10s} owner={app_name:10s} next_fire_at={r['next_fire_at']}")

    fires_dir = run.slots / "fires"
    seen_runs = {}          # timer_name -> [fire docs in arrival order]
    state = {k: "waiting" for k in timers}
    handled = set()
    started = time.monotonic()
    stale_first_fire = None
    wd_reply_hash = None
    reject_log_marks = {}

    def newest_fire(key):
        docs = seen_runs.get(f"rc-{key}", [])
        return docs[-1] if docs else None

    say("waiting for the clock ...")
    while time.monotonic() - started < 420:
        for p in sorted(fires_dir.glob("fire-*.json")):
            try:
                doc = json.loads(p.read_text())
            except (OSError, ValueError):
                continue
            nm = doc.get("timer_name", "")
            rid = doc.get("run_id")
            if rid in handled:
                continue
            handled.add(rid)
            seen_runs.setdefault(nm, []).append(doc)
            say(f"FIRE {nm} run_id={rid} kind={doc.get('kind')} "
                f"transport-path={'reply_path' in doc}")

        # --- A: answer immediately, the way lightbulb.py does ---
        if state["happy"] == "waiting" and newest_fire("happy"):
            f = newest_fire("happy")
            write_reply(f["reply_path"], state="acknowledged",
                        acknowledged_at=stamp(), expected_secs=3)
            time.sleep(3)
            write_reply(f["reply_path"], state="completed", completed_at=stamp(),
                        result={"ok": True, "note": "answered from the fire notification"})
            state["happy"] = "done"
            say("A happy: acknowledged then completed")

        # --- B: deliberately silent; revive it after no_ack ---
        if state["noack"] == "waiting" and newest_fire("noack"):
            state["noack"] = "silent"
            EV["noack_fired_at"] = time.monotonic()
        if state["noack"] == "silent":
            ev = [e for e in run.log_lines()
                  if e.get("timer_name") == "rc-noack" and e.get("kind") == "no_ack"]
            if ev:
                say("B no_ack recorded:", json.dumps(ev[-1]))
                EV["noack_event"] = ev[-1]
                EV["noack_status_at_no_ack"] = run.status("rc-noack")
                f = newest_fire("noack")
                write_reply(f["reply_path"], state="completed", completed_at=stamp(),
                            result={"late": True})
                state["noack"] = "revising"
                say("B late reply written — expecting a revision to completed")
        if state["noack"] == "revising":
            st = run.status("rc-noack")
            if st and st.get("state") == "completed":
                EV["noack_status_after_late_reply"] = st
                state["noack"] = "done"
                say("B revision observed: no_ack -> completed")

        # --- C: opt into the watchdog and then go quiet ---
        if state["watchdog"] == "waiting" and newest_fire("watchdog"):
            f = newest_fire("watchdog")
            write_reply(f["reply_path"], state="running", acknowledged_at=stamp(),
                        expected_secs=5, error_detection=True)
            wd_reply_hash = sha(f["reply_path"])
            EV["watchdog_reply_sha_before"] = wd_reply_hash
            EV["watchdog_reply_body_before"] = open(f["reply_path"]).read()
            EV["watchdog_armed_at"] = stamp()
            state["watchdog"] = "silent"
            say("C watchdog armed: expected_secs=5, factor 2.0 -> deadline ~10 s")
        if state["watchdog"] == "silent":
            st = run.status("rc-watchdog")
            if st and st.get("state") == "failed":
                f = newest_fire("watchdog")
                EV["watchdog_status"] = st
                EV["watchdog_reply_sha_after"] = sha(f["reply_path"])
                EV["watchdog_reply_body_after"] = open(f["reply_path"]).read()
                state["watchdog"] = "done"
                say("C watchdog fired:", st.get("failure_kind"),
                    "reply byte-identical:",
                    EV["watchdog_reply_sha_after"] == wd_reply_hash)

        # --- D: same watchdog, kept alive by heartbeats ---
        if state["heartbeat"] == "waiting" and newest_fire("heartbeat"):
            f = newest_fire("heartbeat")
            write_reply(f["reply_path"], state="running", acknowledged_at=stamp(),
                        expected_secs=5, error_detection=True)
            EV["heartbeat_armed_at"] = stamp()
            for i in range(6):          # 6 x 3 s = 18 s, well past 5x2 = 10 s
                time.sleep(3)
                write_reply(f["reply_path"], state="running", heartbeat_at=stamp(),
                            progress=f"step {i + 1}/6")
            EV["heartbeat_status_mid"] = run.status("rc-heartbeat")
            write_reply(f["reply_path"], state="completed", completed_at=stamp(),
                        result={"heartbeats": 6})
            time.sleep(2)
            EV["heartbeat_status_final"] = run.status("rc-heartbeat")
            state["heartbeat"] = "done"
            say("D heartbeat: survived 18 s on a 10 s watchdog, then completed")

        # --- E: four rejections against a live watcher ---
        if state["rejects"] == "waiting" and newest_fire("rejects"):
            f = newest_fire("rejects")
            p = f["reply_path"]
            good = json.load(open(p))
            bad_dir = run.timers_dir / "bad"
            cases = {}

            def do_case(name, body_text):
                mark = run.log_count()
                before = sorted(bad_dir.glob("*")) if bad_dir.exists() else []
                tmp = p + ".tmp"
                with open(tmp, "w") as fh:
                    fh.write(body_text)
                os.replace(tmp, p)
                # give the watcher its debounce + a rescan
                deadline = time.monotonic() + 45
                found = None
                while time.monotonic() < deadline:
                    for e in run.log_lines(since=mark):
                        if e.get("kind") == "reply_rejected":
                            found = e
                            break
                    if found:
                        break
                    time.sleep(0.5)
                after = sorted(bad_dir.glob("*")) if bad_dir.exists() else []
                cases[name] = {
                    "sent_bytes": len(body_text),
                    "reply_rejected_event": found,
                    "live_file_still_present": os.path.exists(p),
                    "live_file_bytes": os.path.getsize(p) if os.path.exists(p) else None,
                    "quarantine_new": [q.name for q in after if q not in before],
                }
                say(f"E {name}: rejected={bool(found)} "
                    f"quarantined={cases[name]['quarantine_new']} "
                    f"live_file={cases[name]['live_file_still_present']}")

            do_case("malformed_bytes", "{ this is not json at all ]]]")
            do_case("wrong_app_name",
                    json.dumps(dict(good, app_name="somebody-else", state="completed")))
            do_case("unknown_run_id",
                    json.dumps(dict(good, run_id="00000000-0000-4000-8000-000000000000",
                                    state="completed")))
            do_case("oversize_payload",
                    json.dumps(dict(good, state="completed",
                                    result={"blob": "x" * 70000})))
            do_case("reserved_state", json.dumps(dict(good, state="no_ack")))
            EV["reject_cases"] = cases
            EV["reject_quarantine_listing"] = sorted(
                q.name for q in bad_dir.glob("*")) if bad_dir.exists() else []
            state["rejects"] = "done"

        # --- F: stale reply to a superseded run ---
        docs = seen_runs.get("rc-stale", [])
        if state["stale"] == "waiting" and docs:
            stale_first_fire = docs[0]
            state["stale"] = "have-run-1"
            say("F captured run 1 of rc-stale:", stale_first_fire["run_id"])
        if state["stale"] == "have-run-1" and len(docs) >= 2:
            mark = run.log_count()
            EV["stale_stub_present_after_supersede"] = os.path.exists(
                stale_first_fire["reply_path"])
            write_reply(stale_first_fire["reply_path"],
                        _compose={"schema": "bellman-reply/1",
                                  "run_id": stale_first_fire["run_id"],
                                  "app_name": stale_first_fire["app_name"]},
                        state="completed",
                        completed_at=stamp(), result={"too": "late"})
            deadline = time.monotonic() + 45
            hit = None
            while time.monotonic() < deadline and not hit:
                for e in run.log_lines(since=mark):
                    if e.get("run_id") == stale_first_fire["run_id"] and \
                            e.get("kind") in ("superseded", "reply_rejected"):
                        hit = e
                        break
                time.sleep(0.5)
            EV["stale_run_id"] = stale_first_fire["run_id"]
            EV["stale_current_run_id"] = docs[1]["run_id"]
            EV["stale_event"] = hit
            EV["stale_status_after"] = run.status("rc-stale")
            state["stale"] = "done"
            say("F stale reply outcome:", json.dumps(hit))

        if all(v == "done" for v in state.values()):
            break
        time.sleep(1.0)

    say("final scenario states:", json.dumps(state))
    EV["scenario_states"] = state
    EV["created"] = created
    EV["status_files"] = {k: run.status(f"rc-{k}") for k in timers}
    EV["log"] = [e for e in run.log_lines() if str(e.get("timer_name", "")).startswith("rc-")]
    out = run.root / "reply_evidence.json"
    out.write_text(json.dumps(EV, indent=2, default=str))
    say("evidence ->", out)
    run.stop()
    return 0 if all(v == "done" for v in state.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
