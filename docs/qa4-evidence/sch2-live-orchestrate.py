#!/usr/bin/env python3
"""SCH2 live evidence: lightbulb timers created ONLY via the slot channel,
fired by the running Bellman's own scheduler on a real clock. No run-now.

Phases (all against one isolated desktop app instance, Xvfb + private dbus):
  B       terminal lightbulb, timer created via `bellman slot-submit`
  A       terminal lightbulb, timer published to free/ (running app claims)
  GUI-A   lightbulb_gui creates its own timer (slot protocol, Path A)
  GUI-B   lightbulb_gui answers a slot-submit-created timer (Path B)

For each phase the event log must show fired -> acknowledged -> completed
under ONE run_id, fired by the schedule (lateness vs scheduled_for printed).
"""
import json, os, pathlib, signal, subprocess, sys, time, uuid
from datetime import datetime, timezone, timedelta

ROOT = pathlib.Path("/home/sami/bellman/.train-worktrees/2026-07-31_0007")
QA = pathlib.Path("/tmp/sch2-live")
DATA = QA / "share" / "io.bellman.desktop"
SLOTS = DATA / "slots"
DB = DATA / "timers.db"
APP = ROOT / "target" / "debug" / "bellman-app"
CLI = ROOT / "target" / "release" / "bellman"
EVENTS = DATA / "logs" / "events.current.jsonl"
TRANSCRIPT = QA / "transcript.log"

procs = []  # (name, Popen)


def log(msg):
    line = f"[{datetime.now(timezone.utc).strftime('%H:%M:%S')}] {msg}"
    print(line, flush=True)
    with open(TRANSCRIPT, "a") as f:
        f.write(line + "\n")


def spawn(name, cmd, env=None, logfile=None):
    out = open(logfile, "a") if logfile else subprocess.DEVNULL
    p = subprocess.Popen(cmd, env=env, stdout=out, stderr=subprocess.STDOUT,
                         start_new_session=True)
    procs.append((name, p))
    log(f"started {name} pid={p.pid}: {' '.join(str(c) for c in cmd)}")
    return p


def teardown():
    for name, p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    time.sleep(1.0)
    for name, p in procs:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def parse_ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def read_events():
    if not EVENTS.exists():
        return []
    out = []
    for line in EVENTS.read_text().splitlines():
        try:
            out.append(json.loads(line))
        except ValueError:
            pass
    return out


def wait_completed(timer_name, since, timeout_s):
    """Poll the event log until one run_id shows fired->ack->completed in
    order for timer_name (events at/after `since`). Returns the 3 events."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        runs = {}
        for e in read_events():
            if e.get("timer_name") != timer_name or not e.get("run_id"):
                continue
            if parse_ts(e["logged_at"]) < since:
                continue
            runs.setdefault(e["run_id"], []).append(e)
        for run_id, evs in runs.items():
            evs.sort(key=lambda e: e["logged_at"])
            kinds = [e["kind"] for e in evs]
            if "fired" in kinds and "acknowledged" in kinds and "completed" in kinds:
                fi, ai, ci = kinds.index("fired"), kinds.index("acknowledged"), kinds.index("completed")
                if fi < ai < ci:
                    return run_id, [evs[fi], evs[ai], evs[ci]]
        time.sleep(1.0)
    return None, None


def report(phase, timer_name, run_id, evs, t_created):
    fired, ack, comp = evs
    sched = parse_ts(fired["scheduled_for"]) if fired.get("scheduled_for") else None
    late_ms = (parse_ts(fired["logged_at"]) - sched).total_seconds() * 1000 if sched else float("nan")
    log(f"{phase}: PASS run_id={run_id}")
    for e in evs:
        log(f"{phase}:   {e['logged_at']}  kind={e['kind']:<13} run_id={e['run_id']}")
    log(f"{phase}:   scheduled_for={sched}  fired lateness={late_ms:.0f} ms  "
        f"(created at {t_created.strftime('%H:%M:%S')}, fired by schedule, no run-now)")
    return {"phase": phase, "timer": timer_name, "run_id": run_id,
            "scheduled_for": str(sched), "fired": fired["logged_at"],
            "acknowledged": ack["logged_at"], "completed": comp["logged_at"],
            "fired_lateness_ms": round(late_ms, 1)}


def make_req(app_name, timer_name, occurrence):
    return {"schema": "bellman-slot/1", "request_id": str(uuid.uuid4()),
            "logged_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "operation": "add",
            "payload": {"app_name": app_name, "timer_name": timer_name,
                        "tz": "UTC", "occurrence": occurrence}}


def slot_submit(req):
    req_path = QA / f"req-{req['payload']['timer_name']}.json"
    req_path.write_text(json.dumps(req, indent=2))
    r = subprocess.run([str(CLI), "slot-submit", str(req_path),
                        "--slots", str(SLOTS), "--db", str(DB)],
                       capture_output=True, text=True, timeout=60)
    log(f"slot-submit rc={r.returncode}: {r.stdout.strip()[:300]}{r.stderr.strip()[:200]}")
    if r.returncode != 0:
        raise RuntimeError(f"slot-submit failed: {r.stderr}")


def publish_only(req):
    """The documented raw protocol: take a free stub, write the complete
    request onto its reserved name via temp + same-directory rename."""
    free = SLOTS / "free"
    for f in sorted(free.glob("slot-*.json")):
        try:
            stub = json.load(open(f))
        except (OSError, ValueError):
            continue
        if stub.get("request_id") is not None and stub.get("operation") is not None:
            continue  # already filled
        req["slot_id"] = stub["slot_id"]
        tmp = free / (f.name + ".tmp")
        tmp.write_text(json.dumps(req, indent=2))
        os.replace(tmp, f)
        log(f"published to free/{f.name} (slot_id={stub['slot_id']}) — waiting for the running app to claim it")
        return f.name
    raise RuntimeError("no free stub available")


def main():
    # ── isolated environment ─────────────────────────────────────────────
    subprocess.run(["rm", "-rf", str(QA)], check=True)
    for d in (DATA / "logs", DATA / "slots", QA / "config", QA / "runtime", QA / "cache"):
        d.mkdir(parents=True, exist_ok=True)
    (DATA / "config.json").write_text(
        '{"wizard_completed":true,"autostart_enabled":false,"start_minimized":false,"wake_enabled":false}\n')
    os.chmod(QA / "runtime", 0o700)

    disp = None
    for n in range(90, 100):
        if not pathlib.Path(f"/tmp/.X{n}-lock").exists():
            disp = f":{n}"
            break
    env = os.environ.copy()
    env.update({
        "DISPLAY": disp,
        "XDG_DATA_HOME": str(QA / "share"),
        "XDG_CONFIG_HOME": str(QA / "config"),
        "XDG_RUNTIME_DIR": str(QA / "runtime"),
        "XDG_CACHE_HOME": str(QA / "cache"),
        "GDK_BACKEND": "x11",
        "LIBGL_ALWAYS_SOFTWARE": "1",
        "WEBKIT_DISABLE_COMPOSITING_MODE": "1",
        "GIO_USE_VFS": "local",
        "GTK_USE_PORTAL": "0",
    })
    log(f"isolated env: DISPLAY={disp} XDG_DATA_HOME={env['XDG_DATA_HOME']} DATA={DATA}")

    spawn("xvfb", ["Xvfb", disp, "-screen", "0", "1280x800x24"])
    time.sleep(1.0)
    spawn("bellman-app", ["dbus-run-session", "--", str(APP)],
          env=env, logfile=QA / "app.log")

    # wait for the engine (scheduler + watcher) to come up
    deadline = time.time() + 90
    while time.time() < deadline:
        if DB.exists() and (SLOTS / "free").exists() and list((SLOTS / "free").glob("slot-*.json")):
            break
        time.sleep(0.5)
    else:
        raise RuntimeError("bellman-app engine did not come up (see app.log)")
    log("bellman-app engine up: db + free stubs present")

    # terminal lightbulb, answering for app_name=lightbulb
    spawn("lightbulb", [sys.executable, str(ROOT / "testing_apps/lightbulb/lightbulb.py"),
                        "--slots", str(SLOTS), "--on-secs", "5"],
          logfile=QA / "bulb.log")
    time.sleep(1.0)

    results = []

    # ── Path B: bellman slot-submit ──────────────────────────────────────
    t0 = datetime.now(timezone.utc)
    log("PATH B: creating timer 'sch2-live-slot-submit' via bellman slot-submit (interval 45 s)")
    slot_submit(make_req("lightbulb", "sch2-live-slot-submit",
                         {"kind": "interval", "every_secs": 45}))
    run_id, evs = wait_completed("sch2-live-slot-submit", t0, 140)
    if not run_id:
        raise RuntimeError("PATH B FAILED: no fired->ack->completed within 140 s")
    results.append(report("PATH-B", "sch2-live-slot-submit", run_id, evs, t0))

    # ── Path A: publish to free/, running app claims ─────────────────────
    t0 = datetime.now(timezone.utc)
    log("PATH A: publishing timer 'sch2-live-free-publish' to free/ (interval 45 s); no slot-submit, no poll")
    publish_only(make_req("lightbulb", "sch2-live-free-publish",
                          {"kind": "interval", "every_secs": 45}))
    run_id, evs = wait_completed("sch2-live-free-publish", t0, 140)
    if not run_id:
        raise RuntimeError("PATH A FAILED: no fired->ack->completed within 140 s")
    results.append(report("PATH-A", "sch2-live-free-publish", run_id, evs, t0))

    # ── GUI-A: lightbulb_gui creates its own timer (Path A by construction)
    t0 = datetime.now(timezone.utc)
    driver = QA / "gui_drive_a.py"
    driver.write_text(f"""
import sys
sys.path.insert(0, {str(ROOT / 'testing_apps/lightbulb_gui')!r})
import lightbulb_gui as lg
app = lg.LightbulbGUI(slots_dir={str(SLOTS)!r}, app_name="lightbulb-gui", on_secs=5)
app.sched_mode_var.set("in_n_secs")
app.val_entry.delete(0, "end")
app.val_entry.insert(0, "40")
app.after(600, app.on_create_timer)
app.after(110000, app.on_close)
app.mainloop()
""")
    log("GUI-A: lightbulb_gui creates its own timer via the slot protocol (once, +40 s)")
    spawn("lightbulb-gui-a", [sys.executable, str(driver)], env=env,
          logfile=QA / "gui-a.log")
    run_id, evs = wait_completed("lightbulb-gui-demo", t0, 140)
    if not run_id:
        raise RuntimeError("GUI-A FAILED: no fired->ack->completed within 140 s")
    results.append(report("GUI-A", "lightbulb-gui-demo", run_id, evs, t0))

    # ── GUI-B: lightbulb_gui answers a slot-submit-created timer ─────────
    t0 = datetime.now(timezone.utc)
    spawn("lightbulb-gui-b", [sys.executable,
                              str(ROOT / "testing_apps/lightbulb_gui/lightbulb_gui.py"),
                              "--slots", str(SLOTS), "--on-secs", "5"],
          env=env, logfile=QA / "gui-b.log")
    time.sleep(2.0)
    fire_at = (datetime.now(timezone.utc) + timedelta(seconds=45)).strftime("%Y-%m-%dT%H:%M:%S")
    log(f"GUI-B: slot-submit creates 'sch2-gui-slot-submit' for app lightbulb-gui (once at {fire_at}Z)")
    slot_submit(make_req("lightbulb-gui", "sch2-gui-slot-submit",
                         {"kind": "once", "time": fire_at}))
    run_id, evs = wait_completed("sch2-gui-slot-submit", t0, 140)
    if not run_id:
        raise RuntimeError("GUI-B FAILED: no fired->ack->completed within 140 s")
    results.append(report("GUI-B", "sch2-gui-slot-submit", run_id, evs, t0))

    # ── verdict ──────────────────────────────────────────────────────────
    log("ALL FOUR LIVE RUNS PASSED — every fire was scheduled by the running "
        "Bellman itself; run-now was never invoked in this transcript.")
    (QA / "results.json").write_text(json.dumps(results, indent=2))


if __name__ == "__main__":
    try:
        main()
    finally:
        teardown()
