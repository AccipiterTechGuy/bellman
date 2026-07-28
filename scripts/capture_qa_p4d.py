#!/usr/bin/env python3
"""QA P4d — WebKitGTK evidence for C8d timer-input ergonomics.

Drives the real Tauri/WebKitGTK app on DISPLAY (default :0) via AT-SPI + XTest.
NO mocked harness, NO Chromium, NO hand-edited images.

Outputs under docs/qa4-screenshots/ (p4d-*) and docs/qa4-evidence/.
"""
from __future__ import annotations

import importlib.util
import json
import os
import sqlite3
import subprocess
import sys
import time
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
        "/tmp/qa-p4d-session/share/io.bellman.desktop",
    )
)

# Load helpers from the C8b driver (same X/AT-SPI stack).
_spec = importlib.util.spec_from_file_location(
    "capture_qa_p4b", ROOT / "scripts" / "capture_qa_p4b.py"
)
p4b = importlib.util.module_from_spec(_spec)
sys.modules["capture_qa_p4b"] = p4b
_spec.loader.exec_module(p4b)

# Point the imported module at our session data dir for list_timers_db.
p4b.DATA_DIR = DATA_DIR
p4b.OUT = OUT
p4b.EVIDENCE = EVIDENCE
p4b.DISPLAY_NAME = DISPLAY_NAME

_cli_candidates = [
    os.environ.get("BELLMAN_CLI", ""),
    str(ROOT / "target/release/bellman-cli"),
    "/tmp/bellman-cli-schema3",
]
p4b.CLI_BIN = next((p for p in _cli_candidates if p and Path(p).exists()), "bellman")


def resize_window(w: int, h: int):
    """Resize via both -x class and title match; verify with wmctrl -lG."""
    for args in (
        ["wmctrl", "-x", "-r", "Bellman.Bellman", "-e", f"0,40,40,{w},{h}"],
        ["wmctrl", "-r", "Bellman", "-e", f"0,40,40,{w},{h}"],
    ):
        subprocess.run(args, check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.55)
    for L in subprocess.check_output(["wmctrl", "-lG"], text=True).splitlines():
        if "Bellman" in L and "Bellman" == L.split()[-1]:
            parts = L.split()
            print(f"  resize target {w}x{h} → wmctrl geom {parts[2:6]}")
            break


def dump_store(path: Path):
    db = DATA_DIR / "timers.db"
    if not db.exists():
        path.write_text("[]\n")
        return []
    con = sqlite3.connect(str(db))
    con.row_factory = sqlite3.Row
    rows = [dict(r) for r in con.execute("SELECT * FROM timers ORDER BY name")]
    con.close()
    # Serialize for evidence (DateTime etc. as str)
    out = []
    for r in rows:
        item = {}
        for k, v in r.items():
            item[k] = v if isinstance(v, (int, float, type(None))) else str(v)
        out.append(item)
    path.write_text(json.dumps(out, indent=2) + "\n")
    return out


def cli_list() -> str:
    """List via the same DB the GUI session writes (explicit --db)."""
    db = DATA_DIR / "timers.db"
    r = subprocess.run(
        [p4b.CLI_BIN, "--db", str(db), "list", "--json"],
        capture_output=True,
        text=True,
    )
    text = (r.stdout or "") + (r.stderr or "")
    return text


def open_new(app, d):
    p4b.close_dialog_if_open(app)
    time.sleep(0.15)
    p4b.click_named(app, "All timers", "push button")
    time.sleep(0.25)
    p4b.click_named(app, "+ New timer", "push button")
    time.sleep(0.55)


def select_kind_click(app, kind: str, d):
    """Select occurrence kind by opening the combo and clicking the menu item.

    The keyboard Home/Down path from P4b sometimes leaves the menu open without
    committing the Svelte bind — clicking the menu item is reliable.
    """
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
        # fallback keyboard path
        p4b.select_kind(app, kind, d)
        return
    p4b.do_action(items[0])
    time.sleep(0.45)
    print(f"  select_kind_click {kind!r}")


def click_chip(app, label: str):
    hits = p4b.walk_find(
        app,
        lambda a: a.getRoleName() == "push button" and (a.name or "") == label,
    )
    if not hits:
        raise RuntimeError(f"weekday chip {label!r} not found")
    p4b.do_action(hits[0])
    time.sleep(0.08)


def set_weekly_days(app, days: list[str]):
    """Toggle chips so exactly `days` (mon..sun short labels Mon..) are on.

    Default form is mon,wed,fri. We click each chip that should change.
    Easier path: click all seven to off then click wanted — but we don't know
    state from a11y reliably. Instead: read aria-pressed if available, else
    click target set from known defaults for create.
    """
    wanted = {d[:3].lower() for d in days}
    label = {
        "mon": "Mon", "tue": "Tue", "wed": "Wed", "thu": "Thu",
        "fri": "Fri", "sat": "Sat", "sun": "Sun",
    }
    # Probe pressed state
    for key, lab in label.items():
        hits = p4b.walk_find(
            app,
            lambda a, lab=lab: a.getRoleName() == "push button" and (a.name or "") == lab,
        )
        if not hits:
            continue
        acc = hits[0]
        pressed = False
        try:
            # AT-SPI states
            st = acc.getState()
            pressed = st.contains(p4b.pyatspi.STATE_PRESSED) if hasattr(p4b, "pyatspi") else False
        except Exception:
            pressed = False
        try:
            import pyatspi
            st = acc.getState()
            pressed = bool(st.contains(pyatspi.STATE_PRESSED))
        except Exception:
            # fallback: default mon,wed,fri on for fresh form
            pressed = key in ("mon", "wed", "fri")
        should = key in wanted
        if pressed != should:
            p4b.do_action(acc)
            time.sleep(0.06)


def create_once_human_date(app, d):
    """Create once timer with typed European date 24.12.2026."""
    print("\n== CREATE once with 24.12.2026 ==")
    open_new(app, d)
    select_kind_click(app, "once", d)
    time.sleep(0.35)
    # Name is auto-focused on open — but re-focus to be sure
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-p4d-once-eu")
    time.sleep(0.1)
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, "Europe/Helsinki")
    time.sleep(0.1)
    # Date free-text (not the native picker)
    p4b.focus_entry(app, "Once date", d)
    p4b.clear_and_type(d, "24.12.2026")
    time.sleep(0.15)
    p4b.focus_entry(app, "Once time", d)
    p4b.clear_and_type(d, "09:00")
    time.sleep(1.0)  # echo + preview
    p4b.capture(d, "p4d-once-echo-24-12-2026", {
        "phase": "echo",
        "typed_date": "24.12.2026",
        "typed_time": "09:00",
        "expect_echo": "Thursday 24 December 2026, 09:00 Europe/Helsinki",
    })
    p4b.click_named(app, "Create", "push button")
    time.sleep(1.0)
    rows = dump_store(EVIDENCE / "store-after-once-eu.json")
    print("  store:", [r.get("name") for r in rows])
    listing = cli_list()
    (EVIDENCE / "bellman-list-after-once-eu.json").write_text(listing)
    print("  cli list head:", listing[:400])
    return rows


def shot_widgets(app, d):
    """One screenshot per input widget family."""
    print("\n== WIDGET SHOTS ==")
    open_new(app, d)
    # Date + time native/text for once
    select_kind_click(app, "once", d)
    time.sleep(0.3)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-widget-once")
    p4b.focus_entry(app, "Once date", d)
    p4b.clear_and_type(d, "24.12.2026")
    p4b.focus_entry(app, "Once time", d)
    p4b.clear_and_type(d, "09:00:00")
    time.sleep(0.6)
    p4b.capture(d, "p4d-widget-date-time", {"widgets": ["date", "time", "echo"]})

    # Timezone list (daily still shows tz)
    select_kind_click(app, "daily", d)
    time.sleep(0.3)
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, "Europe")
    time.sleep(0.4)
    p4b.capture(d, "p4d-widget-timezone", {"widgets": ["timezone-list"]})

    # Weekday chips
    select_kind_click(app, "weekly", d)
    time.sleep(0.35)
    p4b.capture(d, "p4d-widget-weekday-chips", {"widgets": ["weekday-chips"]})

    # Wall-clock time picker
    p4b.focus_entry(app, "Wall-clock", d)
    p4b.clear_and_type(d, "08:15")
    time.sleep(0.3)
    p4b.capture(d, "p4d-widget-wall-time", {"widgets": ["time-picker"]})

    p4b.close_dialog_if_open(app)


def shot_errors_vs_dst(app, d):
    print("\n== ERROR vs DST ADVISORY ==")
    # Invalid cron → preview error banner
    open_new(app, d)
    select_kind_click(app, "cron", d)
    time.sleep(0.3)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-bad-cron")
    p4b.focus_entry(app, "Cron", d)
    p4b.clear_and_type(d, "not a cron")
    time.sleep(1.2)
    p4b.capture(d, "p4d-preview-error-invalid-cron", {
        "phase": "preview-error",
        "expect": "Error banner (not DST advisory)",
    })
    p4b.close_dialog_if_open(app)

    # Field error: empty name / incomplete once
    open_new(app, d)
    select_kind_click(app, "once", d)
    time.sleep(0.3)
    # clear name if any, leave date empty
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "")
    time.sleep(0.2)
    p4b.focus_entry(app, "Once date", d)
    p4b.clear_and_type(d, "99.99.2026")
    time.sleep(0.4)
    p4b.capture(d, "p4d-field-error-invalid-date", {
        "phase": "field-error",
        "expect": "inline field error + Create disabled",
    })
    p4b.close_dialog_if_open(app)

    # DST gap advisory (Europe/Helsinki spring forward 2027-03-28 03:30)
    open_new(app, d)
    select_kind_click(app, "once", d)
    time.sleep(0.3)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-dst-gap")
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, "Europe/Helsinki")
    p4b.focus_entry(app, "Once date", d)
    p4b.clear_and_type(d, "2027-03-28")
    p4b.focus_entry(app, "Once time", d)
    p4b.clear_and_type(d, "03:30:00")
    time.sleep(1.3)
    p4b.capture(d, "p4d-dst-advisory", {
        "phase": "dst-advisory",
        "expect": "Advisory banner (amber), not Error",
    })
    p4b.close_dialog_if_open(app)


def shot_layouts(app, d):
    print("\n== LAYOUT 960 + 1280 ==")
    open_new(app, d)
    select_kind_click(app, "weekly", d)
    time.sleep(0.3)
    p4b.focus_entry(app, "Name", d)
    p4b.clear_and_type(d, "qa-layout")
    p4b.focus_entry(app, "Timezone", d)
    p4b.clear_and_type(d, "Europe/Helsinki")
    p4b.focus_entry(app, "Wall-clock", d)
    p4b.clear_and_type(d, "08:00")
    time.sleep(0.8)

    resize_window(960, 640)
    p4b.capture(d, "p4d-layout-960x640", {"viewport": [960, 640]})
    resize_window(1280, 800)
    p4b.capture(d, "p4d-layout-1280x800", {"viewport": [1280, 800]})
    resize_window(960, 640)
    p4b.close_dialog_if_open(app)


def keyboard_only_create(app, d):
    """Create a daily timer end-to-end after one click to open the dialog.

    Name autofocus → type name → Tab to kind (leave daily) → Tab to tz → type UTC
    → a11y grabFocus on wall-clock (no pointer) → type time → a11y Activate Create.
    Do NOT press Escape — that closes the dialog (backdrop handler).
    """
    print("\n== KEYBOARD-ONLY CREATE ==")
    open_new(app, d)
    time.sleep(0.45)
    p4b.type_string(d, "qa-p4d-keyboard")
    time.sleep(0.08)
    p4b.key_tap(d, XK.string_to_keysym("Tab"))  # kind
    time.sleep(0.08)
    p4b.key_tap(d, XK.string_to_keysym("Tab"))  # timezone
    time.sleep(0.08)
    p4b.clear_and_type(d, "UTC")
    time.sleep(0.12)
    hits = p4b.walk_find(
        app,
        lambda a: a.getRoleName() == "entry" and "Wall-clock" in (a.name or ""),
    )
    if hits:
        try:
            hits[0].queryComponent().grabFocus()
        except Exception:
            pass
        time.sleep(0.1)
        p4b.clear_and_type(d, "07:30")
    time.sleep(0.35)
    btns = p4b.walk_find(
        app,
        lambda a: a.getRoleName() == "push button" and (a.name or "") == "Create",
    )
    if not btns:
        raise RuntimeError("Create button missing")
    p4b.do_action(btns[0])
    time.sleep(0.9)
    names = [t.get("name") for t in p4b.list_timers_db()]
    print("  after keyboard create store:", names)
    p4b.capture(d, "p4d-keyboard-create-result", {"names": names})
    if "qa-p4d-keyboard" not in names:
        print("  WARNING: keyboard create may have failed")


def crud_all_kinds(app, d):
    """Create + edit + delete all seven kinds through the GUI (adapted field names)."""
    print("\n== 7-KIND CRUD ==")
    kinds = [
        ("qa-p4d-once", "once", [
            ("Name", "qa-p4d-once"),
            ("Timezone", "Europe/Helsinki"),
            ("Once date", "2027-06-15"),
            ("Once time", "14:00"),
        ], [("Name", "qa-p4d-once-ed")]),
        ("qa-p4d-interval", "interval", [
            ("Name", "qa-p4d-interval"),
            ("Every", "120"),
        ], [("Every", "180")]),
        ("qa-p4d-daily", "daily", [
            ("Name", "qa-p4d-daily"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "08:15"),
        ], [("Wall-clock", "09:15")]),
        ("qa-p4d-weekly", "weekly", [
            ("Name", "qa-p4d-weekly"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "08:00"),
        ], [("Wall-clock", "09:00")]),
        ("qa-p4d-monthly", "monthly", [
            ("Name", "qa-p4d-monthly"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "10:00"),
            ("Day of month", "15"),
        ], [("Day of month", "20")]),
        ("qa-p4d-yearly", "yearly", [
            ("Name", "qa-p4d-yearly"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "12:00"),
            ("Month", "7"),
            ("Day of month", "28"),
        ], [("Month", "12")]),
        ("qa-p4d-cron", "cron", [
            ("Name", "qa-p4d-cron"),
            ("Timezone", "Europe/Helsinki"),
            ("Cron", "0 9 * * 1-5"),
        ], [("Cron", "30 9 * * 1-5")]),
    ]

    created = []
    for name, kind, fields, _edit in kinds:
        print(f"\n-- create {name} --")
        open_new(app, d)
        select_kind_click(app, kind, d)
        time.sleep(0.35)
        p4b.fill_fields(app, d, fields)
        if kind == "weekly":
            # defaults mon,wed,fri already on
            pass
        time.sleep(0.9)
        p4b.click_named(app, "Create", "push button")
        time.sleep(0.85)
        store = [t.get("name") for t in p4b.list_timers_db()]
        print(f"  store: {store}")
        if name not in store:
            raise RuntimeError(f"create failed for {name}: {store}")
        created.append((name, kind, fields, _edit))

    dump_store(EVIDENCE / "store-after-create-7.json")
    p4b.capture(d, "p4d-all-after-create", {"phase": "after-7-create"})

    # Edit each
    for name, kind, fields, edit_fields in created:
        print(f"\n-- edit {name} --")
        p4b.open_edit_for(app, d, name)
        p4b.fill_fields(app, d, edit_fields)
        time.sleep(0.3)
        for label in ("Save", "Create"):
            btns = p4b.walk_find(
                app,
                lambda a, lab=label: a.getRoleName() == "push button" and (a.name or "") == lab,
            )
            if btns:
                p4b.do_action(btns[0])
                break
        time.sleep(0.75)

    dump_store(EVIDENCE / "store-after-edit-7.json")
    p4b.capture(d, "p4d-all-after-edit", {"phase": "after-7-edit"})

    # Delete each (use edited names where Name was changed)
    delete_names = []
    for name, kind, fields, edit_fields in created:
        new_name = name
        for en, val in edit_fields:
            if en == "Name":
                new_name = val
        delete_names.append(new_name)

    for name in delete_names:
        p4b.delete_kind(app, d, name)

    dump_store(EVIDENCE / "store-after-delete-7.json")
    p4b.capture(d, "p4d-all-after-delete", {"phase": "after-7-delete"})
    print("7-kind CRUD complete")


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    os.environ["DISPLAY"] = DISPLAY_NAME

    log_path = EVIDENCE / "p4d-capture-run.log"
    log_f = open(log_path, "w")

    class Tee:
        def write(self, s):
            sys.__stdout__.write(s)
            log_f.write(s)
            log_f.flush()
        def flush(self):
            sys.__stdout__.flush()
            log_f.flush()

    sys.stdout = Tee()  # type: ignore

    d = p4b.xdisp()
    # P6 dual-binary: GUI process a11y name is "bellman-app" (not "bellman").
    app = p4b.find_app("bellman-app")
    pids = p4b.webkit_pids()
    print("engine pids:", json.dumps(pids, indent=2))
    (EVIDENCE / "p4d-webkit_pids.json").write_text(json.dumps(pids, indent=2) + "\n")
    p4b.capture_user_agent(EVIDENCE)

    resize_window(960, 640)

    # 1. Native probe already committed separately; re-state path
    probe = OUT / "p4d-webkit-native-date-time-probe.png"
    print(f"native date/time probe exists: {probe.exists()} {probe}")

    # 2. Widget shots
    shot_widgets(app, d)

    # 3. Human date once + echo + store
    create_once_human_date(app, d)

    # 4. Errors vs DST
    shot_errors_vs_dst(app, d)

    # 5. Layouts
    shot_layouts(app, d)

    # 6. Keyboard create
    keyboard_only_create(app, d)

    # 7. Full CRUD (cleans up its own timers; leaves the once-eu + keyboard)
    try:
        crud_all_kinds(app, d)
    except Exception as e:
        print(f"CRUD ERROR: {e!r}")
        import traceback
        traceback.print_exc()
        p4b.capture(d, "p4d-crud-failure", {"error": repr(e)})
        return 1

    # Final list screenshot
    p4b.click_named(app, "All timers", "push button")
    time.sleep(0.5)
    p4b.capture(d, "p4d-all-final", {"phase": "final"})
    dump_store(EVIDENCE / "store-final.json")
    (EVIDENCE / "bellman-list-final.json").write_text(cli_list())

    print("\nP4d capture complete.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as e:
        print(f"FATAL: {e!r}", file=sys.stderr)
        import traceback
        traceback.print_exc()
        raise
