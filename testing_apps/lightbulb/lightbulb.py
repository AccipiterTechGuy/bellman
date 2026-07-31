#!/usr/bin/env python3
"""The Bellman lightbulb — the smallest app that answers a Bellman timer.

It demonstrates the whole integration loop end to end:

  1. Bellman fires a timer owned by the app name below and drops a fire
     notification under <slots>/fires/ (the reply stub already exists at
     that notification's reply_path).
  2. This app accepts only notifications carrying its own app_name, then
     writes the reply file: state=acknowledged, expected_secs=ON_SECS.
  3. The bulb turns ON — visibly, in this terminal — for ON_SECS seconds.
  4. The app overwrites its reply: state=completed with the measured
     on-duration. Bellman validates it, folds it into status.json and
     records the run terminal.

The app never opens status.json, never constructs a reply filename, and
never writes identity fields — Bellman pre-filled schema/run_id/app_name
in the stub. Deduplication is by run_id: the same run_id seen twice is
the same firing (Bellman delivery is at-least-once), so we act once.

Usage:  lightbulb.py [--slots DIR] [--app-name NAME] [--on-secs N]
Defaults: --slots $BELLMAN_SLOTS or ~/.bellman/slots, --app-name lightbulb,
--on-secs 15.  Stdlib only.
"""

import argparse
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

ON, OFF = "\033[93m", "\033[90m"  # bright yellow / dim grey
RESET, HOME = "\033[0m", "\033[H\033[J"

BULB = [
    "      ████████      ",
    "    ████████████    ",
    "   ██████████████   ",
    "   ██████████████   ",
    "    ████████████    ",
    "      ████████      ",
    "       ██████       ",
    "       ██████       ",
    "        ████        ",
]


def now_utc():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def reply(fire, **fields):
    """The whole reply protocol: one read, one atomic write, one file."""
    path = fire["reply_path"]           # from the notification — never construct it
    r = json.load(open(path))           # schema, run_id, app_name already filled in
    r.update(fields)
    tmp = path + ".tmp"
    json.dump(r, open(tmp, "w"), indent=2)
    os.replace(tmp, path)


def draw(on, note=""):
    color = ON if on else OFF
    art = "\n".join(color + line + RESET for line in BULB)
    print(f"{HOME}\n   💡 lightbulb is {'ON ' if on else 'off'}\n\n{art}\n\n   {note}   ", flush=True)


def handle(fire, on_secs):
    """One firing: acknowledge, light the bulb, report completion."""
    reply(fire, state="acknowledged", acknowledged_at=now_utc(), expected_secs=int(on_secs))
    t0 = time.monotonic()
    while True:
        left = on_secs - (time.monotonic() - t0)
        if left <= 0:
            break
        draw(True, f"bulb on — {left:4.1f}s remaining")
        time.sleep(0.1)
    draw(False, "bulb off — run reported completed")
    reply(fire, state="completed", completed_at=now_utc(),
          result={"on_duration_secs": round(time.monotonic() - t0, 2)})


def scan(fires_dir):
    """Yield fire notifications currently on disk (a rescan is always safe)."""
    try:
        names = sorted(fires_dir.glob("fire-*.json"))
    except OSError:
        return
    for name in names:
        try:
            yield json.load(open(name))
        except (OSError, ValueError):
            continue  # mid-write or not ours — the next rescan sees it again


def main():
    p = argparse.ArgumentParser(description="The Bellman lightbulb demo app.")
    p.add_argument("--slots", default=os.environ.get(
        "BELLMAN_SLOTS", str(Path.home() / ".bellman" / "slots")))
    p.add_argument("--app-name", default="lightbulb")
    p.add_argument("--on-secs", type=float, default=15)
    args = p.parse_args()

    fires_dir = Path(args.slots) / "fires"
    seen = set()  # run_ids already handled — dedupe redeliveries/rescans
    draw(False, f"watching {fires_dir} for app_name={args.app_name!r}")
    while True:
        for fire in scan(fires_dir):
            if fire.get("app_name") != args.app_name:
                continue  # someone else's timer — not our work
            run_id = fire.get("run_id")
            if run_id in seen:
                continue  # same firing delivered again — act once
            seen.add(run_id)
            print(f"{HOME}fire received: run_id={run_id}", flush=True)
            try:
                handle(fire, args.on_secs)
            except OSError:
                seen.discard(run_id)  # reply path not ready — retry next scan
        time.sleep(0.5)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
