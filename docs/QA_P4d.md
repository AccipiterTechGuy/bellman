# QA P4d — Timer input ergonomics (C8d)

Card: train run `2026-07-28_0008`  
Slug: `bellman-c8d-timer-input-ergonomics-pickers-date-formats-validation`  
Worktree: `.train-worktrees/2026-07-28_0008` · branch `train/2026-07-28_0008`

## Goal (one line)

Make creating a timer possible without knowing the app’s internal formats: human dates/times, weekday chips, searchable timezone, inline validation, distinct errors vs DST advisories.

## WebKitGTK native date/time verdict

**Verdict: USE native `<input type="date">` and `<input type="time">`.**

Probe: real WebKitGTK 4.1 window (not Chromium), same UA as the app:

`Mozilla/5.0 (X11; Ubuntu; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/60.5 Safari/605.1.15`

| Control | JS type | Value after load | Offset height | Usable chrome |
|---|---|---|---|---|
| `type=date` | `date` | `2026-12-24` | 38px | Yes — calendar popup |
| `type=time` step=1 | `time` | `09:00:00` | 40px | Yes — spin/AM-PM chrome |
| `type=datetime-local` | `datetime-local` | `2026-12-24T09:00` | — | Yes (not used; separate fields preferred) |

Screenshot that decided it: **`docs/qa4-screenshots/p4d-webkit-native-date-time-probe.png`**

- Shows three labelled rows: date, time, datetime-local, each with a painted native control (not a plain text box).
- Locale display is US-style in the native widget (`12/24/2026`, `09:00:00 AM`); wire value remains ISO. Free-text fields beside the pickers accept European `24.12.2026`.

No date library and no component framework were added (BUILD_PLAN: plain CSS, no new runtime deps).

## Product choices

### Default wake action

**Kept `none` as the default**, with an explicit in-dialog explanation under Wake action:

> none (default) — the timer fires on schedule but does not notify or launch anything. Choose “desktop notification” or “launch command” if you want a visible effect.

**Why not default to notify:** a new timer that pops a desktop notification without the user asking is surprising (especially for CLI/slot-created workflows that share the same `action: none` default). Opt-in keeps create safe; the dialog no longer leaves “does nothing” unexplained.

### Date parsing rules

| Input | Rule |
|---|---|
| `24.12.2026` | Day-first (dot) |
| `24-12-2026` | Day-first (dash, non-ISO) |
| `2026-12-24` / `2026-12-24T09:00:00` | ISO year-first |
| `24/12/2026` | Slash, day unambiguous → D/M/Y |
| `12/24/2026` | Slash, month unambiguous → M/D/Y |
| `01/02/2026` | Slash, both ≤12 → **day-first**, note echoed |
| Seconds | Optional on times (`09:00` ≡ `09:00:00`) |

Every accepted once value shows an **echo line** in words, e.g.  
`Thursday 24 December 2026, 09:00 Europe/Helsinki`.

### CSS block revisited (C8b `.wizard.timer-dialog`)

C8b capped `max-height: min(92vh, 600px)` with `overflow: hidden`. Chips + tz list + dual date/time controls need more room:

- `max-height` raised to `min(94vh, 720px)`
- body still scrolls; labels/chips forced `overflow: visible` (no ellipsis on labels)
- form rows use stacked layout for multi-control fields

## Evidence ledger

All PNG evidence is **real WebKitGTK** via `scripts/capture_qa_p4d.py` (AT-SPI + XTest + Xlib GetImage), display recipe same family as `docs/QA_P4b.md` path (a) real `:0` NVIDIA. No mocked harness, no Chromium, no hand-edited images.

### Screenshots opened and described

| File | What is in it |
|---|---|
| `p4d-webkit-native-date-time-probe.png` | Standalone WebKitGTK probe: native date / time / datetime-local controls paint and accept values. **Deciding shot for native pickers.** |
| `p4d-widget-date-time.png` | Once kind: free-text date + native date picker + free-text time + native time picker + echo line. |
| `p4d-widget-timezone.png` | Timezone text field + **unfiltered multi-entry** IANA list. Rows are full height (~23px) with legible names (Europe/Helsinki, Africa/Abidjan, Africa/Accra, …). Rework #1 fixed 9px flex-shrink. |
| `p4d-widget-weekday-chips.png` | Seven Mon–Sun toggle chips (Mon/Wed/Fri on by default). Also shows red **ERROR** banner for partial tz `Europe` (bonus contrast sample). |
| `p4d-widget-wall-time.png` | Daily/weekly wall-clock free-text + native time picker. |
| `p4d-once-echo-24-12-2026.png` | Typed `24.12.2026` + `09:00`, echo **“Thursday 24 December 2026, 09:00 Europe/Helsinki”**, preview row 2026-12-24 09:00 local / 07:00Z. |
| `p4d-field-error-invalid-date.png` | Inline field errors: empty name (“Name is required”), invalid `99.99.2026` (day-first calendar error), Create disabled with reason in footer. Red left-border field errors — not the DST advisory. |
| `p4d-preview-error-invalid-cron.png` | Cron kind with `not a cron` (preview empty; client allows Create because expression is non-empty — syntax is server-side). |
| `p4d-dst-advisory.png` | Once at Helsinki spring-forward gap 2027-03-28 03:30: amber **ADVISORY** badge + DST gap text; schedule resolves to 04:00. Distinct shape/label from ERROR. |
| `p4d-layout-960x640.png` | Weekly dialog at **960×640**: chips, tz list, time controls fully visible; no label cut-off. |
| `p4d-layout-1280x800.png` | Same dialog at **1280×800** (wmctrl geom confirmed 1280×800); more vertical air, no clipping. |
| `p4d-keyboard-create-result.png` | After keyboard-driven create: list shows `qa-p4d-keyboard`. |
| `p4d-all-after-create.png` | All-timers after 7-kind GUI create (plus session leftovers). |
| `p4d-all-after-edit.png` | After edit of each of the seven. |
| `p4d-all-after-delete.png` | After delete of the seven; session leftovers remain. |
| `p4d-all-final.png` | Final All timers view. |

### Create-through-GUI walkthrough (`24.12.2026`)

1. Open New timer → kind **once**.
2. Name `qa-p4d-once-eu`, tz `Europe/Helsinki`.
3. Type date `24.12.2026`, time `09:00`.
4. Echo: **Thursday 24 December 2026, 09:00 Europe/Helsinki** (`p4d-once-echo-24-12-2026.png`).
5. Create → store + CLI:

```json
"name": "qa-p4d-once-eu",
"occurrence": { "kind": { "at": "2026-12-24T09:00:00", "occ": "once" }, "tz": "Europe/Helsinki" },
"next_fire_utc": "2026-12-24T07:00:00Z"
```

Evidence files:

- `docs/qa4-evidence/bellman-list-after-once-eu.json`
- `docs/qa4-evidence/store-after-once-eu.json` (if present) / timers.db session

### Keyboard-only creation

One pointer click opens the dialog (Name autofocused). Then: type name → Tab → Tab → type `UTC` → a11y `grabFocus` on wall-clock → type `07:30` → a11y Activate on **Create**. Result: `qa-p4d-keyboard` in store (`p4d-keyboard-create-result.png`).

### 7-kind create + edit + delete

`scripts/capture_qa_p4d.py` `crud_all_kinds` created, edited, and deleted:

`once`, `interval`, `daily`, `weekly`, `monthly`, `yearly`, `cron`

through the real GUI. Log lines: `DELETE OK` ×7. Screenshots: `p4d-all-after-create.png`, `p4d-all-after-edit.png`, `p4d-all-after-delete.png`.

### ERROR vs ADVISORY (non-colour-only)

| Kind | Treatment | Shot |
|---|---|---|
| Field validation | Red left border + text under field + footer reason; Create disabled | `p4d-field-error-invalid-date.png` |
| Preview / server failure | Red box + uppercase **ERROR** badge | `p4d-widget-weekday-chips.png` (tz `Europe`) |
| DST gap/fold | Amber box + uppercase **ADVISORY** badge (C8b behaviour preserved) | `p4d-dst-advisory.png` |

## Build / test commands run

```sh
cd ui && npm ci && npm test && npm run build
cargo test --workspace --lib
cargo tauri build --no-bundle
# GUI binary: target/release/bellman-app
# CLI binary: target/release/bellman
python3 scripts/capture_qa_p4d.py   # real :0 WebKitGTK
```

(Exact test exit codes recorded in the train handoff log entry.)

## What still cannot be entered without knowing a format

Honest residual (not this card’s escape hatches to remove):

1. **Cron** — still a raw 5-/6-field expression by design (power-user escape hatch; no builder).
2. **IANA timezone names** — list + free-type help discovery, but inventing a non-existent zone still fails server-side (surfaced as ERROR).
3. **Interval** — still “every N seconds” (numeric), not “every 5 minutes” prose.
4. **Launch command** — absolute path / argv string as before.
5. **Slash dates with both parts ≤ 12** — accepted day-first with an explicit note; user must read the echo if they meant US month-first.

## Files touched (implementation)

| Path | Role |
|---|---|
| `ui/src/datetime-input.js` | Human date/time parse + echo |
| `ui/src/datetime-input.test.js` | Unit tests for formats |
| `ui/src/dialog-build.js` | Shared `buildInput` wire contract |
| `ui/src/dialog-build.test.js` | Wire contract tests (extended) |
| `ui/src/TimerDialog.svelte` | Pickers, chips, tz list, validation, focus, banners |
| `ui/src/styles.css` | Chip/tz/error/echo layout; dialog height |
| `scripts/capture_qa_p4d.py` | WebKitGTK evidence driver |
| `docs/QA_P4d.md` | This document |
| `docs/qa4-screenshots/p4d-*.png` | Screenshots + `.meta.json` |
| `docs/qa4-evidence/*` | Store/CLI dumps, capture log |

## Display recipe (reproduce)

```sh
set -euo pipefail
ROOT="$(pwd)"   # this worktree
cd "$ROOT"
cd ui && npm ci && npm run build && cd ..
cargo tauri build --no-bundle
cargo build -p bellman-cli --release

QA=/tmp/qa-p4d-session
rm -rf "$QA"
mkdir -p "$QA/share/io.bellman.desktop/logs" "$QA/share/io.bellman.desktop/slots" "$QA/config"
printf '%s\n' '{"wizard_completed":true,"autostart_enabled":false,"start_minimized":false,"wake_enabled":false}' \
  > "$QA/share/io.bellman.desktop/config.json"

export DISPLAY=:0
export XDG_DATA_HOME="$QA/share"
export XDG_CONFIG_HOME="$QA/config"
export GDK_BACKEND=x11
export BELLMAN_CLI="$ROOT/target/release/bellman"
export BELLMAN_QA_DATA="$QA/share/io.bellman.desktop"

"$ROOT/target/release/bellman-app" &
sleep 3
wmctrl -r Bellman -e 0,40,40,960,640 || true
python3 scripts/capture_qa_p4d.py
```

Note: a11y app name is **`bellman-app`** (P6 dual-binary), not `bellman`.
