#!/usr/bin/env python3
"""QA P4f — Official capture script for Visual Polish BEFORE & AFTER screenshots.

This script:
1. Swaps ui/src/styles.css to pre-polish commit 86e3019, rebuilds ui/dist and cargo release binary,
   launches pre-polish app, and captures authentic BEFORE screenshots into docs/qa4-screenshots/before/.
2. Restores post-polish ui/src/styles.css from git HEAD, rebuilds ui/dist and cargo release binary,
   launches post-polish app, and captures authentic AFTER screenshots into docs/qa4-screenshots/after/:
   - Main navigation pages (All timers, Week, Month, Run history, Settings top & below-the-fold)
   - First-run Wizard overlay
   - Zero-result empty filter state (typing non-matching query)
   - All 7 dialog occurrence kind variants (once, interval, daily, weekly, monthly, yearly, cron)
"""
from __future__ import annotations

import json
import os
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

def build_app():
    """Build production shell with embedded ui/dist (NOT devUrl/localhost:1420).

    Plain `cargo build --release` without `custom-protocol` bakes in devUrl and the
    window shows "Could not connect to localhost: Connection refused" when Vite is
    not running. Always enable custom-protocol for capture binaries.
    """
    run_cmd(["npm", "run", "build", "--prefix", "ui"])
    run_cmd(["cargo", "clean", "-p", "bellman-app", "--manifest-path", "src-tauri/Cargo.toml"])
    run_cmd(
        [
            "cargo",
            "build",
            "--release",
            "--features",
            "custom-protocol",
            "--manifest-path",
            "src-tauri/Cargo.toml",
        ]
    )

def safe_click(app, name: str, retries: int = 8):
    for attempt in range(retries):
        try:
            live_app = p4b.find_app("bellman-app")
        except Exception:
            live_app = app
        for target in (name, name.upper(), name.lower(), name.title()):
            for role in (None, "push button", "button", "page tab", "tab", "link", "section", "label"):
                for exact in (False, True):
                    try:
                        return p4b.click_named(live_app, target, role, exact=exact)
                    except Exception:
                        pass
        time.sleep(0.5)
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

def wait_for_ui_render(d, max_wait: float = 15.0):
    start = time.time()
    while time.time() - start < max_wait:
        try:
            win, x, y, w, h, wid = p4b.raise_and_geom(d)
            if w > 200 and h > 200:
                raw = win.get_image(0, 0, w, h, p4b.X.ZPixmap, 0xFFFFFFFF)
                img = Image.frombytes("RGBA", (w, h), raw.data, "raw", "BGRA").convert("RGB")
                small = img.resize((32, 32))
                px = list(small.getdata())
                mean = sum(sum(p[:3]) for p in px) / (3 * len(px))
                print(f"DEBUG LUMA: {mean:.1f}", flush=True)
                if mean < 200:
                    print(f"UI rendered after {time.time()-start:.1f}s (mean luma {mean:.1f})")
                    return
        except Exception as e:
            print(f"LUMA EXCEPTION: {e}", flush=True)
        time.sleep(0.5)
    print("Warning: UI render wait timed out")

def restart_app(d) -> tuple:
    # Kill only the exact binary name — never pkill -f (matches shell/cmdlines).
    subprocess.run(["killall", "-9", "bellman-app"], stderr=subprocess.DEVNULL)
    time.sleep(1.0)

    # Launch fresh production binary (custom-protocol + frontendDist assets).
    env = os.environ.copy()
    env.setdefault("DISPLAY", os.environ.get("DISPLAY", ":0"))
    proc = subprocess.Popen(
        [str(ROOT / "target/release/bellman-app")],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env=env,
        cwd=str(ROOT),
    )
    time.sleep(2.5)

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

    try:
        # Checkout pre-polish styles.css from 86e3019
        pre_css = subprocess.check_output(["git", "show", "86e3019:ui/src/styles.css"], text=True)
        css_path.write_text(pre_css)

        print("Building pre-polish dist & binary...")
        build_app()

        # Restart app with pre-polish binary
        app, _ = restart_app(d)
        time.sleep(3.0)
        dismiss_wizard(app, d)
        time.sleep(1.0)

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
        # Always restore HEAD post-polish CSS & rebuild binary
        print("Restoring HEAD post-polish CSS & rebuilding binary...")
        subprocess.run(["git", "checkout", "HEAD", "--", "ui/src/styles.css"], check=True)
        build_app()

def force_select_labels(app, d):
    """Open each filter combo and re-select current value so native WebKitGTK
    <select> text is painted (avoids blank Sort option after first paint)."""
    combos = p4b.walk_find(
        app, lambda a: a.getRoleName() in ("combo box", "drop down list", "list box")
    )
    for c in combos[:4]:
        try:
            p4b.do_action(c)
            time.sleep(0.15)
            p4b.key_tap(d, XK.string_to_keysym("Return"))
            time.sleep(0.2)
        except Exception:
            try:
                p4b.key_tap(d, XK.string_to_keysym("Escape"))
            except Exception:
                pass


def capture_after_set(d, app):
    """Capture all AFTER surfaces from an already-running post-polish app."""
    # 1. Main Navigation Surfaces
    print("Capturing All timers surface...")
    try:
        safe_click(app, "All timers", retries=2)
    except Exception:
        pass
    time.sleep(1.0)
    force_select_labels(app, d)
    time.sleep(0.5)
    p4b.capture(
        d,
        "after/p4f-list-after",
        {"expect": "All timers surface with design tokens, hit targets, tabular numbers, Sort label visible"},
    )

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

    # Scroll Settings content with mouse wheel (Page_Down often misses the overflow div).
    print("Capturing Settings page surface (Below Fold)...")
    try:
        from Xlib import X as Xcore
        from Xlib.ext import xtest

        win, x, y, w, h, wid = p4b.raise_and_geom(d)
        xtest.fake_input(d, Xcore.MotionNotify, x=x + w // 2, y=y + 350)
        d.sync()
        time.sleep(0.1)
        for _ in range(25):
            xtest.fake_input(d, Xcore.ButtonPress, 5)  # wheel down
            xtest.fake_input(d, Xcore.ButtonRelease, 5)
            d.sync()
            time.sleep(0.04)
        time.sleep(0.5)
    except Exception as e:
        print("Settings wheel-scroll fallback:", e)
        for _ in range(4):
            p4b.key_tap(d, XK.string_to_keysym("Page_Down"))
            time.sleep(0.15)
    p4b.capture(
        d,
        "after/p4f-settings-below-fold",
        {"expect": "Settings page scrolled to bottom showing misfire defaults and engine settings"},
    )

    # Toast (info): Save a Settings control so "Settings saved" / misfire save shows ℹ badge
    print("Capturing info toast after Settings save...")
    try:
        for btn in ("Save",):
            try:
                safe_click(app, btn, retries=3)
                break
            except Exception:
                pass
        time.sleep(0.7)
        p4b.capture(
            d,
            "after/p4f-toast-info",
            {"expect": "Settings save info toast with ℹ Info badge (non-colour encoding)"},
        )
    except Exception as e:
        print("Info toast capture error:", e)

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
    time.sleep(0.5)
    force_select_labels(app, d)
    time.sleep(0.3)

    print("Capturing No results after filter state...")
    try:
        search_inputs = p4b.walk_find(
            app, lambda a: "Filter" in (a.name or "") or a.getRoleName() in ("entry", "text")
        )
        if search_inputs:
            p4b.do_action(search_inputs[0])
            time.sleep(0.3)
            p4b.type_string(d, "nonexistent_query_xyz_123")
            time.sleep(0.8)
            p4b.capture(
                d,
                "after/p4f-empty-filter",
                {"expect": "Zero-result empty filter state showing 0 of N timers and Sort label"},
            )
            p4b.key_tap(d, XK.string_to_keysym("a"), ctrl=True)
            p4b.key_tap(d, XK.string_to_keysym("BackSpace"))
            p4b.key_tap(d, XK.string_to_keysym("Return"))
            time.sleep(0.4)
    except Exception as e:
        print("Filter state error:", e)

    # 4. Disabled Create + hover primary (dialog open, empty name => Create disabled)
    print("Capturing disabled Create control (dialog)...")
    try:
        safe_click(app, "+ New timer")
        time.sleep(0.7)
        # Leave Name empty so canSave is false → Create disabled (opacity 0.5)
        p4b.capture(
            d,
            "after/p4f-dialog-disabled-create",
            {"expect": "New timer dialog with Create disabled (empty name gate)"},
        )
        # Hover primary button region (bottom-right of dialog) for :hover paint
        try:
            win, x, y, w, h, wid = p4b.raise_and_geom(d)
            # Primary Create sits near lower-right of 960x640 shell
            from Xlib import X as Xcore
            from Xlib.ext import xtest

            hx, hy = x + w - 90, y + h - 40
            xtest.fake_input(d, Xcore.MotionNotify, x=hx, y=hy)
            d.sync()
            time.sleep(0.35)
            p4b.capture(
                d,
                "after/p4f-control-hover-disabled",
                {"expect": "Dialog footer: hovered Cancel/Create area + disabled Create state"},
            )
        except Exception as e:
            print("Hover capture error:", e)
        safe_click(app, "Cancel")
        time.sleep(0.4)
    except Exception as e:
        print("Disabled dialog capture error:", e)

    # 5. Error toast: force a failed run-now if possible, else pause-all toggle twice is info;
    #    use invalid backend path via Run now on a known timer is usually ok.
    #    Prefer: open dialog, set bad cron, attempt — but Create stays disabled.
    #    Use Settings re-probe path is hard. Capture pause toggle as secondary info if needed.
    #    Best available without behaviour change: invoke Run now then immediately another —
    #    errors surface via pushToast(..., 'err') on API failures.
    print("Capturing error toast if API error surfaces...")
    try:
        # Kill data path temporarily is too invasive. Click Log on first row is fine.
        # Trigger error by typing garbage into search is not an error toast.
        # Use "Run now" rapidly while engine paused can still succeed.
        # Fallback: toggle Running (pause-all) — produces info toast; if engine errors, err toast.
        try:
            safe_click(app, "Running", retries=3)
            time.sleep(0.5)
            safe_click(app, "Paused", retries=2)
            time.sleep(0.5)
        except Exception:
            try:
                safe_click(app, "Running", retries=2)
                time.sleep(0.6)
            except Exception:
                pass
        # Also try Save on settings with no change still produces info toast — already captured.
        # Attempt Run now for error-badge visibility if action fails:
        try:
            safe_click(app, "Run now", retries=3)
            time.sleep(0.8)
        except Exception:
            pass
        p4b.capture(
            d,
            "after/p4f-toast-or-action",
            {"expect": "Post-action surface; toast present when engine reports status/error"},
        )
    except Exception as e:
        print("Error-toast path:", e)

    # 6. Timer Dialog Occurrence Kinds (once, interval, daily, weekly, monthly, yearly, cron)
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
            kind_combos = p4b.walk_find(
                app,
                lambda a: a.getRoleName() in ("combo box", "drop down list")
                or "Occurrence" in (a.name or ""),
            )
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


def main() -> int:
    import argparse

    parser = argparse.ArgumentParser(description="QA P4f visual polish capture")
    parser.add_argument(
        "--after-only",
        action="store_true",
        help="Skip BEFORE rebuild; capture AFTER set from current release binary only",
    )
    parser.add_argument(
        "--rebuild",
        action="store_true",
        help="With --after-only, rebuild ui/dist + release binary first",
    )
    args = parser.parse_args()

    OUT.mkdir(parents=True, exist_ok=True)
    AFTER_DIR.mkdir(parents=True, exist_ok=True)
    BEFORE_DIR.mkdir(parents=True, exist_ok=True)

    d = p4b.xdisp()

    if not args.after_only:
        # Step 1: Capture BEFORE set from pre-polish binary
        capture_before_set(d)
    elif args.rebuild:
        print("Rebuilding post-polish binary for AFTER captures...")
        build_app()

    # Step 2: Capture AFTER set from post-polish binary
    print("\n--- Capturing AFTER Screenshots from Fresh Post-Polish Build ---")
    app, proc = restart_app(d)
    capture_after_set(d, app)

    print("=== QA P4f Evidence Capture Complete ===")
    return 0


if __name__ == "__main__":
    sys.exit(main())
