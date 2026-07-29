#!/usr/bin/env python3
"""QA P4d — WebKitGTK evidence for C8d timer-input ergonomics.

Drives the real Tauri/WebKitGTK app on an isolated display via
tauri-driver + WebKitWebDriver (no global-input-injection, never the operator X session).

Outputs under docs/qa4-screenshots/ (p4d-*) and docs/qa4-evidence/.
"""
from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qa_webdriver as qa

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
EVIDENCE = ROOT / "docs" / "qa4-evidence"
DATA_DIR = Path(
    os.environ.get(
        "BELLMAN_QA_DATA",
        "/tmp/bellman-qa-session/share/io.bellman.desktop",
    )
)


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


def cli_list() -> str:
    db = DATA_DIR / "timers.db"
    r = subprocess.run(
        [qa.CLI_BIN, "--db", str(db), "list", "--json"],
        capture_output=True,
        text=True,
    )
    return (r.stdout or "") + (r.stderr or "")


def create_once_human_date(d):
    print("\n== CREATE once with 24.12.2026 ==")
    qa.open_new_timer()
    qa.select_kind("once")
    time.sleep(0.3)
    qa.fill_fields(
        [
            ("Name", "qa-p4d-once-eu"),
            ("Timezone", "Europe/Helsinki"),
            ("Once date", "24.12.2026"),
            ("Once time", "09:00"),
        ]
    )
    time.sleep(1.0)
    qa.capture(
        d,
        "p4d-once-echo-24-12-2026",
        {
            "phase": "echo",
            "typed_date": "24.12.2026",
            "typed_time": "09:00",
            "expect_echo": "Thursday 24 December 2026, 09:00 Europe/Helsinki",
        },
    )
    qa.click_button("Create")
    time.sleep(1.0)
    rows = dump_store(EVIDENCE / "store-after-once-eu.json")
    print("  store:", [r.get("name") for r in rows])
    listing = cli_list()
    (EVIDENCE / "bellman-list-after-once-eu.json").write_text(listing)
    print("  cli list head:", listing[:400])
    return rows


def shot_widgets(d):
    print("\n== WIDGET SHOTS ==")
    qa.open_new_timer()
    qa.select_kind("once")
    time.sleep(0.25)
    qa.fill_fields(
        [
            ("Name", "qa-widget-once"),
            ("Once date", "24.12.2026"),
            ("Once time", "09:00:00"),
        ]
    )
    time.sleep(0.6)
    qa.capture(d, "p4d-widget-date-time", {"widgets": ["date", "time", "echo"]})

    qa.select_kind("daily")
    time.sleep(0.25)
    qa.fill_field("Name", "qa-widget-tz")
    time.sleep(0.4)
    qa.capture(
        d,
        "p4d-widget-timezone",
        {
            "widgets": ["timezone-list"],
            "note": "unfiltered multi-entry list; rows must be ≥1.5rem / legible",
        },
    )

    qa.select_kind("weekly")
    time.sleep(0.3)
    qa.capture(d, "p4d-widget-weekday-chips", {"widgets": ["weekday-chips"]})

    qa.fill_field("Wall-clock", "08:15")
    time.sleep(0.3)
    qa.capture(d, "p4d-widget-wall-time", {"widgets": ["time-picker"]})
    qa.close_dialog_if_open()


def shot_errors_vs_dst(d):
    print("\n== ERROR vs DST ADVISORY ==")
    qa.open_new_timer()
    qa.select_kind("cron")
    time.sleep(0.25)
    qa.fill_fields([("Name", "qa-bad-cron"), ("Cron", "not a cron")])
    time.sleep(0.6)
    qa.capture(
        d,
        "p4d-preview-error-invalid-cron",
        {
            "phase": "field-error-cron",
            "expect": "inline cron error + Create disabled (not only toast)",
        },
    )
    qa.close_dialog_if_open()

    qa.open_new_timer()
    qa.select_kind("cron")
    time.sleep(0.25)
    qa.fill_fields(
        [
            ("Name", "qa-named-cron"),
            ("Timezone", "Europe/Helsinki"),
            ("Cron", "0 9 * * MON-FRI"),
        ]
    )
    time.sleep(1.2)
    qa.capture(
        d,
        "p4d-cron-named-fields-create-enabled",
        {
            "phase": "named-cron-ok",
            "expr": "0 9 * * MON-FRI",
            "expect": "Create enabled; no field error",
        },
    )
    qa.close_dialog_if_open()

    qa.open_new_timer()
    qa.select_kind("once")
    time.sleep(0.25)
    qa.fill_fields([("Name", ""), ("Once date", "99.99.2026")])
    time.sleep(0.4)
    qa.capture(
        d,
        "p4d-field-error-invalid-date",
        {
            "phase": "field-error",
            "expect": "inline field error + Create disabled",
        },
    )
    qa.close_dialog_if_open()

    qa.open_new_timer()
    qa.select_kind("once")
    time.sleep(0.25)
    qa.fill_fields(
        [
            ("Name", "qa-dst-gap"),
            ("Timezone", "Europe/Helsinki"),
            ("Once date", "2027-03-28"),
            ("Once time", "03:30:00"),
        ]
    )
    time.sleep(1.3)
    qa.capture(
        d,
        "p4d-dst-advisory",
        {
            "phase": "dst-advisory",
            "expect": "Advisory banner (amber), not Error",
        },
    )
    qa.close_dialog_if_open()


def main() -> int:
    global DATA_DIR
    qa.DATA_DIR = Path(
        os.environ.get("BELLMAN_QA_DATA", str(DATA_DIR))
    )
    qa.DISPLAY_NAME = os.environ.get("DISPLAY", "")
    qa.OUT = OUT
    qa.EVIDENCE = EVIDENCE
    DATA_DIR = qa.DATA_DIR
    if os.environ.get("BELLMAN_CLI"):
        qa.CLI_BIN = os.environ["BELLMAN_CLI"]

    OUT.mkdir(parents=True, exist_ok=True)
    EVIDENCE.mkdir(parents=True, exist_ok=True)

    disp = os.environ.get("DISPLAY", "")
    if disp in (":0", ":0.0") and os.environ.get("BELLMAN_QA_ALLOW_DISPLAY0") != "1":
        print(f"ERROR: refusing DISPLAY={disp}", file=sys.stderr)
        return 2

    print(f"P4d WebDriver session DISPLAY={disp} DATA={DATA_DIR}")
    qa.start_session()
    d = qa.xdisp()
    qa.resize_window(960, 640)

    create_once_human_date(d)
    shot_widgets(d)
    shot_errors_vs_dst(d)

    # Weekly chips create path
    print("\n== CREATE weekly via chips ==")
    qa.open_new_timer()
    qa.select_kind("weekly")
    qa.fill_fields(
        [
            ("Name", "qa-p4d-weekly"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "08:00"),
            ("Weekdays", "tue,thu"),
        ]
    )
    time.sleep(0.8)
    qa.capture(d, "p4d-weekly-chips-create", {"phase": "weekly-chips"})
    qa.click_button("Create")
    time.sleep(0.9)

    dump_store(EVIDENCE / "store-p4d-final.json")
    (EVIDENCE / "session-summary-p4d.json").write_text(
        json.dumps(
            {
                "display": disp,
                "data_dir": str(DATA_DIR),
                "input_backend": "tauri-driver+WebKitWebDriver",
                "screenshots": sorted(p.name for p in OUT.glob("p4d-*.png")),
            },
            indent=2,
        )
        + "\n"
    )
    print("P4d DONE")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    finally:
        try:
            qa.stop_session()
        except Exception:
            pass
