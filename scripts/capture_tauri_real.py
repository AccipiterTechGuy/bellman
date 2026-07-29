#!/usr/bin/env python3
"""Legacy capture helper — superseded by scripts/run_gui_qa.sh + capture_qa_p4b.py.

This script previously injected global pointer/keyboard injection / uinput events into the X session.
That path is removed: GUI QA must never touch the operator's pointer or keyboard.

Use:
  scripts/run_gui_qa.sh p4b
"""
from __future__ import annotations

import sys


def main() -> int:
    print(
        "capture_tauri_real.py: removed global input injection/uinput path.\n"
        "Use scripts/run_gui_qa.sh p4b (isolated Xvfb + tauri-driver + WebKitWebDriver).\n"
        "See docs/QA_P4b.md.",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
