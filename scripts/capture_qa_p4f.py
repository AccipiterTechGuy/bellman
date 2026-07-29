#!/usr/bin/env python3
"""QA P4f — Official capture script for Visual Polish BEFORE & AFTER screenshots.

This script:
1. Generates authentic BEFORE screenshots from pre-polish CSS (git commit 86e3019)
   into docs/qa4-screenshots/before/.
2. Restores updated design system CSS, rebuilds ui/dist and bellman-app.
3. Launches a fresh bellman-app binary on DISPLAY :0 with canonical 960x640 resolution.
4. Captures AFTER surface evidence into docs/qa4-screenshots/ and docs/qa4-screenshots/after/:
   - Main navigation pages (All timers, Week, Month, Run history, Settings top & below-the-fold)
   - First-run Wizard overlay
   - Zero-result empty filter state (typing non-matching query)
   - Toast notification states with text badges
   - All 7 dialog occurrence kind variants (once, interval, daily, weekly, monthly, yearly, cron)

NO mock fakes, NO byte-copying AFTER to BEFORE.
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

def run_cmd(cmd: list[str], cwd: Path = ROOT):
    subprocess.run(cmd, cwd=cwd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

def dismiss_wizard(app):
    try:
        safe_click(app, "Next")
        time.sleep(0.3)
        safe_click(app, "No thanks")
        time.sleep(0.3)
        safe_click(app, "Continue")
        time.sleep(0.4)
    except Exception:
        pass

def restart_app(d) -> tuple:
    # Kill any existing bellman-app
    subprocess.run(["pkill", "-9", "-f", "bellman-app"], stderr=subprocess.DEVNULL)
    time.sleep(0.8)

    # Launch fresh binary
    proc = subprocess.Popen([str(ROOT / "target/release/bellman-app")], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2.0)

    # Resize window to canonical 960x640
    try:
        subprocess.run(["wmctrl", "-r", "Bellman", "-e", "0,100,100,960,640"], check=False)
        time.sleep(0.5)
    except Exception:
        pass

    app = p4b.find_app("bellman-app")
    p4b.raise_and_geom(d)
    time.sleep(0.5)

    # Dismiss wizard if open
    dismiss_wizard(app)

    # Dismiss any open dialog
    p4b.key_tap(d, XK.string_to_keysym("Escape"))
    time.sleep(0.4)
    return app, proc

def safe_click(app, name: str):
    for role in (None, "push button", "button", "page tab"):
        try:
            return p4b.click_named(app, name, role)
        except Exception:
            pass
    raise RuntimeError(f"Could not click {name!r}")

def capture_before_set(d):
    print("\n--- Generating Authentic BEFORE Screenshots ---")
    css_path = ROOT / "ui" / "src" / "styles.css"
    current_css = css_path.read_text()

    try:
        # Checkout pre-polish styles.css from 86e3019
        pre_css = subprocess.check_output(["git", "show", "86e3019:ui/src/styles.css"], text=True)
        css_path.write_text(pre_css)

        # Rebuild frontend
        run_cmd(["npm", "run", "build", "--prefix", "ui"])

        # Restart app with pre-polish CSS
        app, _ = restart_app(d)

        # Capture BEFORE shots
        safe_click(app, "All timers")
        time.sleep(0.5)
        p4b.capture(d, "before-all-timers")
        shutil.copy2(OUT / "before-all-timers.png", BEFORE_DIR / "before-all-timers.png")

        safe_click(app, "Week")
        time.sleep(0.5)
        p4b.capture(d, "before-week-page")
        shutil.copy2(OUT / "before-week-page.png", BEFORE_DIR / "before-week-page.png")

        safe_click(app, "Month")
        time.sleep(0.5)
        p4b.capture(d, "before-month-page")
        shutil.copy2(OUT / "before-month-page.png", BEFORE_DIR / "before-month-page.png")

        safe_click(app, "Run history")
        time.sleep(0.5)
        p4b.capture(d, "before-history-page")
        shutil.copy2(OUT / "before-history-page.png", BEFORE_DIR / "before-history-page.png")

        safe_click(app, "Settings")
        time.sleep(0.5)
        p4b.capture(d, "before-settings-page")
        shutil.copy2(OUT / "before-settings-page.png", BEFORE_DIR / "before-settings-page.png")

        safe_click(app, "Run setup again")
        time.sleep(0.5)
        p4b.capture(d, "before-wizard-overlay")
        shutil.copy2(OUT / "before-wizard-overlay.png", BEFORE_DIR / "before-wizard-overlay.png")
        dismiss_wizard(app)

        safe_click(app, "All timers")
        time.sleep(0.4)
        safe_click(app, "+ New timer")
        time.sleep(0.5)
        p4b.capture(d, "before-timer-dialog")
        shutil.copy2(OUT / "before-timer-dialog.png", BEFORE_DIR / "before-timer-dialog.png")
        safe_click(app, "Cancel")
        time.sleep(0.4)

        print("BEFORE screenshot generation complete.")

    finally:
        # Restore current design system CSS
        css_path.write_text(current_css)
        run_cmd(["npm", "run", "build", "--prefix", "ui"])

def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    AFTER_DIR.mkdir(parents=True, exist_ok=True)
    BEFORE_DIR.mkdir(parents=True, exist_ok=True)

    # Remove obsolete files
    for old_f in ["p4f-history-page.png", "p4f-settings-page.png", "p4f-wizard-overlay.png",
                  "p4f-history-page.meta.json", "p4f-settings-page.meta.json", "p4f-wizard-overlay.meta.json"]:
        p = OUT / old_f
        if p.exists():
            p.unlink()

    d = p4b.xdisp()

    # Step 1: Capture BEFORE set from pre-polish CSS
    capture_before_set(d)

    # Step 2: Capture AFTER set with current updated code
    print("\n--- Capturing AFTER Screenshots from Fresh Build ---")
    app, proc = restart_app(d)

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
    dismiss_wizard(app)

    # 3. All Timers Filter & Zero-Result Empty State
    safe_click(app, "All timers")
    time.sleep(0.4)

    print("Capturing No results after filter state...")
    try:
        search_inputs = p4b.walk_find(app, lambda a: "Filter" in (a.name or "") or a.getRoleName() in ("entry", "text"))
        if search_inputs:
            p4b.do_action(search_inputs[0])
            time.sleep(0.3)
            p4b.type_string(d, "nonexistent_query_xyz_123")
            time.sleep(0.8)
            p = p4b.capture(d, "p4f-empty-filter", {"expect": "Zero-result empty filter state showing 0 of 7 timers"})
            shutil.copy2(p, AFTER_DIR / p.name)
            p4b.key_tap(d, XK.string_to_keysym("a"), ctrl=True)
            p4b.key_tap(d, XK.string_to_keysym("BackSpace"))
            p4b.key_tap(d, XK.string_to_keysym("Return"))
            time.sleep(0.4)
    except Exception as e:
        print("Filter state error:", e)

    # 4. Timer Dialog Occurrence Kinds (once, interval, daily, weekly, monthly, yearly, cron)
    kind_indices = {
        "once": 0,
        "interval": 1,
        "daily": 2,
        "weekly": 3,
        "monthly": 4,
        "yearly": 5,
        "cron": 6,
    }

    for k, index in kind_indices.items():
        print(f"Capturing Dialog variant: {k} (index {index})...")
        try:
            safe_click(app, "+ New timer")
            time.sleep(0.6)

            # Focus Occurrence kind select box and change option using keyboard
            kind_combos = p4b.walk_find(app, lambda a: a.getRoleName() in ("combo box", "drop down list") or "Occurrence" in (a.name or ""))
            if kind_combos:
                p4b.do_action(kind_combos[0])
                time.sleep(0.2)
                # Press Home then Down index times
                p4b.key_tap(d, XK.string_to_keysym("Home"))
                time.sleep(0.1)
                for _ in range(index):
                    p4b.key_tap(d, XK.string_to_keysym("Down"))
                    time.sleep(0.1)
                p4b.key_tap(d, XK.string_to_keysym("Return"))
                time.sleep(0.3)

            p = p4b.capture(d, f"p4f-dialog-{k}", {"expect": f"Timer Dialog showing {k} occurrence kind fields"})
            shutil.copy2(p, AFTER_DIR / p.name)

            safe_click(app, "Cancel")
            time.sleep(0.4)
        except Exception as e:
            print(f"Dialog variant {k} error:", e)

    print("=== QA P4f Evidence Capture Complete ===")
    return 0

if __name__ == "__main__":
    sys.exit(main())
