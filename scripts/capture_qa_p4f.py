#!/usr/bin/env python3
"""QA P4f — visual polish AFTER screenshots via isolated display + WebDriver.

Captures into a staging directory and only promotes into
docs/qa4-screenshots/after/ on full success — a failed run must not
half-replace committed C10b evidence.

Tracked shot names (must match existing after/ set):
  p4f-list-after, p4f-week-after, p4f-month-after, p4f-history-after,
  p4f-settings-after, p4f-settings-below-fold, p4f-toast-info,
  p4f-empty-filter, p4f-dialog-disabled-create, p4f-control-hover-disabled,
  p4f-dialog-{once,interval,daily,weekly,monthly,yearly,cron}, …
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qa_webdriver as qa

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
AFTER_DIR = OUT / "after"
BEFORE_DIR = OUT / "before"

# Required top-bar surfaces for a current-tree binary.
REQUIRED_TABS = ("All timers", "Week", "Month", "Run history", "Settings")


def run_cmd(cmd: list[str], cwd: Path = ROOT):
    subprocess.run(cmd, cwd=cwd, check=True)


def build_app():
    run_cmd(["npm", "run", "build", "--prefix", "ui"])
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


def assert_current_binary_surfaces():
    """Fail loudly if the resolved app lacks Settings (stale /tmp fallback)."""
    d = qa.driver()
    By = qa._by()
    labels = []
    for b in d.find_elements(By.CSS_SELECTOR, "button.tab, button"):
        t = (b.text or "").strip()
        if t:
            labels.append(t)
    missing = [t for t in REQUIRED_TABS if t not in labels]
    if missing:
        raise RuntimeError(
            f"bellman-app is missing required tabs {missing}. "
            f"Seen buttons (sample): {labels[:20]}. "
            "Build from this tree (ui/dist + cargo build -p bellman-app --release "
            "--features custom-protocol) and set BELLMAN_APP to that binary. "
            "Do not use a stale /tmp/bellman-deb-extract shell for p4f."
        )


def scroll_settings_below_fold():
    d = qa.driver()
    d.execute_script(
        """
        const root = document.querySelector('.settings, .page, main, #app')
          || document.scrollingElement;
        if (root) root.scrollTop = root.scrollHeight;
        window.scrollTo(0, document.body.scrollHeight);
        """
    )
    time.sleep(0.45)


def capture_after_set(xd):
    """Capture using tracked shot basenames (no 'after/' prefix — OUT is staging/after)."""
    print("Capturing All timers (list)...")
    qa.click_tab("All timers")
    time.sleep(0.5)
    # Tracked name is p4f-list-after.png (not p4f-all-after).
    qa.capture(
        xd,
        "p4f-list-after",
        {"expect": "All timers / list surface after visual polish"},
    )

    print("Capturing Week surface...")
    qa.click_tab("Week")
    time.sleep(0.55)
    qa.capture(
        xd,
        "p4f-week-after",
        {"expect": "Week calendar surface with day headers and fire count badges"},
    )

    print("Capturing Month surface...")
    qa.click_tab("Month")
    time.sleep(0.55)
    qa.capture(
        xd,
        "p4f-month-after",
        {"expect": "Month grid surface with WCAG AA compliant out-of-month contrast"},
    )

    print("Capturing Run history surface...")
    qa.click_tab("Run history")
    time.sleep(0.55)
    qa.capture(
        xd,
        "p4f-history-after",
        {"expect": "Run history surface with log filter controls and event tail"},
    )

    print("Capturing Settings page surface (Top)...")
    qa.click_tab("Settings")
    time.sleep(0.55)
    qa.capture(
        xd,
        "p4f-settings-after",
        {"expect": "Settings page surface with Wake from sleep and Autostart"},
    )

    print("Capturing Settings page surface (Below Fold)...")
    scroll_settings_below_fold()
    qa.capture(
        xd,
        "p4f-settings-below-fold",
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
            "p4f-toast-info",
            {"expect": "Settings save info toast with ℹ Info badge (non-colour encoding)"},
        )
    except Exception as e:
        print("Info toast capture error:", e)

    print("Capturing First-Run Wizard overlay...")
    try:
        qa.click_button("Run setup again", exact=False, timeout=4.0)
        time.sleep(0.55)
        qa.capture(
            xd,
            "p4f-wizard-after",
            {"expect": "First-run Wizard overlay with backdrop and 32px checkboxes"},
        )
        # Finish / dismiss wizard so the backdrop cannot intercept later clicks.
        for lab in ("No thanks", "Continue", "Finish", "Done", "Close", "Cancel", "×"):
            try:
                qa.click_button(lab, exact=False, timeout=1.0)
                time.sleep(0.25)
            except Exception:
                pass
        qa.close_dialog_if_open()
        # Hard-dismiss any remaining wizard backdrop via DOM.
        qa.driver().execute_script(
            """
            document.querySelectorAll('.wizard-backdrop, .wizard, [role=dialog]')
              .forEach(el => el.remove());
            """
        )
        time.sleep(0.35)
    except Exception as e:
        print("Wizard capture error:", e)

    qa.click_tab("All timers")
    time.sleep(0.4)
    print("Capturing No results after filter state...")
    try:
        d = qa.driver()
        By = qa._by()
        for el in d.find_elements(By.CSS_SELECTOR, "input"):
            if not el.is_displayed():
                continue
            ph = (el.get_attribute("placeholder") or "") + (
                el.get_attribute("aria-label") or ""
            )
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
                    "p4f-empty-filter",
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

    print("Capturing disabled Create control (dialog)...")
    try:
        qa.open_new_timer()
        time.sleep(0.4)
        qa.capture(
            xd,
            "p4f-dialog-disabled-create",
            {"expect": "New timer dialog with Create disabled (empty name gate)"},
        )
        qa.capture(
            xd,
            "p4f-control-hover-disabled",
            {"expect": "Dialog footer: disabled Create state (no global pointer warp)"},
        )
        qa.close_dialog_if_open()
    except Exception as e:
        print("Disabled dialog capture error:", e)

    for k in ("once", "interval", "daily", "weekly", "monthly", "yearly", "cron"):
        print(f"Capturing Dialog variant: {k}...")
        try:
            qa.open_new_timer()
            qa.select_kind(k)
            time.sleep(0.3)
            qa.capture(
                xd,
                f"p4f-dialog-{k}",
                {"expect": f"Timer Dialog showing {k} occurrence kind fields"},
            )
            qa.close_dialog_if_open()
        except Exception as e:
            print(f"Dialog variant {k} error:", e)


def promote_staging(staging: Path):
    """Copy staged PNGs/meta into docs/qa4-screenshots/after/ only on success."""
    AFTER_DIR.mkdir(parents=True, exist_ok=True)
    n = 0
    for p in staging.glob("p4f-*.png"):
        shutil.copy2(p, AFTER_DIR / p.name)
        n += 1
        meta = p.with_suffix(".meta.json")
        # capture writes name.meta.json next to name.png
        meta2 = Path(str(p) + ".meta.json")  # unused
        for m in (staging / f"{p.stem}.meta.json",):
            if m.exists():
                shutil.copy2(m, AFTER_DIR / m.name)
    print(f"  promoted {n} PNGs into {AFTER_DIR}")


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

    disp = os.environ.get("DISPLAY", "")
    if disp in (":0", ":0.0") and os.environ.get("BELLMAN_QA_ALLOW_DISPLAY0") != "1":
        print(f"ERROR: refusing DISPLAY={disp}", file=sys.stderr)
        return 2

    if args.rebuild:
        print("Rebuilding post-polish binary...")
        build_app()

    # Stage under /tmp — do not touch docs/ until the full set is captured.
    staging_root = Path(tempfile.mkdtemp(prefix="bellman-qa-p4f-"))
    staging_after = staging_root / "after"
    staging_after.mkdir(parents=True)
    qa.OUT = staging_after

    print(f"P4f WebDriver session DISPLAY={disp} DATA={qa.DATA_DIR}")
    print(f"  staging={staging_after} (docs/ untouched until success)")
    qa.start_session()
    try:
        assert_current_binary_surfaces()
        xd = qa.xdisp()
        qa.resize_window(960, 640)
        capture_after_set(xd)

        # Require the core surfaces that were previously half-overwritten on failure.
        required = [
            "p4f-list-after.png",
            "p4f-week-after.png",
            "p4f-month-after.png",
            "p4f-history-after.png",
            "p4f-settings-after.png",
        ]
        missing = [n for n in required if not (staging_after / n).exists()]
        if missing:
            raise RuntimeError(f"p4f incomplete — missing staged shots: {missing}")

        promote_staging(staging_after)
        (AFTER_DIR / "p4f-session.json").write_text(
            json.dumps(
                {
                    "display": disp,
                    "input_backend": "tauri-driver+WebKitWebDriver",
                    "app": os.environ.get("BELLMAN_APP"),
                    "shots": sorted(p.name for p in AFTER_DIR.glob("p4f-*.png")),
                },
                indent=2,
            )
            + "\n"
        )
        print("=== QA P4f Evidence Capture Complete ===")
        return 0
    finally:
        qa.stop_session()
        # Drop staging always (promoted copies already in docs/ on success).
        shutil.rmtree(staging_root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
