# Time Watcher — Synthesis (crew 2026-07-27_0001)

Synthesizer: claude (slot S) · Date: 2026-07-27
Inputs: `1_kimi/research.md`, `2_claude/research.md`, `3_opencode/research.md`,
`4_codex/research.md` — four independent research passes over
`agent_terminal_suite/docs/TIME_WATCHER_PLAN.md`.

---

## 1. Headline verdict — unanimous where it matters

All four researchers, working independently, converged on the same core stack
and the same four architectural pillars. Convergence this strong from
independent passes is the strongest signal this crew format can produce.

| Pillar | Unanimous recommendation (4/4) |
|---|---|
| **Framework** | **Tauri v2** — Rust core + OS webview. Close the window → webview is destroyed → idle state is a small Rust process + tray icon, which is exactly the spec's "memory-smart" mandate. Electron rejected (always-resident Chromium), .NET MAUI rejected (no real Linux), Wails/Fyne runner-up (thin tray/plugin ecosystem), PySide6 fallback only if Rust is off the table. |
| **Timing structure** | **Binary min-heap over a near-horizon window**, not a timer wheel. Wheels win at 10⁵+ hot timers; our resident set is the window (dozens–hundreds). Cold timers live on disk only — a once-a-day timer costs one indexed row and zero heap bytes. |
| **Who fires** | **Our own always-on process fires everything.** OS-native schedulers (systemd timers, launchd, Windows Task Scheduler) are minute-granular in practice, have divergent misfire semantics, and would mean three engines. They get exactly ONE job in v1: autostart-on-login (via `tauri-plugin-autostart`). Wake-from-sleep OS assist is a v2 opt-in (privilege-sensitive on 2 of 3 OSes). |
| **Slot IPC mechanics** | **Atomic temp-file + same-dir rename** for every JSON write (ours and integrators'); **`notify` watcher events are hints only**, backed by full re-scan / periodic poll (all OS watch backends drop events); **versioned schema envelope** with unknown-field tolerance; **state machine encoded in filename/directory, transitions only via atomic rename** (rename doubles as the mutex); quarantine dir for malformed input, never silent deletion; replenishment as an invariant (top up `free/` after every claim + periodic sweep). |

Also unanimous: store **wall-clock time + IANA tz** per recurring timer, never a
precomputed UTC instant; compute next-fire lazily; DST spring-forward gap →
first valid instant after the gap, fall-back overlap → fire once (first
occurrence); **interval timers anchor to UTC/elapsed time**, never wall-clock;
sleep/hibernate is the #1 killer and needs a wall-vs-monotonic clock-jump
detector plus a coalescing misfire pass on startup and on every wake.

## 2. Recommended stack (one pick)

- **Tauri v2** (stable since Oct 2024) + first-party plugins: `tray-icon`,
  `tauri-plugin-autostart`, `single-instance`.
- **Rust core crates**: `chrono` + `chrono-tz` (civil time, IANA db), `croner`
  (7-field cron expansion for the optional cron variant — seconds resolution),
  `tokio` (runtime), `notify` v8 + `notify-debouncer-full` (fs watching),
  `tempfile::NamedTempFile::persist` (atomic writes), `rusqlite` with `bundled`
  (storage — see §3.4), `serde`/`serde_json`, `tracing`, `clap` (CLI).
- **Frontend**: Svelte 5 + Vite (2 of 4 named it; smallest runtime), calendar
  grid either hand-rolled or FullCalendar Standard (MIT). Plain CSS, no heavy
  animation (WebKitGTK quirks). **No timer logic in JS ever** — hidden webviews
  throttle timers to ≥1/min.
- **Write the ~300-line heap loop ourselves.** `tokio-cron-scheduler` (all-jobs-
  in-memory model) conflicts with the horizon window; no off-the-shelf runner
  exposes custom misfire + horizon eviction + suspend detection.
- Layout: one Cargo workspace — `tw-core` (lib), `tw-cli` (binary), Tauri shell.
  GUI, CLI, and slot gateway all call the same core functions.
- Packaging: `tauri build` → NSIS exe + MSI, dmg/app, deb/rpm/AppImage.
  **Ship AppImage + deb/rpm on Linux; do NOT ship Flatpak/Snap in v1** (breaks
  tray, autostart, and the single-instance plugin).

## 3. Merged per-question recommendations

### 3.1 Occurrence model (Q2a) — the one real design fork; resolution below

**Canonical storage = a small internal tagged-union (enum) mapping 1:1 to the
spec's six kinds**, each variant implementing
`next_fire(after: DateTime<Tz>) -> Option<DateTime<Tz>>`:

```json
{"occ": "once",    "at": "2026-08-01T14:30:00", "tz": "Europe/Helsinki"}
{"occ": "interval","every_s": 90, "anchor": "<utc-instant>"}
{"occ": "daily",   "at": "07:30:00"}
{"occ": "weekly",  "days": ["mon","fri"], "at": "07:30:00"}
{"occ": "monthly", "day": 31, "at": "09:00:00", "clamp": "last-day"}
{"occ": "yearly",  "month": 2, "day": 29, "at": "12:00:00", "feb29": "feb28"}
{"occ": "cron",    "expr": "*/10 * * * * * *"}          // power-user variant, via croner
```

Rationale (this reconciles the 4-way split — see disagreement table):
- The enum is GUI-friendly (each variant = one form page), trivially testable,
  validated, and sidesteps both parsing wars. 2 of 4 recommended exactly this;
  the other two's picks slot in underneath it:
- **cron** (kimi's pick) survives as the seventh variant via `croner` — gives
  second-resolution and Quartz `L`/`W`/`#` extensions for free.
- **RRULE** (codex's pick, opencode's escape hatch) is **deferred to v2**: two
  researchers independently flagged the main Rust `rrule` crate's own
  "not production ready" warning with missing DST edge-case tests — a bad
  foundation for the one thing we must get right. Revisit for iCal
  import/export once the crate matures; the enum can be lowered to RRULE later.
- **`interval` is a first-class elapsed-time type** (all 4 agree): UTC
  anchor + N seconds, never cron `*/N` (field-boundary restarts can't express
  "every 90 s"), never wall-clock (DST would stretch/shrink it).

**New-year recalibration falls out for free** (3 of 4 explicitly): with lazy
`next_fire()` there is no materialized year grid to recalibrate. Keep the
spec's Jan-1 job as a **validation/consistency pass** (recompute + verify every
stored timer, refresh the GUI year view, log `year_recalibrate`), run
unconditionally at startup if `last_recalibration < year_start(now)` (covers
sleeping over New Year) — not as the correctness mechanism.

### 3.2 Heap loop, DST, sleep, misfire (Q2b–d)

- **Heap**: `BinaryHeap<Reverse<(fire_at_utc, timer_id)>>` holding only timers
  with `next_fire_utc <= now + horizon`, plus all short-period interval timers
  (always "near"). **Horizon default 24 h, configurable** (see disagreements).
  Refill on: fire-drain, any insert/edit (channel wakes the loop), startup,
  wake/clock-jump detection, and a periodic refill check.
- **Sleep in short chunks**: `min(next_fire − now, ~30 s)`, re-reading the wall
  clock each wake. Reason (claude's key find): tokio's `sleep_until` uses the
  monotonic clock, **which freezes during suspend** — a naive sleep-to-head
  oversleeps by the whole suspend. The 30 s chunk is the cross-platform
  correctness backstop; OS resume notifications (login1 `PrepareForSleep`,
  `NSWorkspace.didWakeNotification`, `WM_POWERBROADCAST`) are a later latency
  optimization only.
- **Clock-jump detector**: each tick compare wall-clock delta vs monotonic
  delta; divergence > a few seconds ⇒ suspend or clock-set ⇒ run the misfire
  pass + rebuild the horizon. On **backward** jumps (NTP/manual), never re-fire
  already-fired one-shots — guard with persisted `last_fired`.
- **Misfire policy per timer** (APScheduler vocabulary, industry consensus
  across Quartz/systemd/launchd/WTS: *fire once on recovery, never replay the
  backlog, skip if beyond grace*):
  - `on_misfire`: `run_now` (default for once/daily/weekly/monthly/yearly) |
    `skip` (default for interval) | `catch_up` with explicit `max_catch_up`
    cap (rarely wanted).
  - `coalesce = true` by default (laptop closed all weekend ⇒ daily timer
    fires once, not N times).
  - `misfire_grace`: configurable per timer; default **1 h for calendar
    timers, one period for interval timers** (see disagreement table).
  - Persist `last_fired`; run the misfire scan at startup AND on every
    detected wake/jump; log outcomes (`fired_late`, `skipped_misfire`,
    `coalesced(n)`) to the event log.
- **At-least-once, not exactly-once** (codex): launching a process + committing
  state cannot be atomic. Stamp every firing with a `run_id`, record a run
  claim before acting (claim-before-work: a crash mid-action leaves a
  recoverable record, never a double-fire), pass `TIME_WATCHER_RUN_ID` to
  launched programs so cooperating targets can deduplicate.
- **DST knobs stored explicitly per timer** (defaults sane): `dst_gap_policy`
  = first-valid-after-gap, `dst_fold_policy` = first occurrence,
  `invalid_monthday_policy` (day-31/Feb-29 clamp).

### 3.3 JSON slot IPC (Q3)

Merged lifecycle (kimi's filename state machine + claude's dir layout +
codex's idempotency correction):

```
slots/
  free/   slot-0007.json          # pre-generated empty pairs, valid stubs
  work/   ...                     # claimed by Time Watcher
  done/   ...                     # answered; GC after 24 h
  bad/    ... (+ .err.json)       # malformed input, quarantined with reason
```

- **Spec correction (adopt codex's finding):** an external app must **never
  edit a pre-created input file in place** — that has an inherent torn-read /
  multi-writer race. The "pre-generated pair" is a *reserved slot id*; the
  producer writes its complete request as a temp file in the same directory
  and atomically renames it into place. Every request carries a `request_id`
  (UUID) as a durable **idempotency key** — repeat submissions return the
  original result. Document the 4-line atomic-write recipe in the integration
  README with copy-paste Python/shell/PowerShell/Node examples; optionally
  ship `tw-cli slot submit request.json` to hide the protocol.
- Envelope: `{"schema": "tw-slot/1", "slot_id", "request_id", "ts",
  "operation", "payload"…}`. Tolerant reader: accept same-major, ignore
  unknown fields (**never `deny_unknown_fields`**), keep a v1 reader forever.
  Empty free slots are valid JSON stubs, so integrators can discover the
  format by reading one.
- **Claim = rename into `work/`** — the losing racer gets ENOENT and moves on;
  no locks. Orphan sweep reclaims stale claims past `expires_at`.
- **Watcher = latency sugar; poll = truth.** `notify` + debouncer-full for
  low latency, plus a periodic (~5–60 s) full re-scan of the small slot dir on
  every event and on startup. Slot dir must be on a local disk (network
  filesystems deliver no events; SQLite WAL also requires local semantics).
- Replenish invariant: after every claim and on the sweep,
  `while count(free/) < N_MIN (4–5): create next empty pair`.
- Security (codex): slots live under the per-user data dir with user-only
  permissions; reject symlink/reparse escapes; size-cap reads; store
  executable + argument array separately, **no shell by default, no elevation
  in v1**; execution timeout + output cap.
- Response file carries `request_id`, `timer_id`, `status`, `next_fire`, and a
  bounded list of un-acknowledged run events with a monotonic
  `event_sequence` (a single `last_event` field loses firings under a slow
  consumer).

### 3.4 Storage & pruning (Q4)

**SQLite (WAL) is the timer-table source of truth — 3-of-4 majority** over
claude's `timers.json` snapshot (see disagreement table for the resolution).

- `rusqlite` + `bundled` (static SQLite, no per-OS drift), `journal_mode=WAL`,
  **`synchronous=FULL`** — timer mutations are rare, so the extra fsync is
  free, and FULL survives power loss, not just app crash. Busy timeout set;
  WAL checkpoint (TRUNCATE) on clean exit (`-wal`/`-shm` sidecars confuse
  backup tools); refuse to start on a network share.
- Tables: `timers` (uuid PK, name, enabled, occurrence JSON, tz,
  `next_fire_utc` **indexed**, last_fired, misfire/overlap/retry policies,
  validity window, max_runs, tags, action JSON, revision, …), `runs` (run_id,
  timer_id+scheduled_for unique — the at-least-once claim ledger), `meta`
  (schema version, last_prune, last_recalibration, tzdata version).
  Edits are optimistic (`WHERE revision = ?`) so a stale GUI can't clobber a
  CLI edit; every mutation recomputes next-fire in the same transaction.
- **Events = append-only JSONL**, one self-contained line per lifecycle event
  (`registered|fired|fired_late|skipped_misfire|coalesced|wake_delivered|
  wake_failed|pruned|year_recalibrate`…) with ts, timer_id, run_id, resolved
  offset, duration, redacted error. Readers **skip unparseable lines and log
  the skip count** — torn tail lines after a crash are expected, never
  truncate/abort. Never log secrets/full env.
- **Weekly prune is itself a persisted internal timer** (`system.prune`,
  visible in the GUI — dogfooding), with startup catch-up
  (`now > last_prune + 7d` ⇒ run immediately). It:
  1. Rotates/compacts the JSONL: rename current → archive segment (atomic),
     open fresh; delete archives past retention (default ~30–56 days,
     configurable — "weekly" is the *cadence*, not a 7-day retention).
  2. Deletes elapsed one-shots — **only** those in a terminal state
     (fired/skipped and past validity + ack grace); failed/pending one-shots
     are *not* "elapsed" and must not be deleted. Each deletion writes a
     `pruned` tombstone line to the log.
- Crash-safety invariant: SQLite (atomic transactions) = *what will happen*;
  JSONL (append) = *what happened*; nothing is ever edited in place; recovery
  = open db (WAL replays) + recover pending run claims + misfire scan +
  rebuild horizon.
- `config.json` stays hand-editable JSON with atomic-rename writes.

### 3.5 Prior art (Q5) — merged takeaways

**The niche is genuinely empty**: every OSS offering is either a cron *wrapper*
(Zeit, KCron, Gnome Schedule — single-OS, minute floor, no misfire policy, no
history) or a *server* (Cronicle, Dkron — Node/Redis-heavy, headless). The only
close UX match, Task Till Dawn, is closed-source, stalled since 2019, no Linux.
Owning the engine is what buys cross-platform + seconds + logs.

What the best prior art gets right (design lessons, independently implemented):
- **KAlarm** — richest per-alarm model (per-alarm tz, late-cancel grace,
  recurrence exceptions, positional monthly, Feb-29 policy, wake-from-suspend).
- **Windows Task Scheduler** — policy vocabulary (StartWhenAvailable, four
  overlap rules, run conditions) but not its bad defaults (AC-only silently
  skipping laptop runs).
- **Cronicle** — per-job timezone, catch-up toggle, overlap limits, retries,
  and the most-loved feature: a **live per-timer log tail** (free for us — a
  filtered JSONL view).
- **systemd vocabulary** — `Persistent` (catch-up), `RandomizedDelaySec`
  (jitter), `AccuracySec` (coalescing tolerance; pairs perfectly with the heap
  window and battery-friendly firing slack).
- **Naming** (codex): a saved definition is a **Timer**, each firing is a
  **Run**; UI tabs "All timers / Week / Month / Run history" — "Events" is
  ambiguous. Anti-pattern to avoid: KCron destroys data it can't model.

**Per-timer settings to add to the spec** (union, deduped, priority order):
1. Exclusion dates / skip-next-occurrence (EXDATE-equivalent) — first feature
   request every scheduler gets; painful to retrofit (3 of 4 flagged).
2. Explicit DST gap/fold + invalid-monthday policy fields (3 of 4).
3. `catch_up_limit` / `max_catch_up` — one boolean isn't enough (2 of 4).
4. **Next-occurrences preview** ("next 5" in CLI + GUI) — the single best
   trust/debug feature; pure `next_fire()` iteration (2 of 4).
5. **Run now / Try button** — the most-loved feature in KAlarm and Task Till
   Dawn.
6. Overlap policy per timer: `skip_running | queue_one | parallel(cap) |
   replace` (all 4, vocab merged).
7. Backpressure: global `max_concurrent_actions` (default ~16) + queue — a
   mass-fire after resume must not fork-bomb the box (2 of 4).
8. Pause-all / vacation mode (distinct from per-timer enabled).
9. Jitter (`RandomizedDelaySec`-style, applied to execution not display) +
   `AccuracySec`-style coalescing tolerance.
10. Pre-notification offset; defer/snooze for ack-able timers; templates +
    import/export; positional monthly ("2nd Tuesday") + sub-repetition (v2).
11. Run conditions (only-on-AC, only-when-idle) — v1.1+, laptop-relevant.
12. Slot TTL + optional per-app slot auth key (v1.1).

## 4. Where the researchers disagreed

| # | Topic | kimi (rs#1) | claude (rs#2) | opencode (rs#3) | codex (rs#4) | Synthesis resolution |
|---|---|---|---|---|---|---|
| 1 | **Canonical occurrence format** | Extended cron via `croner`; interval separate | Own typed enum; cron as extra variant | Own enum; RRULE escape hatch (enum generates RRULE) | RRULE canonical (`rrule` crate); interval separate | **Own enum canonical** (2 direct votes + both others' picks embed cleanly as variants); cron variant via croner in v1; RRULE deferred to v2 — 2 of 4 independently flagged `rrule`'s own "not production ready" warning, which outweighs codex's guarded-use plan. All 4 agree interval is a separate elapsed-time type. |
| 2 | **Timer-table storage** | SQLite WAL FULL (rusqlite) | `timers.json` atomic snapshot + `.bak` ("simpler, human-readable") | SQLite WAL NORMAL (sqlx) | SQLite WAL FULL + `runs` claim table | **SQLite, WAL, `synchronous=FULL`, rusqlite+bundled** (3–1; claude's own caveat concedes the swap "if write rate explodes" — a 1 s interval timer already forces his debounce workaround). Keep claude's point by making export/import human-readable JSON. Adopt codex's `runs` table for at-least-once claims. |
| 3 | **`synchronous` level** | FULL | n/a | NORMAL ("JSONL is source of truth") | FULL | **FULL** — mutations are rare so cost ≈ 0; opencode's premise (rebuild from JSONL) is inverted: the db is authoritative, the log is audit. |
| 4 | **JSONL fsync per line** | Yes (`fdatasync` each write) | No ("not worth a battery hit; crash loses ≤1 line") | not stated | Log is non-authoritative; dedupe via event_id | **No per-line fsync** — with the `runs` table authoritative (res. #2), a lost tail line is harmless and dedupable; flush on write, sync on rotation. |
| 5 | **JSONL file layout** | Single file + weekly compaction rewrite | Single file + rename-to-`.old` compaction | One file per ISO week | `events.current.jsonl` + weekly rename to dated archive | **current + dated weekly archives** (codex/opencode style): rotation is a pure atomic rename — cheaper and safer than kimi/claude's rewrite-and-filter compaction, and prune = delete old archive files. |
| 6 | **Horizon default** | 24 h (`max(24 h, 2× longest hot interval)`) | 48 h + all sub-hour intervals | 24 h + intervals ≤ 60 s | 15 min | **24 h default, configurable** (median; 2 votes). 15 min minimizes RAM but multiplies refill queries for zero practical gain at this scale; 48 h costs nothing but adds nothing. Always-resident: interval timers with period ≤ refill interval. |
| 7 | **Misfire grace default** | 1 h (calls APScheduler's 1 s "dangerously low") | 60 s | n/a (policy-only) | bounded `max_lateness` per timer | **1 h for calendar timers, one period for interval timers, per-timer override.** A tray app on a laptop routinely resumes minutes late; 60 s would misclassify normal resumes as misfires. |
| 8 | **Misfire default action** | coalesce + fire once | `run_now` (once/daily), `skip` (interval) | `skip` all | `coalesce_latest` | **coalesce + run_now for calendar kinds, skip for interval** (3 of 4 favor firing once on recovery; opencode's skip-all silently drops a missed daily reminder — worst UX failure mode). |
| 9 | **Slot dir topology** | One dir, state in filename (`.open/.claimed/.filled/.consumed`) | `free/ work/ done/ bad/` dirs | `in/ out/` pairs + `_quarantine/` | `available/ incoming/ processing/ output/ rejected/` | **State-per-directory (free/work/done/bad)** — same rename-only state machine as kimi's, but `ls slots/free` is self-documenting for integrators; adopt codex's immutable-publication + `request_id` idempotency on top. |
| 10 | **Producer writes** | Claim via O_EXCL create, then rename | Write into pre-created pair | Write into pre-created pair | **Never edit in place — publish new file atomically; spec change** | **Adopt codex's correction** (spec errata, §3.3): in-place editing of a shared pre-created file is an unfixable torn-read race; kimi's O_EXCL claim is the compatible primitive. |
| 11 | **Jan-1 recalibration job** | Implicit (lazy compute) | Validation pass only, "falls out for free" | Real re-anchoring job that rewrites wall_time | No script at all; consistency check only | **Validation/consistency pass, no persisted year grid** (3 of 4). opencode's rewrite job mutates stored intent for no benefit and adds a corruption surface. |
| 12 | **SQLite driver** | rusqlite (bundled) | n/a | sqlx 0.9 (async) | unspecified | **rusqlite + bundled** — the store is low-QPS and transactional; async SQL buys nothing here and sqlx adds compile/runtime weight. |
| 13 | **Frontend framework** | "plain CSS", framework-agnostic | any JS / FullCalendar | SvelteKit | Svelte 5 + Vite + FullCalendar Standard | **Svelte 5 + Vite** (2 named votes, none opposed); FullCalendar Standard (MIT) for the month grid is optional — keep CSS plain per kimi's WebKitGTK warning. |
| 14 | **macOS wake-from-sleep stance** | OS schedulers rejected outright for firing | v2 opt-in per-timer `wake_machine` | Use launchd catch-up as a "resync trigger" in v1 hooks (M6) | Phase-2 single next-wake bridge via privileged helper | **v1: none — misfire pass on resume covers it. v2: codex's "single next-wake instant" bridge** (one OS job programming only the next wake) — cleaner than per-timer mirroring or opencode's M6 hooks. |

Minor divergences not worth a row: idle-RAM figures quoted (30–110 MB — all
directionally identical, and codex is right that we should gate on our own
measurements, not marketing numbers); retention default (30 vs 56 days —
config knob); replenish N_MIN (4 vs 5 — config knob).

## 5. v1 architecture (merged)

```
time-watcher/                          (one Cargo workspace)
├─ crates/tw-core/                     # the engine — everything below is this lib
│  ├─ occurrence/   enum (once|interval|daily|weekly|monthly|yearly|cron),
│  │                lazy next_fire() via chrono-tz + croner; DST gap/fold +
│  │                monthday-clamp policies; occurrence preview iterator
│  ├─ scheduler/    BinaryHeap<Reverse<(fire_at_utc, TimerId)>> over 24 h
│  │                horizon; chunked ≤30 s sleeps; wall-vs-monotonic clock-jump
│  │                detector; misfire pass (coalesce+grace) at startup & wake;
│  │                channel-driven refill on edits
│  ├─ store/        rusqlite bundled, WAL, synchronous=FULL:
│  │                timers / runs (at-least-once claims) / slot_requests / meta
│  ├─ events/       JSONL appender (events.current.jsonl), line-tolerant
│  │                reader, weekly rotate→archive + retention delete
│  ├─ slots/        free|work|done|bad dirs; notify(hints)+rescan; atomic
│  │                publish + request_id idempotency; claim-by-rename;
│  │                replenish invariant; orphan sweep; quarantine
│  ├─ actions/      launch (no shell, arg array, timeout, output cap) |
│  │                write output slot | desktop notify; overlap policy;
│  │                retry+backoff; global max_concurrent_actions
│  └─ pruner/       system.prune internal weekly timer + startup catch-up;
│                   Jan-1 consistency pass
├─ crates/tw-cli/   timewatcherctl add|list|edit|rm|next|run-now|pause|slot-submit
│                   (the AI-skill surface; works headless, talks to the daemon)
├─ src-tauri/       tray, single-instance, autostart plugin, on-demand windows
└─ ui/ (Svelte 5)   All timers | Week | Month | Run history; edit dialog with
                    occurrence preview; NO timer logic in JS

data dir (XDG / Application Support / AppData)/time-watcher/
  timers.db(+wal,shm)   logs/events.current.jsonl + logs/archive/*.jsonl
  slots/{free,work,done,bad}/   config.json
```

One process owns the scheduler; second launch focuses the GUI
(single-instance). GUI closed ⇒ webview destroyed ⇒ resident = Rust + tray.

## 6. v1 build phases

1. **P0 — core, headless**: `tw-core` occurrence enum + `next_fire()` with
   exhaustive golden tests (DST gap/fold, half-hour zones, Feb-29, day-31,
   year boundary); store (SQLite schema + runs claims); heap loop + clock-jump
   detector + misfire pass. *Exit gate: crash-at-every-boundary and
   sleep/resume simulation tests pass.*
2. **P1 — CLI**: `timewatcherctl` against the core — the AI interface works
   before any GUI exists. Occurrence preview (`next 5`).
3. **P2 — slot IPC**: slot dirs, watcher+rescan, idempotent publish protocol,
   replenisher, quarantine; integration README + example client scripts;
   JSONL event log + weekly rotation.
4. **P3 — Tauri shell**: tray, single-instance, autostart, All-timers page
   (next-fire countdown, enable toggle, Run-now, log tail).
5. **P4 — calendar UI**: Week page (day-of-week grid), Month page, edit
   dialogs with occurrence preview + DST warnings.
6. **P5 — pruner + hardening**: system.prune timer, elapsed-one-shot cleanup,
   Jan-1 consistency pass, backpressure, action timeouts.
7. **P6 — packaging + per-OS QA**: NSIS/MSI + WebView2 bootstrap, signed dmg +
   notarization, deb/AppImage; autostart QA per OS; idle-footprint measurement
   gates (hidden-window RSS, wakeups/min, CPU over 30 min idle) on Win 11,
   macOS, Ubuntu GNOME/Wayland, KDE/X11. Signing/notarization is release
   *work*, not a final-week chore.

## 7. Consolidated risks & gotchas (deduped, ranked)

1. **Sleep/hibernate oversleep** — monotonic clocks freeze during suspend;
   chunked ≤30 s sleeps + wall-vs-monotonic divergence check are mandatory;
   misfire pass on every resume; test with a real lid-close.
2. **DST** — gap/fold policies explicit per timer; intervals on elapsed time
   (else a 30-min interval fires twice at fall-back); surface next-fire in
   local + UTC + offset in the GUI ("why did my 9am alarm fire at 8?" is the
   most common scheduler bug report in history).
3. **JS timer throttling** — any timing in the webview dies at ≥1-min
   granularity when hidden. All timing in Rust; webview only poked to refresh.
4. **File watching is lossy on every OS** — events are hints; re-scan is
   truth; slot dir + db must be on local disk.
5. **Windows atomic rename** — naive rename fails if destination exists; use
   `NamedTempFile::persist`/ReplaceFile semantics; retry with backoff around
   AV/indexer transient locks; never parse a partial file.
6. **Backward clock jumps** — distinguish from suspend; never re-fire done
   one-shots; guard with `last_fired` + run claims.
7. **Mass-fire after resume/restart** — coalesce + `max_concurrent_actions`
   cap + queue; without it a weekend of missed timers fork-bombs the login.
8. **Linux tray** — needs StatusNotifierItem/libappindicator; GNOME needs the
   AppIndicator extension (detect + degrade gracefully); tray bugs differ
   between deb and AppImage builds; **no Flatpak/Snap in v1**.
9. **Per-OS autostart quirks** — macOS 13+ Login Items approval UX + orphan
   LaunchAgent entries on uninstall (SMAppService path if polish needed);
   Windows Run-key path breaks if the binary moves (re-register on startup);
   XDG autostart absent on bare WMs; autostart ≠ wake-from-sleep — never
   promise firing while the machine is off.
10. **Slot-dir abuse** — malformed/huge/looping input: quarantine with
    `.err.json`, size caps, symlink rejection, no shell, no elevation.
11. **Hot interval timers vs disk** — a 1 s timer must not turn the store into
    a metronome: batch/debounce next-fire persistence; per-line JSONL fsync
    off.
12. **WAL sidecars + backup tools** — checkpoint TRUNCATE on clean exit;
    document `-wal`/`-shm`.
13. **WebKitGTK rendering quirks** — keep calendar CSS plain; test WebKitGTK
    first, it is always the outlier of the three webviews.
14. **WebView2 runtime** on Windows — bundle the evergreen bootstrapper;
    decide online vs offline install.
15. **tzdata staleness** — `chrono-tz` embeds the IANA db; a DST-rule change
    requires a release; record tzdata version in `meta`, retest after bumps.
16. **Antivirus/SmartScreen false positives** — code-sign + notarize from day
    one.
17. **Battery** — second-resolution wakeups keep CPUs busy; expose
    `AccuracySec`-style per-timer slack, default a few seconds for
    high-frequency timers.
18. **Never `deny_unknown_fields`** on any persisted/wire JSON — forward
    compatibility of the integration contract.

---
*Synthesis method: 4/4 convergent picks adopted as-is; forks resolved by
majority + strength-of-evidence (a cited upstream warning or a demonstrated
race beats a preference), each recorded in §4 with its resolution rationale.*
