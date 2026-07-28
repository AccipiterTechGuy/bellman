# QA P4b — WebKitGTK GUI evidence for C8 calendar UI (rework #3)

Card: train run `2026-07-28_0001`.
C8 base: `d49b7dc` / `ec8a330`. Layout + preview + kind-label tweaks ship on this card.

## Display setup (path a — real `:0` NVIDIA)

Copy-paste from a clean shell (repo root = this worktree or `~/bellman`):

```sh
set -euo pipefail
ROOT="$(pwd)"                          # worktree or main checkout
cd "$ROOT"

# 1. Frontend + production Tauri shell
#    MUST use cargo-tauri: plain `cargo build -p bellman --release` sets cfg(dev)
#    and loads http://localhost:1420 → blank white window.
cd ui && npm ci && npm run build && cd ..
cargo tauri build --no-bundle

# 2. Schema-v3 CLI as a SIDE copy (both packages emit a binary named "bellman")
cargo build -p bellman-cli --release
cp -a target/release/bellman /tmp/bellman-cli-schema3
# Restore the GUI binary that cargo-cli may have overwritten:
cp -a target/release/bundle/deb/*/data/usr/bin/bellman target/release/bellman 2>/dev/null \
  || cargo tauri build --no-bundle

BIN="$ROOT/target/release/bellman"
test -x "$BIN"
test -x /tmp/bellman-cli-schema3

# 3. Isolated data dir + wizard already completed
QA=/tmp/qa-p4b-session
rm -rf "$QA"
mkdir -p "$QA/share/io.bellman.desktop/logs" "$QA/share/io.bellman.desktop/slots" "$QA/config"
printf '%s\n' '{"wizard_completed":true,"autostart_enabled":false,"start_minimized":false,"wake_enabled":false}' \
  > "$QA/share/io.bellman.desktop/config.json"

# 4. Launch GUI on the real display; tee raw stdout/stderr (0-byte is fine)
export DISPLAY=:0
export XDG_DATA_HOME="$QA/share"
export XDG_CONFIG_HOME="$QA/config"
export GDK_BACKEND=x11
export RUST_LOG=info
: > /tmp/qa-p4b.out
: > /tmp/qa-p4b.err
"$BIN" >>/tmp/qa-p4b.out 2>>/tmp/qa-p4b.err &
echo $! > /tmp/qa-p4b.pid
sleep 3
wmctrl -x -r Bellman.Bellman -e 0,40,40,960,640 || true

# 5. Drive GUI + capture
export BELLMAN_CLI=/tmp/bellman-cli-schema3
export BELLMAN_QA_DATA="$QA/share/io.bellman.desktop"
python3 scripts/capture_qa_p4b.py

# 6. Commit-ready log copies (raw; do not rewrite empty files as prose)
cp -a /tmp/qa-p4b.out docs/qa4-evidence/app-stdout.log
cp -a /tmp/qa-p4b.err docs/qa4-evidence/app-stderr.log
cat /tmp/qa-p4b.out /tmp/qa-p4b.err > docs/qa4-evidence/app-combined.log
```

If `CARGO_TARGET_DIR` is set (e.g. a shared target from another worktree), replace
`BIN="$ROOT/target/release/bellman"` with
`BIN="$CARGO_TARGET_DIR/release/bellman"`.

### Paths tried for the WebKit empty-shell blocker

| Path | Result |
|---|---|
| **(a) Real `:0` + NVIDIA** | **Works** — full UI paints; used for all evidence |
| (b) Software under Xvfb (`LIBGL_ALWAYS_SOFTWARE`, `WEBKIT_DISABLE_*`, …) | Still empty shell (C8 rework); WebKitWebProcess alive but no pixels |
| (c) Xephyr / DRI3 Xvfb config | Not required once (a) worked |

## Acceptance ledger

| Item | Status | Evidence |
|---|---|---|
| WebKitGTK All/Week/Month/History | **PASS** | `p4b-all.png` (8 timers), week/month, `p4b-history.png` (4 records after Run now) |
| Timer dialogs | **PASS** | kind select shows full value (`once` / `weekly` / … after rework #3 short labels); DST warning full text |
| 7-kind create/edit via GUI | **PASS** | `store-after-create.json` / `store-after-edit.json` |
| 7-kind delete via GUI | **PASS** | 8× `DELETE OK` in `capture-run.log`; store emptied mid-session |
| Preview vs `bellman next` | **PASS** | full `2026-07-29T05:00:00Z` + `+03:00 Europe/Helsinki` ×5 in `p4b-dialog-preview-weekly.png` and `p4b-dialog-1280x800.png` |
| DST gap warning | **PASS** | `p4b-dialog-dst-gap.png` |
| Layout 960 + larger | **PASS** | no ellipsis on preview data; Enabled visible; large dialog shot distinct md5 |
| WebKit pid + userAgent | **PASS with caveat** | see OPEN #2 |
| Per-shot `.meta.json` | **PASS** | next to each PNG |
| App logs for rejects | **PASS** | raw teed **0-byte** files |
| JSONL via Run now | **PASS** | 4 lines `fired` / `wake_delivered` |

### OPEN / caveats (honest)

1. **GUI create does not write `EventKind::Registered`** — only CLI/slot writers in `bellman-cli`. Own-card defect; session uses GUI **Run now** for JSONL. REPRO: create via GUI only → JSONL empty until Run now.  
   WHERE `src-tauri/src/commands.rs:291-303` (no log write) vs `crates/bellman-cli/src/commands.rs:315-328` (`registered`, message `cli add`). `crates/bellman-core/src/events/record.rs:12` documents the kind as "Timer created (CLI / slot / GUI)", so the GUI is not meeting a stated contract.  
   **Owner: card `bellman-c8f-gui-create-writes-no-lifecycle-event-registered`** (filed from this run — not patched here, per this card's "defect to file, not to patch").

2. **userAgent is library default, not live `navigator.userAgent`.**  
   Method: `WebKit2.Settings.get_user_agent()` (same `libwebkit2gtk-4.1`; app never `set_user_agent`).  
   Value: `Mozilla/5.0 (X11; Ubuntu; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/60.5 Safari/605.1.15`

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
ui/src/styles.css
ui/src/TimerDialog.svelte
scripts/capture_qa_p4b.py
docs/QA_P4b.md
docs/qa4-screenshots/p4b-*.png + p4b-*.meta.json
docs/qa4-evidence/*
```
