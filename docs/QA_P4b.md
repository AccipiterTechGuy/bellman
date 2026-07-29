# QA P4b — WebKitGTK GUI evidence for C8 calendar UI

Card: train run `2026-07-29_0001` (isolated display + WebDriver rewrite).
Earlier C8 evidence lives under `docs/qa4-screenshots/` / `docs/qa4-evidence/`.

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

# 2. CLI sidecar (binary name `bellman`, package bellman-cli)
cargo build -p bellman-cli --release
# If cargo overwrote paths, stage from deb extract or keep a side copy.

export BELLMAN_APP="$ROOT/target/release/bellman-app"
export BELLMAN_CLI="$ROOT/target/release/bellman"   # or /tmp/bellman-cli-schema3
test -x "$BELLMAN_APP"

# 3. Python deps for the harness (venv recommended)
python3 -m venv /tmp/bellman-qa-venv
/tmp/bellman-qa-venv/bin/pip install selenium pillow python-xlib
export BELLMAN_QA_PYTHON=/tmp/bellman-qa-venv/bin/python

# 4. System deps (once per machine) — see docs/BUILD_PLAN.md
#    sudo apt install -y webkit2gtk-driver   # version-match libwebkit2gtk-4.1
#    cargo install tauri-driver --locked
#    Window manager: metacity is already on Mint (`metacity --sm-disable`).

# 5. Run the suite on an isolated display
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
3. Tears the display down on exit (no orphan Xvfb).

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
| (b) Real operator session (legacy) | Works for paint, but **steals pointer/keyboard** — forbidden |
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

## Acceptance ledger

| Item | Status | Evidence |
|---|---|---|
| Isolated display (not operator session) | **required** | `scripts/qa_display.sh`, `run_gui_qa.sh` |
| WebDriver in-webview input (no global pointer) | **required** | `scripts/qa_webdriver.py` |
| WebKitGTK All/Week/Month/History | prior PASS | `p4b-*.png` under `docs/qa4-screenshots/` |
| 7-kind create/edit/delete via GUI | prior PASS | store JSON + capture log |
| Per-shot `.meta.json` | prior PASS | next to each PNG |
| Real pixels (unique colours ≫ 1) | required each run | meta `unique_colors_cap200k` |

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
