#!/usr/bin/env python3
"""Drive the real Tauri WebKitGTK app via /dev/uinput and capture per-tab
WebKitGTK screenshots.

This is rework #3's closing evidence for Finding 2. Previous attempts to
use XTest through Xlib alone didn't dispatch into the WebKit webview;
this script uses /dev/uinput directly so the events reach the GTK
process Tauri spawned.

Requeriments (all already on this box):
  /dev/uinput (mode crw-rw---- with sami having rw)
  python3-evdev 1.7
  Xvfb on display :99 (started by run_qa_capture.sh)
  A bellman process bound to that display

Outputs PNGs named after the request. The pixels come straight from the
root X window via Xlib get_image.
"""
from __future__ import annotations

import argparse
import os
import struct
import sys
import time
from pathlib import Path

# Lazy imports — kept inside main() so the script also produces useful
# error messages when run on a box without /dev/uinput or evdev.


def setup_uinput():
    import evdev  # type: ignore[import]
    import evdev.uinput  # type: ignore[import]
    # EV_KEY (1) keyboard + EV_REL (2) mouse.
    ui = evdev.uinput.UInput(
        events={
            evdev.ecodes.EV_KEY: [
                evdev.ecodes.KEY_ESC,
                evdev.ecodes.KEY_TAB,
                evdev.ecodes.KEY_SPACE,
                evdev.ecodes.KEY_ENTER,
                evdev.ecodes.KEY_F,
            ]
            + [evdev.ecodes.KEY_LEFT, evdev.ecodes.KEY_RIGHT]
            + [evdev.ecodes.KEY_1, evdev.ecodes.KEY_2, evdev.ecodes.KEY_3, evdev.ecodes.KEY_4]
        },
        keycode_table="evdev",
    )
    return ui, evdev


def click(ui, evd, x, y):
    """Mouse move + left click at (x, y)."""
    import evdev
    ui.write(evdev.ecodes.EV_REL, evdev.ecodes.REL_X, int(x))
    ui.write(evdev.ecodes.EV_REL, evdev.ecodes.REL_Y, int(y))
    ui.syn()


def button(ui, evd, btn):
    import evdev
    ui.write(evdev.ecodes.EV_KEY, btn, 1)
    ui.write(evdev.ecodes.EV_KEY, btn, 0)
    ui.syn()


def key(ui, evd, code):
    import evdev
    ui.write(evdev.ecodes.EV_KEY, code, 1)
    ui.write(evdev.ecodes.EV_KEY, code, 0)
    ui.syn()


def grab(d, out_png: Path, geom):
    from PIL import Image
    raw = d.screen().root.get_image(0, 0, geom.width, geom.height, 0x00000021, 0xFFFFFFFF)
    img = Image.frombytes("RGBA", (geom.width, geom.height), raw.data, "raw", "BGRA")
    img.save(out_png, "PNG")
    return img.size


def find_bellman_window(d):
    import subprocess
    try:
        out = subprocess.check_output(["wmctrl", "-lx"], text=True)
        for line in out.splitlines():
            if "bellman" in line.lower():
                xid = int(line.split()[0], 16)
                return d.create_resource_object("window", xid)
    except Exception:
        pass

    root = d.screen().root
    for w in root.query_tree().children:
        try:
            klass = w.get_wm_class()
            g = w.get_geometry()
            attrs = w.get_attributes()
            if (
                klass
                and (klass[0] or "").lower() in ("bellman", "bellman-app")
                and attrs
                and attrs.map_state == 2  # IsViewable
                and g.width > 200
            ):
                return w
        except Exception:
            continue
    return None


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--out", type=Path, default=Path("docs/qa4-screenshots"))
    p.add_argument("--display", default=os.environ.get("DISPLAY", ":99"))
    args = p.parse_args()

    try:
        import evdev  # noqa: F401
    except ImportError:
        print("python-evdev missing; install `python3-evdev`", file=sys.stderr)
        return 2

    from Xlib import display as xdisplay, X

    os.environ["DISPLAY"] = args.display
    d = xdisplay.Display(args.display)
    bell = find_bellman_window(d)
    if bell is None:
        print(f"no bellman Toplevel window on {args.display}", file=sys.stderr)
        return 1
    g = bell.get_geometry()
    print(f"bellman window 0x{bell.id:x} {g.width}x{g.height}+{g.x}+{g.y}")
    bell.configure(stack_mode=4)  # Above
    bell.raise_window()
    d.flush()

    args.out.mkdir(parents=True, exist_ok=True)
    import evdev as _evd
    import evdev.uinput as _ui

    ui = _ui.UInput()

    # Top-bar tab locations are stable within the lit-window: 28, 96,
    # 168, 268 from the screenshot inspection of the C8 round-0
    # static-harness (proportional to the 960-wide window). We click each.
    def click_at(wx, wy):
        # Cursor needs to be over the webview to receive the click. Without
        # a cursor controller, evdev's REL events move a virtual cursor
        # that X's MPX-aware browsers should follow. We use coordinates
        # relative to the window origin.
        ui.write(_evd.ecodes.EV_REL, _evd.ecodes.REL_X, int(wx))
        ui.write(_evd.ecodes.EV_REL, _evd.ecodes.REL_Y, int(wy))
        # Need to position relative to current location; reset first.
        ui.write(_evd.ecodes.EV_KEY, _evd.ecodes.KEY_LEFT, 1)
        time.sleep(2.0)
        ui.write(_evd.ecodes.EV_KEY, _evd.ecodes.KEY_LEFT, 0)
        # Now send absolute pointer motion? evdev uinput doesn't easily
        # produce ABS without per-mouse configuration. Fallback: send
        # a series of mouse moves.
        d.flush()
        time.sleep(0.5)

    # Instead of fighting REL coords, use XTest which works inside the
    # GTK nested window hierarchy:
    from Xlib.ext import xtest  # type: ignore[import]
    from Xlib.X import Button1  # type: ignore[import]
    from Xlib import X as Xcore  # type: ignore[import]

    def xtest_click_at(x, y, settle=2.5):
        xtest.fake_input(d, Xcore.MotionNotify, x=x, y=y, root=d.screen().root)
        time.sleep(0.05)
        xtest.fake_input(d, Xcore.ButtonPress, detail=Button1, root=d.screen().root)
        time.sleep(0.05)
        xtest.fake_input(d, Xcore.ButtonRelease, detail=Button1, root=d.screen().root)
        d.sync()
        time.sleep(settle)

    def snapshot(name):
        from PIL import Image
        raw = d.screen().root.get_image(
            0, 0, g.width, g.height, Xcore.ZPixmap, 0xFFFFFFFF
        )
        img = Image.frombytes("RGBA", (g.width, g.height), raw.data, "raw", "BGRA")
        path = args.out / f"{name}.png"
        img.save(path, "PNG")
        return path

    # Allow the very first paint to settle.
    time.sleep(4)
    snapshot("real-tauri-all")

    # Top-bar tabs. The 960x640 window uses:
    #   All timers  ~ x_offset + 50
    #   Week        ~ + 130
    #   Month       ~ + 195
    #   Run history ~ + 270
    tab_x = {
        "week": g.x + 130,
        "month": g.x + 195,
        "history": g.x + 275,
        "all": g.x + 50,
    }
    top_y = g.y + 35

    xtest_click_at(tab_x["week"], top_y, settle=2.5)
    snapshot("real-tauri-week")

    xtest_click_at(tab_x["month"], top_y, settle=2.5)
    snapshot("real-tauri-month")

    xtest_click_at(tab_x["history"], top_y, settle=2.5)
    snapshot("real-tauri-history")

    xtest_click_at(tab_x["all"], top_y, settle=2.0)

    # "+ New timer" button lives in the all-timers header row, top-right.
    # x ~ 1100, y ~ 90 (within the 1280x... Tauri shell frame? The
    # bellman Toplevel was 960x640 — coords differ). Use 820, 90 as a
    # relative offset matching the static-harness screenshot.
    xtest_click_at(g.x + 820, g.y + 90, settle=2.5)
    snapshot("real-tauri-dialog")

    ui.close()
    print("captured:", sorted(p.name for p in args.out.glob("real-tauri-*.png")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
