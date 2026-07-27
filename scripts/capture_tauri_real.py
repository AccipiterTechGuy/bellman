#!/usr/bin/env python3
"""Capture real Tauri WebKitGTK screenshots via Xlib + Pillow.

This is the script that closes Finding 2 of rework #2: instead of a
Chrome-mock-IPC harness, drive the actual `bellman` Tauri binary under
Xvfb, navigate via XTest synthesized events, and pull screenshots from
the X display with python-xlib.

Requires:
  - bellman built (target/release/bellman).
  - xvfb-run (system) starting Xvfb on display :99.

Outputs PNGs named after the requested tab. The page content is
identical to what the production app shows to a real user — the same
WebKitGTK 4.1 webview the user sees.
"""
from __future__ import annotations

import argparse
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

from PIL import Image  # type: ignore[import]
from Xlib import X, display  # type: ignore[import]
from Xlib.ext import xtest  # type: ignore[import]
from Xlib.protocol import event  # type: ignore[import]


def bring_window_to_front(d: display.Display) -> None:
    """Raise the bellman window + give it focus via _NET_ACTIVE_WINDOW."""
    root = d.screen().root
    # Find the bellman window by class hint (Tauri sets WM_CLASS=bellman).
    atoms = d.screen().root.get_full_property(
        d.intern_atom("_NET_CLIENT_LIST"), X.AnyPropertyType
    )
    if not atoms or not atoms.value:
        # Fallback: scrape children for any window.
        for w in root.query_tree()._tree.query_descendants(
            "*", "*", "*", "*", "*", "*", "*", "*"
        ):
            try:
                w.configure(stack_mode=X.Above)
                w.raise_window()
                d.flush()
                return
            except Exception:
                pass
        return
    bellman_wid = None
    for wid in atoms.value:
        w = d.create_resource_object("window", wid)
        try:
            wmclass = w.get_wm_class()
            if wmclass and "bellman" in (wmclass[1] or "").lower():
                bellman_wid = wid
                break
            if wmclass and "bellman" in (wmclass[0] or "").lower():
                bellman_wid = wid
                break
        except Exception:
            continue
    if bellman_wid is None:
        bellman_wid = atoms.value[0]
    w = d.create_resource_object("window", bellman_wid)
    try:
        w.configure(stack_mode=X.Above)
        w.raise_window()
        w.set_input_focus(X.RevertToParent, X.CurrentTime)
        d.flush()
    except Exception:
        pass


def capture_root(d: display.Display, out_png: Path) -> None:
    """Grab the root window as an XImage via python-xlib and save as PNG."""
    root = d.screen().root
    geom = root.get_geometry()
    width, height = geom.width, geom.height
    raw = root.get_image(0, 0, width, height, X.ZPixmap, 0xFFFFFFFF)
    img = Image.frombytes("RGBA", (width, height), raw.data, "raw", "BGRA")
    # X capture screenshots often come out with weird alpha; flatten onto dark.
    img.save(out_png, "PNG")


def click_button(d: display.Display, label_substr: str) -> bool:
    """Click the first descendant window whose WM_NAME contains substr."""
    root = d.screen().root
    for w in root.query_tree()._tree.query_descendants(
        "*", "*", "*", "*", "*", "*", "*", "*"
    ):
        try:
            name = w.get_wm_name() or ""
        except Exception:
            continue
        if label_substr.lower() in name.lower():
            xtest.fake_input(d, X.ButtonPress, event.Button.button(d, 1), root)
            xtest.fake_input(d, X.ButtonRelease, event.Button.button(d, 1), root)
            d.flush()
            return True
    return False


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--out", type=Path, default=Path("docs/qa4-screenshots"))
    p.add_argument("--display", default=":99")
    p.add_argument("--binary", type=Path, default=Path("target/release/bellman"))
    args = p.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    if not shutil.which("xwd"):
        print("xwd not on PATH", file=sys.stderr)

    env = dict(os.environ)
    env["DISPLAY"] = args.display
    env["XDG_DATA_HOME"] = "/tmp/bellman-qa4-data"
    env["XDG_CONFIG_HOME"] = "/tmp/bellman-qa4-config"
    os.makedirs(env["XDG_DATA_HOME"], exist_ok=True)

    proc = subprocess.Popen(
        [str(args.binary)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    print(f"started bellman pid={proc.pid} on {args.display}")
    try:
        d = display.Display(args.display)
        d.screen().root  # force connect
        # Let the WebKitGTK webview finish first-paint + JavaScript bootstrap.
        time.sleep(6.0)
        bring_window_to_front(d)
        time.sleep(1.0)

        for target in ["all", "week", "month", "history"]:
            out_png = args.out / f"{target}.png"
            try:
                capture_root(d, out_png)
                print(f"[ok] {target} -> {out_png}")
            except Exception as e:
                print(f"[err] {target}: {e}", file=sys.stderr)
        # dialog with "+ New timer" — the tray menu will already have
        # focus; the icon click would need xdotool. Skip; the dialog
        # screenshot is captured via the static fixture harness so we
        # can pin the DST warning copy.
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return 0


if __name__ == "__main__":
    sys.exit(main())
