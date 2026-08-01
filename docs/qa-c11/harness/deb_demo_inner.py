#!/usr/bin/env python3
"""C11 §4 — the installed .deb's own shipped demo, run from the path the
first-run wizard names, woken by a scheduled fire.

Nothing from the source tree is used: `bellman-app`, `bellman` and
`/usr/share/bellman/testing_apps/lightbulb/lightbulb.py` all come out of the
package that `sudo apt install ./Bellman_*.deb` put on this machine.
"""
import json
import os
import subprocess
import sys
import time
import uuid
from datetime import datetime, timedelta
from pathlib import Path

DEMO = Path("/usr/share/bellman/testing_apps/lightbulb/lightbulb.py")
ROOT = Path("/deb")
XDG = ROOT / "xdg"
APPDATA = XDG / "io.bellman.desktop"
RT = ROOT / "run"
for d in (XDG, RT, ROOT / "cfg", ROOT / "cache", APPDATA):
    d.mkdir(parents=True, exist_ok=True)
RT.chmod(0o700)
(APPDATA / "config.json").write_text(json.dumps({"wizard_completed": True}))

env = dict(os.environ)
env.update(DISPLAY=":99", XDG_DATA_HOME=str(XDG), XDG_CONFIG_HOME=str(ROOT / "cfg"),
           XDG_RUNTIME_DIR=str(RT), XDG_CACHE_HOME=str(ROOT / "cache"),
           HOME=str(ROOT), GDK_BACKEND="x11", GIO_USE_VFS="local",
           GTK_USE_PORTAL="0", LIBGL_ALWAYS_SOFTWARE="1",
           WEBKIT_DISABLE_COMPOSITING_MODE="1", TERM="xterm")
env.pop("DBUS_SESSION_BUS_ADDRESS", None)

out = {"installed_paths": {}}
for exe in ("bellman", "bellman-app"):
    p = subprocess.run(["command", "-v", exe], capture_output=True, text=True,
                       shell=False, executable="/bin/bash")
    which = subprocess.run(["bash", "-lc", f"command -v {exe}"],
                           capture_output=True, text=True)
    out["installed_paths"][exe] = which.stdout.strip()
out["demo_path"] = str(DEMO)
out["demo_present"] = DEMO.exists()
out["desktop_entry"] = sorted(
    p.name for p in Path("/usr/share/applications").glob("*ellman*"))
print("installed:", json.dumps(out), flush=True)
if not DEMO.exists():
    print("FAIL: the package did not ship the demo the wizard names", flush=True)
    sys.exit(2)

subprocess.Popen(["Xvfb", ":99", "-screen", "0", "1024x768x24", "-ac",
                  "-nolisten", "tcp"], stdout=subprocess.DEVNULL,
                 stderr=subprocess.DEVNULL)
time.sleep(2)

applog = open(ROOT / "app.log", "wb")
subprocess.Popen(["dbus-run-session", "--", "/usr/bin/bellman-app"], env=env,
                 stdout=applog, stderr=applog, start_new_session=True)
db = APPDATA / "timers.db"
for _ in range(1200):
    if db.exists():
        break
    time.sleep(0.2)
if not db.exists():
    print("FAIL: the packaged bellman-app never created its store", flush=True)
    print(open(ROOT / "app.log", errors="replace").read()[-2000:], flush=True)
    sys.exit(2)
time.sleep(3)
print("packaged bellman-app running; data dir", APPDATA, flush=True)

# The demo claims its own timer through the slot protocol, exactly as its
# README says — Bellman never creates it.
fire_at = (datetime.now() + timedelta(seconds=100)).strftime("%Y-%m-%dT%H:%M:%S")
req = {"schema": "bellman-slot/1", "request_id": str(uuid.uuid4()),
       "operation": "add",
       "payload": {"app_name": "lightbulb", "timer_name": "lightbulb-demo",
                   "occurrence": {"kind": "once", "time": fire_at}}}
f = ROOT / "req.json"
f.write_text(json.dumps(req))
p = subprocess.run(["/usr/bin/bellman", "slot-submit", str(f),
                    "--slots", str(APPDATA / "slots"), "--db", str(db), "--json"],
                   capture_output=True, text=True, env=env)
resp = json.loads(p.stdout.strip().splitlines()[-1])
out["slot_response"] = resp
print("timer registered by the demo's app_name:", resp.get("next_fire_at"), flush=True)

demo_log = open(ROOT / "demo.out", "wb")
demo = subprocess.Popen([sys.executable, str(DEMO), "--slots",
                         str(APPDATA / "slots"), "--on-secs", "4"],
                        stdout=demo_log, stderr=demo_log, env=env)
print("started the PACKAGED demo:", DEMO, flush=True)

events = APPDATA / "logs" / "events.current.jsonl"
status = None
end = time.monotonic() + 300
while time.monotonic() < end:
    for d in (APPDATA / "timers").glob("lightbulb-demo-*"):
        sp = d / "status.json"
        if sp.exists():
            try:
                status = json.loads(sp.read_text())
            except ValueError:
                status = None
    if status and status.get("state") == "completed":
        break
    time.sleep(2)

kinds = []
if events.exists():
    for ln in events.read_text(errors="replace").splitlines():
        try:
            e = json.loads(ln)
        except ValueError:
            continue
        if e.get("timer_name") == "lightbulb-demo":
            kinds.append(e["kind"])

folder = None
for d in (APPDATA / "timers").glob("lightbulb-demo-*"):
    folder = d
out["timer_folder"] = str(folder) if folder else None
out["timer_folder_listing"] = sorted(p.name for p in folder.iterdir()) if folder else []
out["reply_files"] = {p.name: p.read_text(errors="replace")[:900]
                      for p in (folder.glob("reply-*.json") if folder else [])}
out["fires_listing"] = sorted(p.name for p in (APPDATA / "slots" / "fires").glob("*"))
out["fire_docs"] = {p.name: json.loads(p.read_text())
                    for p in (APPDATA / "slots" / "fires").glob("fire-*.json")}
out["events_raw"] = (events.read_text(errors="replace").splitlines()[-25:]
                     if events.exists() else [])
out["publisher_health"] = (APPDATA / "logs" / "publisher_health.json").read_text() \
    if (APPDATA / "logs" / "publisher_health.json").exists() else None
out["app_log_tail"] = open(ROOT / "app.log", errors="replace").read()[-3000:]
out["bad_dir"] = sorted(p.name for p in (APPDATA / "timers" / "bad").glob("*")) \
    if (APPDATA / "timers" / "bad").exists() else []
out["status"] = status
out["event_kinds"] = kinds
out["demo_stdout_tail"] = open(ROOT / "demo.out", errors="replace").read()[-800:]
Path("/deb/result.json").write_text(json.dumps(out, indent=2, default=str))
print("RESULT", json.dumps(out, indent=2, default=str), flush=True)
demo.terminate()
sys.exit(0 if status and status.get("state") == "completed" else 1)
