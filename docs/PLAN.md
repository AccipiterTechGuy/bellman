# Bellman — cross-platform task scheduler (plan)

Status: RESEARCH RUNNING — R2 crew `2026-07-27_0001` (kimi/claude/opencode/codex + SY)
validating design choices; architecture drafting proceeds in parallel.
Project home: `~/bellman/` (standalone git repo, will get its own GitHub project).
Date: 2026-07-27

## What it is
A **task scheduler** (the desktop cousin of cron / Windows Task Scheduler) for
Windows, macOS and Linux. Ships as a normal installable desktop application with a
launcher entry, so it shows up like any other app. Users and other applications
register timers/events; Bellman fires them at the right moment.

## Core requirements (from operator brief)

### Scheduling engine
- Year-round calendar awareness: dates, months, weeks, days, hours, minutes, seconds.
- Timers/tasks carry at minimum: **name, time, occurrence** (once / daily / weekly /
  monthly / yearly / interval).
- Monthly calendar auto-recalibrates each new year (script-based calibration kicks in
  on Jan 1 of each year, e.g. 1.1.20xx) so repeating dates map onto the new year's grid.
- User can view all stored events and their next trigger time, and can edit any timer
  in the timetable.

### Two ways to drive it
1. **AI skill / CLI** — a command-line interface an AI agent (or human) uses to create,
   list, modify and delete timers and occurrences.
2. **Manual** — user starts/stops the app and manages timers through the GUI.

### App-to-app connection layer (JSON slot files)
- A directory of **slot files**: for every timer trigger there is a **pair** — one
  input JSON (the requesting app writes what it wants: name, time, occurrence, wake
  action) and one output JSON (Bellman writes back status / next-fire /
  fire events).
- The app always keeps **at least 5 empty slot pairs pre-generated**, so multiple
  DIFFERENT systems can each claim their own slot independently (one system never
  blocks another). When a slot is consumed, a new empty pair is generated
  automatically to hold the count at ≥5. Integrating a new app = "modify those JSON
  files" — no SDK required.
- The slot files are also the **startup hook**: an external system that wants the
  time app running can claim a slot; if Bellman is not running when a slot is
  written, the slot is processed on its next start (and with autostart enabled it
  is effectively always listening).
- **Slot layer is a full command channel** (operator decision 2026-07-27): the
  input JSON carries an `action` field — `add` | `modify` | `delete` — so an
  external app can also change its own timer's time or remove it entirely through
  the same slot files. Ownership rule: an app may only modify/delete timers it
  created (matched by `app_name` + the timer `id` returned in its output slot).
- **Input-slot JSON schema (v1)**: `action`, `app_name`, `timer_name`, `time`,
  `occurrence`, `launch_command` (+ optional `args`, `workdir`, `misfire_policy`);
  for modify/delete additionally the `id` from the earlier output slot. Output
  slot returns: `status`, `id`, `next_fire`, `error` (if any).
- Purpose: any external application can register a **wake-up call** so Bellman
  wakes it (launch command / signal / callback file) at the scheduled time.

### Event log + pruning
- All fired events append to a **JSONL** event log.
- **Weekly automatic pruning** of the event log.
- Pruning also removes **already-elapsed one-shot timers** (events that already
  happened and won't recur).

### Memory-smart scheduling
- Reserve a safe bounded amount of memory for actions.
- **High-frequency timers** (seconds/minutes cadence) may live in memory;
  **low-frequency timers** (e.g. once a day) must NOT be held hot in memory — load
  them near their fire time (classic next-wake min-heap + persistent store; only the
  near-horizon window stays resident).

### UI pages
- **Events page** — every stored event + when it will trigger next.
- **Weekly page** — weekly repeating occurrences.
- **Monthly page** — month calendar views (with the yearly auto-updater above).

## Decided logic (operator answers, 2026-07-27)
- **Wake action** (target app not running): **launch + write JSON** — start the app
  via its registered launch command AND write the output-slot JSON so it finds its
  trigger data on startup.
- **Misfire** (machine/app was off at fire time): **per-timer policy** (amended
  after research, confirmed 2026-07-27) — defaults: calendar timers coalesce +
  run once on recovery (grace 1 h); interval timers skip (grace = one period).
  `skip` always selectable per timer for actions dangerous to re-run. Every
  miss/outcome is logged to JSONL (`fired_late`, `skipped_misfire`,
  `coalesced`).
- **Run model**: **ask per install** — installer/first-run asks whether to enable
  autostart+tray; both modes fully supported (manual-only users get the misfire
  policy applied at each start).
- **Timezones**: **system-local only**, DST-aware. No per-timer timezones in v1.
- **Event-log retention**: weekly prune keeps the **last 30 days** of fired events.
- **Interval limits**: minimum repeat = **1 second**; anything repeating faster
  than every **5 minutes** is "high-frequency" and stays memory-resident.
- **Wake machine from sleep**: **best-effort** — use the OS RTC-wake facility
  where available/permitted (Linux `rtcwake`, Windows waitable timers with
  WakeToRun, macOS `pmset schedule`); where not possible, skip and let the
  per-timer misfire policy handle it on wake.

Locked defaults (operator approved 2026-07-27):
- empty slot pairs kept ready: **≥5** — at least 5 placeholders so several
  different systems can hook in / start the time app without waiting
- overlap policy default: **skip** (don't double-fire while previous action runs)
- retry on failed wake action: **1 retry after 30 s**, then log `FAILED` to JSONL
- slots dir: `~/.bellman/slots/` (OS app-data equivalent on Win/Mac)
- single instance: second launch focuses the existing window
- malformed input slot: reject → error into output slot → JSONL log → fresh pair
- ack grace: woken app confirms via output slot within **60 s**, else `NO_ACK` log
- UI: 24-hour clock, week starts Monday, English-only v1
- desktop notification available as a wake-action type in v1

## Naming (operator decision 2026-07-27)
- App: **Bellman** — after the profession that rang bells / knocked on windows to
  wake people before alarm clocks existed; fits an app whose job is waking other
  applications. CLI command: **`bellman`**. GitHub repo: **`bellman`** (private
  for now, public when v1 is presentable). Project home: `~/bellman/`.
- Working title "Time Watcher" is retired; the R2 research folder keeps the old
  name (historical).

## Suggested additional per-timer settings (to confirm)
- `id` (stable UUID) and `enabled` on/off toggle
- timezone + DST rule (store tz-aware; default = system tz)
- misfire / catch-up policy: if the app or machine was off at fire time — run
  immediately on next start, or skip
- overlap policy: skip / queue / run-parallel if the previous run is still going
- retry count + backoff on failed wake-up action
- validity window: start date / end date, and `max_runs`
- jitter (± seconds) for high-frequency timers
- priority + tags/description
- action type: launch command, write output-slot JSON, desktop notification
- grace period for the target app to acknowledge the wake-up (via output slot)

## Architecture (v1 draft — simple internal connections, easy to code)

Design rule: every arrow below is ONE simple mechanism — a function call, a file
read/write, or a queue push. No component talks to more than its direct neighbors.

```
                              ┌─────────────────────────┐
                              │        BELLMAN      │
                              │  (normal desktop app +   │
                              │   launcher/tray entry)   │
                              └────────────┬────────────┘
                                           │
        ┌──────────────────┬───────────────┼────────────────┬──────────────────┐
        │                  │               │                │                  │
┌───────▼───────┐  ┌───────▼───────┐  ┌────▼─────┐  ┌───────▼───────┐  ┌───────▼───────┐
│    GUI         │  │   CLI (AI     │  │ SCHEDULER│  │  SLOT LAYER    │  │  EVENT LOG    │
│  3 pages:      │  │   skill)      │  │   CORE   │  │  (JSON pairs)  │  │  (JSONL)      │
│  · events+next │  │  add / list   │  │ min-heap │  │ slots/         │  │ events.jsonl  │
│  · weekly      │  │  edit / del   │  │ near-    │  │  in_001.json ─┐│  │ append-only   │
│  · monthly     │  │  start / stop │  │ horizon  │  │  out_001.json◀┘│  │               │
└───────┬───────┘  └───────┬───────┘  │ window   │  │  … (≥5 empty   │  └───────▲───────┘
        │                  │          └────┬─────┘  │  pairs kept)   │          │
        │   read/write     │  read/write   │        └───────┬───────┘          │ append
        └────────┬─────────┘               │ fire           │ watch/replenish  │
                 │                         │                │                  │
          ┌──────▼─────────────────────────▼────────────────▼──────┐          │
          │                    TIMER STORE (timers.json)            │──────────┘
          │  one record per timer: id, name, time, occurrence, …    │
          │  single source of truth — everyone reads/writes HERE    │
          └──────────────────────────┬──────────────────────────────┘
                                     │
                      ┌──────────────┼──────────────┐
                      │              │              │
              ┌───────▼──────┐ ┌─────▼──────┐ ┌─────▼───────────┐
              │ WAKE ACTIONS │ │  PRUNER    │ │ YEAR CALIBRATOR │
              │ launch cmd / │ │ weekly log │ │ runs on Jan 1:  │
              │ write out-   │ │ prune +    │ │ remap monthly/  │
              │ slot JSON /  │ │ elapsed    │ │ yearly dates to │
              │ notification │ │ one-shots  │ │ the new year    │
              └──────────────┘ └────────────┘ └─────────────────┘
```

Data flow in one sentence per path:
- **GUI/CLI → timer store**: both edit the same `timers.json` through one small
  storage module (no separate daemons talking to each other).
- **Scheduler core**: loads only the near-horizon window from the store into its
  min-heap; a once-a-day timer sits on disk until its window approaches.
- **Slot layer → store**: an external app writes an input-slot JSON → watcher
  validates it → registers a timer in the store → replenishes an empty pair.
- **Fire time**: scheduler pops the heap → runs the wake action → writes the
  output-slot JSON (status/next-fire) → appends the event to `events.jsonl`.
- **Pruner + calibrator**: internal timers themselves (the app schedules its own
  weekly prune and Jan-1 recalibration through the same scheduler core).

## Open design questions → R2 research swarm
1. **Stack**: best cross-platform desktop framework for a tray/launcher app with a
   calendar UI and tiny idle footprint (Tauri vs Electron vs Python+Qt vs Go+Wails
   vs .NET MAUI…), including installer/packaging story per OS (msi/exe, dmg/pkg,
   deb/AppImage) and autostart-on-login mechanisms.
2. **Scheduler core**: proven designs/libraries for a second-resolution scheduler
   with calendar recurrences (cron expressions vs RRULE/iCal recurrence), timer
   wheel vs min-heap, DST-safe local-time math, misfire handling.
3. **JSON slot IPC**: file-based integration patterns (watch dirs, atomic writes,
   schema versioning, slot lifecycle), and how competitors do app wake-up
   (systemd timers, launchd, Windows Task Scheduler COM) — what to reuse or wrap.
4. **Storage + pruning**: JSONL event log layout, weekly prune job, elapsed-timer
   cleanup, and the low-memory near-horizon loading design.
5. **Prior art**: existing open-source cross-platform schedulers — what they got
   right/wrong; naming/UX conventions for the two calendar pages.

Research runs as **R2** (4 researchers: kimi, claude, opencode, codex + 1
synthesizer) under `agent_terminal_suite/Research_main/`; synthesis feeds the build
plan for `~/bellman/`.
