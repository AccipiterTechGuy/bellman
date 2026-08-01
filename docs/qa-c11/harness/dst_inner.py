#!/usr/bin/env python3
"""DST behaviour against a real clock, inside a disposable container.

libfaketime places the whole process tree a couple of minutes before a real
Europe/Helsinki DST transition and then lets the container's own seconds carry
it through. Nothing is sped up, nothing is stepped, and the host clock is
never touched — the transition happens because time passes.

  argv[1] = "gap" | "fold"
  argv[2] = seconds to watch

gap  2027-03-28: local 03:00:00 EET becomes 04:00:00 EEST, so local 03:30
     never happens that day. `DstGapPolicy::FirstValidAfterGap` says the
     daily 03:30 timer must fire at the first valid instant after the gap —
     04:00:00 EEST = 01:00:00 UTC.
fold 2026-10-25: local 04:00:00 EEST becomes 03:00:00 EET, so local 03:02
     happens twice. `DstFoldPolicy::FirstOccurrence` says the daily 03:02
     timer must fire exactly ONCE, at the earlier instant (00:02:00 UTC),
     and must NOT fire again at 01:02:00 UTC an hour later.

A 5-minute interval timer rides along in both runs: elapsed-time schedules
are anchored in UTC and must be untouched by the offset change.
"""
import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path

MODE = sys.argv[1]
WATCH = int(sys.argv[2]) if len(sys.argv) > 2 else 900
# Local wall-clock start, a couple of minutes before the first interesting fire.
START = {
    "gap": "2027-03-28 02:55:00",    # EET, 00:55 UTC — gap opens at 03:00 local
    "fold": "2026-10-25 02:58:00",   # EEST (+0300, unambiguous), 23:58 UTC
}[MODE]
TARGET_TIME = {"gap": "03:30:00", "fold": "03:02:00"}[MODE]
CONTROL_TIME = {"gap": "02:58:00", "fold": "03:03:00"}[MODE]

ROOT = Path("/dst")
XDG = ROOT / "xdg"
APPDATA = XDG / "io.bellman.desktop"
RT = ROOT / "run"
for d in (XDG, RT, ROOT / "cfg", ROOT / "cache", APPDATA):
    d.mkdir(parents=True, exist_ok=True)
RT.chmod(0o700)
(APPDATA / "config.json").write_text(json.dumps({
    "wizard_completed": True, "autostart_enabled": False,
    "start_minimized": False, "wake_enabled": False,
    # Long pickup grace: no_ack noise is not the subject of this run.
    "pickup_grace_secs": 86400,
}))

env = dict(os.environ)
env.update(
    TZ="Europe/Helsinki",
    DISPLAY=":99",
    XDG_DATA_HOME=str(XDG),
    XDG_CONFIG_HOME=str(ROOT / "cfg"),
    XDG_RUNTIME_DIR=str(RT),
    XDG_CACHE_HOME=str(ROOT / "cache"),
    HOME=str(ROOT),
    GDK_BACKEND="x11",
    GIO_USE_VFS="local",
    GTK_USE_PORTAL="0",
    LIBGL_ALWAYS_SOFTWARE="1",
    WEBKIT_DISABLE_COMPOSITING_MODE="1",
)
env.pop("DBUS_SESSION_BUS_ADDRESS", None)
# libfaketime fakes CLOCK_MONOTONIC as well by default, and that stalls the
# scheduler's chunked sleeps outright — the loop never wakes and nothing
# fires (proved with a control run: same container, same binary, 60 s
# interval; no faketime fires at t=65 s, faketime-with-monotonic never does).
# Only the wall clock needs moving, so leave monotonic alone.
env["FAKETIME_DONT_FAKE_MONOTONIC"] = "1"
FAKE = ["faketime", START]


def fake_clock():
    p = subprocess.run(FAKE + ["date", "+%Y-%m-%d %H:%M:%S %Z%z"],
                       capture_output=True, text=True, env=env)
    return p.stdout.strip()


subprocess.Popen(["Xvfb", ":99", "-screen", "0", "1024x768x24", "-ac",
                  "-nolisten", "tcp"], stdout=subprocess.DEVNULL,
                 stderr=subprocess.DEVNULL)
time.sleep(2)

resolved = fake_clock()
print(f"mode={MODE} fake start={START} -> {resolved}", flush=True)
want_offset = {"gap": "+0200", "fold": "+0300"}[MODE]
if want_offset not in resolved:
    print(f"FAIL: start resolved to the wrong UTC offset (wanted {want_offset}); "
          f"the run would begin on the wrong side of the transition", flush=True)
    sys.exit(3)

applog = open(ROOT / "app.log", "wb")
app = subprocess.Popen(FAKE + ["dbus-run-session", "--", "/usr/bin/bellman-app"],
                       env=env, stdout=applog, stderr=applog,
                       start_new_session=True)
db = APPDATA / "timers.db"
for _ in range(1200):
    if db.exists():
        break
    time.sleep(0.2)
if not db.exists():
    print("FAIL: bellman-app never created the store", flush=True)
    print(open(ROOT / "app.log", errors="replace").read()[-3000:], flush=True)
    sys.exit(2)
time.sleep(2)
print("store ready; fake clock now", fake_clock(), flush=True)

created = {}
plan = {
    "dst-target": {"kind": "daily", "time": TARGET_TIME},
    "dst-control-daily": {"kind": "daily", "time": CONTROL_TIME},
    "dst-control-interval": {"kind": "interval", "every_secs": 300},
}
for name, occ in plan.items():
    req = {"schema": "bellman-slot/1", "request_id": str(uuid.uuid4()),
           "operation": "add",
           "payload": {"app_name": "dst-app", "timer_name": name,
                       "tz": "Europe/Helsinki", "occurrence": occ}}
    f = ROOT / f"req-{name}.json"
    f.write_text(json.dumps(req))
    p = subprocess.run(FAKE + ["/usr/bin/bellman", "slot-submit", str(f),
                               "--slots", str(APPDATA / "slots"),
                               "--db", str(db), "--json"],
                       capture_output=True, text=True, env=env)
    out = json.loads(p.stdout.strip().splitlines()[-1])
    created[name] = out
    print(f"submitted {name}: {json.dumps(occ)} next_fire_at={out.get('next_fire_at')}",
          flush=True)

events_path = APPDATA / "logs" / "events.current.jsonl"
seen_ids = set()
fires = []
end = time.monotonic() + WATCH
last = 0
while time.monotonic() < end:
    if events_path.exists():
        for ln in events_path.read_text(errors="replace").splitlines():
            try:
                e = json.loads(ln)
            except ValueError:
                continue
            if e.get("event_id") in seen_ids:
                continue
            seen_ids.add(e.get("event_id"))
            if str(e.get("timer_name", "")).startswith("dst-") and \
                    e.get("kind") in ("fired", "fired_late", "coalesced",
                                      "skipped_misfire"):
                fires.append(e)
                print(f"EVENT {e['timer_name']} {e['kind']} "
                      f"scheduled_for={e.get('scheduled_for')} "
                      f"logged_at={e.get('logged_at')}", flush=True)
    if time.monotonic() - last > 300:
        last = time.monotonic()
        print("... fake clock", fake_clock(), flush=True)
    time.sleep(2)

target_fires = [e for e in fires if e["timer_name"] == "dst-target"]
result = {
    "mode": MODE,
    "tz": "Europe/Helsinki",
    "fake_start_local": START,
    "watch_secs": WATCH,
    "target_local_time": TARGET_TIME,
    "created": created,
    "fires": fires,
    "target_fire_count": len(target_fires),
    "target_fires": target_fires,
    "interval_fire_count": len([e for e in fires
                                if e["timer_name"] == "dst-control-interval"]),
    "final_fake_clock": fake_clock(),
}
Path("/dst/result.json").write_text(json.dumps(result, indent=2, default=str))
print("RESULT", json.dumps(result, indent=2, default=str), flush=True)
app.terminate()
