#!/usr/bin/env python3
"""Re-take every product screenshot in docs/screenshots/ with friendly sample data.

One command (from the repo root, Linux only):

    /tmp/bellman-qa-venv/bin/python scripts/capture_docs_screenshots.py

What it does:
  1. Starts its own isolated Xvfb + metacity display (scripts/qa_display.sh) under
     a throwaway root (default /tmp/bellman-docs). HOME and every XDG dir point
     inside that root, so the operator's real data, settings and autostart entry
     are never read or written.
  2. Seeds sample timers there with the bellman CLI and the slot protocol (no QA names).
  3. Starts the lightbulb demo app, so the "Lightbulb demo" timer runs for real:
     fired -> acknowledged -> completed.
  4. Drives the app through tauri-driver + WebKitWebDriver (DOM clicks inside the
     webview only; no global pointer/keyboard injection) and grabs each screen,
     the window frame (title bar) included, with Xlib GetImage on the isolated display.
  5. Refuses to save a screen whose text holds the operator's user name, host name
     or home path.
  6. Stops everything it started and removes the throwaway root (--keep keeps it).

Prerequisites: docs/BUILD_PLAN.md, "Additionally, to RUN the GUI test suite", and a
current build of this tree (`cargo tauri build --no-bundle`).
"""
from __future__ import annotations

import argparse
import getpass
import json
import os
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "screenshots"

# Final size of every image: the whole window, title bar included.
FRAME_W, FRAME_H = 1260, 800

# Things that must never appear in an image (checked against the page text).
REAL_HOME = str(Path.home())
FORBIDDEN = sorted(
    {s for s in (REAL_HOME, getpass.getuser(), socket.gethostname()) if s and len(s) >= 3},
    key=len,
    reverse=True,
)

TZ = "Europe/London"
INSTALLED_DEMO = Path("/usr/share/bellman/testing_apps")

# name, CLI args (after --name). Every occurrence kind and both actions.
SAMPLE_TIMERS = [
    ("Morning backup", ["--occurrence", "daily", "--time", "07:30",
                        "--action", "launch", "--command", "{bin}/backup.sh"]),
    ("Stand-up reminder", ["--occurrence", "weekly", "--days", "mon,tue,wed,thu,fri",
                           "--time", "09:55", "--action", "notify",
                           "--title", "Stand-up in 5 minutes", "--body", "Video call, room 2"]),
    ("Water the plants", ["--occurrence", "weekly", "--days", "sat", "--time", "10:00",
                          "--action", "notify", "--title", "Water the plants"]),
    ("Pay rent", ["--occurrence", "monthly", "--day", "1", "--time", "09:00",
                  "--action", "notify", "--title", "Pay rent today"]),
    ("Renew domain", ["--occurrence", "yearly", "--month", "3", "--day", "14", "--time", "12:00",
                      "--action", "notify", "--title", "Renew the domain name"]),
    ("Sync photos", ["--occurrence", "interval", "--every-secs", str(12 * 3600),
                     "--action", "launch", "--command", "{bin}/sync-photos.sh"]),
    ("Clean temp files", ["--occurrence", "cron", "--cron", "0 3 * * *",
                          "--action", "launch", "--command", "{bin}/clean-temp.sh"]),
    ("Dentist appointment", ["--occurrence", "once", "--time", "{once}",
                             "--action", "notify", "--title", "Dentist at 14:30"]),
]
PAUSED = "Water the plants"
DEMO_TIMER = "Lightbulb demo"

SCRIPTS = {
    "backup.sh": "sleep 2\necho 'backup done: 1,284 files'\n",
    "sync-photos.sh": "sleep 1\necho 'photos in sync'\n",
    "clean-temp.sh": "echo 'removed 37 old files'\n",
}


def log(msg: str):
    print(msg, flush=True)


# ---------------------------------------------------------------------------
# Isolated display + environment
# ---------------------------------------------------------------------------

def start_display(root: Path, state: Path) -> None:
    os.environ["BELLMAN_QA_ROOT"] = str(root)
    os.environ["BELLMAN_QA_STATE"] = str(state)
    # Dark window frame from metacity (GTK theme), like the app itself.
    os.environ["GTK_THEME"] = "Adwaita:dark"
    subprocess.run([str(ROOT / "scripts/qa_display.sh"), "start"], check=True)
    env_txt = subprocess.check_output([str(ROOT / "scripts/qa_display.sh"), "env"], text=True)
    for line in env_txt.splitlines():
        parts = shlex.split(line)
        if len(parts) == 2 and parts[0] == "export" and "=" in parts[1]:
            k, v = parts[1].split("=", 1)
            os.environ[k] = v


def stop_display() -> None:
    subprocess.run([str(ROOT / "scripts/qa_display.sh"), "stop"], check=False)


def isolate_home(root: Path) -> None:
    """Point HOME inside the throwaway root (the driver path is resolved first)."""
    os.environ.setdefault("TAURI_DRIVER", str(Path.home() / ".cargo/bin/tauri-driver"))
    home = root / "home"
    home.mkdir(parents=True, exist_ok=True)
    os.environ["HOME"] = str(home)
    # Window-manager title bar instead of GTK's client-side one: without a
    # compositor its shadow would draw as a black border round the window.
    os.environ["GTK_CSD"] = "0"
    os.environ["TZ"] = TZ
    os.environ["LANG"] = os.environ["LC_ALL"] = "en_US.UTF-8"
    if (INSTALLED_DEMO / "lightbulb_gui/lightbulb_gui.py").is_file():
        os.environ["BELLMAN_DEMO_DIR"] = str(INSTALLED_DEMO)
    else:
        # Never let the app fall back to the source tree path (it holds a home dir).
        shutil.copytree(ROOT / "testing_apps", root / "testing_apps")
        os.environ["BELLMAN_DEMO_DIR"] = str(root / "testing_apps")


# ---------------------------------------------------------------------------
# Sample data
# ---------------------------------------------------------------------------

class Store:
    def __init__(self, data: Path, cli: str):
        self.data = data
        self.cli = cli
        self.db = data / "timers.db"
        self.slots = data / "slots"

    def run(self, *args: str) -> dict:
        r = subprocess.run([self.cli, "--json", "--db", str(self.db), *args],
                           capture_output=True, text=True, timeout=30)
        if r.returncode != 0:
            raise RuntimeError(f"bellman {' '.join(args)} failed:\n{r.stdout}\n{r.stderr}")
        return json.loads(r.stdout) if r.stdout.strip() else {}

    def timer_id(self, name: str) -> str:
        for t in self.run("list").get("timers", []):
            if t.get("name") == name:
                return t["id"]
        raise RuntimeError(f"no timer named {name!r}")


def seed(store: Store, root: Path) -> None:
    data = store.data
    for sub in ("logs", "slots"):
        (data / sub).mkdir(parents=True, exist_ok=True)
    (data / "config.json").write_text(json.dumps({
        "wizard_completed": True,
        "autostart_enabled": False,
        "start_minimized": False,
        "wake_enabled": False,
        "demo_opt_in": True,
    }) + "\n")

    bin_dir = root / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    for name, body in SCRIPTS.items():
        p = bin_dir / name
        p.write_text("#!/bin/sh\n" + body)
        p.chmod(0o755)

    once = time.strftime("%Y-%m-%dT14:00:00", time.localtime(time.time() + 9 * 86400))
    for name, args in SAMPLE_TIMERS:
        args = [a.format(bin=bin_dir, once=once) for a in args]
        store.run("add", "--name", name, "--tz", TZ, *args)
    store.run("pause", store.timer_id(PAUSED))

    # The lightbulb owns its timer: created by the app through the slot protocol.
    req = {
        "schema": "bellman-slot/1",
        "request_id": str(uuid.uuid4()),
        "operation": "add",
        "payload": {
            "app_name": "lightbulb",
            "timer_name": DEMO_TIMER,
            "tz": TZ,
            "occurrence": {"kind": "daily", "time": "19:00:00"},
        },
    }
    req_path = root / "lightbulb-add.json"
    req_path.write_text(json.dumps(req))
    store.run("slot-submit", str(req_path), "--slots", str(store.slots))
    log(f"seeded {len(SAMPLE_TIMERS) + 1} timers in {data}")


def start_lightbulb(store: Store, root: Path, on_secs: int) -> subprocess.Popen:
    demo = Path(os.environ["BELLMAN_DEMO_DIR"]) / "lightbulb/lightbulb.py"
    if not demo.is_file():
        demo = ROOT / "testing_apps/lightbulb/lightbulb.py"
    logf = open(root / "lightbulb.log", "w")
    return subprocess.Popen(
        [sys.executable, str(demo), "--slots", str(store.slots),
         "--app-name", "lightbulb", "--on-secs", str(on_secs)],
        stdout=logf, stderr=subprocess.STDOUT, start_new_session=True,
    )


def demo_state(store: Store) -> str:
    for folder in (store.data / "timers").glob("*/"):
        p = folder / "status.json"
        if p.exists():
            try:
                doc = json.loads(p.read_text())
            except Exception:
                continue
            if doc.get("timer_name") == DEMO_TIMER or DEMO_TIMER.lower().replace(" ", "-") in folder.name.lower():
                return doc.get("state", "")
    return ""


def wait_demo_state(store: Store, states: set[str], timeout: float) -> str:
    end = time.time() + timeout
    st = ""
    while time.time() < end:
        st = demo_state(store)
        if st in states:
            return st
        time.sleep(0.3)
    raise RuntimeError(f"{DEMO_TIMER} never reached {states}; last state {st!r}")


# ---------------------------------------------------------------------------
# WebView helpers (DOM only)
# ---------------------------------------------------------------------------

def js(drv, script: str, *args):
    return drv.execute_script(script, *args)


def row_button(drv, timer_name: str, label_re: str) -> str:
    return js(drv, """
        const [name, re] = arguments;
        const rows = [...document.querySelectorAll('table.timer-table tbody tr')];
        const row = rows.find(r => r.querySelector('.name-text')?.textContent.trim() === name);
        if (!row) return 'no-row';
        const btn = [...row.querySelectorAll('button')].find(b => new RegExp(re).test(b.textContent.trim()));
        if (!btn) return 'no-btn';
        btn.click();
        return 'ok';
    """, timer_name, label_re)


def must(result: str, what: str):
    if result != "ok":
        raise RuntimeError(f"{what}: {result}")


def wait_text(drv, needle: str, timeout: float = 12.0, css: str = "body") -> None:
    end = time.time() + timeout
    while time.time() < end:
        txt = js(drv, "return document.querySelector(arguments[0])?.innerText || '';", css) or ""
        if needle in txt:
            return
        time.sleep(0.25)
    raise RuntimeError(f"text {needle!r} never appeared in {css}")


def wait_quiet(drv, timeout: float = 15.0) -> None:
    """No toast on screen (they fade after ~3.5 s). An error toast is a failure."""
    end = time.time() + timeout
    while time.time() < end:
        n_err = js(drv, "return document.querySelectorAll('.toast.err').length;")
        if n_err:
            txt = js(drv, "return document.querySelector('.toast.err').innerText;")
            raise RuntimeError(f"error toast on screen: {txt!r}")
        if not js(drv, "return document.querySelectorAll('.toast').length;"):
            return
        time.sleep(0.25)
    raise RuntimeError("toasts did not clear")


def tab(qa, label: str, settle: float = 1.0):
    qa.click_tab(label)
    time.sleep(settle)


def scroll_main(drv, where: str):
    """Scroll the page (and any scrolling main container) to top, bottom, or an element."""
    js(drv, """
        const where = arguments[0];
        const els = [document.scrollingElement, document.querySelector('main')].filter(Boolean);
        if (where === 'top') { els.forEach(e => e.scrollTop = 0); return; }
        if (where === 'bottom') { els.forEach(e => e.scrollTop = e.scrollHeight); return; }
        const t = document.querySelector(where);
        if (t) t.scrollIntoView({block: 'start'});
    """, where)
    time.sleep(0.5)


def page_scrolls(drv) -> bool:
    return bool(js(drv, """
        const els = [document.scrollingElement, document.querySelector('main')].filter(Boolean);
        return els.some(e => e.scrollHeight > e.clientHeight + 8);
    """))


# ---------------------------------------------------------------------------
# Window placement + capture (Xlib GetImage on the isolated display, read-only)
# ---------------------------------------------------------------------------

def client_wid(qa) -> int:
    _, _, _, _, _, wid = qa.raise_and_geom(None)
    return wid


def frame_of(xd, wid: int):
    root = xd.screen().root
    w = xd.create_resource_object("window", wid)
    while True:
        parent = w.query_tree().parent
        if parent.id == root.id:
            return w
        w = parent


def visible_rect(xd, wid: int) -> tuple[int, int, int, int]:
    """The window as a person sees it: client area + the WM's visible frame
    (_NET_FRAME_EXTENTS), without metacity's invisible resize borders, which
    would show as a black margin on a display with no compositor."""
    client = xd.create_resource_object("window", wid)
    root = xd.screen().root
    pos = root.translate_coords(client, 0, 0)
    geo = client.get_geometry()
    prop = client.get_full_property(xd.intern_atom("_NET_FRAME_EXTENTS"), 0)
    left, right, top, bottom = (list(prop.value) if prop else [0, 0, 0, 0])[:4]
    return pos.x - left, pos.y - top, geo.width + left + right, geo.height + top + bottom


def place_window(qa) -> None:
    """Size the window so what is visible (title bar included) is FRAME_W x FRAME_H."""
    xd = qa.xdisp()
    wid = client_wid(qa)
    x0 = max(0, (xd.screen().width_in_pixels - FRAME_W) // 2)
    px, py = x0, 0  # position asked of wmctrl; corrected by where the window lands
    for _ in range(8):
        vx, vy, vw, vh = visible_rect(xd, wid)
        geo = xd.create_resource_object("window", wid).get_geometry()
        if (vx, vy, vw, vh) == (x0, 0, FRAME_W, FRAME_H):
            log(f"window {FRAME_W}x{FRAME_H} at ({x0},0), title bar {FRAME_H - geo.height}px")
            return
        if (vw, vh) == (FRAME_W, FRAME_H):
            px, py = px + (x0 - vx), py + (0 - vy)
        subprocess.run(
            ["wmctrl", "-i", "-r", hex(wid), "-e",
             f"0,{px},{py},{FRAME_W - (vw - geo.width)},{FRAME_H - (vh - geo.height)}"],
            check=False,
        )
        time.sleep(0.8)
    raise RuntimeError(f"could not size the window: visible rect {visible_rect(xd, wid)}")


def privacy_check(drv, name: str) -> None:
    txt = (js(drv, """
        const parts = [document.body.innerText];
        document.querySelectorAll('input, textarea').forEach(e => parts.push(e.value || ''));
        return parts.join('\\n');
    """) or "") + "\n" + (js(drv, "return document.title;") or "")
    low = txt.lower()
    for s in FORBIDDEN:
        if s.lower() in low:
            raise RuntimeError(f"{name}: screen text contains personal info ({s[:2]}...) - not saved")


def capture(qa, drv, name: str) -> Path:
    from Xlib import X
    from PIL import Image

    privacy_check(drv, name)
    xd = qa.xdisp()
    wid = client_wid(qa)
    time.sleep(0.4)
    vx, vy, vw, vh = visible_rect(xd, wid)
    root = xd.screen().root
    raw = root.get_image(vx, vy, vw, vh, X.ZPixmap, 0xFFFFFFFF)
    img = Image.frombytes("RGB", (vw, vh), raw.data, "raw", "BGRX")
    if img.size != (FRAME_W, FRAME_H):
        raise RuntimeError(f"{name}: window is {img.size}, expected {(FRAME_W, FRAME_H)}")
    OUT.mkdir(parents=True, exist_ok=True)
    path = OUT / f"{name}.png"
    img.save(path, optimize=True)
    if path.stat().st_size > 300_000:
        img.quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(
            path, optimize=True)
    log(f"  captured {path.name} {img.size[0]}x{img.size[1]} {path.stat().st_size // 1024} KB")
    return path


# ---------------------------------------------------------------------------
# The tour
# ---------------------------------------------------------------------------

def tour(qa, store: Store, only: set[str] | None) -> tuple[list[str], list[str]]:
    taken: list[str] = []
    skipped: list[str] = []

    def want(name: str) -> bool:
        return only is None or name in only

    def shot(name: str):
        wait_quiet(drv)
        capture(qa, drv, name)
        taken.append(name)

    drv = qa.start_session()
    time.sleep(2.0)
    place_window(qa)
    tab(qa, "All timers", 1.5)

    # Real runs so Run history, Log, Week and Month have entries.
    for name in ("Morning backup", "Pay rent", "Clean temp files", "Stand-up reminder"):
        must(row_button(drv, name, "^Run now$"), f"run now {name}")
        time.sleep(1.2)
    must(row_button(drv, DEMO_TIMER, "^Run now$"), "run now lightbulb")
    wait_demo_state(store, {"acknowledged", "running"}, 20)
    wait_text(drv, "acknowledged", 15, "table.timer-table")
    if want("run-live"):
        time.sleep(1.0)
        shot("run-live")
    wait_demo_state(store, {"completed"}, 60)
    time.sleep(3.0)  # let the scripts finish and the rows refresh

    # 1. All timers
    tab(qa, "Week", 0.5)
    tab(qa, "All timers", 1.5)
    if want("all-timers"):
        shot("all-timers")

    # 9. A timer's Log view
    if want("timer-log"):
        must(row_button(drv, DEMO_TIMER, "^(Log|Hide log)$"), "open log")
        wait_text(drv, "completed", 10, ".log-panel")
        time.sleep(0.6)
        scroll_main(drv, "bottom")
        shot("timer-log")
        must(row_button(drv, DEMO_TIMER, "^(Log|Hide log)$"), "close log")
        scroll_main(drv, "top")

    # 7. New timer, empty
    if want("timer-new"):
        qa.click_button("+ New timer")
        time.sleep(1.2)
        shot("timer-new")
        qa.close_dialog_if_open()
        time.sleep(0.6)

    # 8. Edit timer with its preview of next fire times
    if want("timer-edit"):
        must(row_button(drv, "Stand-up reminder", "^Edit$"), "open edit")
        end = time.time() + 8
        while time.time() < end and qa.get_input_value("#td-name") != "Stand-up reminder":
            time.sleep(0.25)
        if qa.get_input_value("#td-name") != "Stand-up reminder":
            raise RuntimeError("edit dialog did not open for Stand-up reminder")
        end = time.time() + 10
        while time.time() < end and not js(drv, "return document.querySelectorAll('.preview-table tbody tr').length;"):
            time.sleep(0.25)
        time.sleep(0.8)
        shot("timer-edit")
        qa.close_dialog_if_open()
        time.sleep(0.6)

    # 2-4. Week, Month, Run history
    for label, name in (("Week", "week"), ("Month", "month"), ("Run history", "run-history")):
        if want(name):
            tab(qa, label, 1.8)
            scroll_main(drv, "top")
            shot(name)

    # 5. Settings (and the rest of it if the page scrolls)
    tab(qa, "Settings", 1.8)
    scroll_main(drv, "top")
    if want("settings"):
        shot("settings")
    if want("settings-2") and page_scrolls(drv):
        # The middle of the page: Misfire defaults and Engine (Demo + Data are in demo.png).
        js(drv, """
            const h = [...document.querySelectorAll('.settings-section h3')]
              .find(e => e.textContent.trim() === 'Misfire defaults');
            if (h) h.closest('.settings-section').scrollIntoView({block: 'start'});
        """)
        time.sleep(0.5)
        shot("settings-2")
    elif want("settings-2"):
        skipped.append("settings-2 (Settings fits in one screen)")

    # 10. Demo panel (the lightbulb demo offer in Settings)
    if want("demo"):
        scroll_main(drv, "[data-testid='settings-demo']")
        shot("demo")

    # 6. First-run wizard, every step (opened with Settings > Run setup again)
    if only is None or any(n.startswith("wizard") for n in only):
        scroll_main(drv, "bottom")
        qa.click_button("Run setup again")
        wait_text(drv, "Welcome to Bellman", 8, ".wizard")
        js(drv, """
            const d = document.querySelector('#w-demo');
            if (d && !d.checked) d.click();
        """)
        time.sleep(0.8)
        shot("wizard-1")
        qa.click_button("Next")
        wait_text(drv, "wake-up from sleep", 8, ".wizard")
        time.sleep(0.8)
        shot("wizard-2")
        qa.click_button("No thanks")
        wait_text(drv, "Setup complete", 15, ".wizard")
        time.sleep(1.2)
        shot("wizard-3")
        qa.click_button("Continue")
        time.sleep(1.0)

    skipped.append("tray-menu (the tray menu is drawn by the desktop's tray host; "
                   "opening it needs a real click, which the harness forbids)")
    return taken, skipped


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", default="/tmp/bellman-docs",
                    help="throwaway session root (wiped at start and at the end)")
    ap.add_argument("--keep", action="store_true", help="keep the throwaway root for debugging")
    ap.add_argument("--only", nargs="*", help="capture only these names (e.g. week month)")
    args = ap.parse_args()

    root = Path(args.root)
    if root.resolve() in (Path("/"), Path.home().resolve(), ROOT.resolve()) or ROOT.resolve().is_relative_to(root.resolve()):
        print(f"refusing unsafe --root {root}", file=sys.stderr)
        return 2
    state = root.with_name(root.name + ".display-state")
    cli = os.environ.get("BELLMAN_CLI", str(ROOT / "target/release/bellman"))
    app = os.environ.get("BELLMAN_APP", str(ROOT / "target/release/bellman-app"))
    for p in (cli, app):
        if not os.access(p, os.X_OK):
            print(f"missing {p} - build first: cargo tauri build --no-bundle", file=sys.stderr)
            return 2
    os.environ["BELLMAN_APP"] = app
    os.environ["BELLMAN_CLI"] = cli

    real_autostart = Path.home() / ".config/autostart/Bellman.desktop"
    autostart_before = real_autostart.read_bytes() if real_autostart.exists() else None

    lightbulb = None
    qa = None
    try:
        start_display(root, state)
        isolate_home(root)
        store = Store(Path(os.environ["BELLMAN_QA_DATA"]), cli)
        seed(store, root)
        lightbulb = start_lightbulb(store, root, on_secs=25)

        sys.path.insert(0, str(ROOT / "scripts"))
        import qa_webdriver as qa  # noqa: E402  (reads DISPLAY at import)

        taken, skipped = tour(qa, store, set(args.only) if args.only else None)
        log("\ncaptured: " + ", ".join(taken))
        for s in skipped:
            log(f"not captured: {s}")
        return 0
    finally:
        if qa is not None:
            qa.stop_session()
        if lightbulb is not None and lightbulb.poll() is None:
            os.killpg(lightbulb.pid, signal.SIGTERM)
            try:
                lightbulb.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(lightbulb.pid, signal.SIGKILL)
        stop_display()
        state.unlink(missing_ok=True)
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)
        after = real_autostart.read_bytes() if real_autostart.exists() else None
        if after != autostart_before:
            log("WARNING: the real autostart entry changed during the run")


if __name__ == "__main__":
    sys.exit(main())
