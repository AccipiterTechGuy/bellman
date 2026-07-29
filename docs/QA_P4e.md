# QA P4e — Fire neighbours, list triage, calendar create

Card: train run `2026-07-28_0009`  
Slug: fire-neighbour collision awareness (timer dialog + All timers + calendar)  
Worktree: `.train-worktrees/2026-07-28_0009` · branch `train/2026-07-28_0009`

## Goal (one line)

Answer **"if I set this timer for this moment, what else is already going to fire then?"** — names, exact instants, collision vs nearby — without blocking Create/Save.

## Thresholds and caps (named constants)

Defined once in `src-tauri/src/neighbours.rs`:

| Constant | Value | Meaning |
|---|---|---|
| `NEIGHBOUR_COLLISION_TO_SECOND` | `true` | Same UTC **second** = **collision** |
| `NEIGHBOUR_WINDOW_SECS` | **300** (5 minutes) | Within this window, not same second = **nearby** (both directions) |
| `NEIGHBOUR_HORIZON_SECS` | **14 days** | Product bound for near-now scans / dialog hang guard |
| `NEIGHBOUR_MAX_FIRES_PER_TIMER` | **48** | Cap on `Occurrence::preview` expansions per store timer |

Implementation reuses `Occurrence::preview` (same path as `preview_fires` / `bellman next`). No second scheduler. Disabled timers are excluded. Optional `exclude_timer_id` skips the timer being edited.

### What the query does **NOT** catch

- Disabled timers (by design — they will not fire).
- Fires beyond the per-timer expansion cap (interval timers denser than 48 steps inside the candidate window).
- Runtime action concurrency / pile-up **execution** — this card only **warns**; capping concurrent actions is P7 (`max_concurrent_actions`). Not built here.
- Store schema, occurrence semantics, or schedule timing — read-only.
- Interval anchors / last_fired jitter on displayed `next_fire_utc` (display stays clean; jitter is execution-only).

## Product choices

### Nearby panel: always visible (not behind a button)

**Why:** The dialog’s purpose is to surface pile-ups while the user chooses a time. Hiding neighbours behind a click re-creates the blind spot this card removes. Always-visible panel under “Next 5 fires” keeps collision vs nearby in one glance; Create/Save remain enabled (information, not a veto).

### Collision vs DST advisory

| Banner | Left border | Label | Blocks Create? |
|---|---|---|---|
| Preview/server error | red | **Error** | yes |
| DST gap/fold | amber | **Advisory** | no |
| Same-second pile-up | amber + ⚠ COLLISION badge | **Collision** | **no** |

Pattern follows C8d: errors vs advisories are not colour-only (badge text + border).

### Event log affordance (All timers)

**Chose explicit `Log` button** on each row (not fold into Run history).  
**Why:** Per-timer JSONL tail is a quick inspect while triaging the list; Run history remains the global page. Row-click still toggles the same panel; `Log` makes the affordance visible without relying on cursor alone.

### Calendar create

Week empty cells and Month day cells open **New timer** with `occurrence.kind = once` and `onceDate` pre-filled to that day. Crowded days show a numeric fire-count badge; empty cells show “+ New”.

## Evidence ledger

All PNG evidence is **real WebKitGTK** via `scripts/capture_qa_p4e.py` (tauri-driver + WebKitWebDriver + Xlib GetImage on an isolated Xvfb). See `docs/QA_P4b.md`. No mocked harness, no Chromium, no hand-edited images.

Session data: `/tmp/qa-p4e-session/share/io.bellman.desktop`  
CLI: `/tmp/bellman-cli-p4e` (release `bellman` sidecar)

### Screenshots opened and described

| File | What is in it |
|---|---|
| `p4e-dialog-collision-names-three.png` | Fourth timer at **09:00:00 UTC** daily. **Also firing** column shows ⚠ COLLISION naming **qa-collide-alpha-backup**, **qa-collide-beta-launch-heavy-workload** (action `launch`), **qa-collide-gamma-notify** with times. Create still enabled. 960×640. |
| `p4e-dialog-collision-1280x800.png` | Same collision dialog at 1280×800 — names remain readable. |
| `p4e-dialog-nearby-not-collision.png` | Draft at **09:01:00** — no same-second ⚠ in table; **Nearby** lists **alpha / beta (launch) / gamma** at −60s (and scroll for +120s nearby-two-min). Distinct from collision badge. |
| `p4e-dialog-no-collision.png` | Draft at **15:30:00** — green “✓ No other timers fire at or near these instants (within ±5 min).” Panel not blank. |
| `p4e-dialog-collision-50plus.png` | Same 09:00 collision after **≥50** store timers (CLI bulk seed). Dialog still responsive. |
| `p4e-list-sort-next-fire.png` | All timers: **Sort = Next fire (default)**; Density column ⚠ +2 with peer names; **Log** button visible. |
| `p4e-list-filter-search.png` | Search `qa-collide` → **3 of 6** timers; non-matching rows hidden. |
| `p4e-list-long-name-readable.png` | Deliberately long timer name fully readable (wraps; no silent ellipsis). |
| `p4e-week-day-counts.png` | Week grid with day fire counts and empty-day **+ New**. |
| `p4e-week-create-prefill.png` | Click empty week day → New timer **once**, date pre-filled, nearby panel present. |
| `p4e-month-fire-counts.png` | Month cells show **numeric fire-count badges** + **+ Add** per day; crowded chrome. |
| `p4e-month-create-prefill.png` | Click day → dialog **once** with date **2026-08-15** pre-filled (echo Saturday 15 August…). |
| `p4e-month-create-dialog.png` | Named `qa-from-month-cell-aug` before Create. |
| `p4e-list-after-month-create.png` | Filter shows store row `qa-from-month-cell-aug` once @ 2026-08-15; toast “Created…”. |

### CLI ↔ dialog instant parity (cannot fake)

Dialog collision rows show local/UTC **2026-07-29 09:00:00 / 2026-07-29T09:00:00Z**.

`bellman next --json` for each named timer (committed under `docs/qa4-evidence/`):

| Timer | First next fire |
|---|---|
| qa-collide-alpha-backup | `2026-07-29T09:00:00+00:00` |
| qa-collide-beta-launch-heavy-workload | `2026-07-29T09:00:00+00:00` |
| qa-collide-gamma-notify | `2026-07-29T09:00:00+00:00` |
| qa-nearby-two-min | `2026-07-29T09:02:00+00:00` |

Files: `bellman-next-qa-collide-*.json`, `collision-cli-parity.json`, `store-after-collision-create.json`, `store-final.json`.

### Bounded work (≥50 timers)

- After GUI demo + CLI seed + month creates: **58** timers in store (`store-final.json`).
- Caps: window 300s, horizon 14d, max 48 fires/timer.
- Dialog neighbour refresh wait in capture meta ~1.8–2.0s wall (includes debounce + IPC); no hang observed with 50+ timers.
- Unit tests cover classification + expansion without Tauri (`neighbours::*`, 10 tests).

### Builds / tests run on this card

```text
cargo test --workspace --lib          # green (incl. neighbours::* 10 tests)
cargo test -p bellman-app --lib neighbours
cd ui && npm ci && npm test && npm run build
cargo tauri build --no-bundle         # green → target/release/bellman-app
```

## Files touched (summary)

| Area | Paths |
|---|---|
| Neighbour query | `src-tauri/src/neighbours.rs`, `commands.rs` (`query_neighbours`), `lib.rs` |
| Dialog | `ui/src/TimerDialog.svelte`, `api.js` |
| List triage | `ui/src/TimerList.svelte` |
| Calendar create | `ui/src/WeekPage.svelte`, `MonthPage.svelte`, `App.svelte` |
| CSS | `ui/src/styles.css` (no silent name ellipsis; collision/nearby/list toolbar) |
| QA | `docs/QA_P4e.md`, `scripts/capture_qa_p4e.py`, `docs/qa4-screenshots/p4e-*`, `docs/qa4-evidence/*` |

## Hard limits respected

- No scheduler / store-schema / occurrence-semantics changes.
- No concurrency limiting of actions.
- No visual token/theme work (C10b).
- No new runtime dependency / CSS framework.
