#!/usr/bin/env python3
"""C11 §4 — the two data directories, from the point of view of someone who
has not read LOCAL.md yet.

The GUI runs on its app-data dir. A timer is then created the way the CLI's
own help suggests — `bellman add`, no flags — which lands in the *other*
store. The question this answers is the one a new user actually asks: does
the timer I just made go off?
"""
import json
import subprocess
import sys
from pathlib import Path
import time
from datetime import datetime, timedelta

# The harness lives next to the evidence it produced; importing the shared
# helper from beside this file keeps every script runnable from anywhere.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_lib import CLI, Run, say  # noqa: E402


def main():
    run = Run("datadirs", display=":97").fresh()
    run.start_app()
    say("GUI data dir (XDG_DATA_HOME/io.bellman.desktop):", run.appdata)

    env = run.env()          # HOME is the isolated run root
    cli_home = run.root / ".bellman"

    helptext = subprocess.run([str(CLI), "--help"], capture_output=True,
                              text=True, env=env).stdout
    names_default = "~/.bellman/" in helptext
    say("`bellman --help` names its default data dir:", names_default)

    # A timer created the plain way — no --db, no BELLMAN_DB.
    when = (datetime.now() + timedelta(seconds=90)).strftime("%Y-%m-%dT%H:%M:%S")
    p = subprocess.run([str(CLI), "add", "--name", "cli-default-timer",
                        "--occurrence", "once", "--time", when, "--json"],
                       capture_output=True, text=True, env=env)
    added = json.loads(p.stdout.strip().splitlines()[-1])
    say("bellman add ->", added.get("ok"), added.get("timer", {}).get("next_fire_utc"))

    cli_db = cli_home / "timers.db"
    say("CLI store created at:", cli_db, cli_db.exists())

    gui_list = run.cli_json("list")
    cli_list = json.loads(subprocess.run([str(CLI), "list", "--json"],
                                         capture_output=True, text=True,
                                         env=env).stdout.strip().splitlines()[-1])

    # Wait past the fire time and see whether anything happened in either store.
    time.sleep(150)

    cli_after = json.loads(subprocess.run([str(CLI), "list", "--json"],
                                          capture_output=True, text=True,
                                          env=env).stdout.strip().splitlines()[-1])
    cli_events = cli_home / "logs" / "events.current.jsonl"
    cli_log = cli_events.read_text(errors="replace").splitlines() if cli_events.exists() else []
    gui_log = [e for e in run.log_lines() if e.get("timer_name") == "cli-default-timer"]

    fired = [json.loads(l) for l in cli_log
             if '"cli-default-timer"' in l and '"fired"' in l]

    out = {
        "gui_data_dir": str(run.appdata),
        "cli_data_dir": str(cli_home),
        "help_names_cli_default": names_default,
        "help_excerpt": [l for l in helptext.splitlines() if ".bellman" in l],
        "cli_store_exists": cli_db.exists(),
        "gui_sees_cli_timer": any(t["name"] == "cli-default-timer"
                                  for t in gui_list.get("timers", [])),
        "cli_sees_cli_timer": any(t["name"] == "cli-default-timer"
                                  for t in cli_list.get("timers", [])),
        "gui_timers": [t["name"] for t in gui_list.get("timers", [])],
        "cli_timers": [t["name"] for t in cli_list.get("timers", [])],
        "cli_timer_last_fired_after_due": [
            t.get("last_fired") for t in cli_after.get("timers", [])
            if t["name"] == "cli-default-timer"],
        "cli_log_lines": len(cli_log),
        "cli_fired_events": fired,
        "gui_log_lines_for_cli_timer": gui_log,
    }
    p = run.root / "datadirs_evidence.json"
    p.write_text(json.dumps(out, indent=2, default=str))
    say("GUI store timers:", out["gui_timers"])
    say("CLI store timers:", out["cli_timers"])
    say("the CLI-created timer fired:", bool(fired),
        "| last_fired:", out["cli_timer_last_fired_after_due"])
    say("evidence ->", p)
    run.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
