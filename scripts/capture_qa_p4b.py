#!/usr/bin/env python3
"""QA P4b — real WebKitGTK GUI evidence for the C8 calendar UI.

Path (a): drive the already-running Tauri/WebKitGTK app on the real X display
(:0, NVIDIA GLX) via AT-SPI clicks + XTest keystrokes, then grab window
pixels with Xlib GetImage.

NO mocked harness, NO Chromium, NO store-side fakes. Every create/edit/delete
originates from a real accessible action + typed keystroke in the rendered
window.

Outputs under docs/qa4-screenshots/ and docs/qa4-evidence/.
"""
from __future__ import annotations

import json
import os
import sqlite3
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import pyatspi
from PIL import Image
from Xlib import XK, X, display as xdisplay
from Xlib.ext import xtest

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
EVIDENCE = ROOT / "docs" / "qa4-evidence"
DISPLAY_NAME = os.environ.get("DISPLAY", ":0")
DATA_DIR = Path(
    os.environ.get(
        "BELLMAN_QA_DATA",
        "/tmp/qa-p4b-session/share/io.bellman.desktop",
    )
)
# CLI is a separate package (bellman-cli) from the Tauri shell binary.
# Prefer an explicit env, then a side-by-side bellman-cli artifact, then
# a known schema-v3 CLI built for this run.
_cli_candidates = [
    os.environ.get("BELLMAN_CLI", ""),
    str(ROOT / "target/release/bellman-cli"),
    "/tmp/bellman-cli-schema3",
    str(ROOT / "target-cli-bellman"),
]
CLI_BIN = next((p for p in _cli_candidates if p and Path(p).exists()), "bellman")


# ---------------------------------------------------------------------------
# X / capture
# ---------------------------------------------------------------------------

def xdisp():
    return xdisplay.Display(DISPLAY_NAME)


def find_bellman_window(d):
    root = d.screen().root

    def walk(w):
        try:
            children = w.query_tree().children
        except Exception:
            return None
        for c in children:
            try:
                klass = c.get_wm_class()
                attrs = c.get_attributes()
                if (
                    attrs.map_state == 2
                    and klass
                    and (klass[0] or "").lower() == "bellman"
                ):
                    return c
            except Exception:
                pass
            hit = walk(c)
            if hit is not None:
                return hit
        return None

    return walk(root)


def raise_and_geom(d):
    """Raise Bellman and return (window, x, y, w, h) in root coords via wmctrl."""
    subprocess.run(
        ["wmctrl", "-x", "-a", "Bellman.Bellman"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.25)
    line = None
    for L in subprocess.check_output(["wmctrl", "-lG"], text=True).splitlines():
        if "Bellman" in L:
            line = L
            break
    if not line:
        raise RuntimeError("Bellman window not found via wmctrl")
    parts = line.split()
    wid = int(parts[0], 16)
    x, y, w, h = map(int, parts[2:6])
    win = d.create_resource_object("window", wid)
    return win, x, y, w, h, wid


def capture(d, name: str, annotate: dict | None = None) -> Path:
    win, x, y, w, h, wid = raise_and_geom(d)
    time.sleep(0.35)
    raw = win.get_image(0, 0, w, h, X.ZPixmap, 0xFFFFFFFF)
    img = Image.frombytes("RGBA", (w, h), raw.data, "raw", "BGRA").convert("RGB")
    path = OUT / f"{name}.png"
    img.save(path)
    # Pixel stats so a blind auditor can still see non-empty
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
    }
    if annotate:
        meta.update(annotate)
    (OUT / f"{name}.meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(f"  captured {path.name} {img.size} bytes={meta['bytes']} "
          f"mean={meta['mean_luma']} stdev={meta['stdev']} colors~{ncolors}")
    if ncolors < 50 or stdev < 2:
        print(f"  WARNING: possibly empty shell for {name}")
    return path


def key_tap_code(d, keycode, *, shift=False, ctrl=False):
    mods = []
    if ctrl:
        mods.append(d.keysym_to_keycode(XK.string_to_keysym("Control_L")))
    if shift:
        mods.append(d.keysym_to_keycode(XK.string_to_keysym("Shift_L")))
    for m in mods:
        xtest.fake_input(d, X.KeyPress, m)
    xtest.fake_input(d, X.KeyPress, keycode)
    xtest.fake_input(d, X.KeyRelease, keycode)
    for m in reversed(mods):
        xtest.fake_input(d, X.KeyRelease, m)
    d.sync()


def key_tap(d, keysym, *, shift=False, ctrl=False):
    keycode = d.keysym_to_keycode(keysym)
    if keycode == 0:
        raise RuntimeError(f"no keycode for keysym {keysym}")
    key_tap_code(d, keycode, shift=shift, ctrl=ctrl)


def _build_char_map(d):
    """Map printable chars to (keycode, need_shift) for the active XKB layout.

    Finnish layout (fi classic) has '/' on Shift+7 — keysym_to_keycode('slash')
    returns the same keycode as '7', so an unshifted press types '7'. We must
    look up the keysym on each level of every keycode.
    """
    mapping: dict[str, tuple[int, bool]] = {}
    for code in range(8, 256):
        for level, shift in ((0, False), (1, True)):
            try:
                ks = d.keycode_to_keysym(code, level)
            except Exception:
                continue
            if not ks:
                continue
            # Prefer Latin-1 / ASCII printables
            if 32 <= ks <= 126:
                ch = chr(ks)
                # Keep the first (lowest code) mapping for each char
                mapping.setdefault(ch, (code, shift))
            else:
                name = XK.keysym_to_string(ks)
                if name and len(name) == 1 and name.isprintable():
                    mapping.setdefault(name, (code, shift))
    return mapping


_CHAR_MAP_CACHE: dict[int, dict] = {}


def type_string(d, s: str):
    """Type text via XTest using the live keyboard layout (fi/us/…)."""
    did = id(d)
    if did not in _CHAR_MAP_CACHE:
        _CHAR_MAP_CACHE[did] = _build_char_map(d)
    cmap = _CHAR_MAP_CACHE[did]
    for ch in s:
        if ch in cmap:
            code, shift = cmap[ch]
            key_tap_code(d, code, shift=shift)
        elif ch.isupper() and ch.lower() in cmap:
            code, _ = cmap[ch.lower()]
            key_tap_code(d, code, shift=True)
        else:
            ks = XK.string_to_keysym(ch)
            if ks:
                # last resort — may be wrong on non-US layouts
                key_tap(d, ks, shift=ch.isupper())
            else:
                print(f"  skip char {ch!r}")
        time.sleep(0.015)


def clear_and_type(d, text: str):
    key_tap(d, XK.string_to_keysym("a"), ctrl=True)
    time.sleep(0.03)
    # Delete selection
    key_tap(d, XK.string_to_keysym("BackSpace"))
    time.sleep(0.03)
    type_string(d, text)


# ---------------------------------------------------------------------------
# AT-SPI helpers
# ---------------------------------------------------------------------------

def find_app(name: str = "bellman"):
    for app in pyatspi.Registry.getDesktop(0):
        try:
            if (app.name or "").lower() == name:
                return app
        except Exception:
            pass
    raise RuntimeError(f"a11y app {name!r} not found")


def walk_find(root, pred, max_depth=24):
    hits = []

    def walk(a, depth=0):
        if depth > max_depth:
            return
        try:
            if pred(a):
                hits.append(a)
            for i in range(a.childCount):
                walk(a.getChildAtIndex(i), depth + 1)
        except Exception:
            pass

    walk(root)
    return hits


def do_action(acc, preferred=("click", "press", "Activate", "select")):
    ai = acc.queryAction()
    names = [ai.getName(i) for i in range(ai.nActions)]
    for want in preferred:
        for i, n in enumerate(names):
            if n == want:
                ai.doAction(i)
                return n
    if ai.nActions:
        ai.doAction(0)
        return names[0]
    raise RuntimeError("no actions")


def click_named(app, name: str, role: str | None = None, exact=True):
    def pred(a):
        try:
            n = a.name or ""
            r = a.getRoleName()
            if role and r != role:
                return False
            return (n == name) if exact else (name.lower() in n.lower())
        except Exception:
            return False

    hits = walk_find(app, pred)
    if not hits:
        raise RuntimeError(f"no accessible {role or '*'} named {name!r}")
    do_action(hits[0])
    return hits[0]


def focus_entry(app, name: str, d):
    hits = walk_find(
        app,
        lambda a: a.getRoleName() == "entry" and (a.name or "") == name,
    )
    if not hits:
        # partial match
        hits = walk_find(
            app,
            lambda a: a.getRoleName() == "entry" and name.lower() in (a.name or "").lower(),
        )
    if not hits:
        raise RuntimeError(f"entry {name!r} not found")
    entry = hits[0]
    comp = entry.queryComponent()
    try:
        comp.grabFocus()
    except Exception:
        pass
    ext = comp.getExtents(pyatspi.DESKTOP_COORDS)
    cx = ext.x + max(ext.width // 2, 4)
    cy = ext.y + max(ext.height // 2, 4)
    xtest.fake_input(d, X.MotionNotify, x=int(cx), y=int(cy))
    xtest.fake_input(d, X.ButtonPress, detail=1)
    xtest.fake_input(d, X.ButtonRelease, detail=1)
    d.sync()
    time.sleep(0.12)
    return entry


def read_entry(entry) -> str:
    try:
        return entry.queryText().getText(0, -1)
    except Exception:
        return ""


# Occurrence kind order matches TimerDialog.svelte <option> order.
KIND_ORDER = ("once", "interval", "daily", "weekly", "monthly", "yearly", "cron")


def select_kind(app, kind_prefix: str, d=None):
    """Select occurrence kind via pointer click + keyboard (not AT-SPI menu Action).

    AT-SPI doAction on menu items left the combo label stuck on 'daily' while
    fields followed the real selection (auditor F3). Keyboard selection updates
    the visible label.
    """
    if d is None:
        d = xdisp()
    prefix = kind_prefix.lower()
    if prefix not in KIND_ORDER:
        raise RuntimeError(f"unknown kind {kind_prefix!r}")
    target_idx = KIND_ORDER.index(prefix)

    combos = walk_find(app, lambda a: a.getRoleName() == "combo box")
    if not combos:
        raise RuntimeError("no combo box")
    combo = combos[0]
    comp = combo.queryComponent()
    try:
        comp.grabFocus()
    except Exception:
        pass
    ext = comp.getExtents(pyatspi.DESKTOP_COORDS)
    cx = int(ext.x + max(ext.width // 2, 4))
    cy = int(ext.y + max(ext.height // 2, 4))
    xtest.fake_input(d, X.MotionNotify, x=cx, y=cy)
    xtest.fake_input(d, X.ButtonPress, detail=1)
    xtest.fake_input(d, X.ButtonRelease, detail=1)
    d.sync()
    time.sleep(0.35)

    # Jump to first option then Down to target (default is daily = index 2).
    key_tap(d, XK.string_to_keysym("Home"))
    time.sleep(0.08)
    for _ in range(target_idx):
        key_tap(d, XK.string_to_keysym("Down"))
        time.sleep(0.05)
    key_tap(d, XK.string_to_keysym("Return"))
    time.sleep(0.4)

    # Read back combo name if exposed
    label = combo.name or ""
    try:
        # Some WebKits expose the selected text via accessible name of combo
        label = combo.name or label
    except Exception:
        pass
    print(f"  select_kind {prefix!r} via keyboard (idx={target_idx}) combo_name={label!r}")
    return prefix


def close_dialog_if_open(app):
    dialogs = walk_find(app, lambda a: a.getRoleName() == "dialog")
    if not dialogs:
        return
    # Cancel or close
    for label in ("Cancel", "close", "×"):
        btns = walk_find(
            app,
            lambda a, lab=label: a.getRoleName() == "push button" and (a.name or "") == lab,
        )
        if btns:
            do_action(btns[0])
            time.sleep(0.4)
            return
    # Escape via X
    d = xdisp()
    key_tap(d, XK.string_to_keysym("Escape"))
    time.sleep(0.4)


# ---------------------------------------------------------------------------
# Store / CLI evidence
# ---------------------------------------------------------------------------

def list_timers_db():
    db = DATA_DIR / "timers.db"
    if not db.exists():
        return []
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    # schema: inspect columns
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
        elif "target/release/bellman" in line and "awk" not in line and "bash" not in line:
            out["bellman"].append(line.strip())
    return out


# ---------------------------------------------------------------------------
# CRUD scenarios
# ---------------------------------------------------------------------------

@dataclass
class KindSpec:
    name: str
    kind_prefix: str  # menu item prefix
    fields: list[tuple[str, str]]  # (entry accessible name substring, value)
    edit_fields: list[tuple[str, str]]  # fields to change on edit


KINDS: list[KindSpec] = [
    KindSpec(
        "qa-once",
        "once",
        [
            ("Name", "qa-once"),
            ("Timezone", "Europe/Helsinki"),
            ("When", "2027-03-28T03:30:00"),  # DST gap — also used for warning shot
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
            ("Month (1", "7"),
            ("Day of month", "28"),
        ],
        [("Month (1", "12")],
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


def fill_fields(app, d, fields: list[tuple[str, str]]):
    for ename, value in fields:
        entry = focus_entry(app, ename, d)
        clear_and_type(d, value)
        time.sleep(0.08)
        got = read_entry(entry)
        print(f"    field {ename!r} -> {got!r} (want {value!r})")
        if got != value:
            # retry once
            focus_entry(app, ename, d)
            clear_and_type(d, value)
            time.sleep(0.1)
            got = read_entry(entry)
            print(f"    retry {ename!r} -> {got!r}")


def create_kind(app, d, spec: KindSpec, *, snap_prefix: str | None = None):
    print(f"\n== CREATE {spec.name} ({spec.kind_prefix}) ==")
    close_dialog_if_open(app)
    time.sleep(0.2)
    click_named(app, "All timers", "push button")
    time.sleep(0.3)
    click_named(app, "+ New timer", "push button")
    time.sleep(0.6)

    # kind first so conditional fields appear (keyboard path — F3)
    select_kind(app, spec.kind_prefix, d)
    time.sleep(0.45)
    fill_fields(app, d, spec.fields)
    # allow preview debounce + IPC (longer so Next-5 table paints)
    time.sleep(1.2)

    if snap_prefix:
        capture(d, f"{snap_prefix}-dialog", {"kind": spec.kind_prefix, "phase": "create"})

    click_named(app, "Create", "push button")
    time.sleep(0.9)
    # verify in store
    names = [t.get("name") for t in list_timers_db()]
    print(f"  store names after create: {names}")
    if spec.name not in names and not any(spec.name in (n or "") for n in names):
        # name might have been edited partially
        print(f"  WARNING: {spec.name} not in store")
    return True


def ui_timer_names(app) -> list[str]:
    """Timer names in All-page table order (document order of name cells)."""
    # WebKit exposes the name column as table cells / static text under rows.
    # Fall back to store order when the table is empty.
    cells = walk_find(
        app,
        lambda a: a.getRoleName() in ("table cell", "cell")
        and (a.name or "").startswith("qa-"),
    )
    names = []
    for c in cells:
        n = (c.name or "").strip()
        if n.startswith("qa-") and n not in names:
            names.append(n)
    if names:
        return names
    return [t.get("name") for t in list_timers_db() if t.get("name")]


def dialog_title(app) -> str:
    dialogs = walk_find(app, lambda a: a.getRoleName() == "dialog")
    if not dialogs:
        return ""
    heads = walk_find(dialogs[0], lambda a: a.getRoleName() == "heading")
    if heads:
        return heads[0].name or ""
    return dialogs[0].name or ""


def open_edit_for(app, d, timer_name: str):
    """Open Edit dialog for timer_name by title-matching (never silent wrong row)."""
    close_dialog_if_open(app)
    # Force list refresh (avoids stale rows after create/delete).
    click_named(app, "Week", "push button")
    time.sleep(0.35)
    click_named(app, "All timers", "push button")
    time.sleep(0.7)
    close_dialog_if_open(app)
    time.sleep(0.2)

    # Map every Edit button → dialog title, then open the match.
    # Re-scan from index 0 each outer attempt because cancel can reshuffle.
    for attempt in range(3):
        edits = walk_find(
            app,
            lambda a: a.getRoleName() == "push button" and (a.name or "") == "Edit",
        )
        print(f"  open_edit attempt={attempt} n_edit={len(edits)} store={[t.get('name') for t in list_timers_db()]}")
        if not edits:
            btns = walk_find(app, lambda a: a.getRoleName() == "push button")
            print("  buttons:", [b.name for b in btns])
            raise RuntimeError(f"no Edit button for {timer_name}")

        for i in range(len(edits)):
            edits = walk_find(
                app,
                lambda a: a.getRoleName() == "push button" and (a.name or "") == "Edit",
            )
            if i >= len(edits):
                break
            do_action(edits[i])
            time.sleep(0.65)
            title = dialog_title(app)
            print(f"  try Edit[{i}] title={title!r} want={timer_name!r}")
            if timer_name in title:
                time.sleep(0.15)
                return
            close_dialog_if_open(app)
            time.sleep(0.35)
        # refresh list and retry full scan
        click_named(app, "Month", "push button")
        time.sleep(0.3)
        click_named(app, "All timers", "push button")
        time.sleep(0.7)

    raise RuntimeError(f"could not open Edit for {timer_name}")


def edit_kind(app, d, spec: KindSpec):
    print(f"\n== EDIT {spec.name} ==")
    close_dialog_if_open(app)
    click_named(app, "All timers", "push button")
    time.sleep(0.4)
    open_edit_for(app, d, spec.name)
    # dialog should say Edit "..."
    fill_fields(app, d, spec.edit_fields)
    time.sleep(0.4)
    # Save button label is "Save" on edit
    for label in ("Save", "Update", "Create"):
        btns = walk_find(
            app,
            lambda a, lab=label: a.getRoleName() == "push button" and (a.name or "") == lab,
        )
        if btns:
            do_action(btns[0])
            print(f"  clicked {label}")
            break
    else:
        raise RuntimeError("no Save button")
    time.sleep(0.8)


def delete_kind(app, d, name: str):
    """Delete timer by GUI; FAIL loudly if the store row is still present."""
    print(f"\n== DELETE {name} ==")
    before = {t.get("name") for t in list_timers_db()}
    if name not in before:
        raise RuntimeError(f"delete_kind: {name!r} not in store before delete: {sorted(before)}")
    close_dialog_if_open(app)
    open_edit_for(app, d, name)
    # Verify we have the right dialog before deleting
    title = dialog_title(app)
    if name not in title:
        raise RuntimeError(f"delete_kind: wrong dialog title {title!r} for {name!r}")

    btns = walk_find(
        app,
        lambda a: a.getRoleName() == "push button"
        and (a.name or "") in ("Delete…", "Delete...", "Delete", "Delete timer"),
    )
    if not btns:
        btns = walk_find(
            app,
            lambda a: a.getRoleName() == "push button" and "elete" in (a.name or ""),
        )
    if not btns:
        raise RuntimeError("no Delete… button")
    do_action(btns[0])
    time.sleep(0.5)
    conf = walk_find(
        app,
        lambda a: a.getRoleName() == "push button" and (a.name or "") == "Confirm delete",
    )
    if not conf:
        raise RuntimeError("no Confirm delete button after Delete…")
    do_action(conf[-1])
    print("  clicked Confirm delete")
    time.sleep(1.0)
    close_dialog_if_open(app)
    after = {t.get("name") for t in list_timers_db()}
    print(f"  store after delete: {sorted(after)}")
    if name in after:
        raise RuntimeError(
            f"DELETE NO-OP: {name!r} still in store after Confirm delete "
            f"(before={sorted(before)} after={sorted(after)}). Product defect if title matched."
        )
    print(f"  DELETE OK {name}")


def tab(app, label: str):
    click_named(app, label, "push button")
    time.sleep(0.7)


# ---------------------------------------------------------------------------
# Main flow
# ---------------------------------------------------------------------------

def capture_user_agent(evidence: Path) -> dict:
    """Capture page/engine userAgent evidence (auditor F5).

    Attempts, in order:
      1. WebKitGTK default WebKitSettings user-agent (library default the
         page process inherits when the app does not override it).
      2. WEBKIT_INSPECTOR_SERVER HTTP probe if the env is set.
    Records every command + output so impossibility is evidence, not silence.
    """
    attempts = []
    ua = None

    # 1) WebKit2 Settings default UA (same lib as the running WebKitWebProcess)
    try:
        import gi

        gi.require_version("WebKit2", "4.1")
        from gi.repository import WebKit2

        settings = WebKit2.Settings()
        ua_lib = settings.get_user_agent()
        attempts.append(
            {
                "method": "WebKit2.Settings.get_user_agent()",
                "ok": True,
                "userAgent": ua_lib,
            }
        )
        ua = ua_lib
    except Exception as e:
        attempts.append(
            {"method": "WebKit2.Settings.get_user_agent()", "ok": False, "error": repr(e)}
        )

    # 2) Inspector server (only if launched with WEBKIT_INSPECTOR_SERVER)
    insp = os.environ.get("WEBKIT_INSPECTOR_SERVER", "")
    if insp:
        hostport = insp.replace("http://", "").strip()
        url = f"http://{hostport}/"
        try:
            r = subprocess.run(
                ["curl", "-sS", "-m", "2", url],
                capture_output=True,
                text=True,
            )
            attempts.append(
                {
                    "method": f"curl {url}",
                    "ok": r.returncode == 0,
                    "stdout": (r.stdout or "")[:2000],
                    "stderr": (r.stderr or "")[:500],
                }
            )
        except Exception as e:
            attempts.append({"method": f"curl {url}", "ok": False, "error": repr(e)})
    else:
        attempts.append(
            {
                "method": "WEBKIT_INSPECTOR_SERVER",
                "ok": False,
                "error": "env not set for this session",
            }
        )

    # 3) /proc cmdline of WebKitWebProcess (engine proof companion)
    pids = webkit_pids()
    out = {
        "userAgent": ua,
        "attempts": attempts,
        "webkit_pids": pids,
        "note": (
            "WebKit2.Settings.get_user_agent() is the default UA string the "
            "running libwebkit2gtk-4.1 WebKitWebProcess inherits when the app "
            "does not call set_user_agent. App does not override (grep: no "
            "set_user_agent in src-tauri)."
        ),
    }
    evidence.joinpath("userAgent.json").write_text(json.dumps(out, indent=2) + "\n")
    if ua:
        evidence.joinpath("userAgent.txt").write_text(ua + "\n")
    print("userAgent:", ua)
    return out


def run_now_first_timer(app, d) -> str | None:
    """Click the first 'Run now' on All timers. Returns timer name if known."""
    click_named(app, "All timers", "push button")
    time.sleep(0.5)
    names = ui_timer_names(app)
    runs = walk_find(
        app,
        lambda a: a.getRoleName() == "push button" and (a.name or "") == "Run now",
    )
    if not runs:
        raise RuntimeError("no Run now button")
    do_action(runs[0])
    time.sleep(1.2)
    name = names[0] if names else None
    print(f"  Run now clicked for row0 name={name!r}")
    return name


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    os.environ["DISPLAY"] = DISPLAY_NAME

    d = xdisp()
    app = find_app()
    pids = webkit_pids()
    print("engine pids:", json.dumps(pids, indent=2))
    (EVIDENCE / "webkit_pids.json").write_text(json.dumps(pids, indent=2) + "\n")

    # F5 — page/engine userAgent
    ua_info = capture_user_agent(EVIDENCE)

    # --- 0. baseline empty All page ---
    close_dialog_if_open(app)
    tab(app, "All timers")
    capture(d, "p4b-all-empty", {"phase": "baseline", "webkit": pids})

    # --- 1. DST gap warning (once @ Helsinki spring gap) ---
    print("\n== DST gap warning dialog ==")
    click_named(app, "+ New timer", "push button")
    time.sleep(0.5)
    select_kind(app, "once", d)
    fill_fields(
        app,
        d,
        [
            ("Name", "qa-dst-gap"),
            ("Timezone", "Europe/Helsinki"),
            ("When", "2027-03-28T03:30:00"),
        ],
    )
    time.sleep(1.4)
    capture(d, "p4b-dialog-dst-gap", {"phase": "dst-warning"})
    click_named(app, "Create", "push button")
    time.sleep(1.0)

    # --- 2. Create all seven KINDS ---
    for spec in KINDS:
        snap = None
        if spec.kind_prefix == "weekly":
            snap = "p4b-dialog-weekly"
        elif spec.kind_prefix == "once":
            snap = "p4b-dialog-once"
        create_kind(app, d, spec, snap_prefix=snap)

    (EVIDENCE / "store-after-create.json").write_text(
        json.dumps(list_timers_db(), indent=2, default=str) + "\n"
    )
    (EVIDENCE / "cli-list-after-create.json").write_text(
        json.dumps(cli_list_json(), indent=2) + "\n"
    )

    # --- 3. Pages with data (wait for list refresh after last Create toast) ---
    tab(app, "All timers")
    time.sleep(1.5)  # TimerList polls; avoid 7-of-8 refresh lag (F6a)
    capture(d, "p4b-all", {"phase": "with-data", "expected_min_timers": 8})

    tab(app, "Week")
    capture(d, "p4b-week", {"phase": "with-data"})

    tab(app, "Month")
    capture(d, "p4b-month", {"phase": "with-data"})

    # --- 3b. Run now → JSONL Fired + Run history with records (F2) ---
    print("\n== RUN NOW for JSONL evidence ==")
    run_now_first_timer(app, d)
    time.sleep(0.8)
    # second fire for a different row if available
    runs = walk_find(
        app,
        lambda a: a.getRoleName() == "push button" and (a.name or "") == "Run now",
    )
    if len(runs) > 1:
        do_action(runs[1])
        time.sleep(1.0)
        print("  Run now clicked for row1")

    log = event_log_tail(500)
    (EVIDENCE / "events.current.jsonl").write_text(log)
    print(f"  event log lines after Run now: {len(log.splitlines()) if log else 0}")

    tab(app, "Run history")
    time.sleep(0.8)
    capture(d, "p4b-history", {"phase": "with-run-now-events"})

    # --- 4. Preview vs CLI for qa-weekly ---
    print("\n== PREVIEW vs CLI (qa-weekly) ==")
    open_edit_for(app, d, "qa-weekly")
    time.sleep(1.4)
    capture(d, "p4b-dialog-preview-weekly", {"phase": "preview-vs-cli"})
    timers = list_timers_db()
    weekly = next((t for t in timers if t.get("name") == "qa-weekly"), None)
    if weekly:
        tid = weekly.get("id")
        nxt = cli_next(str(tid), 5)
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
    close_dialog_if_open(app)

    # --- 5. Edit all seven KINDS ---
    for spec in KINDS:
        edit_kind(app, d, spec)

    tab(app, "All timers")
    time.sleep(0.8)
    capture(d, "p4b-all-after-edit", {"phase": "after-edit"})
    (EVIDENCE / "store-after-edit.json").write_text(
        json.dumps(list_timers_db(), indent=2, default=str) + "\n"
    )

    # --- 6. Delete ALL qa-* rows through GUI; fail on no-op (F1) ---
    # Taller window so every timer row + Edit is on-screen (8 rows overflow 640).
    subprocess.run(
        ["wmctrl", "-x", "-r", "Bellman.Bellman", "-e", "0,20,20,960,900"],
        check=False,
    )
    time.sleep(0.5)
    remaining = [t.get("name") for t in list_timers_db() if (t.get("name") or "").startswith("qa-")]
    print("deleting", remaining)
    failed = []
    for name in list(remaining):
        try:
            delete_kind(app, d, name)
        except Exception as e:
            print(f"  DELETE FAIL {name}: {e}")
            failed.append((name, str(e)))
            close_dialog_if_open(app)
            time.sleep(0.3)

    tab(app, "All timers")
    time.sleep(0.5)
    capture(d, "p4b-all-after-delete", {"phase": "after-delete"})
    left = [t.get("name") for t in list_timers_db() if (t.get("name") or "").startswith("qa-")]
    if left:
        # One more pass by title match
        print("retry deletes for", left)
        for name in list(left):
            try:
                delete_kind(app, d, name)
            except Exception as e:
                failed.append((name, f"retry: {e}"))
                close_dialog_if_open(app)
        left = [t.get("name") for t in list_timers_db() if (t.get("name") or "").startswith("qa-")]
        tab(app, "All timers")
        capture(d, "p4b-all-after-delete", {"phase": "after-delete-retry"})

    if left:
        raise RuntimeError(f"F1 FAIL: qa-* still in store after delete pass: {left}; errors={failed}")

    # --- 7. Larger layout: 1280x800 including dialog + Month/History (F4) ---
    print("\n== layout at 1280x800 ==")
    subprocess.run(
        ["wmctrl", "-x", "-r", "Bellman.Bellman", "-e", "0,20,20,1280,800"],
        check=False,
    )
    time.sleep(0.6)
    create_kind(app, d, KINDS[2])  # daily for non-empty pages
    create_kind(app, d, KINDS[3])  # weekly for week/month chips
    tab(app, "All timers")
    time.sleep(0.8)
    capture(d, "p4b-all-1280x800", {"phase": "layout-large"})
    tab(app, "Week")
    capture(d, "p4b-week-1280x800", {"phase": "layout-large"})
    tab(app, "Month")
    capture(d, "p4b-month-1280x800", {"phase": "layout-large"})
    tab(app, "Run history")
    capture(d, "p4b-history-1280x800", {"phase": "layout-large"})
    # Dialog at larger size (distinct from the 960 preview-vs-CLI shot — R2).
    open_edit_for(app, d, "qa-weekly")
    time.sleep(1.2)
    capture(d, "p4b-dialog-1280x800", {"phase": "layout-large-dialog-full-utc"})
    close_dialog_if_open(app)

    subprocess.run(
        ["wmctrl", "-x", "-r", "Bellman.Bellman", "-e", "0,40,40,960,640"],
        check=False,
    )
    time.sleep(0.4)

    # --- 8. Persist evidence ---
    final_store = list_timers_db()
    (EVIDENCE / "store-final.json").write_text(
        json.dumps(final_store, indent=2, default=str) + "\n"
    )
    (EVIDENCE / "cli-list-final.json").write_text(
        json.dumps(cli_list_json(), indent=2) + "\n"
    )
    log = event_log_tail(500)
    (EVIDENCE / "events.current.jsonl").write_text(log)
    (EVIDENCE / "webkit_pids_final.json").write_text(
        json.dumps(webkit_pids(), indent=2) + "\n"
    )

    # Copy live app logs exactly as teed (R3): 0-byte files stay 0-byte.
    # Do NOT write commentary into the evidence files.
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
        "display": DISPLAY_NAME,
        "data_dir": str(DATA_DIR),
        "cli_bin": CLI_BIN,
        "screenshots": sorted(p.name for p in OUT.glob("p4b-*.png")),
        "meta_json": sorted(p.name for p in OUT.glob("p4b-*.meta.json")),
        "timers_final": [t.get("name") for t in final_store],
        "webkit": webkit_pids(),
        "userAgent": ua_info.get("userAgent"),
        "event_log_lines": len(log.splitlines()) if log else 0,
        "delete_failures": failed,
    }
    (EVIDENCE / "session-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print("\nDONE", json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
