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

## Prior art (background reading while building)

Projects worth understanding before building the matching module. These are
**prior art we read, not sources we copy from**: the rule is read the idea,
then implement it independently. No code is lifted from any of them — where we
depend on someone's work we do it properly, as a declared dependency with its
licence (`croner`, `notify`, `rusqlite`, Tauri). Anything reimplemented here is
our own code, and where our design diverges from theirs the table says so.

Several of these are cloned locally for offline reading during development;
those clones live outside this repository and nothing from them is vendored in.

| Repo / link | Use it for |
|---|---|
| https://github.com/tauri-apps/tauri + https://v2.tauri.app/learn/system-tray/ | Tauri v2 patterns: tray, windows, IPC commands |
| https://v2.tauri.app/plugin/autostart/ · https://v2.tauri.app/plugin/single-instance/ | The two required plugins — wiring examples |
| https://github.com/vjousse/pomodorolm | Small real-world **Tauri tray timer app** — the closest existing shape to Bellman's shell; useful for understanding tray + window lifecycle on this stack |
| https://github.com/Hexagon/croner-rust | The cron-variant parser we ship; seconds field + Quartz `L/W/#` |
| https://github.com/mvniekerk/tokio-cron-scheduler | **Read, don't depend**: how a tokio scheduler loop is structured; we replace its all-in-memory model with the horizon window |
| https://github.com/notify-rs/notify + https://docs.rs/notify-debouncer-full | Slot-dir watching; issue tracker documents every OS's lossy-event caveat |
| https://github.com/rusqlite/rusqlite | Store layer; `bundled` feature, WAL pragmas |
| https://github.com/KDE/kalarm | **Richest per-alarm data model in OSS** — per-alarm tz, late-cancel grace, recurrence exceptions, Feb-29 policy. A benchmark for how much per-timer nuance a mature scheduler ends up needing |
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
│  ├─ pruner/       system.prune weekly internal timer (visible in GUI) +
│  │                startup catch-up; Jan-1 consistency pass
│  └─ platform/     wake/ — RTC wake-from-sleep (research synthesis 2026-07-27):
│                   trait Wake { program_wake(utc), cancel_wake(),
│                   capability() -> WakeCapability } with per-OS impls behind
│                   cfg: linux.rs (timerfd CLOCK_REALTIME_ALARM via rustix;
│                   sysfs wakealarm cooperative fallback), windows.rs
│                   (SetWaitableTimer fResume=TRUE, absolute UTC; power-policy
│                   probe), macos.rs (XPC client to the helper daemon).
│                   single_next_wake.rs — the bridge: elect min next_due of
│                   wake-enabled timers, rearm() on store mutation / pre-suspend
│                   / resume / start, arm at wake_utc − 45 s; the wake event
│                   NEVER fires actions (normal loop + misfire pass do).
├─ crates/bellman-cli/   timer CRUD/run-now + slot-submit + scan/task +
│                        calendar/agenda (stable --json envelopes)
├─ helpers/macos-wake-daemon/  tiny root daemon (SMAppService, macOS 13+):
│                   XPC schedule_wake/cancel_my_wakes, client code-sig check,
│                   IOPMSchedulePowerEvent one-shots tagged "com.bellman.wake";
│                   never pmset repeat / cancelall. Bundled + signed in P6.
├─ src-tauri/            tray, single-instance, autostart, on-demand windows,
│                        first-run wizard (see "First-run wizard" below)
└─ ui/  (Svelte 5)       tabs: All timers | Week | Month | Run history | Settings

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

## Wake-from-sleep: settings page + first-run wizard + per-OS setup (2026-07-27)

Full design: `docs/research/synthesis.md` (RTC pair, adopted verdicts: Linux
timerfd/CAP_WAKE_ALARM, Windows waitable timer policy-gated, macOS root daemon
via SMAppService or Disabled). Wake is an OPTIONAL enhancement everywhere —
probe-driven, never elevation at runtime, never an unexpected prompt; when
Disabled the misfire-on-resume pass covers the gap.

### Settings page (new UI tab)

- **Wake from sleep** section, per-OS aware via `capability()`:
  - Master toggle "Allow Bellman to wake this machine" (config.json:
    `wake.enabled`, default follows the wizard answer). Greyed with the reason
    sentence when capability is Disabled.
  - Status line, always visible: `Wake from sleep: ON via <mechanism>
    (<caveat>)` or `OFF — <DisabledReason sentence>` — same string logged as
    the `wake_capability` JSONL event.
  - **Fix-it buttons** (each user-initiated, the ONLY places elevation ever
    appears): Windows policy-blocked → elevated
    `powercfg /setacvalueindex SCHEME_CURRENT SUB_SLEEP RTCWAKE 1`;
    macOS helper not enrolled → one-click SMAppService register + deep-link to
    System Settings → Login Items; Linux pre-systemd-254 → show the udev-rule
    snippet with a copy button (admin applies it manually).
  - Re-probe button + auto re-probe on resume / power-source change (Windows
    AC↔DC flips the answer — show which rail is active).
- Also on Settings: autostart toggle (moved from tray-only), misfire defaults,
  `max_concurrent_actions`, pause-all vacation mode. Per-timer `wake_machine`
  stays in the timer edit dialog (default false, greyed when Disabled).

### First-run wizard (install window, src-tauri)

Runs once on first launch (and re-runnable from Settings → "Run setup again"):

1. **Autostart?** yes/no (existing spec choice — XDG desktop file / Run key /
   Login Item). On Linux XDG autostart is ALSO what preserves the ambient
   CAP_WAKE_ALARM lineage — say so in the wizard body text.
2. **"Do you want to set up automatic wake-up from sleep?" yes/no.**
   - yes → run the per-OS wake setup (below), then show the live probe result
     as the wizard's confirmation screen (the Settings status line, verbatim).
   - no → `wake.enabled=false`; feature stays available later via Settings.
3. **Dependency check** (informational, per-OS): Windows — WebView2 runtime
   presence (evergreen bootstrapper already in the installer, P6); Linux —
   webkit2gtk + tray AppIndicator extension detection (GNOME: link to install
   it, degrade gracefully); macOS — nothing extra. Missing optional deps are
   listed with install hints, never blocking.

### Per-OS wake setup (what the wizard's "yes" actually does)

| OS | First-install action | Elevation? |
|---|---|---|
| Linux, systemd ≥ 254 | **Nothing to install** — ambient CAP_WAKE_ALARM makes the in-process timerfd probe pass on a local session. Wizard just runs the probe. | none |
| Linux, older / probe EPERM | Offer the sysfs fallback: display the one-line udev rule (`ETC` snippet granting wakealarm group-write) with copy button; user applies as admin, hits Re-probe. | admin, manual, optional |
| Windows | **Nothing to install** — user-process waitable timers. Probe checks the Allow-wake-timers policy per power rail; if Disabled-by-policy the wizard offers the elevated powercfg fix-it (UAC prompt, user-initiated). | optional UAC |
| macOS | Register the bundled wake daemon via SMAppService → macOS shows the Login Items approval; wizard deep-links and waits, then probes through the daemon (sentinel schedule/cancel round-trip). Decline ⇒ Disabled(HelperAwaitingApproval), feature reads as optional, never broken. | one-time approval |

No installer-time scripts run outside these wizard-driven, user-answered steps —
capability is always re-derived by the runtime probe, never assumed from what
the installer did.

## Dev-machine toolchain (Linux, what we actually installed — 2026-07-27)

Verified from apt history + tool versions on the build box (Mint, x86_64).
Three commands + the cargo install:

```sh
# 1. Rust toolchain (rustup) → rustc 1.97.1
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Tauri v2 Linux system deps (verbatim from apt history)
sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev

# 3. Node 24 via nvm → v24.13.0 (ui/ toolchain)
nvm install 24

# then: Tauri CLI → tauri-cli 2.11.4
cargo install tauri-cli --locked
```

These four are exactly what a fresh contributor machine (or CI runner) needs
before `cargo tauri dev` works; P6's CI recipe starts from this list.

### Additionally, to RUN the GUI test suite (added 2026-07-29)

```sh
# WebKit's WebDriver server — lets the QA harness drive the app from inside the
# webview instead of injecting synthetic mouse/keyboard events into the desktop.
sudo apt install -y webkit2gtk-driver          # → WebKitWebDriver 2.52.3

# Tauri's WebDriver bridge (no root needed)
cargo install tauri-driver --locked

# Python harness deps (venv recommended; system packages also fine)
python3 -m venv /tmp/bellman-qa-venv
/tmp/bellman-qa-venv/bin/pip install selenium pillow python-xlib
# X tools used by the harness: Xvfb, metacity, wmctrl
```

**The `webkit2gtk-driver` version must match the installed `libwebkit2gtk-4.1`**
(both 2.52.3 here) — a mismatched driver fails to attach with an unhelpful error.

A window manager is also required on the headless QA display, but needs no
install on Mint: `metacity` and `muffin` ship with the desktop. Use
`metacity --sm-disable` (via `scripts/qa_display.sh`).

Entry point: `scripts/run_gui_qa.sh p4b` (isolated Xvfb + private D-Bus +
tauri-driver). See `docs/QA_P4b.md`. Do **not** point the harness at the
operator session — that path is what this prerequisite block exists to replace.
See `docs/archive/qa_isolated_display_no_input_hijack.md` (shipped).

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

### P7 — wake-from-sleep (RTC) + Settings page + first-run wizard
Board position: after C10 (packaging), before C11 (validation) — the macOS
daemon rides the P6 bundle/signing machinery, and C11 then validates wake as
part of the full system.
`platform::wake` trait + linux.rs/windows.rs/macos.rs impls + probes exactly
per `docs/research/synthesis.md` §2–3; single-next-wake bridge (§4) wired to
store mutations, pre-suspend hooks (login1 inhibitor / WM_POWERBROADCAST /
daemon IORegisterForSystemPower), resume and start; macOS helper daemon +
SMAppService enrollment; Settings tab (status line, master toggle, fix-it
buttons, re-probe); first-run wizard (autostart? / wake-up? / dependency
check); per-timer `wake_machine` flag in edit dialogs; `wake_capability` JSONL
events; packaging amendment — daemon in the dmg, wizard in all three builds.
**Exit gate:** Linux (this box, systemd 255): timerfd probe Enabled from a
desktop launch AND Disabled(NoPermission) from a daemon-descended shell (both
observed live in research); arm − suspend − RTC resume − timer fires once via
the normal loop − displaced-alarm restore on the sysfs fallback path. Windows +
macOS: probe decision tree unit-tested against mocked API answers (real-HW QA
lands in C11). Wizard yes/no both paths leave config + probe + Settings line
consistent; declining the macOS helper reads as optional, not broken.

## Top risks to re-read before each phase
(1) suspend oversleep — chunked sleeps mandatory; (2) DST — explicit policies,
elapsed-time intervals; (3) webview JS throttling; (4) lossy file watchers;
(5) Windows rename semantics; (6) backward clock jumps; (7) resume mass-fire;
(8) Linux tray/AppIndicator; (9) autostart quirks per OS; (10) slot-dir abuse.
Full ranked list: synthesis §7.
