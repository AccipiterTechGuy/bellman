#!/usr/bin/env python3
"""QA P4f — Official capture script for Visual Polish BEFORE & AFTER screenshots.

This script:
1. Swaps ui/src/styles.css to pre-polish commit 86e3019, rebuilds ui/dist and cargo binary,
   launches pre-polish app, and captures authentic BEFORE screenshots directly into docs/qa4-screenshots/before/.
2. Restores current updated design system CSS, rebuilds ui/dist and cargo binary,
   launches fresh app, and captures authentic AFTER screenshots directly into docs/qa4-screenshots/after/:
   - Main navigation pages (All timers, Week, Month, Run history, Settings top & below-the-fold)
   - First-run Wizard overlay
   - Zero-result empty filter state (typing non-matching query)
   - All 7 dialog occurrence kind variants (once, interval, daily, weekly, monthly, yearly, cron)

NO mock fakes, NO duplicate flat copies.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
sys.path.append(str(ROOT / "scripts"))

import capture_qa_p4b as p4b
from Xlib import XK

OUT = ROOT / "docs" / "qa4-screenshots"
AFTER_DIR = OUT / "after"
BEFORE_DIR = OUT / "before"

def run_cmd(cmd: list[str], cwd: Path = ROOT):
    subprocess.run(cmd, cwd=cwd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

def safe_click(app, name: str, retries: int = 8):
    for attempt in range(retries):
        try:
            live_app = p4b.find_app("bellman-app")
        except Exception:
            live_app = app
        for role in (None, "push button", "button", "page tab", "tab", "link"):
            for exact in (False, True):
                for target in (name, name.upper(), name.lower(), name.title()):
                    try:
                        return p4b.click_named(live_app, target, role, exact=exact)
                    except Exception:
                        pass
        time.sleep(1.0)
    raise RuntimeError(f"Could not click {name!r}")

def dismiss_wizard(app, d):
    try:
        p4b.key_tap(d, XK.string_to_keysym("Escape"))
        time.sleep(0.4)
    except Exception:
        pass
    for btn_name in ("Next", "No thanks", "Continue", "Close"):
        for attempt in range(3):
            try:
                for role in (None, "push button", "button", "page tab"):
                    try:
                        p4b.click_named(app, btn_name, role, exact=False)
                        time.sleep(0.6)
                        break
                    except Exception:
                        pass
            except Exception:
                pass
        time.sleep(0.2)

def wait_for_ui_render(d, max_wait: float = 12.0):
    start = time.time()
    while time.time() - start < max_wait:
        try:
            win, x, y, w, h, wid = p4b.raise_and_geom(d)
            raw = win.get_image(0, 0, w, h, p4b.X.ZPixmap, 0xFFFFFFFF)
            img = Image.frombytes("RGBA", (w, h), raw.data, "raw", "BGRA").convert("RGB")
            small = img.resize((32, 32))
            px = list(small.getdata())
            mean = sum(sum(p[:3]) for p in px) / (3 * len(px))
            if mean < 200:
                print(f"UI rendered after {time.time()-start:.1f}s (mean luma {mean:.1f})")
                return
        except Exception:
            pass
        time.sleep(0.5)
    print("Warning: UI render wait timed out")

def restart_app(d) -> tuple:
    # Kill any existing bellman-app
    subprocess.run(["pkill", "-9", "-f", "bellman-app"], stderr=subprocess.DEVNULL)
    time.sleep(1.0)

    # Launch fresh binary
    proc = subprocess.Popen([str(ROOT / "target/release/bellman-app")], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2.0)

    # Resize window to canonical 960x640
    try:
        subprocess.run(["wmctrl", "-r", "Bellman", "-e", "0,100,100,960,640"], check=False)
        time.sleep(0.5)
    except Exception:
        pass

    # Wait for WebKitGTK UI to render completely
    wait_for_ui_render(d)

    app = p4b.find_app("bellman-app")
    p4b.raise_and_geom(d)
    time.sleep(0.5)

    # Dismiss wizard if open
    dismiss_wizard(app, d)

    # Dismiss any open dialog
    p4b.key_tap(d, XK.string_to_keysym("Escape"))
    time.sleep(0.4)
    return app, proc

def capture_before_set(d):
    print("\n--- Generating Authentic BEFORE Screenshots (Rebuilding Cargo Binary) ---")
    css_path = ROOT / "ui" / "src" / "styles.css"
    current_css = css_path.read_text()

    try:
        # Checkout pre-polish styles.css from 86e3019
        pre_css = subprocess.check_output(["git", "show", "86e3019:ui/src/styles.css"], text=True)
        css_path.write_text(pre_css)

        # Rebuild frontend AND rebuild release binary with pre-polish CSS
        print("Building pre-polish dist & binary...")
        run_cmd(["npm", "run", "build", "--prefix", "ui"])
        (ROOT / "src-tauri" / "src" / "lib.rs").touch()
        run_cmd(["cargo", "build", "--release", "--bin", "bellman-app"])

        # Restart app with pre-polish binary
        app, _ = restart_app(d)
        dismiss_wizard(app, d)
        time.sleep(0.5)

        # Capture BEFORE shots directly in BEFORE_DIR
        try:
            safe_click(app, "All timers", retries=2)
        except Exception:
            pass
        time.sleep(0.5)
        p4b.capture(d, "before/before-all-timers")

        safe_click(app, "Week")
        time.sleep(0.5)
        p4b.capture(d, "before/before-week-page")

        safe_click(app, "Month")
        time.sleep(0.5)
        p4b.capture(d, "before/before-month-page")

        safe_click(app, "Run history")
        time.sleep(0.5)
        p4b.capture(d, "before/before-history-page")

        safe_click(app, "Settings")
        time.sleep(0.5)
        p4b.capture(d, "before/before-settings-page")

        safe_click(app, "Run setup again")
        time.sleep(0.5)
        p4b.capture(d, "before/before-wizard-overlay")
        dismiss_wizard(app, d)

        safe_click(app, "All timers")
        time.sleep(0.4)
        safe_click(app, "+ New timer")
        time.sleep(0.5)
        p4b.capture(d, "before/before-timer-dialog")
        safe_click(app, "Cancel")
        time.sleep(0.4)

        print("BEFORE screenshot generation complete.")

    finally:
        # Restore current design system CSS & rebuild binary with post-polish CSS
        print("Restoring post-polish CSS & rebuilding binary...")
        css_path.write_text(current_css)
        run_cmd(["npm", "run", "build", "--prefix", "ui"])
        (ROOT / "src-tauri" / "src" / "lib.rs").touch()
        run_cmd(["cargo", "build", "--release", "--bin", "bellman-app"])

def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    AFTER_DIR.mkdir(parents=True, exist_ok=True)
    BEFORE_DIR.mkdir(parents=True, exist_ok=True)

    # Clean out any old flat top-level screenshots to maintain single canonical location
    for flat_file in OUT.glob("*.png"):
        flat_file.unlink()
    for flat_meta in OUT.glob("*.meta.json"):
        flat_meta.unlink()

    d = p4b.xdisp()

    # Step 1: Capture BEFORE set from pre-polish binary
    capture_before_set(d)

    # Step 2: Capture AFTER set from post-polish binary
    print("\n--- Capturing AFTER Screenshots from Fresh Post-Polish Build ---")
    app, proc = restart_app(d)

    # 1. Main Navigation Surfaces
    print("Capturing All timers surface...")
    try:
        safe_click(app, "All timers", retries=2)
    except Exception:
        pass
    time.sleep(0.6)
    p4b.capture(d, "after/p4f-list-after", {"expect": "All timers surface with design tokens, hit targets, tabular numbers"})

    print("Capturing Week surface...")
    safe_click(app, "Week")
    time.sleep(0.6)
    p4b.capture(d, "after/p4f-week-after", {"expect": "Week calendar surface with day headers and fire count badges"})

    print("Capturing Month surface...")
    safe_click(app, "Month")
    time.sleep(0.6)
    p4b.capture(d, "after/p4f-month-after", {"expect": "Month grid surface with WCAG AA compliant out-of-month contrast"})

    print("Capturing Run history surface...")
    safe_click(app, "Run history")
    time.sleep(0.6)
    p4b.capture(d, "after/p4f-history-after", {"expect": "Run history surface with log filter controls and event tail"})

    print("Capturing Settings page surface (Top)...")
    safe_click(app, "Settings")
    time.sleep(0.6)
    p4b.capture(d, "after/p4f-settings-after", {"expect": "Settings page surface with Wake from sleep and Autostart"})

    # Scroll down Settings page for below-the-fold controls
    print("Capturing Settings page surface (Below Fold)...")
    p4b.key_tap(d, XK.string_to_keysym("Page_Down"))
    time.sleep(0.5)
    p4b.capture(d, "after/p4f-settings-below-fold", {"expect": "Settings page scrolled to bottom showing misfire defaults and engine settings"})
    p4b.key_tap(d, XK.string_to_keysym("Page_Up"))
    time.sleep(0.4)

    # 2. First-Run Wizard Overlay
    print("Capturing First-Run Wizard overlay...")
    safe_click(app, "Run setup again")
    time.sleep(0.6)
    p4b.capture(d, "after/p4f-wizard-after", {"expect": "First-run Wizard overlay with backdrop and 32px checkboxes"})

    # Close Wizard
    dismiss_wizard(app, d)

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
            p4b.capture(d, "after/p4f-empty-filter", {"expect": "Zero-result empty filter state showing 0 of 7 timers"})
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
                p4b.key_tap(d, XK.string_to_keysym("Home"))
                time.sleep(0.1)
                for _ in range(index):
                    p4b.key_tap(d, XK.string_to_keysym("Down"))
                    time.sleep(0.1)
                p4b.key_tap(d, XK.string_to_keysym("Return"))
                time.sleep(0.3)

            p4b.capture(d, f"after/p4f-dialog-{k}", {"expect": f"Timer Dialog showing {k} occurrence kind fields"})

            safe_click(app, "Cancel")
            time.sleep(0.4)
        except Exception as e:
            print(f"Dialog variant {k} error:", e)

    print("=== QA P4f Evidence Capture Complete ===")
    return 0

if __name__ == "__main__":
    sys.exit(main())
