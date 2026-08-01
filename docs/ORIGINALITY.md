# Originality — Bellman vs the projects it was read against

`docs/BUILD_PLAN.md` names seven projects as **prior art we read, not sources
we copy from**. Copies of all seven are kept outside this repository for
offline reading during development; nothing from any of them is vendored,
imported or pasted in. This document records the check.

Scope: every Rust module in `crates/` and `src-tauri/src/`, the Svelte
frontend under `ui/src/`, both demo apps under `testing_apps/`, and
`helpers/macos-wake-daemon/`. **Verdict for every one of them: original.**
Nothing was found close enough to need a rewrite.

Where we depend on someone else's work we do it properly, as a declared
dependency with its licence — `croner`, `notify`, `rusqlite`, `chrono`,
`chrono-tz`, `tokio`, `serde`, `tracing`, `clap`, `resvg`, Tauri and its
plugins are all in `Cargo.toml` and are used as libraries, never copied.

## How this was checked (2026-08-02, C11)

A mechanical sweep compared every Bellman source file against every source
file in the seven clones — 5.5 M characters of reference code — on four
independent axes. It is reproducible from
[`docs/qa-c11/originality_sweep.py`](qa-c11/originality_sweep.py); the raw
output is [`docs/qa-c11/originality.json`](qa-c11/originality.json).

| probe | what it looks for |
|---|---|
| **shingle** | every 6-line window of comment-stripped, whitespace-normalised code, hashed, intersected with the same windows from all seven repos — the probe that catches pasted code even after renaming |
| **identifier** | declared `fn` / `struct` / `enum` / `trait` / `const` / `type` names (≥ 6 chars) that also exist in a reference repo |
| **literal** | string literals ≥ 16 chars appearing verbatim in a reference repo |
| **comment** | comment lines ≥ 40 chars appearing verbatim in a reference repo |

### What the sweep found

**Shingles: 9 hits across ~51 000 lines, every one of them noise.**

- 7 of the 9 are one block in `crates/bellman-core/src/calendar/types.rs`:
  the list `"January", "February", …`, which also appears in croner's
  `describe/lang/english.rs`. Two programs that print English month names in
  order will always collide here; there is no other way to spell them.
- The other 2 (`actions/executor.rs`, `slots/service.rs`) are runs of six
  closing braces, matching a brace run in `notify/src/kqueue.rs`.

No shingle matched anything that carries logic. That is the strongest single
signal in this document: not one 6-line window of Bellman logic exists in any
of the seven projects.

**Identifiers: 30 hits, all generic Rust or generic domain vocabulary** —
`as_str`, `from_str`, `partial_cmp`, `contains`, `resolve`, `advance`,
`shutdown`, `rescan`, `Action`, `Target`, `Request`, `AppState`, `Commands`,
`Scheduler`, `DEFAULT_TIMEOUT`, `iter_after`, `describe_month`,
`describe_time`, `bucket`, `expected`. These are the words the language and
the problem domain supply; sharing them is not derivation. Nothing
distinctive to a reference project (no `CronPattern`, no `PomodoroState`, no
`KAAlarm`, no `EventLoop` internals) appears in Bellman.

**Literals: 90 hits, all fragments of `format!`/`assert!` punctuation** —
`",\n    "`, `")\n        .expect("`, `": {\n            "` and similar. The
probe deliberately keeps short punctuation strings so nothing can hide behind
them; every hit is layout, not content. One real literal matched: the
`strftime` format `%Y-%m-%d %H:%M:%S` (also in kalarm), which is ISO 8601.

**Comments: 35 hits, of which 34 are the probe mis-reading `#[derive(…)]`
attribute lines and `// ─────` separator rules as comments.**

The one genuine comment match is worth naming:

- `src-tauri/src/main.rs` line 1 —
  `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`,
  which also appears in pomodorolm's `main.rs`. This is the line
  `create-tauri-app` writes into every Tauri project's `main.rs`; it is
  Tauri's own scaffold, present in thousands of unrelated repositories, and
  it is not optional (without it a release build on Windows opens a console
  window). Kept.

## Per-module verdicts

The relationship column says what we read the project **for**, and where our
design deliberately diverges. Shingle counts are from the sweep above.

### `crates/bellman-core`

| module | lines | closest prior art | relationship | verdict |
|---|---|---|---|---|
| `occurrence/` | 1 448 | croner-rust; kalarm's alarm model | croner is a **dependency**, used through its public API for the cron kind only. The other six kinds, `next_fire(after)`, the DST gap/fold policies and the invalid-month-day clamp are ours; kalarm was read for *how much* per-alarm nuance a mature scheduler needs, not for how to compute it (and kalarm is C++/KDE, structurally unrelated). | **original** — 0 shingles |
| `scheduler/` | 3 736 | tokio-cron-scheduler | BUILD_PLAN's instruction was "read, don't depend". We kept the idea of a tokio-driven loop and replaced its all-in-memory model with a horizon window: `BinaryHeap<Reverse<(fire_at, TimerId)>>`, ≤30 s chunked sleeps, a wall-vs-monotonic jump detector, `PRAGMA data_version` external-writer probe, and a misfire pass at boot and on every detected wake. None of those exist upstream. | **original** — 0 shingles |
| `store/` | 4 397 | rusqlite | rusqlite is a **dependency**. Schema, WAL/`synchronous=FULL` pragmas, the timers/runs/slot_requests/meta tables, the claim ledger and optimistic revision are ours. | **original** — 0 shingles |
| `events/` | 2 316 | Cronicle's job log | Cronicle is Node.js and was read for job-scheduler *UX* (per-job catch-up, overlap limits, the live log tail). Our JSONL appender, elected single publisher with `fdatasync`, ISO-week gzip rotation, tolerant reader and two-stage retention (age, then byte budget) share no code and no structure with it. | **original** — 0 shingles |
| `slots/` | 3 487 | notify + notify-debouncer-full | notify is a **dependency** used for watch hints. The `free/work/done/bad/fires` lifecycle, claim-by-rename, `request_id` idempotency, the ≥5 replenisher, the orphan sweep and quarantine are ours. The two shingle hits here are closing braces. | **original** — 2 noise shingles |
| `reply/` | 5 697 | — | No prior art: the per-run reply channel, pickup grace, the opt-in silence watchdog, reply accumulation, revision-after-`no_ack`, `superseded`, and quarantine-with-live-file-intact were designed for this product. | **original** — 0 shingles |
| `ipc/` | 1 361 | — | Newline-delimited JSON over a 0600 Unix socket / named pipe, one server for all timers, claim-then-stream. Written here. | **original** — 0 shingles |
| `actions/` | 1 888 | Cronicle (vocabulary), tokio-cron-scheduler | Overlap policy names and retry vocabulary follow the industry consensus (APScheduler / systemd / Windows Task Scheduler wording, cited in BUILD_PLAN). The bounded dispatcher, lane model and `ActionLimiter` are ours. | **original** — 1 noise shingle |
| `pruner/` | 1 205 | systemd timer vocabulary | `Persistent`/`AccuracySec` supplied vocabulary only. | **original** — 0 shingles |
| `platform/wake/` | 2 293 | — | timerfd `CLOCK_REALTIME_ALARM` (Linux), `SetWaitableTimer` (Windows), an XPC helper (macOS) are OS APIs, not anyone's source. The probe decision trees and the single-next-wake election are ours. | **original** — 0 shingles |
| `tree/` | 1 744 | — | The per-timer folder (`timer.json` / `status.json` / `reply-<run_id>.json`) and the R10 fire transaction. Written here. | **original** — 0 shingles |
| `calendar/` | 3 134 | zeit; FullCalendar (not used) | zeit is a C++/Qt cron GUI, read to see what a thin wrapper looks like. FullCalendar was evaluated for the month grid and **not adopted** — the SVG renderer is hand-written. The 7 shingle hits are the English month-name list. | **original** — 7 noise shingles |
| `visible/` | 3 886 | — | The machine-wide schedule inventory (crontab, cron.d, run-parts, anacron, systemd timers, `at`) reads system formats; the parsers, the safety model (read-free, mutate only with `--apply`, never `sudo`) and the explainer are ours. | **original** — 0 shingles |
| `service/`, `app_config.rs`, `lib.rs` | 711 | — | Glue. | **original** — 0 shingles |

### The rest of the workspace

| component | lines | prior art | verdict |
|---|---|---|---|
| `crates/bellman-cli` | 2 541 | — | clap is a dependency; the command surface, the stable `--json` envelope and the error-code vocabulary are ours. **original** — 0 shingles |
| `src-tauri` (`bellman-app`) | 4 905 | pomodorolm; Tauri docs | pomodorolm is the closest existing shape (a small Tauri tray timer) and was read for tray + window lifecycle. The only shared line is Tauri's own `windows_subsystem` scaffold attribute (see above). Tray, single-instance, autostart, wizard, Settings and every command are ours. **original** — 0 shingles |
| `ui/` (Svelte 5) | 5 454 | super-productivity (layout inspiration) | No JS/TS was taken; super-productivity is Angular. **original** — 0 shingles |
| `testing_apps/lightbulb` | 106 | — | **original** — 0 shingles |
| `testing_apps/lightbulb_gui` | 659 | — | stdlib tkinter, written here. **original** — 0 shingles |
| `helpers/macos-wake-daemon` | 427 | — | SMAppService + XPC are Apple APIs. **original** — 0 shingles |
| `crates/bellman-core/tests`, `examples/` | 3 567 | — | **original** — 0 shingles |

## What was rewritten

**Nothing.** No module was found close enough to a reference project to
require rewriting. This is the expected outcome of the BUILD_PLAN rule that
each module be written after reading the *idea*, and it is what the sweep
independently confirms: zero logic-bearing shingles shared with 5.5 M
characters of prior art.

The one deliberate retention is `src-tauri/src/main.rs`'s
`windows_subsystem` attribute, which is Tauri scaffold rather than
pomodorolm's work.

## Licence position

Bellman is MIT (`LICENSE`). Every third-party dependency is declared in
`Cargo.toml` / `ui/package-lock.json` and used as a library. None of the seven
read-only reference projects is a dependency, and none of their code is
present in this tree, so none of their licences (GPL-2.0+ for kalarm, others
various) attaches to it.
