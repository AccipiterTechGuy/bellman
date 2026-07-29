# Calendar Snapshot — render any month as a clean calendar image

Repo: `~/bellman`

## Goal

Ask Bellman for a month and get back a readable calendar image showing what is scheduled:
task, time, repeat, status. Answers "show me August", "what runs next Tuesday",
"snapshot September" — as an image a human can glance at and as JSON an agent can read.

## Rendering — decide this correctly up front

**Render in Rust, straight to SVG, then rasterise to PNG. Do NOT render through the
webview.**

Driving the GUI to screenshot a calendar would make this feature depend on a working
display, a GPU and a running window — and the GUI capture path on this machine is
currently broken for exactly those reasons (see `docs/todo/qa_isolated_display_no_input_hijack.md`).
A pure-Rust renderer is headless, deterministic, fast, and testable as text.

SVG first because it is plain text: golden-file tests diff it directly, and layout bugs
are readable in the diff. PNG is a rasterisation step on top (`resvg`/`tiny-skia` or
equivalent — pick one and justify it in the PR).

## Core behaviour

- Input: an explicit month (`2026-08`), or a date range.
- Output: `svg`, `png`, or `json` — same data, three shapes. The JSON is the contract;
  the images are views of it.
- Grid: correct weekday alignment, correct day count, leading/trailing days of adjacent
  months greyed. Week-start configurable (Mon default, Europe).
- Per day cell: each task's time, short name, a repeat marker, and a status colour
  (upcoming / ok / failed / disabled / unknown).
- Overflow: cap items drawn per cell and render `+N more` rather than overlapping text.
- Header shows the month, year and the **timezone** the times are in.
- Empty month renders a valid empty calendar, not an error.

## Data source

Bellman's own timers on day one. When the Visible Scheduler card lands
(`docs/todo/visible_scheduler.md`), cron / systemd / at rows appear in the same calendar
for free — so read through a source-agnostic query, not directly off the timer table.

## Natural language — draw the line here

Do **not** build a natural-language engine. Bellman accepts structured flags plus a small
fixed set of relative phrases: `today`, `tomorrow`, `this month`, `next month`,
`next <weekday>`, and a bare month name. Anything richer is the calling agent's job to
translate into flags. State this boundary in the docs so it does not creep.

## CLI / API sketch

```
bellman calendar --month 2026-08 [--format svg|png|json] [--out PATH]
bellman calendar --from 2026-08-01 --to 2026-08-14 --format json
bellman calendar --month next --week-start mon --tz Europe/Helsinki
bellman agenda "next tuesday" --json
```

Writing to stdout when `--out` is absent (SVG/JSON) keeps it pipeable.

## Safety / privacy rules

- **Commands are hidden by default.** A cell shows the task *name*; full command lines can
  contain tokens, paths and private details, and this output is an image people paste into
  chats. `--show-commands` is opt-in and documented as such.
- **Deterministic output.** Same input ⇒ byte-identical SVG. No render timestamp, no
  random ids, no locale-dependent formatting baked in. This is what makes it testable.
- **No display required.** Must work with `DISPLAY` unset, over SSH, with no GPU. Prove it
  in a test that unsets the variable.
- **Bounded.** Cap items per cell, cap total tasks considered, cap render time. A month
  with 5,000 entries must produce a sane image quickly, not hang.
- Writes only to the path given, and never outside the data dir without `--out`.

## Acceptance

- `--month 2026-08 --format svg` run twice produces byte-identical output.
- Renders correctly with `DISPLAY` unset (test asserts this explicitly).
- Correct grids for: a month starting Sunday, a month starting Monday, February 2028
  (leap), and a 31-day month spanning six calendar weeks.
- A DST-transition month shows the correct local times either side of the change.
- A day holding 20 tasks renders `+N more` with no overlapping or clipped text.
- Zero-task month renders a valid, complete empty grid.
- JSON output and SVG output describe the same set of tasks — asserted in a test, not by eye.
- Commands absent from output unless `--show-commands` is passed.
- PNG is produced from the same SVG, and both land at the requested `--out` path.
