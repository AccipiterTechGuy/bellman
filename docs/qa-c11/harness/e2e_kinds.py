#!/usr/bin/env python3
"""C11 §2 — all seven occurrence kinds observed firing on their own schedule.

The desktop app is started first and stays up for the whole run; every timer
is then created from OUTSIDE it through `bellman slot-submit`, which is the
SCH2 path (a foreign commit a running scheduler has to notice without a
restart). Nothing calls run-now.
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
from e2e_lib import Run, say, utcnow  # noqa: E402

TZ = "Europe/Helsinki"
APP_NAME = "e2e-kinds"
WEEKDAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]


def main():
    run = Run("kinds").fresh()
    say("starting bellman-app (release) on", run.display)
    run.start_app()
    say("data dir:", run.appdata)

    local = ZoneInfo(TZ)
    base = datetime.now(local)
    plan = []

    def at(offset):
        return base + timedelta(seconds=offset)

    t = at(110)
    plan.append(("once", {"kind": "once", "time": t.strftime("%Y-%m-%dT%H:%M:%S")}, t))
    t = at(120)
    plan.append(("interval", {"kind": "interval", "every_secs": 120}, None))
    t = at(130)
    plan.append(("daily", {"kind": "daily", "time": t.strftime("%H:%M:%S")}, t))
    t = at(140)
    plan.append(("weekly", {"kind": "weekly", "time": t.strftime("%H:%M:%S"),
                            "days": [WEEKDAYS[t.weekday()]]}, t))
    t = at(150)
    plan.append(("monthly", {"kind": "monthly", "time": t.strftime("%H:%M:%S"),
                             "day": t.day}, t))
    t = at(160)
    plan.append(("yearly", {"kind": "yearly", "time": t.strftime("%H:%M:%S"),
                            "month": t.month, "day": t.day}, t))
    t = at(170)
    plan.append(("cron", {"kind": "cron",
                          "cron": f"{t.second} {t.minute} {t.hour} * * *"}, t))

    created = {}
    for kind, occ, want in plan:
        name = f"kind-{kind}"
        resp = run.submit({
            "app_name": APP_NAME,
            "timer_name": name,
            "tz": TZ,
            "occurrence": occ,
        })
        assert resp.get("ok"), resp
        r = resp["response"] if "response" in resp else resp
        created[kind] = {
            "timer_name": name,
            "occurrence": occ,
            "submit_response": r,
        }
        say(f"submitted {name:16s} occ={json.dumps(occ)} -> next_fire_at="
            f"{r.get('next_fire_at')}")

    say("all seven submitted; the app was already running — nothing restarted it.")
    say("now waiting for the clock. No run-now is issued anywhere in this script.")

    results = {}
    deadline = time.monotonic() + 420
    remaining = set(created)
    while remaining and time.monotonic() < deadline:
        for ev in run.log_lines():
            if ev.get("kind") not in ("fired", "fired_late", "coalesced"):
                continue
            nm = ev.get("timer_name", "")
            if not nm.startswith("kind-"):
                continue
            k = nm[len("kind-"):]
            if k in remaining:
                remaining.discard(k)
                results[k] = ev
                say(f"FIRED  {nm:16s} kind={ev['kind']} scheduled_for={ev.get('scheduled_for')} "
                    f"logged_at={ev.get('logged_at')} run_id={ev.get('run_id')}")
        if remaining:
            time.sleep(1.0)

    say("still missing:", sorted(remaining) or "none")

    # Fire notifications prove the integration owner path fired too.
    fires = sorted((run.slots / "fires").glob("fire-*.json"))
    fire_docs = [json.loads(p.read_text()) for p in fires]

    out = {
        "data_dir": str(run.appdata),
        "tz": TZ,
        "created": {k: {"timer_name": v["timer_name"],
                        "occurrence": v["occurrence"],
                        "next_fire_at": v["submit_response"].get("next_fire_at"),
                        "timer_id": v["submit_response"].get("timer_id")}
                    for k, v in created.items()},
        "fired": {k: results[k] for k in results},
        "missing": sorted(remaining),
        "fire_notifications": fire_docs,
        "timer_list": run.cli_json("list"),
    }
    outp = run.root / "kinds_evidence.json"
    outp.write_text(json.dumps(out, indent=2, default=str))
    say("evidence ->", outp)

    run.stop()
    return 0 if not remaining else 1


if __name__ == "__main__":
    sys.exit(main())
