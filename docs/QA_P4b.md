# QA P4b — WebKitGTK GUI evidence for C8 calendar UI (rework #1)

Card: train run `2026-07-28_0001`.
C8 code under audit: `d49b7dc` / `ec8a330`.
This card also ships layout CSS fixes (dialog width + overflow).

## Display setup (path chosen)

**Path (a): real X display `:0` (NVIDIA GLX + DRI).** Xvfb `:99` still empty-shell.

### Re-run from a clean shell

```sh
cd /path/to/worktree
cd ui && npm ci && npm run build && cd ..
# MUST use cargo-tauri (plain cargo build --release sets cfg(dev) → blank window)
cargo tauri build --no-bundle

# Schema-v3 CLI side copy (do not clobber the GUI binary name permanently)
cargo build -p bellman-cli --release
cp -a target/release/bellman /tmp/bellman-cli-schema3
# restore GUI binary from cargo tauri build output if clobbered

QA=/tmp/qa-p4b-session
rm -rf "$QA"
mkdir -p "$QA/share/io.bellman.desktop/logs" "$QA/share/io.bellman.desktop/slots" "$QA/config"
cat > "$QA/share/io.bellman.desktop/config.json" <<'EOF'
{"wizard_completed":true,"autostart_enabled":false,"start_minimized":false,"wake_enabled":false}
EOF

export DISPLAY=:0
export XDG_DATA_HOME="$QA/share"
export XDG_CONFIG_HOME="$QA/config"
export GDK_BACKEND=x11
export RUST_LOG=info

BIN=target/release/bellman   # or your CARGO_TARGET_DIR path
: > /tmp/qa-p4b-combined.log
"$BIN" > >(tee /tmp/qa-p4b.out >> /tmp/qa-p4b-combined.log) \
       2> >(tee /tmp/qa-p4b.err >> /tmp/qa-p4b-combined.log) &

export BELLMAN_CLI=/tmp/bellman-cli-schema3
export BELLMAN_QA_DATA="$QA/share/io.bellman.desktop"
python3 scripts/capture_qa_p4b.py
```

### Engine proof

| Proof | Location |
|---|---|
| WebKitWebProcess / NetworkProcess pids | `docs/qa4-evidence/webkit_pids*.json` |
| Page/engine userAgent | `docs/qa4-evidence/userAgent.txt` / `userAgent.json` |
| Per-shot geometry + pixel stats | `docs/qa4-screenshots/p4b-*.meta.json` |
| App stdout/stderr | `docs/qa4-evidence/app-stdout.log`, `app-stderr.log`, `app-combined.log` |

**userAgent (this session):**
```
Mozilla/5.0 (X11; Ubuntu; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/60.5 Safari/605.1.15
```
Captured via `WebKit2.Settings.get_user_agent()` (libwebkit2gtk-4.1 default; app never calls `set_user_agent`). Inspector bind attempt also recorded in `userAgent.json` (port in use on one try).

## Acceptance ledger (rework #1)

| Item | Status | Evidence |
|---|---|---|
| Real WebKitGTK All/Week/Month/History | **PASS** | `p4b-all.png` (8 timers after create+refresh), `p4b-week.png`, `p4b-month.png`, `p4b-history.png` (**4 records** after GUI Run now) |
| Timer dialog | **PASS** | `p4b-dialog-*.png`; combo label matches kind (`once — one-shot…` in `p4b-dialog-once-dialog.png`) |
| 7 kinds create via GUI | **PASS** | `store-after-create.json` — 8 rows (7 KINDS + qa-dst-gap), correct `occ` payloads |
| 7 kinds edit via GUI | **PASS** | `store-after-edit.json` — rev≥2, field changes for all 7 KINDS |
| 7 kinds delete via GUI | **PASS** | capture-run: `DELETE OK` for all 8 qa-* names including once/yearly; intermediate store empty; `delete_kind` raises on no-op |
| Live dialog IPC | **PASS** | create/edit/delete reflected in store; app logs empty of errors; driver fails hard on silent delete no-op |
| Preview vs `bellman next` | **PASS** | `p4b-dialog-preview-weekly.png` + `cli-next-qa-weekly.txt` (08:00 +03:00 = 05:00Z ×5) |
| DST gap warning | **PASS** | `p4b-dialog-dst-gap.png` amber text + resolved 04:00:00 |
| Layout 960×640 no clip/overflow | **PASS** (after CSS) | dialog max-height + `minmax(0,fr)` grid; Enabled fully visible; no x-scrollbar in rework shots |
| Larger size incl. dialog | **PASS** | `p4b-*-1280x800.png` for All/Week/Month/History + `p4b-dialog-1280x800.png` |
| userAgent + WebKit pid | **PASS** | `userAgent.txt` + `webkit_pids_final.json` |
| Per-shot `.meta.json` | **PASS** | committed next to each `p4b-*.png` |
| App stderr for rejects | **PASS** | teed logs committed; empty = no runtime errors logged this session |
| JSONL round-trip | **PASS** (via Run now) | `events.current.jsonl` has `fired` / `wake_delivered` lines; history page shows 4 records |
| GUI create → `registered` event | **DEFECT (own card)** | GUI create still does not write `EventKind::Registered` (only CLI/slot do). Documented below — not patched here |

### Defect to file (not fixed on this card)

**GUI create path emits no `registered` event.**

- Core defines `EventKind::Registered = "Timer created (CLI / slot / GUI)"` in
  `crates/bellman-core/src/events/record.rs`.
- Writers exist only in `crates/bellman-cli/src/commands.rs` — **zero** writers under `src-tauri/`.
- REPRO: create timers only via GUI; `wc -l …/logs/events.current.jsonl` stays 0 until **Run now**.
- Workaround used for acceptance: GUI **Run now** → `fired` / `wake_delivered` in JSONL + Run history shot with session data.

### Layout CSS shipped here

1. `.wizard.timer-dialog { width: 760px }` — stops wizard 460px override (preview sliver).
2. `.wizard.timer-dialog` max-height + `dialog-body` `minmax(0, fr)` + `overflow-x: hidden` — stops Enabled clipping and horizontal scrollbar at 960×640.

### Capture driver fixes (rework #1)

- **F1** `delete_kind` asserts store row gone; taller window during delete; title-matched Edit.
- **F2** GUI Run now ×2 before history shot.
- **F3** kind select via click + Home/Down/Enter (not AT-SPI menu Action).
- **F5** `userAgent.json` / `.txt`.
- **F6** wait after create for list refresh; in-session `p4b-dialog-layout-fixed.png`; commit `*.meta.json`.
- **F7** tee app stdout/stderr into evidence.

## Preview ↔ CLI (qa-weekly)

| # | GUI | CLI |
|---|---|---|
| 1–5 | local 08:00 Europe/Helsinki, UTC 05:00:00Z, +03:00 | `2026-07-29`…`2026-08-07` `T05:00:00+00:00` |

## Files

```
ui/src/styles.css
scripts/capture_qa_p4b.py
docs/QA_P4b.md
docs/qa4-screenshots/p4b-*.png
docs/qa4-screenshots/p4b-*.meta.json
docs/qa4-evidence/*
```
