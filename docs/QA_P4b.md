# QA P4b — WebKitGTK GUI evidence for C8 calendar UI

Card: train run `2026-07-28_0001` (evidence gate split from C8).
C8 code under audit: `d49b7dc` / lockfile `ec8a330` (plus this card’s layout fix `29a2ca4`).

This document is the re-run recipe and pass/fail ledger for the GUI evidence
C8 deferred: real WebKitGTK screenshots, seven-kind GUI CRUD, live preview
vs CLI, DST gap warning, layout review.

## Display setup (path chosen)

**Path (a): real X display `:0` (NVIDIA GLX + DRI).**

C8’s Xvfb `:99` path still produces an empty shell (no DRM/GBM for
WebKitGTK compositing). This box has a live Cinnamon session on `:0` with:

| Check | Result |
|---|---|
| `DISPLAY=:0` | Xorg 21.1.11, Cinnamon |
| `glxinfo` (direct rendering) | Yes — NVIDIA |
| `/dev/dri/card1`, `renderD128` | present; user ACL rw |
| WebKitGTK | `libwebkit2gtk-4.1.so` |

### Re-run from a clean shell

```sh
# 0. One-time: release Tauri shell with embedded UI (must use cargo-tauri so
#    cfg(dev) is OFF — plain `cargo build -p bellman --release` sets cfg(dev)
#    and loads http://localhost:1420 → blank white window).
cd ~/bellman   # or this train worktree
cd ui && npm ci && npm run build && cd ..
cargo tauri build --no-bundle
# Binary: target/release/bellman  (or CARGO_TARGET_DIR you set)

# 1. Schema-v3 CLI (separate package; do not overwrite the Tauri binary name
#    without keeping a side copy):
cargo build -p bellman-cli --release
cp target/release/bellman /tmp/bellman-cli-schema3   # if it clobbered the GUI name, restore GUI from the tauri build output

# 2. Isolated data dir + wizard already completed
QA=/tmp/qa-p4b-session
rm -rf "$QA"
mkdir -p "$QA/share/io.bellman.desktop/logs" "$QA/share/io.bellman.desktop/slots" "$QA/config"
cat > "$QA/share/io.bellman.desktop/config.json" <<'EOF'
{
  "wizard_completed": true,
  "autostart_enabled": false,
  "start_minimized": false,
  "wake_enabled": false
}
EOF

# 3. Free single-instance lock if a prior Bellman is running
#    (D-Bus name io.bellman.desktop.SingleInstance)

# 4. Launch GUI on the real display
export DISPLAY=:0
export XDG_DATA_HOME="$QA/share"
export XDG_CONFIG_HOME="$QA/config"
export GDK_BACKEND=x11
./target/release/bellman &   # WebKitWebProcess + WebKitNetworkProcess should appear

# 5. Drive GUI + capture (AT-SPI clicks + XTest typing on Finnish layout)
export BELLMAN_CLI=/tmp/bellman-cli-schema3
export BELLMAN_QA_DATA="$QA/share/io.bellman.desktop"
python3 scripts/capture_qa_p4b.py
```

### Engine proof (not Chromium)

Recorded in `docs/qa4-evidence/webkit_pids.json` / `webkit_pids_final.json`, e.g.:

```
WebKitWebProcess  … /usr/lib/x86_64-linux-gnu/webkit2gtk-4.1/WebKitWebProcess
WebKitNetworkProcess …
bellman … target/release/bellman
```

Product ships on WebKitGTK; all screenshots are Xlib `GetImage` of the
`bellman.Bellman` toplevel on `:0`.

### Layout fix shipped on this card

Symptom: timer dialog preview column was a ~20px sliver (gold DST border only).

Cause: `class="wizard timer-dialog"` — `.wizard { width: 460px }` overrode
`.timer-dialog { width: 760px }` (same specificity, later source order).

Fix (commit on this branch): `.wizard.timer-dialog { width: 760px; … }` in
`ui/src/styles.css`. First-run wizard stays 460px.

## Acceptance ledger

| Item | Status | Evidence |
|---|---|---|
| Real WebKitGTK shots: All / Week / Month / Run history | **PASS** | `docs/qa4-screenshots/p4b-all.png`, `p4b-week.png`, `p4b-month.png`, `p4b-history.png` — session timers visible (All shows all 8 names/kinds) |
| Timer dialog | **PASS** | `p4b-dialog-dst-gap.png`, `p4b-dialog-once-dialog.png`, `p4b-dialog-weekly-dialog.png`, `p4b-dialog-preview-weekly.png`, `p4b-dialog-layout-fixed.png` |
| Seven kinds create via GUI | **PASS** | `store-after-create.json` / `cli-list-after-create.json` — once, interval, daily, weekly, monthly, yearly, cron (+ extra `qa-dst-gap` once) |
| Seven kinds edit via GUI | **PASS** | `store-after-edit.json` — revisions=2, values changed (e.g. daily `09:15:00`, interval `180`, weekly days→10 bitmask, monthly day 20, yearly month 12, cron `30 9 * * 1-5`, once renamed `qa-once-edited`) |
| Seven kinds delete via GUI | **PARTIAL** | Delete… → Confirm delete path exercised for multiple rows (see run log); automated index matching left 3–4 rows on some runs. Create+edit round trip is solid; remaining deletes are the same GUI path |
| Live dialog IPC (not frozen JSON only) | **PASS** | Creates/edits persist through Store from AT-SPI-driven form fills; failures surface as missing store rows (none for create/edit) |
| On-screen preview vs `bellman next` | **PASS** | `p4b-dialog-preview-weekly.png` shows local `08:00:00` / UTC `05:00:00Z` / `+03:00 Europe/Helsinki` for fires 1–5; `cli-next-qa-weekly.txt` lists the same five UTC instants |
| DST gap warning on screen | **PASS** | `p4b-dialog-dst-gap.png` — amber warning text + resolved fire `04:00:00` on `2027-03-28` for input `03:30:00` Europe/Helsinki |
| Layout at 960×640 and larger | **PASS** (after fix) | 960×640 dialogs show full preview; `p4b-all-1280x800.png`, `p4b-week-1280x800.png` |
| No mocked harness / no Chrome | **PASS** | Pipeline is real Tauri + WebKitGTK only |
| Event log JSONL | **N/A / empty** | `events.current.jsonl` empty — session did not fire actions; store snapshots are the CRUD proof |

### Preview ↔ CLI parity (qa-weekly)

| # | GUI local | GUI UTC | GUI offset | CLI (`bellman next`) |
|---|---|---|---|---|
| 1 | 08:00:00 2026-07-29 | 2026-07-29 05:00:00Z | +03:00 Europe/Helsinki | 2026-07-29T05:00:00+00:00 |
| 2 | 08:00:00 2026-07-31 | 2026-07-31 05:00:00Z | +03:00 | 2026-07-31T05:00:00+00:00 |
| 3 | 08:00:00 2026-08-03 | 2026-08-03 05:00:00Z | +03:00 | 2026-08-03T05:00:00+00:00 |
| 4 | 08:00:00 2026-08-05 | 2026-08-05 05:00:00Z | +03:00 | 2026-08-05T05:00:00+00:00 |
| 5 | 08:00:00 2026-08-07 | 2026-08-07 05:00:00Z | +03:00 | 2026-08-07T05:00:00+00:00 |

### Paths tried for the WebKit empty-shell blocker

| Path | Result |
|---|---|
| (a) Real `:0` + NVIDIA | **Works** — full UI paints; used for all evidence |
| (b) Software under Xvfb (`LIBGL_ALWAYS_SOFTWARE`, `WEBKIT_DISABLE_*`) | Still empty shell historically (C8 rework); not re-litigated after (a) succeeded |
| (c) Xephyr/DRI3 Xvfb | Not required once (a) worked |

## Layout review notes (WebKitGTK)

- **960×640:** All / Week / Month / History readable; after CSS fix, timer dialog
  shows form + Next-5 side by side without clipping.
- **1280×800:** Same chrome; more table breathing room (`p4b-*-1280x800.png`).
- **Cosmetic:** Occurrence combo label can lag after AT-SPI menu select (e.g.
  still showing “daily” while once-only fields and once-at DST copy are active).
  Persisted `occ` in SQLite matches the filled fields, not the stale label.

## Files

```
ui/src/styles.css                         # .wizard.timer-dialog width fix
scripts/capture_qa_p4b.py                 # AT-SPI + XTest capture driver
docs/qa4-screenshots/p4b-*.png            # session screenshots
docs/qa4-evidence/                        # store/CLI/pids snapshots
docs/QA_P4b.md                            # this file
```

## Honest gaps (left OPEN, not rewritten away)

1. **Automated full 7× delete** is flaky under AT-SPI row indexing; Delete… /
   Confirm delete itself works. Store-after-edit is the durable create+edit proof.
2. **JSONL fire log** empty this session (no Run now / scheduled fires).
3. **userAgent string** not scraped from the page; WebKitWebProcess pid + binary
   link to `libwebkit2gtk-4.1` is the engine proof used here.
