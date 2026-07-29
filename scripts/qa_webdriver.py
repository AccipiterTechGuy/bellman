#!/usr/bin/env python3
"""Bellman GUI QA — WebDriver session + in-webview interaction.

Replaces global-input-injection / global pointer injection. Clicks and typing are WebDriver
commands against DOM elements inside the WebKit webview (via tauri-driver +
WebKitWebDriver). Screenshots may still use Xlib GetImage on the isolated
display (read-only; does not hijack input).

Prerequisites (see docs/BUILD_PLAN.md "to RUN the GUI test suite"):
  - webkit2gtk-driver  (WebKitWebDriver, version-matched to libwebkit2gtk-4.1)
  - cargo install tauri-driver --locked
  - selenium (Python)
  - an isolated display from scripts/qa_display.sh
"""
from __future__ import annotations

import atexit
import json
import os
import shutil
import signal
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]

# ---------------------------------------------------------------------------
# Paths / env
# ---------------------------------------------------------------------------

DISPLAY_NAME = os.environ.get("DISPLAY", "")
DATA_DIR = Path(
    os.environ.get(
        "BELLMAN_QA_DATA",
        "/tmp/bellman-qa-session/share/io.bellman.desktop",
    )
)
OUT = ROOT / "docs" / "qa4-screenshots"
EVIDENCE = ROOT / "docs" / "qa4-evidence"

_cli_candidates = [
    os.environ.get("BELLMAN_CLI", ""),
    str(ROOT / "target/release/bellman"),
    "/tmp/bellman-deb-extract/usr/bin/bellman",
    "/tmp/bellman-cli-schema3",
]
CLI_BIN = next((p for p in _cli_candidates if p and Path(p).exists()), "bellman")

_app_candidates = [
    os.environ.get("BELLMAN_APP", ""),
    str(ROOT / "target/release/bellman-app"),
    "/tmp/bellman-deb-extract/usr/bin/bellman-app",
    str(Path.home() / "bellman/target/release/bellman-app"),
]
APP_BIN = next((p for p in _app_candidates if p and Path(p).exists()), "")

TAURI_DRIVER = os.environ.get(
    "TAURI_DRIVER",
    str(Path.home() / ".cargo/bin/tauri-driver"),
)
WEBDRIVER_PORT = int(os.environ.get("BELLMAN_WEBDRIVER_PORT", "0"))  # 0 = auto-pick free port
WEBDRIVER_URL = os.environ.get("BELLMAN_WEBDRIVER_URL", "")

# Module-level driver (set by start_session)
_driver = None
_driver_proc: subprocess.Popen | None = None
_driver_log_path: Path | None = None
_active_port: int | None = None


# ---------------------------------------------------------------------------
# Session lifecycle
# ---------------------------------------------------------------------------

def _require_display() -> str:
    disp = os.environ.get("DISPLAY", "")
    if not disp:
        raise RuntimeError(
            "DISPLAY is unset. Start an isolated display first:\n"
            "  scripts/qa_display.sh start && eval \"$(scripts/qa_display.sh env)\""
        )
    if disp in (":0", ":0.0") and os.environ.get("BELLMAN_QA_ALLOW_DISPLAY0") != "1":
        raise RuntimeError(
            f"Refusing to run GUI QA on operator display {disp!r}. "
            "Use scripts/qa_display.sh (isolated Xvfb). "
            "Set BELLMAN_QA_ALLOW_DISPLAY0=1 only for emergency debugging."
        )
    return disp


def _resolve_app() -> str:
    if APP_BIN and Path(APP_BIN).exists():
        return APP_BIN
    raise RuntimeError(
        "bellman-app binary not found. Build with `cargo tauri build --no-bundle` "
        "or set BELLMAN_APP=/path/to/bellman-app. "
        f"Tried: {_app_candidates}"
    )


def _pick_free_port() -> int:
    import socket

    if WEBDRIVER_PORT > 0:
        return WEBDRIVER_PORT
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def start_session(*, application: str | None = None) -> Any:
    """Start tauri-driver under a private D-Bus session and open a WebDriver client.

    Returns the selenium WebDriver. Env (DISPLAY, XDG_*) must already be set
    for the isolated display (qa_display.sh).
    """
    global _driver, _driver_proc, _driver_log_path, _active_port, WEBDRIVER_URL

    if _driver is not None:
        return _driver

    _require_display()
    app = application or _resolve_app()
    if not Path(TAURI_DRIVER).exists():
        raise RuntimeError(
            f"tauri-driver not found at {TAURI_DRIVER}. "
            "Install: cargo install tauri-driver --locked"
        )
    if not shutil.which("WebKitWebDriver"):
        raise RuntimeError(
            "WebKitWebDriver not on PATH. Install: sudo apt install -y webkit2gtk-driver "
            "(must match libwebkit2gtk-4.1 version)"
        )

    try:
        from selenium import webdriver
        from selenium.webdriver.common.options import ArgOptions
    except ImportError as e:
        raise RuntimeError(
            "selenium is required. Install into a venv, e.g.\n"
            "  python3 -m venv /tmp/bellman-qa-venv && "
            "/tmp/bellman-qa-venv/bin/pip install selenium pillow python-xlib"
        ) from e

    # Private session bus so tauri_plugin_single_instance does not forward to
    # a live operator instance (and so a second QA launch is not killed).
    env = os.environ.copy()
    env["GDK_BACKEND"] = "x11"
    env.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
    env.setdefault("WEBKIT_DISABLE_COMPOSITING_MODE", "1")
    env["GIO_USE_VFS"] = "local"
    env["GTK_USE_PORTAL"] = "0"
    # Marker so stop_session / run_gui_qa can reap only QA-owned drivers.
    env["BELLMAN_QA"] = "1"
    env.setdefault("BELLMAN_QA_DATA", str(DATA_DIR))

    port = _pick_free_port()
    _active_port = port
    WEBDRIVER_URL = f"http://127.0.0.1:{port}"
    _driver_log_path = Path(os.environ.get("BELLMAN_QA_ROOT", "/tmp")) / "tauri-driver.log"

    driver_cmd = [
        TAURI_DRIVER,
        "--port",
        str(port),
    ]
    native = shutil.which("WebKitWebDriver")
    if native:
        driver_cmd.extend(["--native-driver", native])

    logf = open(_driver_log_path, "w")
    # Private D-Bus session so tauri_plugin_single_instance does not attach to a
    # live operator instance. The app is a child of tauri-driver → dbus-run-session.
    # start_new_session=True puts the whole tree in its own process group so
    # stop_session can killpg (dbus-run-session + tauri-driver + WebKitWebDriver).
    use_private_bus = os.environ.get("BELLMAN_QA_PRIVATE_BUS", "1") != "0"
    full_cmd = (["dbus-run-session", "--"] + driver_cmd) if use_private_bus else driver_cmd
    _driver_proc = subprocess.Popen(
        full_cmd,
        env=env,
        stdout=logf,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=True,
    )
    atexit.register(stop_session)

    # Wait for the driver port.
    deadline = time.time() + 25
    import socket

    while time.time() < deadline:
        if _driver_proc.poll() is not None:
            logf.flush()
            out = _driver_log_path.read_text() if _driver_log_path.exists() else ""
            raise RuntimeError(f"tauri-driver exited early (port={port}):\n{out}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.3):
                break
        except OSError:
            time.sleep(0.15)
    else:
        stop_session()
        out = _driver_log_path.read_text() if _driver_log_path and _driver_log_path.exists() else ""
        raise RuntimeError(f"tauri-driver did not listen on :{port}\n{out}")

    class TauriOptions(ArgOptions):
        def __init__(self, application_path: str):
            super().__init__()
            self._caps = {
                "browserName": "wry",
                "tauri:options": {"application": application_path},
            }

        def to_capabilities(self):
            # Selenium 4 merges these into firstMatch/alwaysMatch; keep minimal.
            return dict(self._caps)

        @property
        def default_capabilities(self):
            return {}

        def set_capability(self, name, value):
            self._caps[name] = value
            return self

        def to_capabilities_for_session(self):
            return self.to_capabilities()

    opts = TauriOptions(app)
    try:
        # No trailing slash — selenium joins "/session" and would produce "//session".
        _driver = webdriver.Remote(
            command_executor=f"http://127.0.0.1:{port}",
            options=opts,
        )
    except Exception as e:
        logf.flush()
        out = _driver_log_path.read_text() if _driver_log_path.exists() else ""
        stop_session()
        raise RuntimeError(
            f"WebDriver NEW_SESSION failed (port={port}, app={app}): {e}\n"
            f"tauri-driver log:\n{out}"
        ) from e

    # Let the webview paint and Svelte mount.
    time.sleep(2.0)
    # Best-effort: wait for topbar.
    try:
        from selenium.webdriver.common.by import By
        from selenium.webdriver.support.ui import WebDriverWait
        from selenium.webdriver.support import expected_conditions as EC

        WebDriverWait(_driver, 15).until(
            EC.presence_of_element_located((By.CSS_SELECTOR, "button.tab, .topbar"))
        )
    except Exception as e:
        print(f"  warn: webview ready wait: {e}")
    print(f"  WebDriver session ready on port {port} app={app}")
    return _driver


def driver():
    if _driver is None:
        raise RuntimeError("WebDriver session not started — call start_session() first")
    return _driver


def stop_session():
    """Tear down WebDriver client + the whole driver process group.

    Must not leave tauri-driver / WebKitWebDriver listening (card G7).
    """
    global _driver, _driver_proc, _active_port
    if _driver is not None:
        try:
            _driver.quit()
        except Exception:
            pass
        _driver = None

    if _driver_proc is not None:
        pgid = None
        try:
            pgid = os.getpgid(_driver_proc.pid)
        except Exception:
            pgid = None
        # Prefer process-group kill: covers dbus-run-session → tauri-driver →
        # WebKitWebDriver → bellman-app. Plain kill(pid) only hits the wrapper
        # and leaves orphans reparented to init with LISTENING ports.
        try:
            if pgid is not None and pgid > 1:
                os.killpg(pgid, signal.SIGTERM)
            else:
                os.kill(_driver_proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except Exception:
            try:
                os.kill(_driver_proc.pid, signal.SIGTERM)
            except Exception:
                pass
        try:
            _driver_proc.wait(timeout=4)
        except subprocess.TimeoutExpired:
            try:
                if pgid is not None and pgid > 1:
                    os.killpg(pgid, signal.SIGKILL)
                else:
                    os.kill(_driver_proc.pid, signal.SIGKILL)
            except Exception:
                pass
            try:
                _driver_proc.wait(timeout=2)
            except Exception:
                pass
        except Exception:
            pass
        _driver_proc = None
    _active_port = None

    # Sweep any remaining QA-owned processes (env guard so operator instances live).
    try:
        data = str(DATA_DIR).encode()
        out = subprocess.check_output(["ps", "-eo", "pid,cmd"], text=True)
        for line in out.splitlines():
            low = line.lower()
            if not any(
                k in low
                for k in ("bellman-app", "tauri-driver", "webkitwebdriver")
            ):
                continue
            if "grep" in low:
                continue
            try:
                pid = int(line.split()[0])
            except (ValueError, IndexError):
                continue
            try:
                envp = Path(f"/proc/{pid}/environ").read_bytes()
            except Exception:
                continue
            if (
                data in envp
                or b"bellman-qa" in envp
                or b"BELLMAN_QA" in envp
                or b"qa-p4" in envp
            ):
                try:
                    os.kill(pid, signal.SIGTERM)
                except Exception:
                    pass
    except Exception:
        pass


# ---------------------------------------------------------------------------
# DOM interaction (NO global-input-injection)
# ---------------------------------------------------------------------------

def _by():
    from selenium.webdriver.common.by import By
    return By


def click_button(label: str, *, exact: bool = True, timeout: float = 8.0):
    """Click a <button> (or role=button) whose visible text matches label.

    Falls back to JS click when the native click is intercepted (e.g. wizard
    backdrop still fading out).
    """
    d = driver()
    By = _by()
    end = time.time() + timeout
    last_err = None
    while time.time() < end:
        try:
            buttons = d.find_elements(By.CSS_SELECTOR, "button, [role='button']")
            for b in buttons:
                try:
                    text = (b.text or "").strip()
                    al = (b.get_attribute("aria-label") or "").strip()
                    match = (
                        (text == label or al == label)
                        if exact
                        else (
                            label.lower() in text.lower()
                            or label.lower() in al.lower()
                        )
                    )
                    if not match:
                        continue
                    try:
                        b.click()
                    except Exception as click_err:
                        last_err = click_err
                        d.execute_script("arguments[0].click()", b)
                    return b
                except Exception as e:
                    last_err = e
                    continue
        except Exception as e:
            last_err = e
        time.sleep(0.15)
    raise RuntimeError(f"button {label!r} not found (last={last_err})")


def click_tab(label: str):
    """Click a top-bar tab by label (All timers / Week / Month / Run history)."""
    click_button(label)
    time.sleep(0.55)


def set_input_value(css: str, value: str, *, clear: bool = True):
    """Set an input/textarea value and dispatch input/change so Svelte binds update."""
    from selenium.webdriver.support.ui import WebDriverWait

    d = driver()
    By = _by()
    # Wait for the field (kind switches remount conditional inputs).
    WebDriverWait(d, 6).until(lambda drv: len(drv.find_elements(By.CSS_SELECTOR, css)) > 0)
    el = d.find_element(By.CSS_SELECTOR, css)
    # Prefer JS assignment + events — more reliable than send_keys with WebKitGTK.
    d.execute_script(
        """
        const el = arguments[0];
        const val = arguments[1];
        const clear = arguments[2];
        el.focus();
        const proto = el.tagName === 'TEXTAREA'
          ? window.HTMLTextAreaElement.prototype
          : window.HTMLInputElement.prototype;
        const desc = Object.getOwnPropertyDescriptor(proto, 'value');
        const setVal = (v) => {
          if (desc && desc.set) desc.set.call(el, v);
          else el.value = v;
          el.dispatchEvent(new Event('input', { bubbles: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));
        };
        if (clear) setVal('');
        setVal(val);
        """,
        el,
        value,
        clear,
    )
    time.sleep(0.05)
    return el


def get_input_value(css: str) -> str:
    d = driver()
    By = _by()
    el = d.find_element(By.CSS_SELECTOR, css)
    return el.get_attribute("value") or ""


def select_kind(kind: str):
    """Select occurrence kind via the native <select id=td-kind>.

    Svelte 5 `bind:value` on <select> listens for the browser change event and
    also tracks the property setter — we do both, then wait for kind-specific
    fields to mount (e.g. #td-once-date for once).
    """
    from selenium.webdriver.support.ui import Select
    from selenium.webdriver.support.ui import WebDriverWait

    d = driver()
    By = _by()
    kind = kind.lower()
    el = d.find_element(By.CSS_SELECTOR, "#td-kind")
    # Native Select path
    try:
        Select(el).select_by_value(kind)
    except Exception:
        pass
    # Force Svelte bind:value to see the new value
    d.execute_script(
        """
        const s = document.getElementById('td-kind');
        const val = arguments[0];
        const proto = window.HTMLSelectElement
          ? window.HTMLSelectElement.prototype
          : Object.getPrototypeOf(s);
        const desc = Object.getOwnPropertyDescriptor(proto, 'value');
        if (desc && desc.set) {
          desc.set.call(s, val);
        } else {
          s.value = val;
        }
        s.dispatchEvent(new Event('input', { bubbles: true }));
        s.dispatchEvent(new Event('change', { bubbles: true }));
        """,
        kind,
    )
    # Wait for kind-specific controls (support both older single-field UI and
    # newer C8d split once-date/time + weekday chips).
    wait_any = {
        "once": ("#td-once-date", "#td-once"),
        "interval": ("#td-every",),
        "daily": ("#td-time",),
        "weekly": ("button.weekday-chip", "#td-days", "input[id*='day']"),
        "monthly": ("#td-day",),
        "yearly": ("#td-month",),
        "cron": ("#td-cron",),
    }.get(kind, ())

    def _visible(drv, css):
        els = drv.find_elements(By.CSS_SELECTOR, css)
        return any(e.is_displayed() for e in els)

    if wait_any:
        try:
            WebDriverWait(d, 5).until(
                lambda drv: any(_visible(drv, css) for css in wait_any)
            )
        except Exception as e:
            print(f"  warn: select_kind wait for {wait_any}: {e}")
    time.sleep(0.25)
    print(f"  select_kind {kind!r} via <select>")
    return kind


def set_weekdays(days_csv: str):
    """Toggle weekday chips so exactly the days in 'mon,wed,fri' are on."""
    wanted = {p.strip()[:3].lower() for p in days_csv.split(",") if p.strip()}
    labels = {
        "mon": "Mon",
        "tue": "Tue",
        "wed": "Wed",
        "thu": "Thu",
        "fri": "Fri",
        "sat": "Sat",
        "sun": "Sun",
    }
    d = driver()
    By = _by()
    for key, lab in labels.items():
        chips = d.find_elements(By.CSS_SELECTOR, "button.weekday-chip")
        chip = None
        for c in chips:
            if (c.text or "").strip() == lab:
                chip = c
                break
        if chip is None:
            continue
        pressed = (chip.get_attribute("aria-pressed") or "").lower() == "true"
        # class "on" fallback
        if not pressed:
            cls = chip.get_attribute("class") or ""
            pressed = " on" in f" {cls}" or cls.endswith(" on") or "on" in cls.split()
        should = key in wanted
        if pressed != should:
            chip.click()
            time.sleep(0.06)


# Map legacy AT-SPI field names / substrings to CSS selectors + special handlers.
FIELD_MAP = {
    "name": "#td-name",
    "timezone": "#td-tz",
    "every": "#td-every",
    "every (seconds)": "#td-every",
    "wall-clock": "#td-time",
    "wall-clock time": "#td-time",
    "day of month": "#td-day",
    "month": "#td-month",
    "month (1": "#td-month",
    "cron": "#td-cron",
    "cron expression": "#td-cron",
    "once date": "#td-once-date",
    "once time": "#td-once-time",
    "when": "WHEN",  # special: single #td-once (older UI) or split date/time
    "weekdays": "WEEKDAYS",  # special: chip toggles or #td-days text
}


def _resolve_field(name: str) -> str:
    key = name.lower().strip()
    if key in FIELD_MAP:
        return FIELD_MAP[key]
    for k, v in FIELD_MAP.items():
        if k in key or key in k:
            return v
    raise RuntimeError(f"unknown form field {name!r}")


def _first_visible(*css_list: str) -> str | None:
    d = driver()
    By = _by()
    for css in css_list:
        for el in d.find_elements(By.CSS_SELECTOR, css):
            try:
                if el.is_displayed():
                    return css
            except Exception:
                continue
    return None


def fill_field(name: str, value: str):
    target = _resolve_field(name)
    if target == "WHEN":
        # Older binary: single #td-once ISO field.
        # Newer C8d UI: #td-once-date + #td-once-time.
        if _first_visible("#td-once"):
            set_input_value("#td-once", value)
            got = get_input_value("#td-once")
            print(f"    field {name!r} -> {got!r} (want {value!r})")
            return
        raw = value.strip().replace(" ", "T")
        if "T" in raw:
            date, time_part = raw.split("T", 1)
        else:
            date, time_part = raw, "09:00"
        set_input_value("#td-once-date", date)
        set_input_value("#td-once-time", time_part)
        got = f"{get_input_value('#td-once-date')}T{get_input_value('#td-once-time')}"
        print(f"    field {name!r} -> {got!r} (want {value!r})")
        return
    if target == "WEEKDAYS":
        if _first_visible("button.weekday-chip"):
            set_weekdays(value)
            print(f"    field {name!r} -> chips {value!r}")
            return
        # Older free-text weekdays field
        css = _first_visible("#td-days", "input[id*='day']") or "#td-days"
        set_input_value(css, value)
        print(f"    field {name!r} -> {get_input_value(css)!r} (want {value!r})")
        return
    # Prefer visible day field: yearly uses #td-day2
    if target == "#td-day":
        target = _first_visible("#td-day", "#td-day2") or "#td-day"
    # Older UI: single #td-once instead of split date/time
    if target in ("#td-once-date", "#td-once-time") and not _first_visible(target):
        if _first_visible("#td-once"):
            # Best-effort: append into the ISO When field.
            cur = get_input_value("#td-once")
            if target == "#td-once-date":
                time_part = cur.split("T")[1] if "T" in cur else "09:00:00"
                set_input_value("#td-once", f"{value}T{time_part}")
            else:
                date_part = cur.split("T")[0] if "T" in cur else "2026-01-01"
                set_input_value("#td-once", f"{date_part}T{value}")
            print(f"    field {name!r} -> {get_input_value('#td-once')!r} (via #td-once)")
            return
    set_input_value(target, value)
    got = get_input_value(target)
    print(f"    field {name!r} -> {got!r} (want {value!r})")
    if str(got) != str(value):
        # retry once via send_keys
        from selenium.webdriver.common.keys import Keys

        d = driver()
        By = _by()
        el = d.find_element(By.CSS_SELECTOR, target)
        el.click()
        el.send_keys(Keys.CONTROL, "a")
        el.send_keys(Keys.BACKSPACE)
        el.send_keys(value)
        time.sleep(0.08)
        got = get_input_value(target)
        print(f"    retry {name!r} -> {got!r}")


def fill_fields(fields: list[tuple[str, str]]):
    for ename, value in fields:
        fill_field(ename, value)
        time.sleep(0.06)


def close_dialog_if_open():
    d = driver()
    By = _by()
    # Cancel or × close
    for label in ("Cancel", "×", "close"):
        try:
            buttons = d.find_elements(By.CSS_SELECTOR, "button, [role='button']")
            for b in buttons:
                text = (b.text or "").strip()
                al = (b.get_attribute("aria-label") or "").strip()
                if text == label or al == label:
                    # Only if a dialog is visible
                    dialogs = d.find_elements(
                        By.CSS_SELECTOR, ".timer-dialog, [role='dialog'], .wizard-backdrop"
                    )
                    if dialogs:
                        b.click()
                        time.sleep(0.35)
                        return
        except Exception:
            continue
    # Escape via JS
    try:
        d.execute_script(
            "document.dispatchEvent(new KeyboardEvent('keydown', {key:'Escape', bubbles:true}))"
        )
        time.sleep(0.2)
    except Exception:
        pass


def dialog_title() -> str:
    d = driver()
    By = _by()
    for css in (".timer-dialog h2", ".wizard h2", "[role='dialog'] h2", "h2"):
        els = d.find_elements(By.CSS_SELECTOR, css)
        for e in els:
            if e.is_displayed() and (e.text or "").strip():
                return (e.text or "").strip()
    return ""


def open_new_timer():
    close_dialog_if_open()
    time.sleep(0.15)
    click_tab("All timers")
    time.sleep(0.25)
    click_button("+ New timer")
    time.sleep(0.55)


def open_edit_for(timer_name: str):
    """Open Edit dialog for timer_name by matching dialog title."""
    close_dialog_if_open()
    click_tab("Week")
    time.sleep(0.3)
    click_tab("All timers")
    time.sleep(0.65)
    close_dialog_if_open()
    time.sleep(0.15)

    d = driver()
    By = _by()
    for attempt in range(3):
        edits = [
            b
            for b in d.find_elements(By.CSS_SELECTOR, "button")
            if (b.text or "").strip() == "Edit"
        ]
        print(
            f"  open_edit attempt={attempt} n_edit={len(edits)} "
            f"store={[t.get('name') for t in list_timers_db()]}"
        )
        if not edits:
            raise RuntimeError(f"no Edit button for {timer_name}")
        for i in range(len(edits)):
            edits = [
                b
                for b in d.find_elements(By.CSS_SELECTOR, "button")
                if (b.text or "").strip() == "Edit"
            ]
            if i >= len(edits):
                break
            edits[i].click()
            time.sleep(0.6)
            title = dialog_title()
            print(f"  try Edit[{i}] title={title!r} want={timer_name!r}")
            if timer_name in title:
                time.sleep(0.1)
                return
            close_dialog_if_open()
            time.sleep(0.3)
        click_tab("Month")
        time.sleep(0.25)
        click_tab("All timers")
        time.sleep(0.6)
    raise RuntimeError(f"could not open Edit for {timer_name}")


def click_save_or_create():
    for label in ("Save", "Update", "Create"):
        try:
            click_button(label, timeout=2.0)
            print(f"  clicked {label}")
            return label
        except RuntimeError:
            continue
    raise RuntimeError("no Save/Create button")


# ---------------------------------------------------------------------------
# Screenshots (Xlib GetImage — read-only, no input injection)
# ---------------------------------------------------------------------------

def xdisp():
    from Xlib import display as xdisplay

    return xdisplay.Display(DISPLAY_NAME or os.environ.get("DISPLAY"))


def raise_and_geom(d):
    """Raise Bellman and return (window, x, y, w, h, wid) in root coords."""
    for cls in ("Bellman.Bellman-app", "Bellman.Bellman", "bellman-app.Bellman-app"):
        subprocess.run(
            ["wmctrl", "-x", "-a", cls],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    subprocess.run(
        ["wmctrl", "-a", "Bellman"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.25)
    line = None
    for L in subprocess.check_output(["wmctrl", "-lG"], text=True).splitlines():
        # Prefer the real toplevel named Bellman (not tray placeholders).
        parts = L.split()
        if len(parts) >= 7 and parts[-1] == "Bellman":
            line = L
            break
        if "Bellman" in L and "tray" not in L.lower():
            line = L
    if not line:
        raise RuntimeError("Bellman window not found via wmctrl")
    parts = line.split()
    wid = int(parts[0], 16)
    x, y, w, h = map(int, parts[2:6])
    win = d.create_resource_object("window", wid)
    return win, x, y, w, h, wid


def capture(d, name: str, annotate: dict | None = None) -> Path:
    from Xlib import X
    from PIL import Image

    win, x, y, w, h, wid = raise_and_geom(d)
    time.sleep(0.35)
    raw = win.get_image(0, 0, w, h, X.ZPixmap, 0xFFFFFFFF)
    img = Image.frombytes("RGBA", (w, h), raw.data, "raw", "BGRA").convert("RGB")
    path = OUT / f"{name}.png"
    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path)
    small = img.resize((64, 64))
    px = list(small.getdata())
    mean = sum(sum(p[:3]) for p in px) / (3 * len(px))
    stdev = statistics.pstdev([(p[0] + p[1] + p[2]) / 3 for p in px])
    ncolors = len(img.getcolors(maxcolors=200000) or [])
    meta = {
        "file": path.name,
        "size": list(img.size),
        "bytes": path.stat().st_size,
        "mean_luma": round(mean, 2),
        "stdev": round(stdev, 2),
        "unique_colors_cap200k": ncolors,
        "window_id": hex(wid),
        "wmctrl_geom": [x, y, w, h],
        "input_backend": "tauri-driver+WebKitWebDriver",
        "display": os.environ.get("DISPLAY"),
    }
    if annotate:
        meta.update(annotate)
    (OUT / f"{name}.meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(
        f"  captured {path.name} {img.size} bytes={meta['bytes']} "
        f"mean={meta['mean_luma']} stdev={meta['stdev']} colors~{ncolors}"
    )
    if ncolors < 50 or stdev < 2:
        print(f"  WARNING: possibly empty shell for {name}")
    return path


def resize_window(w: int, h: int):
    for args in (
        ["wmctrl", "-x", "-r", "Bellman.Bellman-app", "-e", f"0,40,40,{w},{h}"],
        ["wmctrl", "-x", "-r", "Bellman.Bellman", "-e", f"0,40,40,{w},{h}"],
        ["wmctrl", "-r", "Bellman", "-e", f"0,40,40,{w},{h}"],
    ):
        subprocess.run(args, check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.55)


# ---------------------------------------------------------------------------
# Store / CLI evidence
# ---------------------------------------------------------------------------

def list_timers_db():
    db = DATA_DIR / "timers.db"
    if not db.exists():
        return []
    import sqlite3

    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    cols = [r[1] for r in con.execute("pragma table_info(timers)").fetchall()]
    rows = con.execute("select * from timers order by name").fetchall()
    out = []
    for r in rows:
        out.append({c: r[c] for c in cols})
    con.close()
    return out


def cli_next(timer_id: str, n: int = 5) -> str:
    env = os.environ.copy()
    env["BELLMAN_DB"] = str(DATA_DIR / "timers.db")
    r = subprocess.run(
        [CLI_BIN, "--db", str(DATA_DIR / "timers.db"), "next", timer_id, str(n)],
        capture_output=True,
        text=True,
        env=env,
    )
    return (r.stdout or "") + (r.stderr or "")


def cli_list_json() -> dict | list:
    r = subprocess.run(
        [CLI_BIN, "--db", str(DATA_DIR / "timers.db"), "list", "--json"],
        capture_output=True,
        text=True,
    )
    try:
        return json.loads(r.stdout)
    except Exception:
        return {"raw": r.stdout, "err": r.stderr}


def event_log_tail(n: int = 200) -> str:
    p = DATA_DIR / "logs" / "events.current.jsonl"
    if not p.exists():
        return ""
    lines = p.read_text().splitlines()
    return "\n".join(lines[-n:]) + ("\n" if lines else "")


def webkit_pids() -> dict:
    out = {"WebKitWebProcess": [], "WebKitNetworkProcess": [], "bellman": []}
    try:
        ps = subprocess.check_output(["ps", "-eo", "pid,ppid,rss,cmd"], text=True)
    except Exception:
        return out
    for line in ps.splitlines():
        if "WebKitWebProcess" in line and "awk" not in line:
            out["WebKitWebProcess"].append(line.strip())
        elif "WebKitNetworkProcess" in line and "awk" not in line:
            out["WebKitNetworkProcess"].append(line.strip())
        elif "bellman-app" in line and "awk" not in line and "bash" not in line:
            out["bellman"].append(line.strip())
    return out


def capture_user_agent(evidence: Path) -> dict:
    attempts = []
    ua = None
    # Prefer live navigator.userAgent from the WebDriver webview.
    try:
        ua_live = driver().execute_script("return navigator.userAgent")
        attempts.append(
            {"method": "WebDriver navigator.userAgent", "ok": True, "userAgent": ua_live}
        )
        ua = ua_live
    except Exception as e:
        attempts.append(
            {"method": "WebDriver navigator.userAgent", "ok": False, "error": repr(e)}
        )
    try:
        import gi

        gi.require_version("WebKit2", "4.1")
        from gi.repository import WebKit2

        settings = WebKit2.Settings()
        ua_lib = settings.get_user_agent()
        attempts.append(
            {"method": "WebKit2.Settings.get_user_agent()", "ok": True, "userAgent": ua_lib}
        )
        if not ua:
            ua = ua_lib
    except Exception as e:
        attempts.append(
            {"method": "WebKit2.Settings.get_user_agent()", "ok": False, "error": repr(e)}
        )
    out = {
        "userAgent": ua,
        "attempts": attempts,
        "webkit_pids": webkit_pids(),
        "note": "Live navigator.userAgent via WebDriver when available.",
    }
    evidence.joinpath("userAgent.json").write_text(json.dumps(out, indent=2) + "\n")
    if ua:
        evidence.joinpath("userAgent.txt").write_text(ua + "\n")
    print("userAgent:", ua)
    return out


# ---------------------------------------------------------------------------
# Kind specs + CRUD (shared by p4b)
# ---------------------------------------------------------------------------

@dataclass
class KindSpec:
    name: str
    kind_prefix: str
    fields: list[tuple[str, str]]
    edit_fields: list[tuple[str, str]]


KINDS: list[KindSpec] = [
    KindSpec(
        "qa-once",
        "once",
        [
            ("Name", "qa-once"),
            ("Timezone", "Europe/Helsinki"),
            ("When", "2027-03-28T03:30:00"),
        ],
        [("Name", "qa-once-edited")],
    ),
    KindSpec(
        "qa-interval",
        "interval",
        [("Name", "qa-interval"), ("Every", "120")],
        [("Every", "180")],
    ),
    KindSpec(
        "qa-daily",
        "daily",
        [
            ("Name", "qa-daily"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "08:15:00"),
        ],
        [("Wall-clock", "09:15:00")],
    ),
    KindSpec(
        "qa-weekly",
        "weekly",
        [
            ("Name", "qa-weekly"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "08:00:00"),
            ("Weekdays", "mon,wed,fri"),
        ],
        [("Weekdays", "tue,thu")],
    ),
    KindSpec(
        "qa-monthly",
        "monthly",
        [
            ("Name", "qa-monthly"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "10:00:00"),
            ("Day of month", "15"),
        ],
        [("Day of month", "20")],
    ),
    KindSpec(
        "qa-yearly",
        "yearly",
        [
            ("Name", "qa-yearly"),
            ("Timezone", "Europe/Helsinki"),
            ("Wall-clock", "12:00:00"),
            ("Month", "7"),
            ("Day of month", "28"),
        ],
        [("Month", "12")],
    ),
    KindSpec(
        "qa-cron",
        "cron",
        [
            ("Name", "qa-cron"),
            ("Timezone", "Europe/Helsinki"),
            ("Cron", "0 9 * * 1-5"),
        ],
        [("Cron", "30 9 * * 1-5")],
    ),
]


def create_kind(d, spec: KindSpec, *, snap_prefix: str | None = None):
    print(f"\n== CREATE {spec.name} ({spec.kind_prefix}) ==")
    open_new_timer()
    select_kind(spec.kind_prefix)
    time.sleep(0.35)
    fill_fields(spec.fields)
    time.sleep(1.0)
    if snap_prefix:
        capture(d, f"{snap_prefix}-dialog", {"kind": spec.kind_prefix, "phase": "create"})
    click_button("Create")
    time.sleep(0.9)
    names = [t.get("name") for t in list_timers_db()]
    print(f"  store names after create: {names}")
    if spec.name not in names and not any(spec.name in (n or "") for n in names):
        print(f"  WARNING: {spec.name} not in store")
    return True


def edit_kind(d, spec: KindSpec):
    print(f"\n== EDIT {spec.name} ==")
    close_dialog_if_open()
    click_tab("All timers")
    time.sleep(0.35)
    open_edit_for(spec.name)
    fill_fields(spec.edit_fields)
    time.sleep(0.35)
    click_save_or_create()
    time.sleep(0.8)


def delete_kind(d, name: str):
    print(f"\n== DELETE {name} ==")
    before = {t.get("name") for t in list_timers_db()}
    if name not in before:
        raise RuntimeError(f"delete_kind: {name!r} not in store before delete: {sorted(before)}")
    close_dialog_if_open()
    open_edit_for(name)
    title = dialog_title()
    if name not in title:
        raise RuntimeError(f"delete_kind: wrong dialog title {title!r} for {name!r}")
    click_button("Delete…", timeout=3.0)
    time.sleep(0.45)
    click_button("Confirm delete", timeout=3.0)
    print("  clicked Confirm delete")
    time.sleep(1.0)
    close_dialog_if_open()
    after = {t.get("name") for t in list_timers_db()}
    print(f"  store after delete: {sorted(after)}")
    if name in after:
        raise RuntimeError(
            f"DELETE NO-OP: {name!r} still in store after Confirm delete "
            f"(before={sorted(before)} after={sorted(after)})."
        )
    print(f"  DELETE OK {name}")


def run_now_first_timer() -> str | None:
    click_tab("All timers")
    time.sleep(0.45)
    d = driver()
    By = _by()
    names = [
        t.get("name")
        for t in list_timers_db()
        if (t.get("name") or "").startswith("qa-")
    ]
    runs = [
        b
        for b in d.find_elements(By.CSS_SELECTOR, "button")
        if (b.text or "").strip() == "Run now"
    ]
    if not runs:
        raise RuntimeError("no Run now button")
    runs[0].click()
    time.sleep(1.1)
    name = names[0] if names else None
    print(f"  Run now clicked for row0 name={name!r}")
    return name


def run_now_nth(n: int = 1):
    d = driver()
    By = _by()
    runs = [
        b
        for b in d.find_elements(By.CSS_SELECTOR, "button")
        if (b.text or "").strip() == "Run now"
    ]
    if len(runs) > n:
        runs[n].click()
        time.sleep(1.0)
        print(f"  Run now clicked for row{n}")
