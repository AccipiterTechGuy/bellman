# Bellman JSON — the normalised shape

Locked design. Everything Bellman writes or reads obeys these rules. Where today's code
differs, IK1 migrates it.

## Why

Three JSON shapes grew in three separate cards and drifted:

- **`kind` means two different things.** Event log: `"kind": "fired"` (the event). Fire
  notification: `"kind": "daily"` (the occurrence). Same field, opposite meaning, in two
  files the same integrator reads. This is the trap worth fixing.
- **Four names for a moment in time**: `ts`, `fired_at`, `next_fire`, `claimed_at`.
- **No version on the event log.** Slot messages carry `schema`; `EventRecord` carries
  nothing, so a consumer cannot version-check.
- **Two status vocabularies**, `SlotStatus` and `EventKind`, overlapping but distinct.

None of it is broken. All of it bites the first third party writing an integration.

## The rules

**R1 — every JSON carries `schema`.** `bellman-event/1` for log lines, `bellman-slot/1` for
slot messages. No exceptions, including the event log.

**R2 — top-level `kind` always means the event kind**, from the one vocabulary in R5. The
occurrence type is `occurrence_kind`. `kind` never means two things again.

**R3 — every timestamp ends in `_at`.** One exception: `scheduled_for`, which is an
*intent*, not something that happened. So `ts` → `logged_at`, `next_fire` → `next_fire_at`.

**R4 — correlation is fixed.** `run_id` joins everything about one firing. `timer_id` joins
everything about one timer. Both present on every message where they apply.

**R5 — one run-state vocabulary**, shared by the event log and the reply channel:

| state | who writes it | terminal? |
|---|---|---|
| `registered` | Bellman | — |
| `fired` / `fired_late` | Bellman | no — run is open |
| `acknowledged` | **the app** | no |
| `running` | **the app** (optional heartbeat) | no |
| `completed` | **the app** | yes |
| `failed` | **the app** | yes |
| `no_ack` | Bellman — **no acknowledgement was received** (a filesystem read leaves no trace, so "nobody read it" is unknowable — say what was observed) | **revisable** — a late valid reply supersedes it while the run is still current |
| `cancelled` | Bellman — the timer was deleted while its run was open | yes |

**What counts as pickup:** any valid reply state (`acknowledged` / `running` / `completed` /
`failed`) **or** the existing slot-feed cursor (`ack_through`) advancing past this run's
event. The slot feed is a real, durable acknowledgement path that predates the reply channel
— declaring `no_ack` while it shows the app acked would be Bellman contradicting its own
records. The pickup deadline is its own persisted deadline; the existing `ack_grace` constant
may seed its default, but the two jobs (pickup timeout vs pruning grace) stay separately
named and separately configurable.
| `failed` (`failure_kind: "timed_out"`) | Bellman — only if the app set `error_detection` | **revisable** — a late app reply supersedes it |
| `skipped_misfire`, `coalesced`, `pruned`, `wake_*`, `year_recalibrate` | Bellman | as today |

**R6 — readers stay tolerant.** Unknown fields ignored, never `deny_unknown_fields`
(BUILD_PLAN rule 7). This is what lets the shape grow without breaking old consumers.

**R7 — grace on pickup, never on completion.**

- **Pickup** has a grace window (`ack_grace`, default 60s). Bellman wrote a file; asking
  whether anything read it is a fair question with a knowable answer. Lapsed ⇒ `no_ack`.
- **Completion has no timeout, ever.** How long the other program takes is unknowable —
  seconds, minutes, hours. A run stays open until the app closes it. **Nothing ever
  auto-completes.**

An unfinished run is not `failed` — `failed` means the app *said* it failed. It stays
`running` and **ages**, so the history reads "running for 3 days", which is the truth and is
obviously wrong to a human without Bellman pretending to know why.

**R8 — the estimate is advisory unless the app OPTS IN to a watchdog.**

By default `expected_secs` is display-only: "running, overdue — 47m elapsed, expected 10m".
Bellman never acts on it. Guessing another program's duration is not Bellman's business.

But an app may set **`error_detection: true`** in its reply, and that changes the contract:
the app is **consenting** to be watched against a deadline **it declared itself**. That is
not Bellman guessing — it is the app asking. Default is `false`; silence means advisory.

When enabled:

- **Deadline = `expected_secs × factor`**, configurable, default forgiving (≈2×). Failing at
  exactly the stated second means every app pads its estimate and the field becomes fiction.
- **The countdown runs on Bellman's own monotonic clock**, started at the moment Bellman
  *receives* the reply — never on the app's timestamp. An app's wall clock can be skewed,
  can jump on NTP correction, and can be wrong on purpose: trusting it means a slow clock
  fails a healthy app and a fast one extends its own deadline forever. The app's timestamps
  stay in the file as display and history data; they are never arithmetic inputs.
  - A **persisted wall-clock deadline** is written alongside it, used only to reconstruct the
    countdown after a restart — a monotonic clock does not survive the process.
  - Clock jumps and DST therefore do not disturb an active countdown, which is the point.
- **A heartbeat restarts the countdown.** An app reporting progress is alive and must not be
  timed out. This is what makes heartbeats worth sending. The restart is timed from Bellman's
  receipt, per the rule above.
- **The outcome is `failed` with `failure_kind: "timed_out"`.** One state to reason about,
  and the distinction is preserved where it matters: `reported` means the app said it
  failed, `timed_out` means the app went quiet past its own deadline. Those need different
  reactions from a human. (`no_ack` stays its own state — nobody ever picked the run up.)
- **Marking is not killing.** Bellman flags the run; it does not terminate the process. If
  Bellman launched it, killing may be a separate opt-in later — a different decision that
  must not ride along silently with this one.
- **A late reply REVISES the state.** `completed` arriving after the run was marked failed
  moves it to `completed`. The state always shows the best available truth; nothing stays a
  lie. Three properties make that safe:

  - **The log does not flip.** `status.json` holds the latest state; `events.current.jsonl`
    is append-only and keeps both facts, so "marked failed 05:15, completed 05:22" survives.
    That sequence is the interesting story and would be lost if only the state remained.
  - **One direction only.** An app's own report always beats Bellman's inference — Bellman
    *deduced* silence, the app *knows*. Bellman must never flip an app's `completed` back to
    failed. Bellman's guesses are overridable; the app's claims are not.

    The app may also revise **itself**: `failed` then `completed` on the still-open run is
    accepted, exactly like revising a watchdog verdict. One rule covers both — the app's
    latest terminal report wins, whatever it replaces. (Moving back to a *non-terminal*
    state is different and is refused; it reopens a closed run. See IK3.)
  - **Only while the run is still current.** A reply for a run the folder has already moved
    past — the timer fired again — is rejected as `superseded`, not applied. Revision reaches
    back through time, never across runs.

Cheap to implement precisely because Bellman is a scheduler — a watchdog deadline is one
entry in the heap it already runs.

**R9 — a reply is data, never a command.** Bellman parses, validates and logs it. It must
never launch, execute, schedule or modify anything because an app said so. Worst case for a
hostile reply is one bad log line.

**R10 — the database commits before the folder changes, and reads it before firing.**

The folder is a view (see the tree section). Two ordering rules make that true in practice
rather than in principle:

- **Ingest before superseding — the pre-fire barrier.** Before the fire transaction runs,
  Bellman **synchronously reads and ingests the current run's reply file**. An app may have
  written a complete, valid `completed` that the watcher simply has not processed yet;
  superseding without looking would log "outcome unknown" for a run whose outcome was
  sitting on disk. The watcher and the scheduler must go through the same serialization
  point, or they will race each other to the same file. (A reply written *after* the barrier
  read still loses — that window is microseconds and the design already accepts it loudly as
  `superseded`. The barrier fixes the common case: watcher lag, not true simultaneity.)
- **One transaction per fire.** The previous run's final known state — including anything the
  barrier just ingested — its `superseded` event if the run was still open, the new `run_id`,
  the `fired` event and any pending log lines commit to SQLite **together**. Only then is
  `status.json` rewritten. Crash before the commit and the previous firing is still current;
  crash after it and startup rebuilds the file. There is no window where the folder claims
  something the database never recorded.
- **Startup reads replies before the scheduler fires anything.** An app can answer while
  Bellman is stopped. If the scheduler runs first, the next fire overwrites that reply and
  it is lost **silently** — the worst kind of loss, because nothing anywhere records that it
  happened. So: scan every `reply.json`, fold in what is valid and still current, flush
  pending log lines, rebuild stale or missing files from the database, and only then start
  delivery.

**R11 — one writer owns the event log, and "one" means across PROCESSES.**

An in-process mutex is not the rule — the rule is interprocess. `EventLog` is opened today
from the GUI process (`src-tauri/src/state.rs:122`, `:219`, `commands.rs:628`), the CLI
(twice in `bellman-cli/src/commands.rs`), the pruner, `run_now` and `log_query`. Those are
**separate OS processes** with independent handles. The live hazard: the pruner rotates
(renames) `events.current.jsonl` while another process keeps appending through its old handle
— post-rotation events land split between the archive and the new current file, and on
Windows the rename itself can fail against an open handle.

The shape that fixes it:

- **Every producer enqueues into the SQLite outbox** — SQLite already serialises across
  processes, which is the whole reason it is the funnel.
- **One publisher, elected by an interprocess lease** (OS file lock), performs every append
  **and** every rotation. A CLI running while the GUI is up enqueues and lets the GUI's
  publisher drain; a CLI running alone takes the lease itself.
- **Append errors surface.** `emit()` currently discards them (`let _ = log.append(&rec)`,
  `actions/runner.rs:128`) — under this rule a failed append leaves the outbox row in place
  for retry and is never silently dropped.

A line is durable when it is flushed, not when it is created: enqueue, append, flush, then
mark published, and retry after a failed write or a restart.

**Delivery is at-least-once, and the file is honest about it.** A crash between flush and
mark-published means the retry appends the same event **again** — `event_id` identifies the
duplicate, it does not prevent it; nothing about an id makes a blind append idempotent. Two
duties follow:

- **The publisher checks before retrying**: on startup, scan the current file's tail for the
  pending `event_id`s and skip the ones already physically present.
- **Every reader dedupes by `event_id` anyway** — GUI, `log_query`, anything counting. The
  publisher check shrinks the window; the reader rule is the guarantee.

**The outbox must also empty.** A published row is deleted, not just marked; terminal run
rows are pruned on the same retention schedule as the archives; the WAL is checkpointed
periodically. Without this, the durability mechanism becomes its own unbounded growth — the
database quietly accumulates one row per event forever while the log it protects is capped.

**R12 — everything an app can send is size-capped, with numbers.**

"Bounded" without a number is not a rule anyone can implement or test. House style already
exists: `DEFAULT_OUTPUT_CAP_BYTES = 64 KB` (launch output), `DEFAULT_MAX_READ_BYTES = 256 KB`
(slot reads). The caps:

| thing | cap | over the cap |
|---|---|---|
| `reply.json`, whole file | **64 KB** | quarantined unread (existing rule) |
| `result` as stored in `status.json` | **32 KB** | truncated, `result_truncated: true` |
| `result` as carried on the log event | **2 KB** | truncated, `result_truncated: true` |
| `reason` / `progress` free text | **1 KB** | truncated |
| one JSONL event line, total | **4 KB** | must not happen if the above hold — assert it |

The asymmetry between 32 KB and 2 KB is deliberate: `status.json` is the current run and is
overwritten next fire, so it can afford detail; the log is append-only and keeps everything
for the retention window, so every byte is multiplied by history. The log line keeps the
head of the result plus the truncation flag — enough to grep, not enough to bloat.

**A large output is the app's to store, not Bellman's.** The documented convention for big
results: write the payload somewhere the app owns and reply with a summary —
`result: { "summary": "…", "path": "/app/owned/file", "sha256": "…" }`. Under R9 the path is
**data**: displayed as text, never opened, followed or executed by Bellman.

**`duration_ms` has one formula.** Bellman's own clock, both ends: monotonic elapsed from
publishing the fire to **ingesting** the terminal reply. App timestamps are never subtracted
— an app may skip `acknowledged_at` entirely (legal), and a skewed app clock must not produce
a negative or absurd duration. One anchor pair, computed by Bellman, clamped at zero, present
on the terminal event only. (`fired → completed` directly, no ack: same formula, no special
case.)

**Archives are compressed.** `events.current.jsonl` stays plain text — grep-ability of the
live log is a feature. Rotated archives are compressed on rotation; JSONL compresses hard
because every line repeats the same field names, so the 1 GB ceiling holds several times the
history. Use gzip via `flate2`, which is already in the dependency tree — do not add a
compression crate for this. `log_query` and the Run-history GUI must read both plain and
compressed archives.

## The shapes

Every JSON Bellman writes or reads, in full. Times are UTC; the example timer fires daily at
08:00 Europe/Helsinki = 05:00 UTC.

### `timer.json` — `bellman-timer/1`

```json
{
  "schema": "bellman-timer/1",
  "timer_id": "3f1a8c2e-6b41-4d9e-8a17-0c2f5d7e9b33",
  "name": "bulb-test",
  "enabled": true,
  "tz": "Europe/Helsinki",
  "occurrence": { "kind": "daily", "time": "08:00:00" },
  "action": {
    "type": "launch",
    "command": "/usr/local/bin/bulb",
    "args": ["--on", "15"]
  },
  "created_at": "2026-07-28T19:12:04Z",
  "created_by": { "source": "slot", "app_name": "lightbulb" },
  "next_fire_at": "2026-07-31T05:00:00Z",
  "last_run": {
    "run_id": "9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
    "state": "completed",
    "completed_at": "2026-07-30T05:00:15Z"
  },
  "note": "Written by Bellman. The database is the source of truth — editing this file has no effect."
}
```

`occurrence.kind` is nested and therefore unambiguous — R2 concerns the **top level** only.

### `status.json` — `bellman-run/1`

Written by Bellman only. The lower block is folded in from the app's `reply.json`.

```json
{
  "schema": "bellman-run/1",
  "state": "completed",

  "run_id": "9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
  "timer_id": "3f1a8c2e-6b41-4d9e-8a17-0c2f5d7e9b33",
  "timer_name": "bulb-test",
  "occurrence_kind": "daily",
  "scheduled_for": "2026-07-30T05:00:00Z",
  "fired_at": "2026-07-30T05:00:00Z",

  "app_name": "lightbulb",
  "acknowledged_at": "2026-07-30T05:00:00Z",
  "expected_secs": 15,
  "heartbeat_at": "2026-07-30T05:00:07Z",
  "progress": "bulb on, 7s elapsed",
  "completed_at": "2026-07-30T05:00:15Z",
  "result": { "on_duration_secs": 15.02 }
}
```

The other endings, same file:

```json
"state": "failed",  "failure_kind": "reported",  "failed_at": "…", "reason": "GPIO write refused"
"state": "failed",  "failure_kind": "timed_out", "failed_at": "…"
"state": "no_ack",  "no_ack_at": "…"
```

Mid-run it reads `"state": "running"` with no `completed_at`. If the app dies it stays exactly
that way and ages — it never becomes `completed`, and it is not `failed` unless the app said
so or a watchdog the app opted into expired.

### `reply-<run8>.json` — `bellman-reply/1`

**The only file an integrating app writes.** Overwritten at each step; never read back.

**The filename is per-run** — `reply-` + the first 8 hex of the `run_id` (`reply-9f2c1d77.json`).
One fixed name shared across generations would let a slow previous app atomically replace the
next run's channel, and any "restore" by Bellman would race the current app's own writes —
a read-check-write on a single path can always overwrite a valid reply written in between.
Per-run names end the problem structurally: each generation owns its own file, nobody ever
writes over anybody, and Bellman never restores anything. A write to a previous run's file is
ingested (or `superseded`) and the stale file deleted; normally exactly one `reply-*.json`
exists. The fire notification carries the exact path as `reply_path`, so an app never
constructs the name.

```json
{
  "schema": "bellman-reply/1",
  "run_id": "9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
  "app_name": "lightbulb",
  "state": "completed",
  "completed_at": "2026-07-30T05:00:15Z",
  "result": { "on_duration_secs": 15.02 }
}
```

With the opt-in watchdog, mid-run:

```json
{
  "schema": "bellman-reply/1",
  "run_id": "9f2c…",
  "app_name": "backup-tool",
  "state": "running",
  "error_detection": true,
  "expected_secs": 900,
  "heartbeat_at": "2026-07-30T05:07:00Z"
}
```

An app may write `acknowledged`, `running`, `completed`, `failed`. Never `fired`, never
`no_ack` — those are Bellman's.

### Event log line — `bellman-event/1`

```json
{
  "schema": "bellman-event/1",
  "logged_at": "2026-07-30T05:00:00Z",
  "kind": "fired",
  "event_id": "aa11b2c3-…",
  "timer_id": "3f1a…",
  "run_id": "9f2c…",
  "timer_name": "bulb-test",
  "scheduled_for": "2026-07-30T05:00:00Z"
}
```

Changes from today: gains `schema`, and `ts` becomes `logged_at`.

### Fire notification — `bellman-slot/1`

```json
{
  "schema": "bellman-slot/1",
  "kind": "fired",
  "occurrence_kind": "daily",
  "timer_id": "3f1a…",
  "timer_name": "bulb-test",
  "run_id": "9f2c…",
  "scheduled_for": "2026-07-30T05:00:00Z",
  "fired_at": "2026-07-30T05:00:00Z",
  "status_path": "~/.bellman/timers/bulb-test-3f1a/status.json",
  "reply_path": "~/.bellman/timers/bulb-test-3f1a/reply-9f2c1d77.json"
}
```

Changes from today: top-level `kind` becomes the **event** (`fired`), the occurrence moves to
`occurrence_kind`, and `status_path` is added so an app never has to guess where to reply.

## Migration

Pre-1.0 and the README says formats can change, so this is a clean break with a version
bump rather than a compatibility layer. Bump `bellman-event/1`; readers already ignore
unknown fields, so additive parts cost nothing.

Renames: `ts` → `logged_at`; `next_fire` → `next_fire_at`; fire-notification `kind` →
`occurrence_kind`, with top-level `kind` becoming the event kind.

---

# The per-timer folder tree

A human-browsable view of state. Open a folder in a file manager and read what happened —
no CLI, no log parsing.

```
~/.bellman/timers/
├── README.txt
├── bulb-test-3f1a/
│   ├── timer.json           what the timer IS        (Bellman writes, you read)
│   ├── status.json          the CURRENT run          (Bellman writes, everyone reads)
│   └── reply-9f2c1d77.json  where the app answers    (the app writes, Bellman reads)
└── morning-backup-7b22/
```

**This tree is a VIEW, not the record.** The database is the source of truth for timers, and
`logs/events.current.jsonl` is the durable history of everything that fired. The folders can
be deleted, rebuilt or lost without losing anything permanent — that is what makes the rules
below safe.

Keep it separate from `slots/`, which is the transient request/response **channel**. Two
trees, two jobs.

## Naming

`<slug>-<short-id>/` — readable *and* unique. The slug rule must be identical on all three
platforms, and must handle Windows' reserved names (`CON`, `PRN`, `AUX`, `NUL`, …) and its
refusal of trailing dots, or a timer that works on Linux breaks on Windows.

**Renaming a timer does not rename the folder.** The path stays stable because integrations
depend on it; the live name lives in `timer.json`.

## Deletion — decided

**Deleting a timer deletes its folder.** No tombstone, no orphan tree.

Safe precisely because the event log survives: "what fired, and when" is answerable from
`events.current.jsonl` and its archives regardless of which folders still exist — **for the
log's retention window, not forever**. Archives are pruned (default 30 days / 1 GB), so every
place the docs or GUI describe history must say "30-day history", never "permanent". A
retention window nobody states reads as a data-loss bug the first time someone looks for a
31-day-old run.

Two cases that follow:

- **An open run at delete time.** Close it first — mark the run terminal (`cancelled`) in the
  event log, *then* remove the folder. An app whose `status.json` has vanished must read that
  as cancelled, not crash. Do not delete out from under a live run silently.
- **Orphan folders.** A crash between the database delete and the folder delete leaves a tree
  with no timer. The pruner already does orphan sweeps for slots — extend it here.

## No history in the folder — a new run wipes it

The folder holds the **current** run only. When a timer fires again, `status.json` and
`reply.json` are overwritten fresh; nothing from the previous run is kept there.

There is deliberately no `runs/` directory. History has exactly **one durable home** — the
append-only `events.current.jsonl` and its archives. The Run history page in the GUI
(`ui/src/HistoryPage.svelte`) is a **reader** of that home, not a second copy — there is no
independent GUI store, and wording that implies two durable homes overstates what exists. A
third copy in the folder would buy only "browse past runs in a file manager", and would cost
size caps, age caps, per-timer count caps, pruning and a freeze-before-wipe ordering rule.
Not worth it.

**Consequence, accepted:** if a run is still open when the timer fires again, its outcome is
never known. The new run overwrites it. Log `superseded` **loudly** — it means the interval
is shorter than the app takes, which is a misconfiguration worth seeing.

## `timer.json` is readable, not authoritative

Bellman writes it, humans read it. Hand edits are ignored — the database wins. The file
carries a `note` field saying so, because someone will open it, change the time, and wonder
why nothing happened.
