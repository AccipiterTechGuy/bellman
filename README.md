# Bellman

Cross-platform (Windows / macOS / Linux) **task scheduler** desktop app — the
desktop cousin of cron. Named after the bellmen (knocker-uppers) who woke
people before alarm clocks existed: Bellman's job is waking *applications*.

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

## Status

Planning / pre-build. See [docs/PLAN.md](docs/PLAN.md) for the full
specification and decided logic, and
[docs/research/synthesis.md](docs/research/synthesis.md) for the four-way
independent research synthesis behind the stack choice (Tauri v2 + Rust core).
