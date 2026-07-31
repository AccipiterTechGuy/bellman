#!/usr/bin/env python3
"""QA WIZ1 — the first-run wizard demo offer, end to end on an isolated display.

Proves the card's exit gate against the real app (WebDriver for the Bellman
webview; XTEST clicks against the demo's own tk window — the isolated Xvfb
only, never the operator session):

  Phase 1 (fresh profile, full demo loop)
    - wizard opens; the demo tick is on the first step and UNTICKED by default
    - unticked finish: completion step has no demo panel; timer count unchanged
    - re-run wizard, tick it, finish: panel shows copyable command + Run button;
      timer count STILL unchanged (Bellman never creates the demo's timer)
    - Run the demo launches lightbulb_gui against THIS install's slots root
    - clicking inside the demo window creates the timer (slot protocol), the
      bulb lights, and the run reaches fired→acknowledged→completed with no
      run-now anywhere (SCH2 proof from the user's side)
    - the choice is persisted (config.json demo_opt_in) and Settings shows the
      same panel
    - closing Bellman leaves the demo process running

  Phase 2 (python3 present, tkinter MISSING — stock Ubuntu 24.04 w/o python3-tk)
    - a PATH shim makes `python3 -c "import tkinter"` fail while python3
      itself works: the Run button must NOT appear; the copyable command and
      the python3-tk note must.

Run via:  scripts/run_gui_qa.sh wiz1
"""
from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qa_webdriver as qa

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "docs" / "qa4-screenshots"

DEMO_TIMER_NAME = "lightbulb-gui-demo"


# ---------------------------------------------------------------------------
# small helpers
# ---------------------------------------------------------------------------

def js(expr: str):
    return qa.driver().execute_script(f"return {expr}")


def wait_js(expr: str, timeout: float = 10.0, desc: str = ""):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if js(expr):
                return True
        except Exception:
            pass
        time.sleep(0.25)
    raise RuntimeError(f"timed out waiting for: {desc or expr}")


def reset_data_dir(wizard_completed: bool | None):
    """Fresh profile. wizard_completed=None leaves no config (true first run)."""
    data = qa.DATA_DIR
    if data.exists():
        shutil.rmtree(data)
    (data / "logs").mkdir(parents=True)
    (data / "slots").mkdir(parents=True)
    if wizard_completed is not None:
        (data / "config.json").write_text(
            json.dumps(
                {
                    "wizard_completed": wizard_completed,
                    "autostart_enabled": False,
                    "start_minimized": False,
                    "wake_enabled": False,
                }
            )
            + "\n"
        )


def timer_rows():
    try:
        return qa.list_timers_db()
    except Exception:
        return []


def read_events():
    p = qa.DATA_DIR / "logs" / "events.current.jsonl"
    if not p.exists():
        return []
    out = []
    for line in p.read_text().splitlines():
        try:
            out.append(json.loads(line))
        except Exception:
            continue
    return out


def parse_ts(s: str) -> datetime:
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def wait_demo_run_completed(since: datetime, timeout_s: float = 90.0):
    """One run of the demo timer reaching fired→acknowledged→completed."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        result = _demo_run_state(since)
        if result:
            return result
        time.sleep(0.7)
    raise RuntimeError("demo run did not reach fired→acknowledged→completed")


def _demo_run_state(since: datetime):
    runs: dict[str, list[dict]] = {}
    for e in read_events():
        if e.get("timer_name") != DEMO_TIMER_NAME or not e.get("run_id"):
            continue
        try:
            if parse_ts(e["logged_at"]) < since:
                continue
        except Exception:
            pass
        runs.setdefault(e["run_id"], []).append(e)
    for run_id, evs in runs.items():
        kinds = [e.get("kind") for e in evs]
        if "fired" in kinds and "acknowledged" in kinds and "completed" in kinds:
            return run_id, evs
    return None


def wait_demo_acknowledged(since: datetime, timeout_s: float = 60.0):
    """The moment the demo acknowledges the fire — from here until the
    `completed` reply (`on_secs`, default 15 s) the bulb is LIT. The demo
    lights its RUNNING chip locally; it replies only acknowledged →
    completed, so `acknowledged` is the live signal to capture against.
    """
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        for p in qa.DATA_DIR.glob("timers/*/status.json"):
            try:
                st = json.loads(p.read_text())
            except Exception:
                continue
            if st.get("timer_name") != DEMO_TIMER_NAME:
                continue
            if st.get("state") == "acknowledged":
                return st
        time.sleep(0.3)
    raise RuntimeError("demo run never acknowledged the fire (bulb never lit)")


def demo_processes():
    """(pid, cmdline) of every live lightbulb_gui process."""
    out = []
    for p in Path("/proc").iterdir():
        if not p.name.isdigit():
            continue
        try:
            cmd = (p / "cmdline").read_bytes().replace(b"\0", b" ").decode()
        except Exception:
            continue
        if "lightbulb_gui.py" in cmd:
            out.append((int(p.name), cmd))
    return out


def find_demo_window():
    """(wid, x, y, w, h) of the demo's tk toplevel, or None."""
    try:
        lines = subprocess.check_output(["wmctrl", "-lG"], text=True).splitlines()
    except Exception:
        return None
    for line in lines:
        parts = line.split()
        if len(parts) >= 7 and " ".join(parts[7:]) == "Bellman Lightbulb Demo":
            return (int(parts[0], 16), *map(int, parts[2:6]))
    return None


def wait_demo_window(timeout: float = 20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        w = find_demo_window()
        if w:
            return w
        time.sleep(0.4)
    raise RuntimeError("lightbulb demo window did not appear")


def capture_window(wid: int, x: int, y: int, w: int, h: int, name: str, annotate=None):
    """Xlib GetImage screenshot of an arbitrary window (read-only)."""
    from Xlib import X
    from PIL import Image

    xd = qa.xdisp()
    win = xd.create_resource_object("window", wid)
    raw = win.get_image(0, 0, w, h, X.ZPixmap, 0xFFFFFFFF)
    img = Image.frombytes("RGBA", (w, h), raw.data, "raw", "BGRA").convert("RGB")
    path = qa.OUT / f"{name}.png"
    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path)
    (qa.OUT / f"{name}.meta.json").write_text(
        json.dumps(
            {
                "file": path.name,
                "size": [w, h],
                "window_id": hex(wid),
                "display": os.environ.get("DISPLAY"),
                "input_backend": "tauri-driver+WebKitWebDriver (+XTEST on isolated Xvfb)",
                **(annotate or {}),
            },
            indent=2,
        )
        + "\n"
    )
    print(f"  captured {path.name} {img.size} bytes={path.stat().st_size}")
    return path


def _leaf_windows(xd, win, out):
    """Collect (root_x, root_y, w, h) of descendant windows, deepest first."""
    geom = win.get_geometry()
    children = win.query_tree().children
    for ch in children:
        _leaf_windows(xd, ch, out)
    if not children:
        # Root coords of this leaf's origin: translate (0,0) from the leaf
        # into root space (dst.translate_coords(src, x, y)).
        root = win.query_tree().root
        coords = root.translate_coords(win, 0, 0)
        out.append((coords.x, coords.y, geom.width, geom.height))


def xtest_click(xd, x: int, y: int):
    from Xlib import X
    from Xlib.ext import xtest

    root = xd.screen().root
    # root= is REQUIRED: without it the x/y are relative pointer offsets,
    # not absolute screen coordinates.
    xtest.fake_input(xd, X.MotionNotify, root=root, x=x, y=y)
    xd.sync()
    time.sleep(0.05)
    xtest.fake_input(xd, X.ButtonPress, detail=1, root=root, x=x, y=y)
    xd.sync()
    time.sleep(0.05)
    xtest.fake_input(xd, X.ButtonRelease, detail=1, root=root, x=x, y=y)
    xd.sync()
    time.sleep(0.1)


def drive_demo_create_timer(wid: int, timeout_s: float = 180.0):
    """Click candidate widget windows inside the demo (bottom-up, right-to-left)
    until the demo's own timer appears in the store.

    Reverse reading order reaches the 'Remove Timer' (disabled — a no-op) and
    then 'Create Timer' buttons before any radio button, so the default
    'in N seconds' mode is never disturbed. Every other clickable widget is
    inert. The assertion is behavioral: the timer row appears ONLY after the
    right click, so the scan is self-verifying. The buttons sit just above
    the 'Seconds:' label in reverse order, i.e. near the END of the scan —
    the per-click settle must be short enough that the scan reaches them
    well inside the timeout.
    """
    subprocess.run(
        ["wmctrl", "-i", "-a", hex(wid)],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.4)
    xd = qa.xdisp()
    win = xd.create_resource_object("window", wid)
    leaves: list[tuple[int, int, int, int]] = []
    _leaf_windows(xd, win, leaves)
    # Widget-sized candidates, bottom-to-top, right-to-left.
    cands = [c for c in leaves if 30 <= c[2] <= 420 and 14 <= c[3] <= 64]
    cands.sort(key=lambda c: (-c[1], -c[0]))
    print(f"  demo window: {len(leaves)} leaf windows, {len(cands)} click candidates")
    deadline = time.time() + timeout_s
    for x, y, w, h in cands:
        if time.time() > deadline:
            break
        print(f"  trying candidate root=({x},{y}) {w}x{h} center=({x + w // 2},{y + h // 2})")
        xtest_click(xd, x + w // 2, y + h // 2)
        # Did the demo create its timer through the slot protocol?
        settle = time.time() + 2.5
        while time.time() < settle:
            rows = [r for r in timer_rows() if r.get("name") == DEMO_TIMER_NAME]
            if rows:
                print(f"  click at root ({x + w // 2},{y + h // 2}) created the timer")
                return rows[0]
            time.sleep(0.3)
    raise RuntimeError(
        "no click candidate produced the demo timer — window layout changed?"
    )


# ---------------------------------------------------------------------------
# Phase 1 — fresh profile, full demo loop
# ---------------------------------------------------------------------------

def phase1(xd):
    print("== phase 1: fresh profile — tick default, unticked path, full demo loop ==")
    qa.stop_session()
    reset_data_dir(wizard_completed=None)
    qa.start_session()
    d = qa.driver()
    By = qa._by()

    # The wizard must be open on a true first run.
    wait_js("!!document.querySelector('.wizard')", 20, "wizard opens on first run")

    # Exit gate: the tick is present on the first step, defaults to UNTICKED.
    wait_js("!!document.querySelector('[data-testid=\"demo-tick\"]')", 10, "demo tick present")
    ticked = js("document.querySelector('[data-testid=\"demo-tick\"]').checked")
    assert ticked is False, f"demo tick must default to unticked, got {ticked!r}"
    assert js(
        "[...document.querySelectorAll('.wizard .hint')].some(h => h.textContent.includes('lightbulb'))"
    ), "demo hint line missing on the first step"
    before = len(timer_rows())
    qa.resize_window(960, 640)
    qa.capture(xd, "wiz1-step0-tick", {"expect": "step 0 with demo tick, unticked"})

    # Unticked path: completion step exactly as before — no demo panel,
    # and the timer count is unchanged.
    qa.click_button("Next")
    time.sleep(0.4)
    qa.click_button("No thanks")
    wait_js("document.body.textContent.includes('Setup complete')", 10, "completion step")
    assert not js("!!document.querySelector('[data-testid=\"demo-panel\"]')"), (
        "unticked finish must NOT show the demo panel"
    )
    after_unticked = len(timer_rows())
    assert after_unticked == before, (
        f"unticked wizard created a timer: before={before} after={after_unticked}"
    )
    qa.capture(xd, "wiz1-complete-unticked", {"expect": "completion step, no demo panel"})
    qa.click_button("Continue")
    time.sleep(0.6)

    # Re-run the wizard (Settings > Run setup again), tick the demo this time.
    qa.click_tab("Settings")
    time.sleep(0.5)
    qa.click_button("Run setup again")
    wait_js("!!document.querySelector('[data-testid=\"demo-tick\"]')", 10, "wizard re-opened")
    d.find_element(By.CSS_SELECTOR, '[data-testid="demo-tick"]').click()
    time.sleep(0.2)
    qa.click_button("Next")
    time.sleep(0.4)
    qa.click_button("No thanks")
    wait_js("!!document.querySelector('[data-testid=\"demo-panel\"]')", 10, "demo panel on completion")

    # The panel: copyable command pointing at THIS install's slots root, and a
    # working Run the demo button (this machine has python3 + tkinter).
    cmd = js("document.querySelector('[data-testid=\"demo-command\"]').value")
    expected_slots = str(qa.DATA_DIR / "slots")
    assert "lightbulb_gui.py" in cmd and "--slots" in cmd, f"bad demo command: {cmd}"
    assert expected_slots in cmd, (
        f"command must use this install's slots root {expected_slots}: {cmd}"
    )
    assert js("!!document.querySelector('[data-testid=\"demo-run\"]')"), "Run the demo missing"
    after_ticked = len(timer_rows())
    assert after_ticked == before, (
        f"ticked wizard created a timer before any launch: before={before} after={after_ticked}"
    )
    qa.capture(xd, "wiz1-complete-demo-panel", {"expect": "completion step with demo panel"})

    # Persisted preference.
    cfg = json.loads((qa.DATA_DIR / "config.json").read_text())
    assert cfg.get("demo_opt_in") is True, f"demo_opt_in not persisted: {cfg}"

    # Press Run the demo — the demo must launch against this install's slots.
    procs_before = {pid for pid, _ in demo_processes()}
    t_launch = datetime.now(timezone.utc)
    run_btn = d.find_element(By.CSS_SELECTOR, '[data-testid="demo-run"]')
    d.execute_script("arguments[0].scrollIntoView({block:'center'})", run_btn)
    time.sleep(0.3)
    try:
        run_btn.click()
    except Exception:
        d.execute_script("arguments[0].click()", run_btn)
    demo_pid = None
    deadline = time.time() + 20
    while time.time() < deadline:
        for pid, cmdline in demo_processes():
            if pid not in procs_before:
                demo_pid, demo_cmdline = pid, cmdline
                break
        if demo_pid:
            break
        time.sleep(0.3)
    assert demo_pid, "Run the demo did not launch lightbulb_gui.py"
    assert f"--slots {expected_slots}" in demo_cmdline, (
        f"demo launched with wrong slots: {demo_cmdline}"
    )
    print(f"  demo launched: pid={demo_pid} cmd={demo_cmdline}")
    assert len(timer_rows()) == before, "launch alone must not create a timer"

    # The user sets a time in the demo window and the bulb lights.
    wid, wx, wy, ww, wh = wait_demo_window()
    row = drive_demo_create_timer(wid)
    assert row.get("name") == DEMO_TIMER_NAME
    print(f"  demo created its own timer through the slot protocol: {row.get('id')}")

    # The bulb is lit exactly between the demo's `acknowledged` and
    # `completed` replies (default 15 s) — capture it mid-run, then wait for
    # the full handshake.
    wait_demo_acknowledged(t_launch, timeout_s=60)
    wid, wx, wy, ww, wh = wait_demo_window()
    subprocess.run(
        ["wmctrl", "-i", "-a", hex(wid)],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.4)
    capture_window(wid, wx, wy, ww, wh, "wiz1-demo-bulb-lit",
                   {"expect": "golden bulb LIT mid-run (acknowledged, pre-completion)"})

    run_id, evs = wait_demo_run_completed(t_launch, timeout_s=90)
    kinds = [e.get("kind") for e in evs]
    print(f"  run completed: run_id={run_id} kinds={kinds}")

    wid, wx, wy, ww, wh = wait_demo_window()
    capture_window(wid, wx, wy, ww, wh, "wiz1-demo-run-completed",
                   {"expect": "demo window after fired→ack→running→completed",
                    "run_id": run_id})

    # Settings offers the same panel afterwards, with the persisted tick.
    qa.click_button("Continue")
    time.sleep(0.6)
    qa.click_tab("Settings")
    time.sleep(0.6)
    assert js("!!document.querySelector('[data-testid=\"settings-demo\"]')"), "Settings demo section missing"
    assert js("!!document.querySelector('[data-testid=\"settings-demo\"] [data-testid=\"demo-panel\"]')"), (
        "Settings demo panel missing"
    )
    wait_js(
        "document.querySelector('#s-demo') && document.querySelector('#s-demo').checked",
        10,
        "Settings demo opt-in checkbox reflects the persisted choice",
    )
    scmd = js("document.querySelector('[data-testid=\"settings-demo\"] [data-testid=\"demo-command\"]').value")
    assert expected_slots in scmd, f"Settings command uses wrong slots: {scmd}"
    js("document.querySelector('[data-testid=\"settings-demo\"]').scrollIntoView({block:'center'}) || true")
    time.sleep(0.5)
    qa.capture(xd, "wiz1-settings-demo", {"expect": "Settings > Demo with panel + ticked offer"})

    # Closing Bellman leaves the demo running.
    qa.stop_session()
    time.sleep(1.0)
    alive = any(pid == demo_pid for pid, _ in demo_processes())
    assert alive, f"demo pid {demo_pid} died with Bellman — must be independent"
    print(f"  demo pid {demo_pid} still alive after Bellman exit")
    return demo_pid


# ---------------------------------------------------------------------------
# Phase 2 — python3 present, tkinter missing (stock Ubuntu 24.04)
# ---------------------------------------------------------------------------

def phase2(xd):
    print("== phase 2: python3 present but tkinter MISSING ==")
    qa.stop_session()
    shim = Path("/tmp/bellman-wiz1-shim")
    shim.mkdir(exist_ok=True)
    (shim / "python3").write_text(
        "#!/bin/sh\n"
        "# Fake python3: interpreter works, tkinter is not installed.\n"
        'if [ "$1" = "-c" ] && [ "$2" = "import tkinter" ]; then\n'
        '  echo "Traceback (most recent call last):" >&2\n'
        '  echo "ModuleNotFoundError: No module named '"'"'tkinter'"'"'" >&2\n'
        "  exit 1\n"
        "fi\n"
        'exec /usr/bin/python3 "$@"\n'
    )
    (shim / "python3").chmod(0o755)
    os.environ["PATH"] = f"{shim}:{os.environ['PATH']}"

    reset_data_dir(wizard_completed=None)
    qa.start_session()
    d = qa.driver()
    By = qa._by()

    wait_js("!!document.querySelector('[data-testid=\"demo-tick\"]')", 20, "wizard opens (phase 2)")
    d.find_element(By.CSS_SELECTOR, '[data-testid="demo-tick"]').click()
    qa.click_button("Next")
    time.sleep(0.4)
    qa.click_button("No thanks")
    wait_js("!!document.querySelector('[data-testid=\"demo-panel\"]')", 10, "demo panel (phase 2)")

    assert not js("!!document.querySelector('[data-testid=\"demo-run\"]')"), (
        "Run the demo must NOT be shown when tkinter is missing"
    )
    note = js("(document.querySelector('[data-testid=\"demo-tk-note\"]')||{}).textContent || ''")
    assert "python3-tk" in note, f"python3-tk note missing: {note!r}"
    cmd = js("document.querySelector('[data-testid=\"demo-command\"]').value")
    assert "lightbulb_gui.py" in cmd, f"copyable command missing (phase 2): {cmd}"
    qa.capture(xd, "wiz1-tkinter-missing", {"expect": "no run button; command + python3-tk note"})
    qa.stop_session()


def main() -> int:
    qa.DATA_DIR = Path(
        os.environ.get(
            "BELLMAN_QA_DATA",
            "/tmp/bellman-qa-session/share/io.bellman.desktop",
        )
    )
    qa.DISPLAY_NAME = os.environ.get("DISPLAY", "")
    qa.OUT = Path(os.environ.get("BELLMAN_QA_OUT", str(OUT_DIR)))
    qa.OUT.mkdir(parents=True, exist_ok=True)

    demo_pid = None
    try:
        xd = qa.xdisp()
        demo_pid = phase1(xd)
        phase2(xd)
    finally:
        qa.stop_session()
        if demo_pid:
            try:
                os.kill(demo_pid, signal.SIGTERM)
            except OSError:
                pass
    print("=== QA WIZ1 complete ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
