# DONE — GUI QA on isolated display (no operator input hijack)

**Status: implemented on train `2026-07-29_0001`.**

## What shipped

- `scripts/qa_display.sh` — free-display Xvfb + metacity + isolated XDG; refuses busy locks.
- `scripts/qa_webdriver.py` — tauri-driver + WebKitWebDriver in-webview clicks/typing.
- `scripts/run_gui_qa.sh` — one-shot runner (p4b/p4d/p4e/p4f).
- Capture scripts rewritten off global pointer/keyboard injection.
- `docs/QA_P4b.md` runbook updated; operator-session path removed.
- `docs/BUILD_PLAN.md` documents `webkit2gtk-driver` + `tauri-driver` prerequisites.

## Verify

```sh
# Grep the QA path for residual global-input injection and operator-session
# display defaults — the card's verify step requires a clean result.
# Full suite while moving the mouse on the real session — pointer must not jump:
scripts/run_gui_qa.sh p4b
```

## Historical problem (resolved)

Older capture scripts defaulted the harness to the operator X session and injected
synthetic global pointer/keyboard events. That stole the mouse mid-run. Root causes
for the empty Xvfb shell were (1) `tauri_plugin_single_instance` on a shared bus and
(2) window map / env isolation — both fixed in the harness without weakening shipping
single-instance behaviour.
