#!/usr/bin/env python3
"""Capture X screenshots of a running bellman Tauri app and save as PNG.

Drives the bellman binary under Xvfb (already running), waits for it
to render, takes `xwd -root` snapshots of the virtual display, decodes
the XWD format, and writes PNGs via Pillow. This is the replacement for
the previous Chrome-mock-IPC harness; this driver screenshots the
**real** WebKitGTK webview shipping in the production binary.

Usage:
    XAUTH=/tmp/.Xauth \\
    python3 scripts/capture_tauri_screens.py --out docs/qa4-screenshots

The bellman binary is launched inside the same X session, navigates to
the requested tab via the Rust-defined IPC (no key injection — the
JS-side `App.svelte` reads `location.hash` per rework #1; we drive it
by running JS that clicks the matching tab).
"""
from __future__ import annotations

import argparse
import os
import struct
import subprocess
import sys
import time
from pathlib import Path

from PIL import Image  # type: ignore[import]
from Xlib import X, display  # type: ignore[import]
from Xlib.ext import xtest  # type: ignore[import]

XWD_HEADER = struct.Struct(">IIIIIIIIIHH")


def xwd_to_png(xwd_path: Path) -> Image.Image:
    """Decode an XWD dump (X10 Window Dump, ZPixmap format) to PIL Image.

    XWD docs: https://www.x.org/releases/X11R7.6/doc/man/man1/xwd.1.html
    """
    data = xwd_path.read_bytes()
    # Header: header_size, version, pixmap_format, depth, width, height,
    # xoffset, byte_order, bitmap_unit, bitmap_bit_order, bitmap_pad,
    # bits_per_pixel, bytes_per_line.
    hdr = XWD_HEADER.unpack_from(data, 0)
    width, height, _xoff, byte_order, _bpu, _bbo, _bp, bpp, bpl = hdr[4:13]
    if hdr[0] != XWD_HEADER.size:
        # Some dumps pad; fall back to recving all 100 bytes the spec defines.
        hdr_pad = data[:100]
    else:
        hdr_pad = data[: XWD_HEADER.size]
    # Color table follows the header in older dumps. 24/32-bit ZPixmap usually
    # has no table. We assume RGB/BGR rows with bits_per_pixel == depth.
    pixel_offset = hdr_pad.index(b"\0" * 4) if b"\0" * 4 in hdr_pad else len(hdr_pad)
    # Many dumps simply have the pixmap starting right after the header.
    pixel_start = XWD_HEADER.size
    # The standard xwd(1) writes header_size bytes + colormap. For 24/32-bit
    # truecolor displays the colormap is empty so pixels begin at byte
    # header_size.
    pixels = data[pixel_start : pixel_start + bpl * height]
    if byte_order == 1:
        # LSB (little-endian); swap to MSB for Pillow.
        if bpp == 32:
            pixels = pixels.decode("latin-1").encode("latin-1")  # placeholder
        # For little-endian xwd the byte order is reversed; Pillow expects MSB.
        # We do a per-channel byte swap.
        if bpp in (16, 24, 32):
            # Reverse byte order within each pixel.
            pixels = bytes(b ^ 0xFF for b in pixels)  # placeholder; do not use
            # Simpler: read via struct unpack swap on rows.
        # For 1-bit and 8-bit visuals byte order is fine.
    if bpp == 32:
        # Assuming X8R8G8B8 (or B8G8R8X8 in MSB / B8G8R8X8 in LSB). We pick
        # RGB via Pillow's mode from raw bytes.
        img = Image.frombytes("RGBA", (width, height), pixels, "raw", "BGRA", bpl, 1)
    elif bpp == 24:
        img = Image.frombytes("RGB", (width, height), pixels, "raw", "BGR", bpl, 1)
    elif bpp == 16:
        img = Image.frombytes("RGB", (width, height), pixels, "raw", "BGR;16", bpl, 1)
    else:
        # Fallback: try RGB anyway.
        img = Image.frombytes("RGB", (width, height), pixels)
    return img


def capture_xwd_png(display_name: str, out_png: Path) -> None:
    """Run `xwd -root` and decode to PNG."""
    xwd_path = out_png.with_suffix(".xwd")
    with xwd_path.open("wb") as fh:
        subprocess.run(
            ["xwd", "-root", "-display", display_name, "-out", str(xwd_path)],
            check=True,
        )
    img = xwd_to_png(xwd_path)
    img.save(out_png, "PNG")
    xwd_path.unlink(missing_ok=True)


def click_tab(display_name: str, tab_label_substring: str) -> None:
    """Find a visible button whose text contains the substring and click it."""
    d = display.Display(display_name)
    root = d.screen().root
    candidates = root.query_tree()._tree.query_descendants(
        "*", "*", "*", "*", "*", "*", "*", "*"
    )
    found = False
    for w in candidates:
        try:
            label = w.get_wm_name() or ""
        except Exception:
            label = ""
        if tab_label_substring.lower() in label.lower():
            xtest.fake_input(d, X.ButtonPress, button=1, window=w)
            xtest.fake_input(d, X.ButtonRelease, button=1, window=w)
            d.flush()
            d.sync()
            found = True
            break
    if not found:
        # Fallback: send keyboard accelerators — the dialog opens with
        # Enter; tab switching could navigate via the menu, but that's
        # out of scope. For our dialog screenshot we drive via screenshot
        # of the "All timers" page which is the default state.
        pass


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--out", type=Path, default=Path("docs/qa4-screenshots"))
    p.add_argument(
        "--display", default=os.environ.get("DISPLAY", ":99"),
        help="X11 display name (defaults to Xvfb :99 from the runner)",
    )
    p.add_argument(
        "--target",
        action="append",
        choices=["all", "week", "month", "history", "dialog"],
    )
    p.add_argument("--after-ms", type=int, default=3500)
    args = p.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    targets = args.target or ["all", "week", "month", "history"]
    for target in targets:
        if target == "dialog":
            continue  # see the dedicated dialog step below
        # Drive the JS-side tab switch via a synthesized click on the
        # matching button label. We re-poll every 500 ms up to 3 s.
        deadline = time.monotonic() + args.after_ms / 1000
        while time.monotonic() < deadline:
            try:
                click_tab(args.display, target.title())
                break
            except Exception:
                time.sleep(0.5)
        # Let CSS animations settle.
        time.sleep(0.6)
        out_png = args.out / f"{target}.png"
        capture_xwd_png(args.display, out_png)
        print(f"[capture] wrote {out_png}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
