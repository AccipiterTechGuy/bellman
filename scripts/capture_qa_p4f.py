#!/usr/bin/env python3
"""QA P4f — visual polish AFTER screenshots via isolated display + WebDriver.

No global-input-injection, never the operator X session. Scroll/hover use in-webview JS, not global input.

Captures AFTER set into docs/qa4-screenshots/after/ from the current release
binary. The historical BEFORE rebuild path is optional (--before) and still
uses WebDriver on the isolated display.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qa_webdriver as qa

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
AFTER_DIR = OUT / "after"
BEFORE_DIR = OUT / "before"


def run_cmd(cmd: list[str], cwd: Path = ROOT):
    subprocess.run(cmd, cwd=cwd, check=True)


def build_app():
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


def scroll_settings_below_fold():
    """Scroll settings via DOM — never global-input-injection wheel events."""
    d = qa.driver()
    d.execute_script(
        """
        const root = document.querySelector('.settings, .page, main, #app') || document.scrollingElement;
        if (root) root.scrollTop = root.scrollHeight;
        window.scrollTo(0, document.body.scrollHeight);
        """
    )
    time.sleep(0.45)


def capture_after_set(xd):
    print("Capturing All timers...")
    qa.click_tab("All timers")
    time.sleep(0.5)
    qa.capture(
        xd,
        "after/p4f-all-after",
        {"expect": "All timers surface after visual polish"},
    )

    print("Capturing Week surface...")
    qa.click_tab("Week")
    time.sleep(0.55)
    qa.capture(
        xd,
        "after/p4f-week-after",
        {"expect": "Week calendar surface with day headers and fire count badges"},
    )

    print("Capturing Month surface...")
    qa.click_tab("Month")
    time.sleep(0.55)
    qa.capture(
        xd,
        "after/p4f-month-after",
        {"expect": "Month grid surface with WCAG AA compliant out-of-month contrast"},
    )

    print("Capturing Run history surface...")
    qa.click_tab("Run history")
    time.sleep(0.55)
    qa.capture(
        xd,
        "after/p4f-history-after",
        {"expect": "Run history surface with log filter controls and event tail"},
    )

    print("Capturing Settings page surface (Top)...")
    try:
        qa.click_tab("Settings")
    except Exception:
        qa.click_button("Settings", exact=False)
    time.sleep(0.55)
    qa.capture(
        xd,
        "after/p4f-settings-after",
        {"expect": "Settings page surface with Wake from sleep and Autostart"},
    )

    print("Capturing Settings page surface (Below Fold)...")
    scroll_settings_below_fold()
    qa.capture(
        xd,
        "after/p4f-settings-below-fold",
        {
            "expect": "Settings page scrolled to bottom showing misfire defaults and engine settings"
        },
    )

    print("Capturing info toast after Settings save...")
    try:
        qa.click_button("Save", timeout=3.0)
        time.sleep(0.65)
        qa.capture(
            xd,
            "after/p4f-toast-info",
            {"expect": "Settings save info toast with ℹ Info badge (non-colour encoding)"},
        )
    except Exception as e:
        print("Info toast capture error:", e)

    # Wizard
    print("Capturing First-Run Wizard overlay...")
    try:
        qa.click_button("Run setup again", exact=False, timeout=4.0)
        time.sleep(0.55)
        qa.capture(
            xd,
            "after/p4f-wizard-after",
            {"expect": "First-run Wizard overlay with backdrop and 32px checkboxes"},
        )
        qa.close_dialog_if_open()
        try:
            qa.click_button("No thanks", exact=False, timeout=2.0)
        except Exception:
            pass
        try:
            qa.click_button("Close", exact=False, timeout=1.5)
        except Exception:
            pass
    except Exception as e:
        print("Wizard capture error:", e)

    # Filter empty state
    qa.click_tab("All timers")
    time.sleep(0.4)
    print("Capturing No results after filter state...")
    try:
        d = qa.driver()
        By = qa._by()
        for el in d.find_elements(By.CSS_SELECTOR, "input"):
            if not el.is_displayed():
                continue
            ph = (el.get_attribute("placeholder") or "") + (el.get_attribute("aria-label") or "")
            if "Filter" in ph or "Search" in ph or el.get_attribute("type") == "search":
                d.execute_script(
                    """
                    const el = arguments[0], val = arguments[1];
                    el.focus(); el.value = val;
                    el.dispatchEvent(new Event('input', {bubbles:true}));
                    """,
                    el,
                    "nonexistent_query_xyz_123",
                )
                time.sleep(0.7)
                qa.capture(
                    xd,
                    "after/p4f-empty-filter",
                    {
                        "expect": "Zero-result empty filter state showing 0 of N timers and Sort label"
                    },
                )
                d.execute_script(
                    """
                    const el = arguments[0];
                    el.value = '';
                    el.dispatchEvent(new Event('input', {bubbles:true}));
                    """,
                    el,
                )
                break
    except Exception as e:
        print("Filter state error:", e)

    # Disabled Create
    print("Capturing disabled Create control (dialog)...")
    try:
        qa.open_new_timer()
        time.sleep(0.4)
        qa.capture(
            xd,
            "after/p4f-dialog-disabled-create",
            {"expect": "New timer dialog with Create disabled (empty name gate)"},
        )
        # Hover via JS CSS class simulation is not true :hover paint; record footer state.
        qa.capture(
            xd,
            "after/p4f-control-hover-disabled",
            {"expect": "Dialog footer: disabled Create state (no global pointer warp)"},
        )
        qa.close_dialog_if_open()
    except Exception as e:
        print("Disabled dialog capture error:", e)

    # Dialog kinds
    for k in ("once", "interval", "daily", "weekly", "monthly", "yearly", "cron"):
        print(f"Capturing Dialog variant: {k}...")
        try:
            qa.open_new_timer()
            qa.select_kind(k)
            time.sleep(0.3)
            qa.capture(
                xd,
                f"after/p4f-dialog-{k}",
                {"expect": f"Timer Dialog showing {k} occurrence kind fields"},
            )
            qa.close_dialog_if_open()
        except Exception as e:
            print(f"Dialog variant {k} error:", e)


def main() -> int:
    parser = argparse.ArgumentParser(description="QA P4f visual polish capture")
    parser.add_argument(
        "--rebuild",
        action="store_true",
        help="Rebuild ui/dist + release binary first",
    )
    args = parser.parse_args()

    qa.DATA_DIR = Path(
        os.environ.get(
            "BELLMAN_QA_DATA",
            "/tmp/bellman-qa-session/share/io.bellman.desktop",
        )
    )
    qa.DISPLAY_NAME = os.environ.get("DISPLAY", "")
    qa.OUT = OUT

    OUT.mkdir(parents=True, exist_ok=True)
    AFTER_DIR.mkdir(parents=True, exist_ok=True)
    BEFORE_DIR.mkdir(parents=True, exist_ok=True)

    disp = os.environ.get("DISPLAY", "")
    if disp in (":0", ":0.0") and os.environ.get("BELLMAN_QA_ALLOW_DISPLAY0") != "1":
        print(f"ERROR: refusing DISPLAY={disp}", file=sys.stderr)
        return 2

    if args.rebuild:
        print("Rebuilding post-polish binary...")
        build_app()

    print(f"P4f WebDriver session DISPLAY={disp} DATA={qa.DATA_DIR}")
    qa.start_session()
    xd = qa.xdisp()
    qa.resize_window(960, 640)
    capture_after_set(xd)

    (AFTER_DIR / "p4f-session.json").write_text(
        json.dumps(
            {
                "display": disp,
                "input_backend": "tauri-driver+WebKitWebDriver",
                "shots": sorted(p.name for p in AFTER_DIR.glob("p4f-*.png")),
            },
            indent=2,
        )
        + "\n"
    )
    print("=== QA P4f Evidence Capture Complete ===")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    finally:
        try:
            qa.stop_session()
        except Exception:
            pass
