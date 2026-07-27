# Bellman — v1 build plan (high detail)

Source: `docs/research/synthesis.md` (4-way independent research, unanimous core) +
`docs/PLAN.md` (spec + decided logic). This is the document build crews work from.

Naming map (synthesis used the old working title): `time-watcher` → **bellman**,
`tw-core` → **bellman-core**, `tw-cli` → **bellman-cli**, CLI `timewatcherctl` →
**`bellman`**, schema `tw-slot/1` → **`bellman-slot/1`**, env `TIME_WATCHER_RUN_ID`
→ **`BELLMAN_RUN_ID`**.

## Stack (locked by unanimous research verdict)

- **Tauri v2** shell: tray icon, `tauri-plugin-autostart`, `single-instance`,
  on-demand windows (GUI closed ⇒ resident = small Rust process + tray).
- **Rust core**: `chrono` + `chrono-tz`, `croner` (cron variant), `tokio`,
  `notify` v8 + `notify-debouncer-full`, `tempfile` (atomic writes), `rusqlite`
  (`bundled`, WAL, `synchronous=FULL`), `serde`/`serde_json`, `tracing`, `clap`.
- **Frontend**: Svelte 5 + Vite, plain CSS (WebKitGTK is the outlier — test it
  first), FullCalendar Standard (MIT) optional for the month grid.
- **Hand-rolled ~300-line heap loop** — no off-the-shelf scheduler crate exposes
  horizon eviction + custom misfire + suspend detection.

## Reference repos (inspiration while coding)

Study these before/while building the matching module. "Steal" = read the idea,
re-implement clean — no code copying without license care.

**Local clones for grepping**: the code-level repos below are shallow-cloned at
`~/reference_repos/bellman/<repo-name>` (croner-rust, Cronicle, kalarm, notify,
pomodorolm, tokio-cron-scheduler, zeit — ~47 MB total). Build agents should
read/grep there instead of browsing GitHub; docs-type links stay links. The
folder is throwaway — deleted when v1 ships.

| Repo / link | Use it for |
|---|---|
| https://github.com/tauri-apps/tauri + https://v2.tauri.app/learn/system-tray/ | Tauri v2 patterns: tray, windows, IPC commands |
| https://v2.tauri.app/plugin/autostart/ · https://v2.tauri.app/plugin/single-instance/ | The two required plugins — wiring examples |
| https://github.com/vjousse/pomodorolm | Small real-world **Tauri tray timer app** — closest existing shape to Bellman's shell; study tray + window lifecycle |
| https://github.com/Hexagon/croner-rust | The cron-variant parser we ship; seconds field + Quartz `L/W/#` |
| https://github.com/mvniekerk/tokio-cron-scheduler | **Read, don't depend**: how a tokio scheduler loop is structured; we replace its all-in-memory model with the horizon window |
| https://github.com/notify-rs/notify + https://docs.rs/notify-debouncer-full | Slot-dir watching; issue tracker documents every OS's lossy-event caveat |
| https://github.com/rusqlite/rusqlite | Store layer; `bundled` feature, WAL pragmas |
| https://github.com/KDE/kalarm | **Richest per-alarm data model in OSS** — per-alarm tz, late-cancel grace, recurrence exceptions, Feb-29 policy; mine its option set for our timer settings |
| https://github.com/jhuckaby/Cronicle | Job-scheduler UX: per-job catch-up toggle, overlap limits, retries, and the live per-timer log tail (our filtered-JSONL view) |
| https://github.com/loimu/zeit | Minimal cron-GUI — what a thin wrapper looks like (and why we own the engine instead) |
| https://github.com/super-productivity/super-productivity | Polished cross-platform productivity UI — layout/UX inspiration for the timer pages |
| https://github.com/fmeringdal/rust-rrule | v2 only (iCal import/export) — upstream itself warns "not production ready"; do NOT use in v1 |
| https://fullcalendar.io/license (Standard, MIT) | Month-grid component option for P4 |
| https://apscheduler.readthedocs.io/en/3.x/userguide.html | Misfire-policy vocabulary (`grace`, `coalesce`) — the industry-consensus semantics we mirror |
| https://man7.org/linux/man-pages/man5/systemd.timer.5.html | `Persistent`/`RandomizedDelaySec`/`AccuracySec` — vocabulary for catch-up, jitter, coalescing slack |
| https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-startwhenavailable | Windows policy vocabulary (StartWhenAvailable, overlap rules) — copy vocabulary, not defaults |
| https://www.sqlite.org/wal.html · https://sqlite.org/pragma.html#pragma_synchronous | WAL + synchronous=FULL rationale |
| https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilea | Windows atomic-replace semantics for slot writes |

## Repository layout

```
bellman/                                (one Cargo workspace)
├─ crates/bellman-core/
│  ├─ occurrence/   enum: once|interval|daily|weekly|monthly|yearly|cron
│  │                lazy next_fire(after) via chrono-tz (+croner for cron);
│  │                DST gap/fold + monthday-clamp policies; preview iterator
│  ├─ scheduler/    BinaryHeap<Reverse<(fire_at_utc, TimerId)>>, 24 h horizon
│  │                (config); chunked ≤30 s sleeps; wall-vs-monotonic clock-
│  │                jump detector; misfire pass (grace+coalesce) at startup
│  │                and on every detected wake; channel-driven refill on edits
│  ├─ store/        rusqlite bundled, WAL, synchronous=FULL
│  │                tables: timers / runs (at-least-once claims) /
│  │                slot_requests / meta; optimistic revision on edits
│  ├─ events/       JSONL appender events.current.jsonl; tolerant reader
│  │                (skip bad lines, count skips); weekly rotate→dated archive
│  ├─ slots/        slots/{free,work,done,bad}/; notify hints + periodic
│  │                rescan; atomic temp+rename publish; request_id idempotency;
│  │                claim-by-rename; replenish ≥5; orphan sweep; quarantine
│  ├─ actions/      launch (arg array, NO shell, timeout, output cap) |
│  │                write output slot | desktop notification; overlap policy;
│  │                retry 1×/30 s; global max_concurrent_actions (16)
│  └─ pruner/       system.prune weekly internal timer (visible in GUI) +
│                   startup catch-up; Jan-1 consistency pass
├─ crates/bellman-cli/   bellman add|list|edit|rm|next|run-now|pause|slot-submit
├─ src-tauri/            tray, single-instance, autostart, on-demand windows
└─ ui/  (Svelte 5)       tabs: All timers | Week | Month | Run history

data dir: ~/.bellman/ on Linux (AppData/Application Support equivalents):
  timers.db(+wal,shm) · logs/events.current.jsonl · logs/archive/*.jsonl
  slots/{free,work,done,bad}/ · config.json
```

## Core design rules (bind every phase)

1. All timing lives in Rust — **never** in the webview (hidden webviews throttle
   JS timers to ≥1/min).
2. Every JSON write, ours or an integrator's, is **temp file + atomic rename in
   the same directory**. Nothing is ever edited in place. On Windows use
   ReplaceFile semantics (`NamedTempFile::persist`) + retry around AV locks.
3. **SQLite = what will happen; JSONL = what happened.** Recovery = WAL replay +
   pending run-claim recovery + misfire scan + horizon rebuild.
4. Watcher events are hints; the periodic rescan is truth. Slot dir + db on
   local disk only.
5. At-least-once, never exactly-once: claim a `run_id` row before acting; pass
   `BELLMAN_RUN_ID` to launched programs.
6. Store wall-clock + IANA tz per calendar timer; compute next-fire lazily.
   Interval timers anchor to UTC elapsed time. Jan-1 job = validation pass only
   (lazy compute means there is no year grid to rebuild).
7. `deny_unknown_fields` is forbidden on any persisted/wire JSON.
8. Backward clock jump ≠ suspend: never re-fire completed one-shots
   (`last_fired` + run claims guard).

## Spec amendments adopted from research (2026-07-27)

- **Slot protocol errata**: integrators never edit the pre-created input file in
  place (torn-read race). A free slot reserves an *id*; the producer publishes
  its complete request via atomic rename, carrying a `request_id` (UUID)
  idempotency key. Envelope: `{"schema":"bellman-slot/1", slot_id, request_id,
  ts, operation: add|modify|delete, payload…}`.
- **Naming**: a stored definition is a **Timer**, each firing is a **Run**; UI
  tabs are All timers / Week / Month / Run history ("Events" was ambiguous).
- **Misfire default** (CONFIRMED by operator 2026-07-27): per-timer policy —
  defaults are **coalesce + run-once-on-recovery for calendar timers** (grace
  1 h) and **skip for interval timers** (grace = one period). **`skip` is always
  selectable per timer** for actions that are dangerous to re-run; coalesce
  guarantees a missed backlog fires at most once, and the global
  `max_concurrent_actions` cap queues mass-fires so nothing runs "all at the
  same time".
- **Per-timer settings additions** (priority order): exclusion dates /
  skip-next; DST gap/fold + invalid-monthday fields; catch-up limit;
  next-5-occurrences preview; Run-now button; overlap policy
  skip|queue_one|parallel(cap)|replace; global action backpressure; pause-all
  vacation mode; jitter + accuracy slack; pre-notification offset (v1.1);
  run conditions only-on-AC/idle (v1.1); slot TTL + per-app auth key (v1.1).

## Build phases (each = one crew card; exit gate before the next departs)

### P0 — core engine, headless  ◀ hardest, highest value
Build `bellman-core`: occurrence enum + `next_fire()`; store schema
(timers/runs/meta); heap loop with chunked sleeps, clock-jump detector, misfire
pass.
**Tests (the gate IS the tests):** golden `next_fire` cases — DST gap + fold
(Europe/Helsinki, US zones, a half-hour zone), Feb-29, day-31 clamp, year
boundary; simulated suspend/resume (mock clock pair); crash-at-every-boundary
around fire (claim-before-work recovery); backward clock jump never re-fires.
**Exit gate:** full test suite green; a demo binary schedules a 2 s interval +
a daily timer and fires both correctly across a simulated sleep.

### P1 — CLI (the AI-skill surface)
`bellman add|list|edit|rm|next|run-now|pause` against bellman-core, headless.
`bellman next <timer> 5` prints the next-5 preview. Machine-readable `--json`
output on every command (AI agents parse this).
**Exit gate:** round-trip script — add all 7 occurrence kinds, edit, preview,
run-now, rm — passes on Linux; `--json` output schema documented.

### P2 — slot IPC + event log
Slot dirs + watcher/rescan + atomic publish protocol + `request_id` idempotency
+ replenisher (≥5) + orphan sweep + quarantine (`bad/` + `.err.json`).
JSONL appender + weekly rotate→archive + tolerant reader.
`bellman slot-submit request.json` helper. Integration README with copy-paste
Python/shell/PowerShell/Node examples (an external app integrates in <10 lines).
**Exit gate:** torture test — concurrent producers, duplicate request_ids,
malformed/huge/symlinked input, kill -9 mid-publish — no lost/duplicated
timer, quarantine catches all garbage, free-count invariant holds.

### P3 — Tauri shell
Tray + menu, single-instance (second launch focuses window), autostart plugin
(the installer/first-run choice from the spec), All-timers page: next-fire
countdown, enable toggle, Run-now, live log tail (filtered JSONL view — the
Cronicle-loved feature).
**Exit gate:** GUI closed ⇒ webview destroyed, measured RSS of resident
process recorded; tray works on Ubuntu GNOME (AppIndicator ext detected +
graceful degrade), KDE, Windows, macOS.

### P4 — calendar UI
Week page (day-of-week grid of weekly repeats), Month page (year-aware grid),
edit dialogs per occurrence variant with live next-5 preview + DST warnings,
next-fire shown in local + UTC + offset.
**Exit gate:** screenshot review of all pages on the three webviews
(WebKitGTK first); create/edit/delete round-trips from the GUI.

### P5 — pruner + hardening
`system.prune` weekly internal timer (visible in GUI) + startup catch-up;
elapsed-one-shot cleanup (terminal-state only, `pruned` tombstones); Jan-1
consistency pass; global `max_concurrent_actions` + queue (mass-fire after a
weekend suspend must not fork-bomb); action timeouts + output caps; per-timer
jitter/accuracy slack.
**Exit gate:** resume-after-72 h simulation coalesces correctly under the
concurrency cap; prune verified against fixture logs; footprint gates —
idle CPU ~0%, wakeups/min bounded with a 1 s timer active.

### P6 — packaging + per-OS QA
`tauri build`: NSIS exe + MSI (WebView2 evergreen bootstrapper), signed +
notarized dmg, deb + AppImage (NO Flatpak/Snap in v1 — breaks tray/autostart/
single-instance). Autostart QA per OS (macOS 13+ Login Items UX, Windows
Run-key re-register on move, XDG). Code-sign early — AV/SmartScreen false
positives are a launch blocker, not a polish item.
**Exit gate:** fresh-VM install + autostart + timer-fires-after-reboot on
Win 11, macOS, Ubuntu GNOME/Wayland, KDE/X11; idle-footprint numbers recorded
in docs.

## Top risks to re-read before each phase
(1) suspend oversleep — chunked sleeps mandatory; (2) DST — explicit policies,
elapsed-time intervals; (3) webview JS throttling; (4) lossy file watchers;
(5) Windows rename semantics; (6) backward clock jumps; (7) resume mass-fire;
(8) Linux tray/AppIndicator; (9) autostart quirks per OS; (10) slot-dir abuse.
Full ranked list: synthesis §7.
