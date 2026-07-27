# Research topic: "Time Watcher" — cross-platform task scheduler desktop app

We are about to build **Time Watcher**: a Windows/macOS/Linux **task scheduler**
(desktop cousin of cron / Windows Task Scheduler). It installs like a normal desktop
application (launcher entry + tray), lets a user or an AI CLI register timers, and
wakes up other applications at scheduled times through a file-based JSON "slot"
connection layer. Full requirements: `agent_terminal_suite/docs/TIME_WATCHER_PLAN.md`
(read it first — it is the spec you are researching FOR).

Key constraints to respect in every recommendation:
- Timers have name + time + occurrence (once/interval/daily/weekly/monthly/yearly),
  second-level resolution, year-round calendar with automatic new-year recalibration.
- Two drive modes: an AI/CLI interface and a manual GUI (events list page, weekly
  repeats page, monthly calendar page; timers editable).
- Integration layer: pre-generated pairs of input/output JSON slot files; an external
  app integrates by writing one input JSON; empty slot pairs are auto-replenished.
- Events append to JSONL with weekly auto-pruning + removal of elapsed one-shots.
- Memory-smart: only near-horizon timers stay resident (min-heap window); a
  once-a-day timer must not sit hot in memory.

# Research questions (each researcher covers ALL, independently)
1. **Framework/stack**: best cross-platform choice for a small always-on tray app
   with calendar UI (Tauri, Electron, Python+Qt/PySide, Go+Wails/Fyne, .NET MAUI,
   Flutter…) — judge on idle RAM/CPU, packaging (msi/exe, dmg, deb/AppImage),
   autostart-on-login per OS, and single-codebase maintainability. Recommend ONE.
2. **Scheduler core**: cron expressions vs iCal RRULE for occurrences; timer wheel
   vs min-heap next-wake design; DST/timezone-safe local-time firing; misfire/
   catch-up policies; proven libraries in the recommended stack.
3. **JSON slot IPC**: robust file-based IPC patterns (atomic write+rename, dir
   watching, schema/versioning, slot lifecycle + replenishment); how OS-native
   schedulers (systemd timers, launchd, Windows Task Scheduler) wake apps and
   whether to wrap them or fire ourselves.
4. **Storage/pruning**: JSONL event-log layout, weekly prune design, elapsed-timer
   cleanup, crash-safe persistence of the timer table.
5. **Prior art**: existing open-source cross-platform scheduler apps — strengths,
   mistakes to avoid, plus any per-timer settings our spec is still missing.

# Deliverable (each researcher: research.md in YOUR OWN folder)
- A concrete recommendation per question (pick, don't just enumerate), with
  trade-off notes and citations/links.
- A proposed minimal architecture sketch (components + data files) for v1.
- A "risks & gotchas" list (DST, sleep/hibernate wakeup, per-OS autostart quirks).

# Acceptance
- research.md answers all 5 questions with explicit recommendations.
- Synthesizer: merge into synthesis.md — one recommended stack + architecture,
  a disagreement table where researchers diverged, and a v1 build-phase list.
