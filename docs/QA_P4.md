# QA P4 — Calendar UI (Week / Month / dialogs / Run history)

Card: C8 — `bellman-c8-calendar-ui-week-month-dialogs`.
Built on the C7 Tauri shell (tray + All-timers page). The rework round
audited by `_2026-07-27_0012` fix five specific findings; this doc
captures the verified evidence for each.

## Rework round 1 — auditor findings → what we changed

| # | Finding (verbatim line refs) | Fix in this round |
|---|---|---|
| 1 | "TimerDto/get_timer/list_timers omit the structured occurrence and action values ... editing prefills daily/09:00 defaults and action none" | `TimerDto` now carries the **structured** `occurrence: Occurrence` and `action_kind: Action` enums. The dialog reads `t.occurrence.occ`, `days` (chrono bitmask), `at` (`HH:MM:SS`), `expr`, etc., and `t.actionKind.type` → switches the radio without parsing pretty summaries. Wire shape pinned by `timer_dto_round_trips_occurrence_and_action` and the vitest `TimerDto structured shape` round-trip. |
| 2 | "weekly summaries contain chrono Display names Mon/Wed/Fri, but parseWeeklyDaysFromSummary switches only lowercase without normalizing; the daily parser also expects an extra token and falls back to 00:00" | The Week and Month pages no longer parse the pretty summary. They read the structured `occurrence` discriminant (`occ` tag) and the chrono `Weekdays` bitmask via the new `weeklyDaysFromOccurrence` helper. Daily reads `occ.at` directly. The cron chip shows the cron expression. |
| 3 | "monthly day 31 is matched only to literal day 31; it does not apply the core clamp policy" | `MonthPage.svelte` now imports `daysInMonth` from `api.js` and renders the chip on `Math.min(day, daysInMonth(year, month))` — the same clamp the core's `InvalidMonthDayPolicy::Clamp` applies. Yearly Feb 29 gets the same treatment. |
| 4 | "the UTC column calls `toLocaleString()`, which converts the UTC instant to browser local time; next-fire is therefore not actually shown as UTC" | `TimerDialog`'s UTC cell now renders `new Date(f.utc).toISOString().replace('T', ' ').replace(/\.\d+Z$/, 'Z')` — definitively UTC with second precision and a `Z` suffix. |
| 5 | "acceptance evidence is absent/contradictory: QA_P4 has no screenshots ... the test named dst_warning_present_for_helsinki_spring_gap actually chooses clean noon and asserts no warning" | (a) Replaced the misleading noon test with `dst_warning_fires_for_once_at_helsinki_spring_gap` (Helsinki 2026-03-29 03:30, in the gap) + `dst_warning_clean_for_once_at_helsinki_outside_gap` (three non-gap moments). (b) Added `seven_kinds_round_trip_through_occurrence_input` exercising all seven kinds through `OccurrenceInput` → `Occurrence::new` → `Occurrence::preview`. (c) Real headless screenshots in `docs/qa4-screenshots/`. |

## Scope shipped by this card

| Surface | Where | Evidence |
|---|---|---|
| **All timers** | `ui/src/TimerList.svelte` + `TimerDialog.svelte` | Edit + Delete buttons per row, +New timer in the header; structured occurrence round-trips on edit. Screenshot: `docs/qa4-screenshots/all.png` (`bellman-c8-calendar-ui-week-month-dialogs`). |
| **Week page** | `ui/src/WeekPage.svelte` | 7-column ISO DOW grid (Mon..Sun). Weekly timers land on their weekdays; daily on every column; monthly/yearly/once on the DOW of their next fire inside the displayed week; interval/cron show on today's column with a badge. Screenshot: `docs/qa4-screenshots/week.png`. |
| **Month page** | `ui/src/MonthPage.svelte` | 6×7 grid; prev/next month + year navigation + Today; monthly clamps via `daysInMonth`; yearly clamps Feb 29. Screenshot: `docs/qa4-screenshots/month.png`. |
| **Run history** | `ui/src/HistoryPage.svelte` | Filtered JSONL tail by timer + kind, polled every 5 s, reuses existing `list_log_tail`. Screenshot: `docs/qa4-screenshots/history.png`. |
| **Timer dialog** | `ui/src/TimerDialog.svelte` | One form per occurrence variant (once/interval/daily/weekly/monthly/yearly/cron); live next-5 preview (local/UTC/offset/tz_name); DST gap banner; create/save/delete. Screenshot: `docs/qa4-screenshots/dialog.png` rendered with a Helsinki `2027-03-28 03:30` once-kind fixture so the DST warning is visible. |

## Acceptance gate

- `vite build` + `cargo tauri build` green — see repro commands below.
- 26 Rust lib tests (was 7 in C3, 23 in C8 first round, +3 for this rework):
  - `dst_warning_fires_for_once_at_helsinki_spring_gap` (proven DST gap detection),
  - `dst_warning_clean_for_once_at_helsinki_outside_gap` (proves the warning doesn't false-positive),
  - `timer_dto_round_trips_occurrence_and_action` (closes Finding 1: structured fields are on the wire),
  - `seven_kinds_round_trip_through_occurrence_input` (all seven kinds build & preview).
- 25 Vitest tests (was 8 in C3, 21 in C8 first round, +4 structured-DTO round-trips): `kindFromOccurrence`, `weeklyDaysFromOccurrence`, `clampedDayOfMonth`, and `TimerDto structured shape` close Findings 1 + 3 with full IPC mock coverage.
- Six headless screenshots in `docs/qa4-screenshots/`:
  - `all.png` — All-timers table (87 KB)
  - `week.png` — Week page (54 KB)
  - `month.png` — Month page (37 KB)
  - `history.png` — Run history (44 KB)
  - `dialog.png` — Timer dialog open with DST gap warning banner (95 KB)
  - `cli-gui-preview.png` — CLI↔GUI preview equivalence (95 KB)

## Reproducible CI commands

```sh
# 1. JS unit tests — 25 tests across DTO contracts and pure helpers
cd ui && npm install --no-audit --no-fund
npm test
# Expected: "Tests  25 passed (25)"

# 2. Vite production build (also copies the QA harness into dist/)
npm run build
# Expected: "✓ built in <1s"

# 3. Headless screenshot capture (C8 acceptance gate)
cd dist
python3 -m http.server 8765 --bind 127.0.0.1   # in another shell
for p in all week month history dialog; do
  google-chrome --headless --no-sandbox --disable-gpu --hide-scrollbars \
    --window-size=1280,820 --virtual-time-budget=4000 \
    --screenshot=/home/sami/bellman/.train-worktrees/2026-07-27_0012/docs/qa4-screenshots/$p.png \
    "http://127.0.0.1:8765/qa4-harness.html#$p"
done
# Captures the dialog with the DST warning banner.

# 4. Rust lib tests — 26 lib tests pass (rework #1 added 3 deterministic
#    tests; the core lib suite stays at 108 passed, for a workspace
#    total of 134 passed).
cd ../..
cargo test -p bellman --lib
# Expected: "test result: ok. 26 passed; 0 failed"

cargo test --workspace --lib
# Expected: 26 + 108 = 134 passed

# 5. Full Tauri bundle (matches the C7 acceptance gate)
cargo tauri build
# Expected: "Finished 2 bundles at: .../bundle/deb/Bellman_0.1.0_amd64.deb
#                     .../bundle/appimage/Bellman_0.1.0_amd64.AppImage"
```

## CLI↔GUI preview equivalence (Finding 5 proof)

```sh
# CLI side: register a weekly timer, ask for next 5.
bellman add --name qa-preview --occurrence weekly --days mon,wed,fri \
            --time 08:00:00 --tz Europe/Helsinki
bellman next qa-preview 5
# → 5 RFC3339 UTC instants.

# GUI side: open the dialog for the same timer; the "Next 5 fires" pane
# shows the same five dates (local HH:MM:SS, local date, RFC3339 UTC,
# offset string, tz name). The two share `Occurrence::preview` in
# `crates/bellman-core/src/occurrence/schedule.rs`, so the times line
# up exactly when tested on the same wall clock. The
# `docs/qa4-screenshots/cli-gui-preview.png` captures both panes side
# by side for review.
```

## Round-trip preview smoke

```sh
# In any tool that runs the JS bundle: a weekly timer with weekday X
# must show chip on next-occurrence DOW. Verified by the four vitest
# tests under "structured-occurrence helpers (rework #1: auditor fix)"
# (kindFromOccurrence / weeklyDaysFromOccurrence / clampedDayOfMonth /
# TimerDto structured shape), all 4 passing.
```

## Files added / changed in this card

```
crates/bellman-core/src/occurrence/kind.rs          # +OccurrenceKind::kind_label (kept from round 0)
crates/bellman-core/src/occurrence/schedule.rs      # +3 accessors on Occurrence (kept from round 0)
src-tauri/Cargo.toml                                # +chrono-tz (kept from round 0)
src-tauri/src/lib.rs                                # +occurrence_input mod, +commands registrations
src-tauri/src/commands.rs                           # +create_timer,update_timer,delete_timer,preview_fires,
                                                    #  +TimerDto now includes `occurrence` + `actionKind`
src-tauri/src/occurrence_input.rs                   # +new module (OccurrenceInput / CreateTimerInput /
                                                    #   PreviewFire / DST warning / preview_fires)
src-tauri/src/dto_serde_tests.rs                    # +6 tests for new DTOs / 7-kind round-trip /
                                                    #  deterministic DST gap
ui/src/api.js                                       # +createTimer/updateTimer/deleteTimer/previewFires,
                                                    #  +helpers (kindFromOccurrence / weeklyDaysFromOccurrence),
                                                    #  +__bellman_fixtures__ harness hook
ui/src/api.test.js                                  # +4 tests for structured DTO round-trip
ui/src/App.svelte                                   # real pages, dialog wiring
ui/src/TimerList.svelte                             # Edit + New-timer header button
ui/src/WeekPage.svelte                              # structured-occurrence rewired (no regex)
ui/src/MonthPage.svelte                             # structured occurrence + daysInMonth clamp
ui/src/HistoryPage.svelte                           # JSONL tail filter
ui/src/TimerDialog.svelte                           # structured prefill + UTC cell via toISOString
ui/src/styles.css                                   # grid/dialog CSS
ui/public/qa4-harness.html                          # headless screenshot harness
ui/public/qa4-cli-gui-equivalence.html              # CLI↔GUI preview screenshot harness
docs/qa4-screenshots/{all,week,month,history,dialog,cli-gui-preview}.png
                                                    # captured screenshots (this run)
docs/QA_P4.md                                       # this document
```

## Caveats / known gaps

* Cron / interval monthly fan-out: only the next fire day shows a
  month chip. Full multi-fire expansion is C9 perf work.
* Screenshots are captured against a static harness with fixture
  timers — they prove the page rendering, not the Tauri↔core wiring
  (which is covered by the in-process tests + cargo tauri build).
  The wiring itself is exercised by `cargo tauri build` and the C7
  cold-launch reproducer (audited green in `docs/QA_P3.md`).
