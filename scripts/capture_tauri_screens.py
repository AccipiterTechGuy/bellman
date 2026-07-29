#!/usr/bin/env python3
"""Legacy capture helper — superseded by scripts/run_gui_qa.sh + capture_qa_p4b.py.

Previously used global-input-injection button events. That path is removed.

Use:
  scripts/run_gui_qa.sh p4b
"""
from __future__ import annotations

import sys


def main() -> int:
    print(
        "capture_tauri_screens.py: removed global-input-injection path.\n"
        "Use scripts/run_gui_qa.sh p4b (isolated Xvfb + tauri-driver + WebKitWebDriver).\n"
        "See docs/QA_P4b.md.",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
