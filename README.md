# Bellman

> ## 🚧 Work in progress — not ready to use
>
> Bellman is **under active construction and is not usable yet.** There is no
> release, no installer, and no working application to run — the GUI, the CLI and
> the packaging are still being built. Nothing here is stable: APIs, file formats,
> the database schema and the slot protocol can all change without notice, and the
> `main` branch is not guaranteed to build at any given moment.
>
> The repository is public so the work can be followed in the open, **not** because
> it is ready to install. Please don't file bugs about missing features yet.
> Progress is tracked in [docs/BUILD_PLAN.md](docs/BUILD_PLAN.md).

Cross-platform (Windows / macOS / Linux) **task scheduler** desktop app — the
desktop cousin of cron. Named after the bellmen (knocker-uppers) who woke
people before alarm clocks existed: Bellman's job is waking *applications*.

**Everything below describes the intended v1 design, not what exists today.**

- Timers with name, time, and occurrence (once / interval / daily / weekly /
  monthly / yearly), second-level resolution, year-round calendar with automatic
  new-year recalibration.
- Two drive modes: a `bellman` CLI (AI-skill friendly) and a GUI with three
  pages — events list with next-fire times, weekly repeats, monthly calendar.
- **JSON slot-pair integration layer**: external apps register, modify, or
  delete their own wake-up timers by writing one JSON file; ≥5 empty slot pairs
  are always kept ready and auto-replenished.
- JSONL event log with weekly pruning; memory-smart core — only near-horizon
  timers stay resident (min-heap window).

## Your data stays yours

Bellman keeps every timer, log and slot in a per-OS data directory (`~/.bellman/` on
Linux) — **never in this repository**. Cloning the code tells nobody what you schedule.
See [docs/LOCAL.md](docs/LOCAL.md) for the data-dir layout and the ignored patterns for
keeping private integrations out of git.

## Status — what actually exists today

| phase | state |
|---|---|
| Occurrence engine (once/interval/daily/weekly/monthly/yearly/cron, DST + clamp policies) | ✅ built |
| SQLite store — timers / runs / claim ledger, WAL | ✅ built |
| Scheduler — horizon heap, chunked sleeps, clock-jump detector, misfire pass | ✅ built |
| CLI (`bellman add\|list\|edit\|rm\|next\|run-now\|pause`, `--json`) | ✅ built |
| Slot IPC layer + JSONL event log | ✅ built |
| Tauri shell + tray | 🚧 in progress |
| Calendar UI (week / month) | 🚧 in progress |
| Pruner, hardening, perf gates | ⬜ not started |
| Packaging (dmg / MSI / deb / AppImage) | ⬜ not started |
| Wake-from-sleep (RTC) + Settings + first-run wizard | ⬜ not started |
| Full-system validation | ⬜ not started |

**No installable build exists yet.** The core library and its test suite run; there is
nothing an end user can install or launch.

See [docs/PLAN.md](docs/PLAN.md) for the full specification and decided logic,
[docs/BUILD_PLAN.md](docs/BUILD_PLAN.md) for the phased build,
[docs/research/synthesis.md](docs/research/synthesis.md) for the four-way independent
research synthesis behind the stack choice (Tauri v2 + Rust core), and
[docs/research/rtc_wake_synthesis.md](docs/research/rtc_wake_synthesis.md) for the
per-OS wake-from-sleep design.
