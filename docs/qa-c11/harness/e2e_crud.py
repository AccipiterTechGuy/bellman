#!/usr/bin/env python3
"""C11 §3 — slot channel CRUD from an external script against a LIVE scheduler.

This is the SCH2 guarantee, exercised for all three operations while the
desktop app stays up the whole time: nothing is restarted, and the clock does
every firing.

  add     a timer created from outside fires on its own schedule
  modify  moving the fire time takes effect on the running scheduler
  delete  a deleted timer does not fire at the time it was due
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
    run = Run("crud", display=":95").fresh()
    run.start_app()
    say("data dir:", run.appdata, "— the app stays up for the whole run")

    local = ZoneInfo(TZ)
    base = datetime.now(local)

    def at(sec):
        return (base + timedelta(seconds=sec)).strftime("%Y-%m-%dT%H:%M:%S")

    ev = {}

    # --- add -------------------------------------------------------------
    add = run.submit({"app_name": "crud-app", "timer_name": "crud-add",
                      "tz": TZ, "occurrence": {"kind": "once", "time": at(110)}})
    add_r = add.get("response", add)
    ev["add_response"] = add_r
    say("add crud-add ->", add_r["next_fire_at"])

    # --- one to be moved later -------------------------------------------
    mv = run.submit({"app_name": "crud-app", "timer_name": "crud-modify",
                     "tz": TZ, "occurrence": {"kind": "once", "time": at(600)}})
    mv_r = mv.get("response", mv)
    ev["modify_original"] = mv_r
    say("add crud-modify (10 min out) ->", mv_r["next_fire_at"])

    # --- one to be deleted before it can fire ----------------------------
    dl = run.submit({"app_name": "crud-app", "timer_name": "crud-delete",
                     "tz": TZ, "occurrence": {"kind": "once", "time": at(140)}})
    dl_r = dl.get("response", dl)
    ev["delete_original"] = dl_r
    say("add crud-delete ->", dl_r["next_fire_at"])

    time.sleep(20)   # the running scheduler picks the three up

    # --- modify: pull the 10-minute timer in to ~2 minutes ---------------
    moved_to = at(170)
    mod = run.submit({"app_name": "crud-app", "timer_id": mv_r["timer_id"],
                      "occurrence": {"kind": "once", "time": moved_to},
                      "tz": TZ}, operation="modify")
    mod_r = mod.get("response", mod)
    ev["modify_response"] = mod_r
    ev["modify_moved_to_local"] = moved_to
    say(f"modify crud-modify -> {moved_to} local, next_fire_at={mod_r.get('next_fire_at')}")

    # --- delete ----------------------------------------------------------
    dele = run.submit({"app_name": "crud-app", "timer_id": dl_r["timer_id"]},
                      operation="delete")
    dele_r = dele.get("response", dele)
    ev["delete_response"] = dele_r
    say("delete crud-delete ->", dele_r.get("status"))
    ev["list_after_delete"] = run.cli_json("list")

    # --- watch the clock -------------------------------------------------
    want = {"crud-add", "crud-modify"}
    fired = {}
    end = time.monotonic() + 300
    while time.monotonic() < end:
        for e in run.log_lines():
            nm = e.get("timer_name")
            if nm in ("crud-add", "crud-modify", "crud-delete") and \
                    e.get("kind") in ("fired", "fired_late", "coalesced"):
                if nm not in fired:
                    fired[nm] = e
                    say(f"FIRED {nm} scheduled_for={e.get('scheduled_for')} "
                        f"logged_at={e.get('logged_at')}")
        if want <= set(fired):
            break
        time.sleep(1)

    # Give the deleted timer's original due time room to pass unfired.
    while datetime.now(local) < base + timedelta(seconds=200):
        time.sleep(2)
    for e in run.log_lines():
        if e.get("timer_name") == "crud-delete" and \
                e.get("kind") in ("fired", "fired_late", "coalesced"):
            fired.setdefault("crud-delete", e)

    ev["fired"] = fired
    ev["deleted_timer_fired"] = "crud-delete" in fired
    ev["all_events"] = [e for e in run.log_lines()
                        if str(e.get("timer_name", "")).startswith("crud-")]
    ev["restarts"] = 0
    p = run.root / "crud_evidence.json"
    p.write_text(json.dumps(ev, indent=2, default=str))
    say("evidence ->", p)
    say("add fired:", "crud-add" in fired,
        "| modify fired at the NEW time:", "crud-modify" in fired,
        "| deleted timer fired:", ev["deleted_timer_fired"])
    run.stop()
    ok = ("crud-add" in fired and "crud-modify" in fired
          and not ev["deleted_timer_fired"])
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
