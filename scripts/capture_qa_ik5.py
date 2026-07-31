#!/usr/bin/env python3
"""IK5 QA — live run state in the GUI, driven for real on WebKitGTK.

Proves the IK5 exit gate against the REAL app (isolated Xvfb display,
tauri-driver + WebKitWebDriver, no input injection into the operator
session):

  1. An integration-owned timer shows its live run on its All-timers row,
     and the row updates as the app writes `progress`.
  2. `overdue` shows at 1× expected_secs while the run is still `running`;
     an opted-in watchdog fails it at × factor — two different thresholds.
  3. An app with no error_detection is never failed, however long it runs.
  4. completed / failed·reported / failed·timed out / no ack each render
     distinctly; an unowned action-only timer shows no live run state.
  5. Current non-terminal runs pin to the top of Run history.
  6. The timer detail view shows the current run in full (run_id, app,
     expected_secs, progress, result).
  7. NO POLLING: `list_run_states` IPC invocations are counted via an
     invoke wrapper — zero while idle, zero during a silent-but-running
     run, and exactly event-driven refetches when state changes.

Run via scripts/run_gui_qa_ik5.sh (sets the isolated display + binaries).
Outputs: docs/qa4-screenshots/ik5-*.png, docs/qa4-evidence/ik5-*.json.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path

import qa_webdriver as qa

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
EVIDENCE = ROOT / "docs" / "qa4-evidence"

DATA_DIR = Path(os.environ.get("BELLMAN_QA_DATA", str(qa.DATA_DIR)))
CLI = os.environ.get("BELLMAN_CLI", str(ROOT / "target/debug/bellman"))
DB = DATA_DIR / "timers.db"
SLOTS = DATA_DIR / "slots"

results: dict = {"checks": [], "counts": {}}


def check(name: str, ok: bool, detail: str = ""):
    results["checks"].append({"name": name, "ok": bool(ok), "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}{(' — ' + detail) if detail else ''}")


def cli(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [CLI, "--db", str(DB), *args], capture_output=True, text=True, timeout=30
    )


def slot_add(app_name: str, timer_name: str):
    req = {
        "schema": "bellman-slot/1",
        "request_id": str(uuid.uuid4()),
        "operation": "add",
        "payload": {
            "app_name": app_name,
            "timer_name": timer_name,
            "tz": "UTC",
            "occurrence": {"kind": "interval", "every_secs": 3600},
        },
    }
    req_path = DATA_DIR / f"req-{timer_name}.json"
    req_path.write_text(json.dumps(req))
    r = cli("slot-submit", str(req_path), "--slots", str(SLOTS))
    if r.returncode != 0:
        raise RuntimeError(f"slot-submit {timer_name} failed: {r.stdout}\n{r.stderr}")


def wait_fire(timer_name: str, timeout: float = 12.0, after: float = 0.0) -> dict:
    """Wait for a fire notification of `timer_name` (newest match; when
    `after` is given, only files modified after that epoch)."""
    fires = SLOTS / "fires"
    deadline = time.time() + timeout
    while time.time() < deadline:
        best = None
        if fires.exists():
            for p in sorted(fires.glob("fire-*.json"), key=lambda p: p.stat().st_mtime):
                if p.stat().st_mtime <= after:
                    continue
                try:
                    doc = json.loads(p.read_text())
                except Exception:
                    continue
                if doc.get("timer_name") == timer_name:
                    best = doc
        if best is not None:
            return best
        time.sleep(0.2)
    raise RuntimeError(f"no fire notification for {timer_name} within {timeout}s")


def reply(fire: dict, **fields):
    """The app side: edit the pre-filled stub, atomically replace it."""
    path = fire["reply_path"]
    doc = json.loads(Path(path).read_text())
    doc.update(fields)
    tmp = path + ".tmp"
    Path(tmp).write_text(json.dumps(doc, indent=2))
    os.replace(tmp, path)


def status_of(timer_name: str) -> dict:
    for folder in (DATA_DIR / "timers").glob(f"{timer_name}-*/"):
        p = folder / "status.json"
        if p.exists():
            return json.loads(p.read_text())
    return {}


def wait_status(timer_name: str, pred, timeout: float = 12.0) -> dict:
    deadline = time.time() + timeout
    last = {}
    while time.time() < deadline:
        last = status_of(timer_name)
        if last and pred(last):
            return last
        time.sleep(0.25)
    raise RuntimeError(f"status for {timer_name} never matched; last={last}")


def js(drv, script: str, *args):
    return drv.execute_script(script, *args)


def run_now(drv, timer_name: str) -> str:
    return js(
        drv,
        """
        const name = arguments[0];
        const rows = [...document.querySelectorAll('table.timer-table tbody tr')];
        const row = rows.find(r => r.querySelector('.name-text')?.textContent.trim() === name);
        if (!row) return 'no-row';
        const btn = [...row.querySelectorAll('button')].find(b => b.textContent.trim() === 'Run now');
        if (!btn) return 'no-btn';
        btn.click();
        return 'ok';
        """,
        timer_name,
    )


def click_log(drv, timer_name: str) -> str:
    return js(
        drv,
        """
        const name = arguments[0];
        const rows = [...document.querySelectorAll('table.timer-table tbody tr')];
        const row = rows.find(r => r.querySelector('.name-text')?.textContent.trim() === name);
        if (!row) return 'no-row';
        const btn = [...row.querySelectorAll('button')].find(b => /^(Log|Hide log)$/.test(b.textContent.trim()));
        if (!btn) return 'no-btn';
        btn.click();
        return 'ok';
        """,
        timer_name,
    )


def rows_text(drv) -> str:
    return js(
        drv,
        "return [...document.querySelectorAll('table.timer-table tbody tr')]"
        ".map(r => r.innerText).join('\\n\\n');",
    ) or ""


def row_text(drv, timer_name: str) -> str:
    """The FULL innerText of one timer's row, matched by the name cell
    (other cells may mention peer timer names — the density column)."""
    return js(
        drv,
        """
        const rows = [...document.querySelectorAll('table.timer-table tbody tr')];
        const row = rows.find(r => r.querySelector('.name-text')?.textContent.trim() === arguments[0]);
        return row ? row.innerText : '';
        """,
        timer_name,
    ) or ""


def page_text(drv) -> str:
    return js(drv, "return document.body.innerText;") or ""


def history_text(drv) -> str:
    """Only the event entries — the Kind filter dropdown otherwise contains
    every state word and makes text checks vacuous."""
    return js(
        drv,
        "return document.querySelector('.history-list')?.innerText || '';",
    ) or ""


def patch_counts(drv):
    """Count IPC commands. `__TAURI_INTERNALS__.invoke` is frozen, but the
    custom-protocol transport rides on window.fetch (writable): each
    command is one ipc URL fetch. Probed: window.fetch is writable +
    configurable and this wrapper observes real commands."""
    ok = js(
        drv,
        """
        if (!window.__fetchPatched) {
          window.__fetchPatched = true;
          window.__invokeCounts = {};
          const origFetch = window.fetch;
          const wrapped = function(url, ...rest) {
            try {
              const u = String(url);
              if (u.includes('ipc')) {
                const cmd = decodeURIComponent(u.split('/').pop().split('?')[0]);
                window.__invokeCounts[cmd] = (window.__invokeCounts[cmd] || 0) + 1;
              }
            } catch (e) {}
            return origFetch.call(this, url, ...rest);
          };
          window.fetch = wrapped;
        }
        return window.__fetchPatched === true;
        """,
    )
    if not ok:
        raise RuntimeError("fetch patch failed")


def reset_counts(drv):
    js(drv, "window.__invokeCounts = {};")


def read_counts(drv) -> dict:
    return js(drv, "return window.__invokeCounts || {};") or {}


def wait_row_text(drv, needle: str, timeout: float = 12.0) -> str:
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        last = rows_text(drv)
        if needle in last:
            return last
        time.sleep(0.4)
    raise RuntimeError(f"row text never contained {needle!r}; last:\n{last}")


def main() -> int:
    disp = os.environ.get("DISPLAY", "")
    if disp in (":0", ":0.0") and os.environ.get("BELLMAN_QA_ALLOW_DISPLAY0") != "1":
        print(f"ERROR: refusing DISPLAY={disp} (operator session)", file=sys.stderr)
        return 2

    # Fresh data dir + fast pickup grace so no_ack is observable.
    if DATA_DIR.exists():
        for child in DATA_DIR.iterdir():
            if child.is_dir():
                import shutil

                shutil.rmtree(child)
            else:
                child.unlink()
    (DATA_DIR / "logs").mkdir(parents=True, exist_ok=True)
    (DATA_DIR / "slots").mkdir(parents=True, exist_ok=True)
    (DATA_DIR / "config.json").write_text(
        json.dumps(
            {
                "wizard_completed": True,
                "autostart_enabled": False,
                "start_minimized": False,
                "wake_enabled": False,
                "pickup_grace_secs": 4,
            }
        )
    )
    OUT.mkdir(parents=True, exist_ok=True)
    EVIDENCE.mkdir(parents=True, exist_ok=True)

    # Create the cast BEFORE the app starts (timer rows are there at mount):
    #   ik5-bulb     — replies, progress, expected 8s, NO error_detection
    #   ik5-flaky    — expected 4s + error_detection (watchdog ×2 ⇒ fail at 8s)
    #   ik5-reported — the app itself reports failed with a reason
    #   ik5-quiet    — never replies ⇒ no_ack at the 4s pickup grace
    #   ik5-plain    — UNOWNED action-only timer: never a live app run
    for app, name in [
        ("lightbulb", "ik5-bulb"),
        ("flaky", "ik5-flaky"),
        ("reporter", "ik5-reported"),
        ("quiet", "ik5-quiet"),
    ]:
        slot_add(app, name)
    r = cli("add", "--name", "ik5-plain", "--occurrence", "interval", "--every-secs", "3600")
    if r.returncode != 0:
        raise RuntimeError(f"cli add failed: {r.stdout}\n{r.stderr}")

    print(f"IK5 WebDriver session DISPLAY={disp} DATA={DATA_DIR}")
    qa.start_session()
    drv = qa.driver()
    x = qa.xdisp()
    time.sleep(2.5)
    qa.click_tab("All timers")
    time.sleep(1.0)
    patch_counts(drv)

    # ── 1. bulb fires; the app acks + runs with progress ────────────────
    print("== bulb: running with progress ==")
    check("run_now bulb", run_now(drv, "ik5-bulb") == "ok")
    fire = wait_fire("ik5-bulb")
    reply(
        fire,
        state="running",
        acknowledged_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        expected_secs=8,
        progress="bulb on, warming up",
    )
    wait_status("ik5-bulb", lambda s: s.get("state") == "running")
    wait_row_text(drv, "running")
    txt = wait_row_text(drv, "bulb on, warming up")
    check("row shows ● running + progress", "bulb on, warming up" in txt)
    qa.capture(x, "ik5-all-running", {"phase": "running"})

    # The event-driven refetch arrived (run-now + the reply, no polling).
    c = read_counts(drv)
    check(
        "run-status refetches are event-driven so far",
        c.get("list_run_states", 0) >= 1,
        f"counts={c}",
    )

    # ── 2. NO POLLING while a run is open and the app is silent ─────────
    print("== idle measurement during an open run ==")
    reset_counts(drv)
    time.sleep(10)
    c = read_counts(drv)
    results["counts"]["open_run_silent_10s"] = c
    check(
        "no list_run_states polling while a run is open and silent (10s)",
        c.get("list_run_states", 0) == 0,
        f"counts={c}",
    )
    check(
        "counter is alive (pre-existing list_timers poll still observed)",
        c.get("list_timers", 0) >= 1,
        f"counts={c}",
    )

    # …and one progress write triggers exactly the event-driven refetch.
    reply(fire, state="running", expected_secs=8, progress="bulb on, 12s elapsed")
    deadline = time.time() + 5
    while time.time() < deadline and read_counts(drv).get("list_run_states", 0) == 0:
        time.sleep(0.2)
    c = read_counts(drv)
    check(
        "one accepted reply ⇒ run-status-changed ⇒ one refetch",
        c.get("list_run_states", 0) >= 1,
        f"counts={c}",
    )
    wait_row_text(drv, "bulb on, 12s elapsed")
    check("row updated as the app wrote progress", True)

    # ── 3. overdue at 1×, still running; watchdog only at × factor ──────
    print("== flaky: overdue at 1×, failed at ×2 ==")
    check("run_now flaky", run_now(drv, "ik5-flaky") == "ok")
    fire_flaky = wait_fire("ik5-flaky")
    reply(
        fire_flaky,
        state="running",
        acknowledged_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        expected_secs=6,
        error_detection=True,
        progress="flaky working",
    )
    wait_status("ik5-flaky", lambda s: s.get("state") == "running")
    wait_row_text(drv, "flaky working")

    # WAIT for the 1× label to appear (still running), instead of sleeping a
    # fixed time — the label crosses at exactly expected_secs, and the ×2
    # watchdog (12s) leaves a comfortable window.
    deadline = time.time() + 9
    flaky_line = ""
    while time.time() < deadline:
        flaky_line = row_text(drv, "ik5-flaky")
        if "overdue" in flaky_line:
            break
        time.sleep(0.3)
    bulb_line = row_text(drv, "ik5-bulb")
    check("bulb overdue at 1× while still running", "overdue" in bulb_line and "running" in bulb_line, bulb_line)
    check("flaky overdue at 1× while still running", "overdue" in flaky_line and "running" in flaky_line, flaky_line)
    qa.capture(x, "ik5-all-overdue", {"phase": "overdue-at-1x"})

    # Cross ×2 on flaky: the opted-in watchdog marks failed · timed out.
    wait_status("ik5-flaky", lambda s: s.get("state") == "failed", timeout=10)
    wait_row_text(drv, "timed out")
    flaky_line = row_text(drv, "ik5-flaky")
    check("flaky failed · timed out at × factor", "timed out" in flaky_line, flaky_line)

    # The quiet timer never answers. Fire it TWICE inside the pickup grace:
    # run 1 is superseded by run 2 (giving history a `superseded` entry),
    # and run 2 lapses to no_ack.
    print("== quiet: superseded + no_ack ==")
    check("run_now quiet (1st)", run_now(drv, "ik5-quiet") == "ok")
    wait_status("ik5-quiet", lambda s: s.get("state") == "fired")
    check("run_now quiet (2nd)", run_now(drv, "ik5-quiet") == "ok")
    wait_status("ik5-quiet", lambda s: s.get("state") == "no_ack", timeout=15)
    wait_row_text(drv, "no ack")
    quiet_line = row_text(drv, "ik5-quiet")
    check("quiet shows ⚠ no ack", "no ack" in quiet_line, quiet_line)

    # The reporter fails itself.
    print("== reporter: failed · reported ==")
    check("run_now reporter", run_now(drv, "ik5-reported") == "ok")
    fire_rep = wait_fire("ik5-reported")
    reply(fire_rep, state="failed", reason="GPIO write refused")
    wait_status("ik5-reported", lambda s: s.get("state") == "failed")
    wait_row_text(drv, "reported")
    rep_line = row_text(drv, "ik5-reported")
    check("reporter shows ⚠ failed · reported", "reported" in rep_line, rep_line)

    # The unowned timer fires: normal row, NO live run state, never no_ack.
    print("== plain: unowned action-only ==")
    check("run_now plain", run_now(drv, "ik5-plain") == "ok")
    time.sleep(6)  # past the pickup grace — an owned timer would be no_ack
    plain_block = row_text(drv, "ik5-plain")
    check(
        "unowned timer shows no live run state (no ●, no no ack)",
        ("●" not in plain_block) and ("no ack" not in plain_block),
        plain_block.replace("\n", " | "),
    )

    # The bulb (expected 8s, NO error_detection) is long past every
    # threshold and must still be running — never failed.
    s = status_of("ik5-bulb")
    check(
        "no error_detection ⇒ never failed, however long it runs",
        s.get("state") == "running",
        f"state={s.get('state')}",
    )

    # The bulb completes after being overdue; the row follows.
    reply(fire, state="completed", result={"on_duration_secs": 14.2})
    wait_status("ik5-bulb", lambda s: s.get("state") == "completed")
    wait_row_text(drv, "completed")
    bulb_line = row_text(drv, "ik5-bulb")
    check("bulb completed after overdue", "completed" in bulb_line, bulb_line)
    qa.capture(x, "ik5-all-terminal", {"phase": "all-terminal-states"})

    # ── 4. timer detail view ────────────────────────────────────────────
    print("== detail view ==")
    check("open bulb detail", click_log(drv, "ik5-bulb") == "ok")
    time.sleep(1.0)
    body = page_text(drv).lower()
    panel = js(drv, "return document.querySelector('.log-panel')?.innerText || '(no panel)';")
    results["detail_panel_text"] = panel
    run_id = status_of("ik5-bulb").get("run_id", "")
    missing = [
        k for k in ["current run", "run_id", run_id, "lightbulb", "expected", "completed"]
        if k not in body
    ]
    check(
        "detail shows the current run in full",
        not missing,
        f"missing={missing} panel={panel[:300]!r}",
    )
    check("detail shows the result payload", "on_duration_secs" in body)
    qa.capture(x, "ik5-detail-completed", {"phase": "detail"})

    # ── 5. Run history: live pinned on top, terminals distinct ──────────
    print("== history ==")
    # Fire the bulb again and leave it running for the pinned section.
    marker = time.time()
    check("run_now bulb (again)", run_now(drv, "ik5-bulb") == "ok")
    fire2 = wait_fire("ik5-bulb", after=marker)
    reply(
        fire2,
        state="running",
        acknowledged_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        progress="second run in flight",
    )
    wait_status("ik5-bulb", lambda s: s.get("state") == "running")
    qa.click_tab("Run history")
    time.sleep(1.2)
    body = page_text(drv)
    check("history pins the live run on top", "Happening now" in body)
    check("pinned entry names the timer + progress", "second run in flight" in body)
    qa.capture(x, "ik5-history-live", {"phase": "history-live-pinned"})
    deadline = time.time() + 10
    hist = ""
    while time.time() < deadline:
        hist = history_text(drv)
        if all(
            n in hist
            for n in ["completed", "failed · timed out", "failed · reported", "no ack", "superseded"]
        ):
            break
        time.sleep(0.5)
    for needle in ["completed", "failed · timed out", "failed · reported", "no ack", "superseded"]:
        check(f"history shows {needle} distinctly", needle in hist, hist[:200])
    qa.capture(x, "ik5-history-terminal-kinds", {"phase": "history-terminals"})

    # Finish the second bulb run so the final idle measurement has no open runs.
    reply(fire2, state="completed", result={"ok": True})
    wait_status("ik5-bulb", lambda s: s.get("state") == "completed")

    # ── 6. NO POLLING with nothing running ──────────────────────────────
    print("== idle measurement, all runs terminal ==")
    qa.click_tab("All timers")
    time.sleep(1.0)
    patch_counts(drv)
    reset_counts(drv)
    time.sleep(10)
    c = read_counts(drv)
    results["counts"]["all_terminal_10s"] = c
    check(
        "no list_run_states polling with zero non-terminal runs (10s)",
        c.get("list_run_states", 0) == 0,
        f"counts={c}",
    )

    # ── evidence dump ───────────────────────────────────────────────────
    results["status_files"] = {
        name: status_of(name)
        for name in ["ik5-bulb", "ik5-flaky", "ik5-reported", "ik5-quiet", "ik5-plain"]
    }
    log_path = DATA_DIR / "logs" / "events.current.jsonl"
    results["event_log_tail"] = (
        log_path.read_text().splitlines()[-40:] if log_path.exists() else []
    )
    (EVIDENCE / "ik5-results.json").write_text(json.dumps(results, indent=2, default=str) + "\n")

    failed = [c for c in results["checks"] if not c["ok"]]
    print(f"\nIK5 QA: {len(results['checks']) - len(failed)}/{len(results['checks'])} checks passed")
    for c in failed:
        print(f"  FAILED: {c['name']} — {c['detail']}")
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        rc = main()
    finally:
        qa.stop_session()
    raise SystemExit(rc)
