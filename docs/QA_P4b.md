# QA P4b — WebKitGTK GUI evidence for C8 calendar UI (rework #2)

Card: train run `2026-07-28_0001`.
C8 base: `d49b7dc` / `ec8a330`. Layout CSS + preview-column tweak ship on this card.

## Display setup (path a — real `:0` NVIDIA)

```sh
cd ui && npm ci && npm run build && cd ..
cargo tauri build --no-bundle   # NOT plain cargo build (cfg(dev) → blank window)

cargo build -p bellman-cli --release
cp -a target/release/bellman /tmp/bellman-cli-schema3
# restore GUI binary from cargo-tauri output if the CLI clobbered the name

QA=/tmp/qa-p4b-session
rm -rf "$QA"
mkdir -p "$QA/share/io.bellman.desktop/logs" "$QA/share/io.bellman.desktop/slots" "$QA/config"
printf '%s\n' '{"wizard_completed":true,"autostart_enabled":false,"start_minimized":false,"wake_enabled":false}' \
  > "$QA/share/io.bellman.desktop/config.json"

export DISPLAY=:0 XDG_DATA_HOME="$QA/share" XDG_CONFIG_HOME="$QA/config" GDK_BACKEND=x11 RUST_LOG=info
BIN=…/target/release/bellman
: > /tmp/qa-p4b.out; : > /tmp/qa-p4b.err
"$BIN" >>/tmp/qa-p4b.out 2>>/tmp/qa-p4b.err &

export BELLMAN_CLI=/tmp/bellman-cli-schema3
export BELLMAN_QA_DATA="$QA/share/io.bellman.desktop"
python3 scripts/capture_qa_p4b.py
# then: cp /tmp/qa-p4b.{out,err} docs/qa4-evidence/app-stdout.log / app-stderr.log
```

## Acceptance ledger

| Item | Status | Evidence |
|---|---|---|
| WebKitGTK All/Week/Month/History | **PASS** | `p4b-all.png` (8 timers), week/month, `p4b-history.png` (4 records after Run now) |
| Timer dialogs | **PASS** | kind combo matches fields (`once — one-shot…`); DST warning full text |
| 7-kind create/edit via GUI | **PASS** | `store-after-create.json` / `store-after-edit.json` |
| 7-kind delete via GUI | **PASS** | 8× `DELETE OK` in `capture-run.log`; store emptied mid-session |
| Preview vs `bellman next` | **PASS** | `p4b-dialog-preview-weekly.png` shows full `2026-07-29T05:00:00Z` and `+03:00 Europe/Helsinki` ×5; matches `cli-next-qa-weekly.txt` |
| DST gap warning | **PASS** | `p4b-dialog-dst-gap.png` |
| Layout 960 + larger, no clip of required data | **PASS** (rework #2) | Removed `table-layout:fixed`+ellipsis; dialog 860px + 3-col preview; Enabled visible; `p4b-dialog-1280x800.png` distinct from 960 preview |
| WebKit pid + userAgent | **PASS with caveat** | pids in `webkit_pids*.json`; UA via lib default — see OPEN |
| Per-shot `.meta.json` | **PASS** | committed next to each PNG |
| App logs for rejects | **PASS** | raw teed files, **0 bytes** (app emitted nothing; no hand-written text) |
| JSONL via Run now | **PASS** | 4 lines `fired`/`wake_delivered` |

### OPEN / caveats (honest)

1. **GUI create does not write `EventKind::Registered`** — only CLI/slot writers exist under `bellman-cli`. Own-card defect; evidence uses GUI **Run now** for JSONL instead. REPRO: create via GUI only → JSONL stays empty until Run now.

2. **userAgent is library default, not live `navigator.userAgent`.**  
   Method: `WebKit2.Settings.get_user_agent()` in the driver process (same `libwebkit2gtk-4.1` the app links; app never calls `set_user_agent`).  
   Value: `Mozilla/5.0 (X11; Ubuntu; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/60.5 Safari/605.1.15`  
   Attempts: inspector server bind may fail if port busy (`userAgent.json`). Live page evaluate was not available without injecting JS into the product.

3. **Form labels** (e.g. Weekdays csv legend) can still clip slightly at 960×640; **control values and preview numbers do not**.

4. **No `p4b-dialog-layout-fixed.png`** — removed; it was a byte-identical re-use of the preview shot (R2). Large-size proof is `p4b-dialog-1280x800.png` only (different geometry/md5 from the 960 preview).

## Preview ↔ CLI parity (qa-weekly)

On-screen (960 and 1280 dialogs):

| # | Local | UTC | Offset / tz |
|---|---|---|---|
| 1 | 2026-07-29 08:00:00 | 2026-07-29T05:00:00Z | +03:00 Europe/Helsinki |
| 2–5 | … | …T05:00:00Z | +03:00 Europe/Helsinki |

CLI `bellman next`: `2026-07-29T05:00:00+00:00` … `2026-08-07T05:00:00+00:00` — matches.

## CSS changes on this card (product)

- `.wizard.timer-dialog` width 860px (was 460 via wizard clash; was 760 after rework #0).
- No `table-layout:fixed` / `text-overflow:ellipsis` on preview cells (rework #1 regression removed).
- Preview table: Local / UTC / Offset·tz only (dropped redundant Date column).

## Files

```
ui/src/styles.css
ui/src/TimerDialog.svelte
scripts/capture_qa_p4b.py
docs/QA_P4b.md
docs/qa4-screenshots/p4b-*.png + p4b-*.meta.json
docs/qa4-evidence/*
```
