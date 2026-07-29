#!/usr/bin/env python3
"""QA P4b — real WebKitGTK GUI evidence for the C8 calendar UI.

Drives the Tauri/WebKitGTK app on an *isolated* Xvfb display via
tauri-driver + WebKitWebDriver (in-webview clicks/typing). Screenshots use
Xlib GetImage on that same display (read-only).

NEVER injects synthetic input into the operator's real X session.
NEVER defaults to the operator X session.

Prerequisites: scripts/qa_display.sh start + WebKitWebDriver + tauri-driver.
See docs/QA_P4b.md and docs/BUILD_PLAN.md ("to RUN the GUI test suite").

Outputs under docs/qa4-screenshots/ and docs/qa4-evidence/.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

# Shared WebDriver harness (no global-input-injection).
import qa_webdriver as qa

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
EVIDENCE = ROOT / "docs" / "qa4-evidence"

# Re-export for p4d/p4e/p4f importers that still reference module attrs.
DATA_DIR = qa.DATA_DIR
DISPLAY_NAME = qa.DISPLAY_NAME
CLI_BIN = qa.CLI_BIN
KINDS = qa.KINDS
KindSpec = qa.KindSpec


def main() -> int:
    global DATA_DIR, DISPLAY_NAME, CLI_BIN, OUT, EVIDENCE

    # Honour env set by qa_display.sh / run_gui_qa.sh
    qa.DATA_DIR = Path(
        os.environ.get(
            "BELLMAN_QA_DATA",
            str(qa.DATA_DIR),
        )
    )
    qa.DISPLAY_NAME = os.environ.get("DISPLAY", qa.DISPLAY_NAME)
    qa.OUT = OUT
    qa.EVIDENCE = EVIDENCE
    if os.environ.get("BELLMAN_CLI"):
        qa.CLI_BIN = os.environ["BELLMAN_CLI"]
    DATA_DIR = qa.DATA_DIR
    DISPLAY_NAME = qa.DISPLAY_NAME
    CLI_BIN = qa.CLI_BIN

    OUT.mkdir(parents=True, exist_ok=True)
    EVIDENCE.mkdir(parents=True, exist_ok=True)

    # Guard: refuse operator display.
    disp = os.environ.get("DISPLAY", "")
    if disp in (":0", ":0.0") and os.environ.get("BELLMAN_QA_ALLOW_DISPLAY0") != "1":
        print(
            f"ERROR: refusing DISPLAY={disp} (operator session). "
            "Run via scripts/run_gui_qa.sh or scripts/qa_display.sh.",
            file=sys.stderr,
        )
        return 2

    print(f"P4b WebDriver session DISPLAY={disp} DATA={DATA_DIR}")
    qa.start_session()
    d = qa.xdisp()
    pids = qa.webkit_pids()
    print("engine pids:", json.dumps(pids, indent=2))
    (EVIDENCE / "webkit_pids.json").write_text(json.dumps(pids, indent=2) + "\n")

    ua_info = qa.capture_user_agent(EVIDENCE)

    # --- 0. baseline All page ---
    qa.close_dialog_if_open()
    qa.click_tab("All timers")
    qa.capture(d, "p4b-all-empty", {"phase": "baseline", "webkit": pids})

    # --- 1. DST gap warning ---
    print("\n== DST gap warning dialog ==")
    qa.open_new_timer()
    qa.select_kind("once")
    qa.fill_fields(
        [
            ("Name", "qa-dst-gap"),
            ("Timezone", "Europe/Helsinki"),
            ("When", "2027-03-28T03:30:00"),
        ]
    )
    time.sleep(1.4)
    qa.capture(d, "p4b-dialog-dst-gap", {"phase": "dst-warning"})
    qa.click_button("Create")
    time.sleep(1.0)

    # --- 2. Create all seven KINDS ---
    for spec in qa.KINDS:
        snap = None
        if spec.kind_prefix == "weekly":
            snap = "p4b-dialog-weekly"
        elif spec.kind_prefix == "once":
            snap = "p4b-dialog-once"
        qa.create_kind(d, spec, snap_prefix=snap)

    (EVIDENCE / "store-after-create.json").write_text(
        json.dumps(qa.list_timers_db(), indent=2, default=str) + "\n"
    )
    (EVIDENCE / "cli-list-after-create.json").write_text(
        json.dumps(qa.cli_list_json(), indent=2) + "\n"
    )

    # --- 3. Pages with data ---
    qa.click_tab("All timers")
    time.sleep(1.5)
    qa.capture(d, "p4b-all", {"phase": "with-data", "expected_min_timers": 8})

    qa.click_tab("Week")
    qa.capture(d, "p4b-week", {"phase": "with-data"})

    qa.click_tab("Month")
    qa.capture(d, "p4b-month", {"phase": "with-data"})

    # --- 3b. Run now → JSONL ---
    print("\n== RUN NOW for JSONL evidence ==")
    qa.run_now_first_timer()
    time.sleep(0.8)
    qa.run_now_nth(1)

    log = qa.event_log_tail(500)
    (EVIDENCE / "events.current.jsonl").write_text(log)
    print(f"  event log lines after Run now: {len(log.splitlines()) if log else 0}")

    qa.click_tab("Run history")
    time.sleep(0.8)
    qa.capture(d, "p4b-history", {"phase": "with-run-now-events"})

    # --- 4. Preview vs CLI for qa-weekly ---
    print("\n== PREVIEW vs CLI (qa-weekly) ==")
    qa.open_edit_for("qa-weekly")
    time.sleep(1.4)
    qa.capture(d, "p4b-dialog-preview-weekly", {"phase": "preview-vs-cli"})
    timers = qa.list_timers_db()
    weekly = next((t for t in timers if t.get("name") == "qa-weekly"), None)
    if weekly:
        tid = weekly.get("id")
        nxt = qa.cli_next(str(tid), 5)
        r = subprocess.run(
            [CLI_BIN, "--db", str(DATA_DIR / "timers.db"), "next", str(tid), "5", "--json"],
            capture_output=True,
            text=True,
        )
        (EVIDENCE / "cli-next-qa-weekly.txt").write_text(nxt)
        if r.returncode == 0 and r.stdout.strip():
            (EVIDENCE / "cli-next-qa-weekly.json").write_text(r.stdout)
        print("cli next:\n", nxt)
    else:
        raise RuntimeError("qa-weekly missing for CLI compare")
    qa.close_dialog_if_open()

    # --- 5. Edit all seven KINDS ---
    for spec in qa.KINDS:
        qa.edit_kind(d, spec)

    qa.click_tab("All timers")
    time.sleep(0.8)
    qa.capture(d, "p4b-all-after-edit", {"phase": "after-edit"})
    (EVIDENCE / "store-after-edit.json").write_text(
        json.dumps(qa.list_timers_db(), indent=2, default=str) + "\n"
    )

    # --- 6. Delete ALL qa-* rows ---
    qa.resize_window(960, 900)
    remaining = [
        t.get("name")
        for t in qa.list_timers_db()
        if (t.get("name") or "").startswith("qa-")
    ]
    print("deleting", remaining)
    failed: list[tuple[str, str]] = []
    for name in list(remaining):
        try:
            qa.delete_kind(d, name)
        except Exception as e:
            print(f"  DELETE FAIL {name}: {e}")
            failed.append((name, str(e)))
            qa.close_dialog_if_open()
            time.sleep(0.3)

    qa.click_tab("All timers")
    time.sleep(0.5)
    qa.capture(d, "p4b-all-after-delete", {"phase": "after-delete"})
    left = [
        t.get("name")
        for t in qa.list_timers_db()
        if (t.get("name") or "").startswith("qa-")
    ]
    if left:
        print("retry deletes for", left)
        for name in list(left):
            try:
                qa.delete_kind(d, name)
            except Exception as e:
                failed.append((name, f"retry: {e}"))
                qa.close_dialog_if_open()
        left = [
            t.get("name")
            for t in qa.list_timers_db()
            if (t.get("name") or "").startswith("qa-")
        ]
        qa.click_tab("All timers")
        qa.capture(d, "p4b-all-after-delete", {"phase": "after-delete-retry"})

    if left:
        raise RuntimeError(
            f"F1 FAIL: qa-* still in store after delete pass: {left}; errors={failed}"
        )

    # --- 7. Larger layout 1280x800 ---
    print("\n== layout at 1280x800 ==")
    qa.resize_window(1280, 800)
    qa.create_kind(d, qa.KINDS[2])  # daily
    qa.create_kind(d, qa.KINDS[3])  # weekly
    qa.click_tab("All timers")
    time.sleep(0.8)
    qa.capture(d, "p4b-all-1280x800", {"phase": "layout-large"})
    qa.click_tab("Week")
    qa.capture(d, "p4b-week-1280x800", {"phase": "layout-large"})
    qa.click_tab("Month")
    qa.capture(d, "p4b-month-1280x800", {"phase": "layout-large"})
    qa.click_tab("Run history")
    qa.capture(d, "p4b-history-1280x800", {"phase": "layout-large"})
    qa.open_edit_for("qa-weekly")
    time.sleep(1.2)
    qa.capture(d, "p4b-dialog-1280x800", {"phase": "layout-large-dialog-full-utc"})
    qa.close_dialog_if_open()

    qa.resize_window(960, 640)
    time.sleep(0.4)

    # --- 8. Persist evidence ---
    final_store = qa.list_timers_db()
    (EVIDENCE / "store-final.json").write_text(
        json.dumps(final_store, indent=2, default=str) + "\n"
    )
    (EVIDENCE / "cli-list-final.json").write_text(
        json.dumps(qa.cli_list_json(), indent=2) + "\n"
    )
    log = qa.event_log_tail(500)
    (EVIDENCE / "events.current.jsonl").write_text(log)
    (EVIDENCE / "webkit_pids_final.json").write_text(
        json.dumps(qa.webkit_pids(), indent=2) + "\n"
    )

    for src_name, dst_name in (
        ("/tmp/qa-p4b.out", "app-stdout.log"),
        ("/tmp/qa-p4b.err", "app-stderr.log"),
        ("/tmp/qa-p4b-combined.log", "app-combined.log"),
    ):
        sp = Path(src_name)
        if sp.exists():
            (EVIDENCE / dst_name).write_bytes(sp.read_bytes())
        else:
            (EVIDENCE / dst_name).write_bytes(b"")

    summary = {
        "display": DISPLAY_NAME or os.environ.get("DISPLAY"),
        "data_dir": str(DATA_DIR),
        "cli_bin": CLI_BIN,
        "input_backend": "tauri-driver+WebKitWebDriver",
        "screenshots": sorted(p.name for p in OUT.glob("p4b-*.png")),
        "meta_json": sorted(p.name for p in OUT.glob("p4b-*.meta.json")),
        "timers_final": [t.get("name") for t in final_store],
        "webkit": qa.webkit_pids(),
        "userAgent": ua_info.get("userAgent"),
        "event_log_lines": len(log.splitlines()) if log else 0,
        "delete_failures": failed,
    }
    (EVIDENCE / "session-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print("\nDONE", json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    # Ensure scripts/ is on sys.path when invoked as a file.
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    try:
        raise SystemExit(main())
    finally:
        try:
            import qa_webdriver as _qa

            _qa.stop_session()
        except Exception:
            pass
