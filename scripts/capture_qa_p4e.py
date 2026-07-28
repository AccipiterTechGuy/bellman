#!/usr/bin/env python3
"""QA P4e — WebKitGTK evidence for fire-neighbour collisions / list triage / calendar create.

Drives the real Tauri/WebKitGTK app on DISPLAY (default :0) via AT-SPI + XTest.
NO mocked harness, NO Chromium, NO hand-edited images.

Outputs under docs/qa4-screenshots/ (p4e-*) and docs/qa4-evidence/.
"""
from __future__ import annotations

import importlib.util
import json
import os
import sqlite3
import subprocess
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

from Xlib import XK, X
from Xlib.ext import xtest

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
EVIDENCE = ROOT / "docs" / "qa4-evidence"
DISPLAY_NAME = os.environ.get("DISPLAY", ":0")
DATA_DIR = Path(
    os.environ.get(
        "BELLMAN_QA_DATA",
        "/tmp/qa-p4e-session/share/io.bellman.desktop",
    )
)

_spec = importlib.util.spec_from_file_location(
    "capture_qa_p4b", ROOT / "scripts" / "capture_qa_p4b.py"
)
p4b = importlib.util.module_from_spec(_spec)
sys.modules["capture_qa_p4b"] = p4b
_spec.loader.exec_module(p4b)

p4b.DATA_DIR = DATA_DIR
p4b.OUT = OUT
p4b.EVIDENCE = EVIDENCE
p4b.DISPLAY_NAME = DISPLAY_NAME

_cli_candidates = [
    os.environ.get("BELLMAN_CLI", ""),
    str(ROOT / "target/release/bellman-cli"),
    "/tmp/bellman-cli-schema3",
    str(ROOT / "target/release/bellman"),
]
p4b.CLI_BIN = next((p for p in _cli_candidates if p and Path(p).exists()), "bellman")
CLI = p4b.CLI_BIN


def resize_window(w: int, h: int):
    for args in (
        ["wmctrl", "-x", "-r", "Bellman.Bellman", "-e", f"0,40,40,{w},{h}"],
        ["wmctrl", "-r", "Bellman", "-e", f"0,40,40,{w},{h}"],
    ):
        subprocess.run(args, check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.55)


def dump_store(path: Path):
    db = DATA_DIR / "timers.db"
    if not db.exists():
        path.write_text("[]\n")
        return []
    con = sqlite3.connect(str(db))
    con.row_factory = sqlite3.Row
    rows = [dict(r) for r in con.execute("SELECT * FROM timers ORDER BY name")]
    con.close()
    out = []
    for r in rows:
        item = {}
        for k, v in r.items():
            item[k] = v if isinstance(v, (int, float, type(None))) else str(v)
        out.append(item)
    path.write_text(json.dumps(out, indent=2) + "\n")
    return out


def cli(*args: str) -> str:
    db = DATA_DIR / "timers.db"
    cmd = [CLI, "--db", str(db), *args]
    r = subprocess.run(cmd, capture_output=True, text=True)
    text = (r.stdout or "") + (r.stderr or "")
    if r.returncode != 0:
        print(f"  cli FAIL ({r.returncode}): {' '.join(args)}\n{text[:500]}")
    return text


def open_new(app, d):
    p4b.close_dialog_if_open(app)
    time.sleep(0.15)
    p4b.click_named(app, "All timers", "push button")
    time.sleep(0.25)
    p4b.click_named(app, "+ New timer", "push button")
    time.sleep(0.55)


def select_kind_click(app, kind: str, d):
    kind = kind.lower()
    combos = p4b.walk_find(app, lambda a: a.getRoleName() == "combo box")
    if not combos:
        raise RuntimeError("no combo box")
    combo = combos[0]
    comp = combo.queryComponent()
    try:
        comp.grabFocus()
    except Exception:
        pass
    ext = comp.getExtents(p4b.pyatspi.DESKTOP_COORDS)
    cx = int(ext.x + max(ext.width // 2, 4))
    cy = int(ext.y + max(ext.height // 2, 4))
    xtest.fake_input(d, X.MotionNotify, x=cx, y=cy)
    xtest.fake_input(d, X.ButtonPress, detail=1)
    xtest.fake_input(d, X.ButtonRelease, detail=1)
    d.sync()
    time.sleep(0.4)
    items = p4b.walk_find(
        app,
        lambda a: a.getRoleName() == "menu item" and (a.name or "").lower() == kind,
    )
    if not items:
        p4b.select_kind(app, kind, d)
        return
    p4b.do_action(items[0])
    time.sleep(0.45)


def set_action_launch(app, d, command="/bin/true"):
    """Select launch command radio and fill Command."""
    radios = p4b.walk_find(app, lambda a: a.getRoleName() == "radio button")
    for r in radios:
        name = (r.name or "").lower()
        if "launch" in name:
            p4b.do_action(r)
            time.sleep(0.25)
            break
    p4b.focus_entry(app, "Command", d)
    p4b.clear_and_type(d, command)
    time.sleep(0.1)


def create_daily(
    app,
    d,
    *,
    name: str,
    time_hhmm: str,
    tz: str = "UTC",
    launch: bool = False,
    snap: str | None = None,
):
    print(f"\n== CREATE daily {name!r} @ {time_hhmm} ==")
    open_new(app, d)
    select_kind_click(app, "daily", d)
    time.sleep(0.3)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, name)
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, tz)
    p4b.focus_entry(app, "Wall-clock", d)
    p4b.clear_and_type(d, time_hhmm)
    if launch:
        set_action_launch(app, d, "/bin/true")
    time.sleep(1.2)  # preview + neighbours
    if snap:
        p4b.capture(d, snap, {"name": name, "time": time_hhmm, "launch": launch})
    p4b.click_named(app, "Create", "push button")
    time.sleep(1.0)


def seed_bulk_cli(n: int = 50):
    """Seed N daily timers via CLI for bounded-work timing (not the core GUI demo)."""
    print(f"\n== CLI seed {n} timers for density/timing ==")
    # Space them so they don't all collide with 09:00 demo.
    for i in range(n):
        minute = (i * 7) % 60
        hour = 11 + (i // 60) % 10
        name = f"bulk-timer-{i:03d}-padding-name-longish"
        cli(
            "add",
            "--name",
            name,
            "--occurrence",
            "daily",
            "--time",
            f"{hour:02d}:{minute:02d}:00",
            "--tz",
            "UTC",
        )
    rows = dump_store(EVIDENCE / "store-after-bulk-seed.json")
    print(f"  store rows after bulk: {len(rows)}")


def measure_neighbours_cli(candidates_iso: list[str]) -> dict:
    """Call the pure path via a tiny cargo test binary is heavy; instead time
    list + next for each and record. For the dialog path we time the GUI wait
    below. Also write bellman next --json for collision timers."""
    out = {"candidates": candidates_iso, "next": {}}
    db = DATA_DIR / "timers.db"
    listing = cli("list", "--json")
    (EVIDENCE / "bellman-list-collision.json").write_text(listing)
    try:
        data = json.loads(listing)
        timers = data if isinstance(data, list) else data.get("timers", data.get("items", []))
    except Exception:
        timers = []
    for t in timers:
        name = t.get("name") or ""
        if name.startswith("qa-collide") or name.startswith("qa-nearby") or name.startswith(
            "qa-long"
        ):
            tid = t.get("id") or t.get("timer_id")
            if not tid:
                continue
            nxt = cli("next", "--json", tid, "3")
            out["next"][name] = nxt
            (EVIDENCE / f"bellman-next-{name}.json").write_text(nxt)
    (EVIDENCE / "collision-cli-parity.json").write_text(json.dumps(out, indent=2) + "\n")
    return out


def shot_list_sort_filter(app, d):
    print("\n== LIST sort/filter ==")
    p4b.close_dialog_if_open(app)
    p4b.click_named(app, "All timers", "push button")
    time.sleep(0.5)
    # Search box
    entries = p4b.walk_find(app, lambda a: a.getRoleName() == "text")
    # Prefer the search entry by name
    search = p4b.walk_find(
        app,
        lambda a: a.getRoleName() in ("text", "entry", "search box")
        and "name" in ((a.name or "") + (getattr(a, "description", lambda: "")() or "")).lower(),
    )
    # Fallback: focus Filter by name
    focused = False
    for label in ("Filter timers by name", "Search"):
        try:
            p4b.focus_entry(app, label, d)
            focused = True
            break
        except Exception:
            continue
    if not focused and entries:
        try:
            entries[0].queryComponent().grabFocus()
            focused = True
        except Exception:
            pass
    if focused:
        p4b.clear_and_type(d, "qa-collide")
        time.sleep(0.5)
    p4b.capture(
        d,
        "p4e-list-filter-search",
        {"filter": "qa-collide", "expect": "only collision timers visible"},
    )
    # Clear search
    if focused:
        p4b.clear_and_type(d, "")
        time.sleep(0.3)
    p4b.capture(d, "p4e-list-sort-next-fire", {"sort": "next fire default", "density": True})


def shot_calendar_create(app, d):
    print("\n== MONTH click-to-create ==")
    p4b.close_dialog_if_open(app)
    p4b.click_named(app, "Month", "push button")
    time.sleep(0.6)
    p4b.capture(d, "p4e-month-fire-counts", {"expect": "day cells show fire counts"})
    # Prefer a future month so once-timers keep a non-null next_fire.
    try:
        p4b.click_named(app, "next month", "push button")
        time.sleep(0.45)
    except Exception:
        pass
    hits = p4b.walk_find(
        app,
        lambda a: a.getRoleName() == "push button"
        and (a.name or "").startswith("Create timer on"),
    )
    if hits:
        target = hits[min(14, len(hits) - 1)]
        print(f"  clicking {target.name!r}")
        p4b.do_action(target)
        time.sleep(0.8)
        p4b.capture(d, "p4e-month-create-prefill", {"from": target.name})
        p4b.focus_entry(app, "Name", d)
        p4b.clear_and_type(d, "qa-from-month-cell")
        time.sleep(0.7)
        p4b.capture(d, "p4e-month-create-dialog", {"name": "qa-from-month-cell"})
        p4b.click_named(app, "Create", "push button")
        time.sleep(1.0)
        dump_store(EVIDENCE / "store-after-month-create.json")
        p4b.click_named(app, "All timers", "push button")
        time.sleep(0.4)
        try:
            p4b.focus_entry(app, "Filter timers by name", d)
            p4b.clear_and_type(d, "qa-from-month-cell")
            time.sleep(0.4)
        except Exception:
            pass
        p4b.capture(d, "p4e-list-after-month-create", {"expect": "qa-from-month-cell row"})
    else:
        print("  no Create timer on … buttons; skip prefill shot")


def shot_week_create(app, d):
    print("\n== WEEK empty-day create ==")
    p4b.close_dialog_if_open(app)
    p4b.click_named(app, "Week", "push button")
    time.sleep(0.5)
    p4b.capture(d, "p4e-week-day-counts", {"expect": "day counts + empty + New"})
    news = p4b.walk_find(
        app,
        lambda a: a.getRoleName() == "push button"
        and ("+ New" in (a.name or "") or "New on this day" in (a.name or "")),
    )
    if news:
        p4b.do_action(news[0])
        time.sleep(0.7)
        p4b.capture(d, "p4e-week-create-prefill", {"expect": "dialog prefilled from week day"})
        p4b.close_dialog_if_open(app)


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    d = p4b.xdisp()
    # Production binary is `bellman-app` (package name); older builds used `bellman`.
    app = None
    for name in ("bellman-app", "bellman", "Bellman"):
        try:
            app = p4b.find_app(name)
            print(f"  a11y app: {name!r}")
            break
        except RuntimeError:
            continue
    if app is None:
        print("Bellman AT-SPI app not found (tried bellman-app/bellman)", file=sys.stderr)
        return 2
    p4b.raise_and_geom(d)
    resize_window(960, 640)
    time.sleep(0.4)

    log_lines = []
    t0 = time.time()

    # --- Core demo: 3 same-second timers via GUI, 4th opens dialog ---
    create_daily(app, d, name="qa-collide-alpha-backup", time_hhmm="09:00:00", tz="UTC")
    create_daily(
        app,
        d,
        name="qa-collide-beta-launch-heavy-workload",
        time_hhmm="09:00:00",
        tz="UTC",
        launch=True,
    )
    create_daily(app, d, name="qa-collide-gamma-notify", time_hhmm="09:00:00", tz="UTC")

    # Nearby but not identical (2 min later)
    create_daily(app, d, name="qa-nearby-two-min", time_hhmm="09:02:00", tz="UTC")

    # Long name for ellipsis proof
    long_name = (
        "qa-long-name-morning-backup-and-sync-pipeline-with-extra-descriptive-words-end"
    )
    create_daily(app, d, name=long_name, time_hhmm="14:30:00", tz="UTC")

    dump_store(EVIDENCE / "store-after-collision-create.json")

    # Open 4th at same time — collision dialog
    print("\n== DIALOG collision naming three peers ==")
    open_new(app, d)
    select_kind_click(app, "daily", d)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-collide-delta-fourth")
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, "UTC")
    p4b.focus_entry(app, "Wall-clock", d)
    p4b.clear_and_type(d, "09:00:00")
    t_n0 = time.time()
    time.sleep(1.8)  # preview + query_neighbours
    t_n1 = time.time()
    p4b.capture(
        d,
        "p4e-dialog-collision-names-three",
        {
            "phase": "collision",
            "expect": "Also firing names the three qa-collide-* timers",
            "neighbour_wait_ms": int((t_n1 - t_n0) * 1000),
        },
    )
    resize_window(1280, 800)
    time.sleep(0.5)
    p4b.capture(d, "p4e-dialog-collision-1280x800", {"viewport": "1280x800"})
    resize_window(960, 640)
    time.sleep(0.4)
    p4b.close_dialog_if_open(app)

    # Nearby case: open dialog at 09:00 should show qa-nearby as nearby not collision
    # (already visible in collision shot). Dedicated shot at 09:01 to show nearby-only.
    print("\n== DIALOG nearby-only ==")
    open_new(app, d)
    select_kind_click(app, "daily", d)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-probe-near-0901")
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, "UTC")
    p4b.focus_entry(app, "Wall-clock", d)
    p4b.clear_and_type(d, "09:01:00")
    time.sleep(1.6)
    p4b.capture(
        d,
        "p4e-dialog-nearby-not-collision",
        {
            "phase": "nearby",
            "expect": "nearby list shows ±60s to collide timers; no same-second badge if none",
        },
    )
    p4b.close_dialog_if_open(app)

    # No-collision case
    print("\n== DIALOG no-collision clear state ==")
    open_new(app, d)
    select_kind_click(app, "daily", d)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-lonely-1530")
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, "UTC")
    p4b.focus_entry(app, "Wall-clock", d)
    p4b.clear_and_type(d, "15:30:00")
    time.sleep(1.5)
    p4b.capture(
        d,
        "p4e-dialog-no-collision",
        {"phase": "clear", "expect": "No other timers fire at or near…"},
    )
    p4b.close_dialog_if_open(app)

    # Long name readable in dialog (create open with long existing as neighbour if we collide)
    # Show the long-named timer itself in edit / list
    p4b.click_named(app, "All timers", "push button")
    time.sleep(0.4)
    try:
        p4b.focus_entry(app, "Filter timers by name", d)
        p4b.clear_and_type(d, "qa-long-name")
        time.sleep(0.4)
    except Exception:
        pass
    p4b.capture(
        d,
        "p4e-list-long-name-readable",
        {"expect": "full long timer name visible, no silent ellipsis"},
    )

    measure_neighbours_cli([])
    shot_list_sort_filter(app, d)
    shot_week_create(app, d)
    shot_calendar_create(app, d)

    # Bulk seed for timing note (CLI — does not claim GUI create)
    seed_bulk_cli(50)
    # Open dialog and time neighbour refresh with ≥50 timers in store
    print("\n== BOUNDED WORK with ≥50 timers ==")
    open_new(app, d)
    select_kind_click(app, "daily", d)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-timing-probe")
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, "UTC")
    p4b.focus_entry(app, "Wall-clock", d)
    t_b0 = time.time()
    p4b.clear_and_type(d, "09:00:00")
    time.sleep(2.0)
    t_b1 = time.time()
    p4b.capture(
        d,
        "p4e-dialog-collision-50plus",
        {
            "phase": "bounded-work",
            "store_timers": len(dump_store(EVIDENCE / "store-final.json")),
            "dialog_response_ms_wait": int((t_b1 - t_b0) * 1000),
            "caps": {
                "window_secs": 300,
                "horizon_secs": 14 * 86400,
                "max_fires_per_timer": 48,
            },
        },
    )
    p4b.close_dialog_if_open(app)

    elapsed = time.time() - t0
    summary = {
        "elapsed_s": round(elapsed, 2),
        "cli": CLI,
        "data_dir": str(DATA_DIR),
        "shots": sorted(p.name for p in OUT.glob("p4e-*.png")),
    }
    (EVIDENCE / "p4e-capture-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print("\nDONE", json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
