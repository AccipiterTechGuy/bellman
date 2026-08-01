#!/usr/bin/env python3
"""C11 §2 — misfire behaviour across a real stop/start, on a real clock.

Two calendar timers are scheduled for the same instant with opposite
policies. The app is then stopped BEFORE that instant and started again
AFTER it, so the miss is genuine — no clock is manipulated, no run-now.
"""
import json
import sys
from pathlib import Path
import time
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

# The harness lives next to the evidence it produced; importing the shared
# helper from beside this file keeps every script runnable from anywhere.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_lib import Run, say  # noqa: E402

TZ = "Europe/Helsinki"


def main():
    run = Run("misfire", display=":93").fresh()
    run.start_app(tag="app-first")
    say("data dir:", run.appdata)

    local = ZoneInfo(TZ)
    due = datetime.now(local) + timedelta(seconds=150)
    hhmmss = due.strftime("%H:%M:%S")
    say("both timers due at", due.isoformat())

    created = {}
    for name, policy in (("mf-coalesce", "coalesce"), ("mf-skip", "skip")):
        resp = run.submit({"app_name": "mf-app", "timer_name": name, "tz": TZ,
                           "occurrence": {"kind": "daily", "time": hhmmss},
                           "misfire_policy": policy})
        r = resp.get("response", resp)
        created[name] = {"policy": policy, "timer_id": r["timer_id"],
                         "next_fire_at": r["next_fire_at"]}
        say(f"submitted {name:12s} misfire_policy={policy:9s} next_fire_at={r['next_fire_at']}")

    listing_before = run.cli_json("list")
    time.sleep(20)               # let the horizon pick both up
    say("stopping the app well before the due time (SIGTERM, like closing it)")
    run.stop_app()
    stopped_at = datetime.now(timezone.utc)

    # Sleep past the due instant while nothing is running.
    while datetime.now(local) < due + timedelta(seconds=45):
        time.sleep(2)
    say("due time passed with Bellman down; restarting")

    mark = run.log_count()
    run.start_app(tag="app-second")
    restarted_at = datetime.now(timezone.utc)

    # Give the startup misfire pass time to run and publish.
    deadline = time.monotonic() + 150
    seen = {}
    ids = set()
    while time.monotonic() < deadline:
        for e in run.log_lines(since=mark):
            nm = e.get("timer_name")
            eid = e.get("event_id")
            if eid in ids:
                continue
            if nm in created and e.get("kind") in (
                    "fired", "fired_late", "coalesced", "skipped_misfire"):
                ids.add(eid)
                seen.setdefault(nm, []).append(e)
        if len(seen) == 2:
            break
        time.sleep(1)

    for nm, evs in seen.items():
        say(f"{nm}: {[e['kind'] for e in evs]}")

    out = {
        "created": created,
        "due_local": due.isoformat(),
        "stopped_at_utc": stopped_at.isoformat(),
        "restarted_at_utc": restarted_at.isoformat(),
        "timers_before": listing_before,
        "post_restart_events": {nm: evs for nm, evs in seen.items()},
        "all_post_restart_log": run.log_lines(since=mark),
        "statuses": {nm: run.status(nm) for nm in created},
        "timers_after": run.cli_json("list"),
    }
    p = run.root / "misfire_evidence.json"
    p.write_text(json.dumps(out, indent=2, default=str))
    say("evidence ->", p)
    run.stop()
    return 0 if len(seen) == 2 else 1


if __name__ == "__main__":
    sys.exit(main())
