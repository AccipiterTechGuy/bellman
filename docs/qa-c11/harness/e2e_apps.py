#!/usr/bin/env python3
"""C11 §3 — real applications woken by a scheduled fire.

  1. testing_apps/lightbulb/lightbulb.py, unmodified, run exactly as its
     README says (minus step 3's run-now: here the clock does the firing).
  2. clock_in.pl — a client in a language docs/INTEGRATION.md does not cover,
     written from that document alone.
  3. The same reply over BOTH transports: one timer on files, one on the local
     IPC socket, answered with an identical bellman-reply/1 document, then the
     resulting state / log lines / status.json compared field by field.
"""
import json
import os
import socket
import subprocess
import sys
from pathlib import Path
import threading
import time
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

# The harness lives next to the evidence it produced; importing the shared
# helper from beside this file keeps every script runnable from anywhere.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_lib import REPO, Run, say, stamp  # noqa: E402

TZ = "Europe/Helsinki"
EV = {}


class IpcClient(threading.Thread):
    """The minimal IPC client from INTEGRATION.md, with a reply payload
    identical to the one the file-transport timer receives."""

    def __init__(self, sock_path, app_name, timer_id, result):
        super().__init__(daemon=True)
        self.sock_path, self.app_name = sock_path, app_name
        self.timer_id, self.result = timer_id, result
        self.claim_ok = None
        self.frames = []
        self.sent = []
        self.error = None
        self.stop_flag = False

    def run(self):
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(300)
            s.connect(self.sock_path)
            f = s.makefile("rw")
            f.write(json.dumps({"schema": "bellman-claim/1",
                                "app_name": self.app_name,
                                "timer_id": self.timer_id}) + "\n")
            f.flush()
            ack = json.loads(f.readline())
            self.claim_ok = ack
            if not ack.get("ok"):
                return
            seen = set()
            for line in f:
                if self.stop_flag:
                    break
                fire = json.loads(line)
                self.frames.append(fire)
                rid = fire.get("run_id")
                if not rid or rid in seen:
                    continue
                seen.add(rid)
                for doc in ({"schema": "bellman-reply/1", "run_id": rid,
                             "app_name": self.app_name, "state": "acknowledged",
                             "acknowledged_at": stamp(), "expected_secs": 3},
                            {"schema": "bellman-reply/1", "run_id": rid,
                             "app_name": self.app_name, "state": "completed",
                             "completed_at": stamp(), "result": self.result}):
                    f.write(json.dumps(doc) + "\n")
                    f.flush()
                    self.sent.append(doc)
                    time.sleep(1)
        except Exception as e:      # noqa: BLE001 - recorded as evidence
            self.error = repr(e)


def file_answer(reply_path, result):
    """The same two documents, written the file way."""
    sent = []
    for fields in ({"state": "acknowledged", "acknowledged_at": stamp(),
                    "expected_secs": 3},
                   {"state": "completed", "completed_at": stamp(),
                    "result": result}):
        r = json.load(open(reply_path))
        r.update(fields)
        tmp = reply_path + ".tmp"
        json.dump(r, open(tmp, "w"), indent=2)
        os.replace(tmp, reply_path)
        sent.append(dict(fields))
        time.sleep(1)
    return sent


def main():
    run = Run("apps", display=":92").fresh()
    run.start_app()
    say("data dir:", run.appdata, "(shipped config.json defaults, untouched)")

    local = ZoneInfo(TZ)
    base = datetime.now(local)

    def at(sec):
        return (base + timedelta(seconds=sec)).strftime("%Y-%m-%dT%H:%M:%S")

    RESULT = {"ok": True, "note": "identical payload on both transports"}

    specs = {
        "lightbulb-demo": ("lightbulb", {"kind": "once", "time": at(120)}, None),
        "clockin-demo":   ("clock-in",  {"kind": "once", "time": at(130)}, None),
        "twin-json":      ("twin-app",  {"kind": "once", "time": at(140)},
                           {"mode": "json"}),
        "twin-ipc":       ("twin-app",  {"kind": "once", "time": at(150)},
                           {"mode": "ipc"}),
    }
    created = {}
    for name, (app_name, occ, transport) in specs.items():
        payload = {"app_name": app_name, "timer_name": name, "tz": TZ,
                   "occurrence": occ}
        if transport:
            payload["transport"] = transport
        resp = run.submit(payload)
        r = resp.get("response", resp)
        created[name] = {"app_name": app_name, "occurrence": occ,
                         "transport": transport, "timer_id": r["timer_id"],
                         "next_fire_at": r["next_fire_at"]}
        say(f"submitted {name:15s} owner={app_name:10s} "
            f"transport={transport} next_fire_at={r['next_fire_at']}")

    # --- the two reference clients, started before their fires -------------
    lb_log = open(run.root / "lightbulb.out", "wb")
    lightbulb = subprocess.Popen(
        [sys.executable, str(REPO / "testing_apps" / "lightbulb" / "lightbulb.py"),
         "--slots", str(run.slots), "--on-secs", "4"],
        stdout=lb_log, stderr=lb_log, env=run.env())
    say("started testing_apps/lightbulb/lightbulb.py (unmodified)")

    pl_log = open(run.root / "clock_in.out", "wb")
    perl = subprocess.Popen(
        ["perl", str(Path(__file__).resolve().parent.parent / "clock_in.pl"),
         str(run.slots), "clock-in", "2"],
        stdout=pl_log, stderr=pl_log, env=run.env())
    say("started clock_in.pl (perl, written from INTEGRATION.md only)")

    sock_path = run.rt / "bellman" / "bellman.sock"
    for _ in range(120):
        if sock_path.exists():
            break
        time.sleep(0.5)
    EV["ipc_socket"] = str(sock_path)
    EV["ipc_socket_exists"] = sock_path.exists()
    EV["ipc_socket_mode"] = oct(sock_path.stat().st_mode & 0o777) if sock_path.exists() else None
    EV["ipc_dir_mode"] = oct(sock_path.parent.stat().st_mode & 0o777) if sock_path.exists() else None
    say("ipc socket:", sock_path, "exists=", sock_path.exists(),
        "mode=", EV["ipc_socket_mode"], "dir=", EV["ipc_dir_mode"])

    ipc = IpcClient(str(sock_path), "twin-app", created["twin-ipc"]["timer_id"], RESULT)
    ipc.start()
    time.sleep(2)
    say("ipc claim:", ipc.claim_ok)

    # --- wait for the clock ------------------------------------------------
    done = set()
    handled = set()
    json_twin_sent = None
    end = time.monotonic() + 420
    while time.monotonic() < end:
        for p in sorted((run.slots / "fires").glob("fire-*.json")):
            try:
                doc = json.loads(p.read_text())
            except (OSError, ValueError):
                continue
            if doc["run_id"] in handled:
                continue
            handled.add(doc["run_id"])
            say(f"FIRE {doc['timer_name']} run_id={doc['run_id']}")
            if doc["timer_name"] == "twin-json":
                json_twin_sent = file_answer(doc["reply_path"], RESULT)
        for nm in specs:
            st = run.status(nm)
            if st and st.get("state") in ("completed", "failed", "no_ack"):
                if nm not in done:
                    done.add(nm)
                    say(f"TERMINAL {nm}: {st['state']} transport={st.get('transport')}")
        if len(done) == len(specs):
            break
        time.sleep(1)

    time.sleep(3)
    lightbulb.terminate()
    perl.terminate()
    ipc.stop_flag = True

    statuses = {nm: run.status(nm) for nm in specs}
    log = [e for e in run.log_lines()
           if e.get("timer_name") in specs or e.get("kind") == "wake_capability"]

    def kinds_for(nm):
        return [e["kind"] for e in run.log_lines() if e.get("timer_name") == nm]

    EV.update(
        created=created,
        statuses=statuses,
        log=log,
        lightbulb_stdout=open(run.root / "lightbulb.out", errors="replace").read()[-4000:],
        clock_in_stdout=open(run.root / "clock_in.out", errors="replace").read()[-4000:],
        ipc_claim=ipc.claim_ok,
        ipc_frames=ipc.frames,
        ipc_sent=ipc.sent,
        ipc_error=ipc.error,
        json_twin_sent=json_twin_sent,
        twin_comparison={
            "json_state_kinds": kinds_for("twin-json"),
            "ipc_state_kinds": kinds_for("twin-ipc"),
            "json_status": statuses.get("twin-json"),
            "ipc_status": statuses.get("twin-ipc"),
        },
        reply_stub_present_for_ipc=(
            (run.timer_dir("twin-ipc") is not None) and
            [p.name for p in run.timer_dir("twin-ipc").glob("reply-*.json")]
        ),
        timer_dir_listing={nm: sorted(p.name for p in (run.timer_dir(nm) or []).iterdir())
                           if run.timer_dir(nm) else None for nm in specs},
        missing=[nm for nm in specs if nm not in done],
    )
    out = run.root / "apps_evidence.json"
    out.write_text(json.dumps(EV, indent=2, default=str))
    say("evidence ->", out)
    say("missing:", EV["missing"] or "none")
    run.stop()
    return 0 if not EV["missing"] else 1


if __name__ == "__main__":
    sys.exit(main())
