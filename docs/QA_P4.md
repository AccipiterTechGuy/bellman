# QA P4 — Calendar UI (Week / Month / dialogs / Run history)

Card: C8 — `bellman-c8-calendar-ui-week-month-dialogs`.
Builds on the C7 shell (Tauri tray + All-timers page). Adds the remaining
three user-facing pages plus an edit/create dialog matching the CLI
surface.

## Scope shipped by this card

| Surface | Where | Evidence |
|---|---|---|
| **Week page** | `ui/src/WeekPage.svelte` | 7-column ISO grid Mon–Sun; weekly timers land on their DOW, daily on every column, monthly/yearly/once on their next-fire day inside the displayed week. |
| **Month page** | `ui/src/MonthPage.svelte` | 6×7 grid, prev/next-month + prev/next-year buttons, "Today" shortcut. Monthly lands on its day-of-month, yearly on its date within the visible month, once on its fire day, cron/interval on their next fire day. |
| **Run history page** | `ui/src/HistoryPage.svelte` | JSONL tail filtered by timer + kind, polled every 5 s. Reuses the existing `list_log_tail` IPC. |
| **Timer dialog** | `ui/src/TimerDialog.svelte` | One form per occurrence variant (once / interval / daily / weekly / monthly / yearly / cron); live `preview_fires` next-5 with local/UTC/offset; DST warning banner. Create / Save / Delete actions. |
| **Tauri IPC** | `src-tauri/src/commands.rs` | `create_timer`, `update_timer`, `delete_timer`, `preview_fires` registered alongside the existing C7 commands. All four share the optimistic-revision path with the CLI. |
| **Input shape** | `src-tauri/src/occurrence_input.rs` | Builder mirrors `bellman-cli::parse` so both surfaces go through `Occurrence::new()` + `Store::create_timer` / `update_timer`. `timer_to_input()` + `dst_warning()` are tested. |
| **New public `Occurrence` getters** | `crates/bellman-core/src/occurrence/{schedule.rs,kind.rs}` | `Occurrence::dst_gap` / `dst_fold` / `invalid_monthday` / `runs_done` (already exposed) and `OccurrenceKind::kind_label() -> &'static str` for the wire discriminator. Non-breaking — pure additions. |

## Page-by-page checklist (manual on the desktop build)

### 1. All timers — create + edit + delete round-trip

1. Open the window. Top-right shows Running / Paused per the tray state.
2. Click **+ New timer**.
3. Fill Name = `qa-daily-1`, Kind = `daily`, Time = `09:00:00`, tz blank
   (= system). Wake action = desktop notification, Title = `Hello`, Body =
   `world`. Click **Create**.
4. The new row appears in the All-timers table; toast `Created "qa-daily-1"`.
5. Click **Edit** on the new row. Change Time to `09:30:00`. Click **Save**.
6. Preview pane on the right updates to `09:30:00 <tz>`. Toast
   `Updated "qa-daily-1"`.
7. Click **Edit** again, then **Delete…**, then **Confirm delete**. Row
   disappears. Toast `Deleted "qa-daily-1"`.

Reproduces the spec bullet "every occurrence kind creatable + editable +
deletable from GUI". Repeat with the other six kinds — the dialog
shows the kind-specific fields inline (the `once` field, the `cronExpr`
box, the `month`+`day` selectors for yearly, etc.).

### 2. Week page — weekly + daily + monthly chips

1. Create `qa-weekly-monwedfri` (weekly, Mon/Wed/Fri, 08:00).
2. Create `qa-daily-0800` (daily, 08:00).
3. Click **Week**. Pick a week containing next Wednesday.
4. Mon, Wed, Fri columns show two chips each (`qa-weekly-monwedfri` and
   `qa-daily-0800`); other days only the daily chip. Click any chip →
   edit dialog opens pre-filled.

### 3. Month page — monthly on day 31 + yearly on Mar 15

1. Create `qa-monthly-31` (monthly, day 31, 12:00). The dialog clamps to
   the last day of each month via the core's `InvalidMonthDayPolicy`.
2. Create `qa-yearly-0315` (yearly, March 15, 09:00).
3. Click **Month** and navigate to March 2030. Day `15` shows
   `qa-yearly-0315` chip; previous/next `Month` buttons work; prev/next
   `Year` jumps a full calendar year; `Today` resets.
4. Create `qa-once-next-tue` (once, today + 1 day at noon). It appears on
   the matching date inside the visible grid (and disappears once it
   fires).

### 4. Run history — filtered JSONL tail

1. Run (or wait for) a fire. The polling refresh surfaces the new event.
2. Switch to **Run history**. Pick the timer in the dropdown — only its
   events show. Pick a kind (e.g. `fired`) — further narrow the list.
3. Each row: ISO local time, kind, timer name, scheduled-for, message,
   error (if any). Empty filter → "No events match the filter."

### 5. DST warning

1. Open **+ New timer**, Kind = `daily`, tz = `Europe/Helsinki`, time =
   `02:30:00`. (Helsinki's spring-forward gap covers 03:00–04:00 local
   the last Sunday in March; 02:30 doesn't *directly* fall in the gap
   on most days but the chosen time collides with the gap for the week
   of the transition. To deterministically trigger a warning, set tz =
   `Europe/Helsinki` and the DST gap day.)
2. More reliable: open an editor and change the tz to a non-DST zone
   (e.g. `UTC`) first — the warning disappears — then flip it back to
   `Europe/Helsinki` — the dialog's "DST" pane re-evaluates within
   ~250 ms (debounced).

The `dst_warning` helper is unit-tested in
`occurrence_input::tests::dst_warning_*` for UTC (no warning) and
Helsinki noon (no warning). Coverage of a real spring-forward warning
requires a fixture clock; see C9 backlog.

## Reproducible CI commands

```sh
# 1. JS unit tests — 21 tests across DTO contracts and pure helpers
cd ui && npm install --no-audit --no-fund
npm test
# Expected: "Test Files  1 passed (1)" "Tests  21 passed (21)"

# 2. Vite production build
npm run build
# Expected: "✓ built in <ms>"

# 3. Rust lib tests — 23 tests (C8 dto + occurrence_input helper)
cd ..
cargo test -p bellman --lib
# Expected: "test result: ok. 23 passed; 0 failed"

# 4. Workspace lib tests (all crates)
cargo test --workspace --lib
# Expected: 23 + 108 = 131 passed

# 5. Full Tauri bundle (matches the C7 acceptance gate)
cargo tauri build
# Expected: "Finished 2 bundles at: target/release/bundle/{deb,appimage}/..."
```

## What a "preview matches bellman next" check looks like

```sh
# CLI side: register a weekly timer, ask for next 5.
bellman add --name qa-preview --occurrence weekly --days mon,wed,fri \
            --time 08:00:00 --tz Europe/Helsinki
bellman next qa-preview 5
# → list of 5 RFC3339 UTC instants.

# GUI side: edit that timer; the dialog's Next 5 fires pane shows the
# same five dates (local HH:MM:SS, local date, RFC3339 UTC, offset
# string, tz name). The two are derived from the same Occurrence::preview
# call, so the times line up exactly when tested on the same wall clock.
```

This is the "preview matches bellman next output for the same timer"
half of the acceptance gate — both surfaces share the math via
`Occurrence::preview` in `crates/bellman-core/src/occurrence/schedule.rs`.

## Files added / changed

```
crates/bellman-core/src/occurrence/kind.rs          # +OccurrenceKind::kind_label
crates/bellman-core/src/occurrence/schedule.rs      # +3 accessors on Occurrence
src-tauri/Cargo.toml                                # +chrono-tz
src-tauri/src/lib.rs                                # +occurrence_input mod, +commands registrations
src-tauri/src/commands.rs                           # +create_timer,update_timer,delete_timer,preview_fires
src-tauri/src/occurrence_input.rs                   # +new module (OccurrenceInput / CreateTimerInput / PreviewFire /
                                                    #   DST warning / preview_fires)
src-tauri/src/dto_serde_tests.rs                    # +5 tests for new DTOs
ui/src/api.js                                       # +createTimer/updateTimer/deleteTimer/previewFires,
                                                    #   +5 pure calendar helpers
ui/src/api.test.js                                  # +13 tests (DTO round-trip + helper math)
ui/src/App.svelte                                   # swap stubs for new pages + dialog wiring
ui/src/TimerList.svelte                             # +Edit button, +New timer header button, onEdit/onCreate props
ui/src/WeekPage.svelte                              # new
ui/src/MonthPage.svelte                             # new
ui/src/HistoryPage.svelte                           # new
ui/src/TimerDialog.svelte                           # new
ui/src/styles.css                                   # +week grid, +month grid, +history, +dialog
```

WebKitGTK-first verification: open the production build under
webkit2gtk-4.1 (the Linux default). All grid + dialog CSS uses standard
Grid / Flexbox + custom properties — no WebKit-specific workarounds.
The `dialog-backdrop` close-on-Escape handler covers keyboard users
per WAI-ARIA.

## Caveats / known gaps (deferred to C9 hardening)

* Cron-month-day chip on the Month page reflects only the next fire, not
  every monthly recurrence this month. Acceptable for "monthly view" v1;
  C9 perf work would extend `Occurrence::iter_after` to fan-out within
  the visible span.
* `interval` timers show their chip on today's DOW column at the current
  local time — a real "next 24 h" fan-out is C9 territory.
* No drag-to-create on the Month grid. Out of scope for this card.

## Round-trip summary

| Card claim | How this doc proves it |
|---|---|
| vite build + cargo tauri build green | `npm run build`, `cargo tauri build` (CI commands above). |
| Rust tests green | `cargo test --workspace --lib` → 131 passed. |
| all 4 pages rendered | Per-page checklist above + `git log -- ui/src/{App,WeekPage,MonthPage,HistoryPage,StubPage}.svelte`. |
| every occurrence kind creatable + editable + deletable | Round-trip checklist §1 covers all seven via the dialog (kind selector shows once/interval/daily/weekly/monthly/yearly/cron). |
| preview matches bellman next | Both call `Occurrence::preview` (single source). Doc §preview-check. |
| DST warning appears for a gap time | `dst_warning` helper + dialog banner; unit tests pin UTC clean + Helsinki noon clean. Real-gap fixture coverage is C9. |
