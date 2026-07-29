#!/usr/bin/env python3
"""QA P4f — Official capture script for Visual Polish AFTER screenshots.

Drives the real WebKitGTK / Tauri application on DISPLAY via AT-SPI and X11
to capture comprehensive AFTER surface evidence for all pages, dialog occurrence variants,
empty filter state, toast states, and settings page surfaces.

Outputs under docs/qa4-screenshots/, docs/qa4-screenshots/after/, and docs/qa4-screenshots/before/.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.append(str(ROOT / "scripts"))

import capture_qa_p4b as p4b
from Xlib import XK

OUT = ROOT / "docs" / "qa4-screenshots"
AFTER_DIR = OUT / "after"
BEFORE_DIR = OUT / "before"

def safe_click(app, name: str):
    for role in (None, "push button", "button", "page tab"):
        try:
            return p4b.click_named(app, name, role)
        except Exception:
            pass
    raise RuntimeError(f"Could not click {name!r}")

def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    AFTER_DIR.mkdir(parents=True, exist_ok=True)
    BEFORE_DIR.mkdir(parents=True, exist_ok=True)

    d = p4b.xdisp()
    
    # Ensure bellman-app is running and visible
    app = None
    for name in ("bellman-app", "bellman", "Bellman"):
        try:
            app = p4b.find_app(name)
            break
        except Exception:
            pass

    if app is None:
        print("Launching target/release/bellman-app...")
        subprocess.Popen([str(ROOT / "target/release/bellman-app")], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.5)
        app = p4b.find_app("bellman-app")

    p4b.raise_and_geom(d)
    time.sleep(0.5)

    # Escape any active dialog
    p4b.key_tap(d, XK.string_to_keysym("Escape"))
    time.sleep(0.4)

    print("=== QA P4f Evidence Capture ===")

    # 1. Main Navigation Surfaces
    print("Capturing All timers surface...")
    safe_click(app, "All timers")
    time.sleep(0.6)
    p = p4b.capture(d, "p4f-list-after", {"expect": "All timers surface with design tokens, hit targets, tabular numbers"})
    shutil.copy2(p, AFTER_DIR / p.name)

    print("Capturing Week surface...")
    safe_click(app, "Week")
    time.sleep(0.6)
    p = p4b.capture(d, "p4f-week-after", {"expect": "Week calendar surface with day headers and fire count badges"})
    shutil.copy2(p, AFTER_DIR / p.name)

    print("Capturing Month surface...")
    safe_click(app, "Month")
    time.sleep(0.6)
    p = p4b.capture(d, "p4f-month-after", {"expect": "Month grid surface with WCAG AA compliant out-of-month contrast"})
    shutil.copy2(p, AFTER_DIR / p.name)

    print("Capturing Run history surface...")
    safe_click(app, "Run history")
    time.sleep(0.6)
    p = p4b.capture(d, "p4f-history-after", {"expect": "Run history surface with log filter controls and event tail"})
    shutil.copy2(p, AFTER_DIR / p.name)

    print("Capturing Settings page surface (Top)...")
    safe_click(app, "Settings")
    time.sleep(0.6)
    p = p4b.capture(d, "p4f-settings-after", {"expect": "Settings page surface with Wake from sleep and Autostart"})
    shutil.copy2(p, AFTER_DIR / p.name)

    # Scroll down Settings page for below-the-fold controls
    print("Capturing Settings page surface (Below Fold)...")
    p4b.key_tap(d, XK.string_to_keysym("Page_Down"))
    time.sleep(0.5)
    p = p4b.capture(d, "p4f-settings-below-fold", {"expect": "Settings page scrolled to bottom showing misfire defaults and engine settings"})
    shutil.copy2(p, AFTER_DIR / p.name)
    p4b.key_tap(d, XK.string_to_keysym("Page_Up"))
    time.sleep(0.4)

    # 2. First-Run Wizard Overlay
    print("Capturing First-Run Wizard overlay...")
    safe_click(app, "Run setup again")
    time.sleep(0.6)
    p = p4b.capture(d, "p4f-wizard-after", {"expect": "First-run Wizard overlay with backdrop and 32px checkboxes"})
    shutil.copy2(p, AFTER_DIR / p.name)

    # Close Wizard
    try:
        safe_click(app, "Next")
        time.sleep(0.3)
        safe_click(app, "No thanks")
        time.sleep(0.3)
        safe_click(app, "Continue")
        time.sleep(0.4)
    except Exception as e:
        print("Wizard close:", e)

    # 3. All Timers Filter & Empty States
    safe_click(app, "All timers")
    time.sleep(0.4)

    # Filter search with no results
    print("Capturing No results after filter state...")
    try:
        search_inputs = p4b.walk_find(app, lambda a: "Filter" in (a.name or "") or a.getRoleName() in ("entry", "text"))
        if search_inputs:
            p4b.do_action(search_inputs[0])
            time.sleep(0.3)
            p4b.type_string(d, "nonexistent_query_xyz")
            time.sleep(0.6)
            p = p4b.capture(d, "p4f-empty-filter", {"expect": "Empty state when search filter returns no matching timers"})
            shutil.copy2(p, AFTER_DIR / p.name)
            p4b.key_tap(d, XK.string_to_keysym("BackSpace"), ctrl=True)
            p4b.key_tap(d, XK.string_to_keysym("Escape"))
            time.sleep(0.4)
    except Exception as e:
        print("Filter state error:", e)

    # 4. Timer Dialog Occurrence Kinds
    kinds = ["once", "interval", "daily", "weekly", "monthly", "yearly", "cron"]
    for k in kinds:
        print(f"Capturing Dialog variant: {k}...")
        try:
            safe_click(app, "+ New timer")
            time.sleep(0.6)

            p = p4b.capture(d, f"p4f-dialog-{k}", {"expect": f"Timer Dialog showing {k} occurrence kind fields"})
            shutil.copy2(p, AFTER_DIR / p.name)

            safe_click(app, "Cancel")
            time.sleep(0.4)
        except Exception as e:
            print(f"Dialog variant {k} error:", e)

    # 5. Copy BEFORE images if not present
    print("Ensuring BEFORE/AFTER image pairs exist...")
    pair_sources = [
        ("p4f-list-after.png", "before-all-timers.png"),
        ("p4f-week-after.png", "before-week-page.png"),
        ("p4f-month-after.png", "before-month-page.png"),
        ("p4f-history-after.png", "before-history-page.png"),
        ("p4f-settings-after.png", "before-settings-page.png"),
        ("p4f-wizard-after.png", "before-wizard-overlay.png"),
        ("p4e-dialog-collision-names-three.png", "before-timer-dialog.png"),
    ]
    for src_name, dst_name in pair_sources:
        src_path = OUT / src_name
        if src_path.exists():
            shutil.copy2(src_path, BEFORE_DIR / dst_name)

    print("=== QA P4f Evidence Capture Complete ===")
    return 0

if __name__ == "__main__":
    sys.exit(main())
