# QA P4b — WebKitGTK GUI evidence for C8 calendar UI

Card: train run `2026-07-29_0001` (isolated display + WebDriver rewrite).
C8 base evidence still under `docs/qa4-screenshots/` / `docs/qa4-evidence/`.

## Display setup — isolated Xvfb (never the operator session)

GUI QA runs on its **own** Xvfb display with a private D-Bus session. It must
not steal the operator's mouse or keyboard. Interaction is
**tauri-driver + WebKitWebDriver** (in-webview). Screenshots use Xlib GetImage
on the isolated display only (read-only).

### One-shot runner

```sh
set -euo pipefail
ROOT="$(pwd)"                          # worktree or main checkout
cd "$ROOT"

# 1. Frontend + production Tauri shell (bellman-app)
#    MUST use cargo-tauri (or cargo build -p bellman-app --features custom-protocol):
#    a plain build without custom-protocol loads http://localhost:1420 → blank window.
cd ui && npm ci && npm run build && cd ..
cargo tauri build --no-bundle
# or:
# cargo build -p bellman-app --release --features custom-protocol --manifest-path src-tauri/Cargo.toml

export BELLMAN_APP="${CARGO_TARGET_DIR:-$ROOT/target}/release/bellman-app"
export BELLMAN_CLI="${CARGO_TARGET_DIR:-$ROOT/target}/release/bellman"
test -x "$BELLMAN_APP"

# 2. Python deps for the harness (venv recommended)
python3 -m venv /tmp/bellman-qa-venv
/tmp/bellman-qa-venv/bin/pip install selenium pillow python-xlib
export BELLMAN_QA_PYTHON=/tmp/bellman-qa-venv/bin/python

# 3. System deps (once per machine) — see docs/BUILD_PLAN.md
#    sudo apt install -y webkit2gtk-driver   # version-match libwebkit2gtk-4.1
#    cargo install tauri-driver --locked
#    Window manager: metacity is already on Mint (`metacity --sm-disable`).

# 4. Run the suite on an isolated display
scripts/run_gui_qa.sh p4b
```

What `scripts/run_gui_qa.sh` does:

1. `scripts/qa_display.sh start` — picks a **free** display (refuses busy
   locks; never attaches to someone else's Xvfb), starts Xvfb + metacity,
   prepares isolated `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_RUNTIME_DIR`
   with `wizard_completed` + `start_minimized=false`.
2. Launches capture under that env; `qa_webdriver.py` starts
   `tauri-driver` under `dbus-run-session` so
   `tauri_plugin_single_instance` does not forward to a live operator instance.
3. Tears the display **and** the driver process group down on exit (no orphan
   Xvfb, tauri-driver, or WebKitWebDriver left listening).

Manual equivalent:

```sh
scripts/qa_display.sh start
eval "$(scripts/qa_display.sh env)"
export BELLMAN_APP=… BELLMAN_CLI=…
/tmp/bellman-qa-venv/bin/python scripts/capture_qa_p4b.py
scripts/qa_display.sh stop
```

### Paths tried for the WebKit empty-shell / window-map issue

| Path | Result |
|---|---|
| **(a) Isolated Xvfb + metacity + private D-Bus + clean XDG + `start_minimized=false`** | **Works** — real 960×640 toplevel maps; multi-colour UI screenshots |
| (b) Operator interactive session (legacy) | Works for paint, but **steals pointer/keyboard** — forbidden |
| (c) Xvfb without private bus while operator instance is live | `tauri_plugin_single_instance` exits the second launch (silent empty shell) |
| (d) Xvfb without isolated/clean env | App can leave a 10×10 unmapped placeholder; use `qa_display.sh` |
| (e) Missing window manager | Ruled out — metacity/muffin ship on Mint; still need (a) for a real map |
| (f) DRI / `LIBGL_*` / `WEBKIT_DISABLE_*` matrix | Not the root cause while single-instance / window map were wrong |

### Why the window used to stay 10×10

Two causes were conflated in earlier probes:

1. **`tauri_plugin_single_instance`** kills every second launch on the same
   session bus. Fix for tests: private bus via `dbus-run-session` (do **not**
   weaken the plugin in shipping code).
2. Polluted operator environment + missing `start_minimized=false` in the
   isolated config. `qa_display.sh` writes the config and sets XDG dirs.

## Isolation / input-backend acceptance (this card)

| Item | Status | Evidence |
|---|---|---|
| Isolated display (not operator session) | **required** | `scripts/qa_display.sh`, `run_gui_qa.sh` |
| WebDriver in-webview input (no global pointer) | **required** | `scripts/qa_webdriver.py` |
| Driver process group torn down | **required** | `stop_session` killpg + EXIT trap |
| Real pixels (unique colours ≫ 1) | required each run | meta `unique_colors_cap200k` |

## Acceptance ledger

| Item | Status | Evidence |
|---|---|---|
| WebKitGTK All/Week/Month/History | **PASS** | `p4b-all.png` (7 timers), week/month, `p4b-history.png` (12 records after Run now: 8 registered + 4 fired/wake) |
| Timer dialogs | **PASS** | kind select shows full value (`once` / `weekly` / … after rework #3 short labels); DST warning full text |
| 7-kind create/edit via GUI | **PASS** | `store-after-create.json` / `store-after-edit.json` |
| 7-kind delete via GUI | **PASS** | 8× `DELETE OK` in `capture-run.log`; store emptied mid-session |
| Preview vs `bellman next` | **PASS** | full `2026-07-29T05:00:00Z` + `+03:00 Europe/Helsinki` ×5 in `p4b-dialog-preview-weekly.png` and `p4b-dialog-1280x800.png` |
| DST gap warning | **PASS** | `p4b-dialog-dst-gap.png` |
| Layout 960 + larger | **PASS** | no ellipsis on preview data; Enabled visible; large dialog shot distinct md5 |
| WebKit pid + userAgent | **PASS with caveat** | see OPEN #2 |
| Per-shot `.meta.json` | **PASS** | next to each PNG |
| App logs for rejects | **PASS** | raw teed **0-byte** files |
| JSONL via Run now | **PASS** | 14 lines total (10 `registered` / `gui create`, 4 `fired` / `wake_delivered`) |

### OPEN / caveats (honest)

1. **GUI create does not write `EventKind::Registered`** — **RESOLVED** on card `2026-07-28_0002` (this card). `create_timer` now emits `EventKind::Registered` with message `gui create`.
   Sibling commands `update_timer`, `delete_timer`, and toggle `set_enabled` / `set_pause_all` do NOT emit lifecycle events because there are no matching `EventKind` variants for update/delete. This gap is recorded and left for C11.

2. **userAgent** — originally library default via `WebKit2.Settings.get_user_agent()` (app never calls `set_user_agent`). The isolated-display harness now also records live `navigator.userAgent` from the WebDriver webview when available (`docs/qa4-evidence/userAgent.json`).
   Library default value: `Mozilla/5.0 (X11; Ubuntu; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/60.5 Safari/605.1.15`

3. **Some form *labels*** (e.g. long Weekdays csv legend) can still clip slightly at 960×640 — the legend renders `mon,tue,wed,thu,fri,sat,su` with the trailing `n)` cut, at 960×640 **and** at 1280×800. No data is hidden and no control is blocked: **preview numbers and the occurrence-kind select value** are fully readable after rework #3 short option labels.  
   **Owner: card `bellman-c8d-timer-input-ergonomics-pickers-date-formats-validation`**, whose scope replaces this CSV field with seven weekday toggle chips — the legend disappears with the field. Left OPEN here rather than restyled, per this card's "do NOT re-fix or refactor C8 code here".

4. **Kind `<option>` labels lost their inline descriptions** to fix the truncated select value (rework #3). A first-time user gets no in-place explanation of each kind; the kind-specific field labels below the select still describe the parameters. C8d rebuilds this control and should restore the descriptions in a form that fits (e.g. a helper line under the select, not inside the option text).

5. **No `p4b-dialog-layout-fixed.png`** — removed as a byte-identical re-use of the preview shot. Large-size dialog proof is only `p4b-dialog-1280x800.png`.

## Preview ↔ CLI parity (qa-weekly)

| # | Local | UTC | Offset / tz |
|---|---|---|---|
| 1 | 2026-07-29 08:00:00 | 2026-07-29T05:00:00Z | +03:00 Europe/Helsinki |
| 2–5 | … | …T05:00:00Z | +03:00 Europe/Helsinki |

CLI: `2026-07-29T05:00:00+00:00` … `2026-08-07T05:00:00+00:00` — matches.

## Product changes on this card

- Dialog width / overflow / preview columns (reworks #0–#2).
- Kind `<option>` labels shortened to bare kind names (rework #3 / S3).

## Files

```
scripts/qa_display.sh
scripts/qa_webdriver.py
scripts/run_gui_qa.sh
scripts/capture_qa_p4b.py
docs/QA_P4b.md
docs/qa4-screenshots/p4b-*.png + p4b-*.meta.json
docs/qa4-evidence/*
```

## Screenshot Descriptions

- **p4b-all.png**: Shows the main list view with 7 created timers (qa-daily, qa-dst-gap, qa-interval, qa-monthly, qa-once, qa-weekly, qa-yearly) visible at 960x640.
- **p4b-all-after-edit.png**: Shows the main list view after editing all 8 timers (e.g. qa-once renamed to qa-once-edited) at 960x640.
- **p4b-all-after-delete.png**: Shows the main list view empty after deleting all timers at 960x900.
- **p4b-all-empty.png**: Shows the initial empty state of the main list view before creating any timers at 960x640.
- **p4b-dialog-dst-gap.png**: Shows the create dialog for qa-dst-gap with a timezone gap warning message visible at 960x640.
- **p4b-dialog-once-dialog.png**: Shows the create dialog configured for the qa-once timer at 960x640.
- **p4b-dialog-weekly-dialog.png**: Shows the create dialog configured for the qa-weekly timer at 960x640.
- **p4b-dialog-preview-weekly.png**: Shows the edit dialog for qa-weekly, featuring the 5-occurrence preview side-pane on the right at 960x640.
- **p4b-week.png**: Shows the weekly calendar view displaying the scheduled occurrences for the week at 960x640.
- **p4b-month.png**: Shows the monthly calendar view displaying the scheduled occurrences for the month at 960x640.
- **p4b-history.png**: Shows the Run History tab containing 12 records (8 `registered` events from creation, and 4 `fired`/`wake_delivered` from running timers) at 960x640.
- **p4b-all-1280x800.png**: Shows the main list view at 1280x800 resolution with qa-daily and qa-weekly timers.
- **p4b-week-1280x800.png**: Shows the weekly calendar view at 1280x800 resolution.
- **p4b-month-1280x800.png**: Shows the monthly calendar view at 1280x800 resolution.
- **p4b-history-1280x800.png**: Shows the Run History tab at 1280x800 resolution containing the registration and fire events.
- **p4b-dialog-1280x800.png**: Shows the edit dialog with preview side-pane at 1280x800 resolution.
