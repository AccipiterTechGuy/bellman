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
| `no_ack` | Bellman — nobody picked it up | yes |
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
- **A heartbeat restarts the countdown.** An app reporting progress is alive and must not be
  timed out. This is what makes heartbeats worth sending.
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
  - **Only while the run is still current.** A reply for a run the folder has already moved
    past — the timer fired again — is rejected as `superseded`, not applied. Revision reaches
    back through time, never across runs.

Cheap to implement precisely because Bellman is a scheduler — a watchdog deadline is one
entry in the heap it already runs.

**R9 — a reply is data, never a command.** Bellman parses, validates and logs it. It must
never launch, execute, schedule or modify anything because an app said so. Worst case for a
hostile reply is one bad log line.

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

### `reply.json` — `bellman-reply/1`

**The only file an integrating app writes.** Overwritten at each step; never read back.

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
  "status_path": "~/.bellman/timers/bulb-test-3f1a/status.json"
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
│   ├── timer.json      what the timer IS        (Bellman writes, you read)
│   ├── status.json     the CURRENT run          (Bellman writes, everyone reads)
│   └── reply.json      where the app answers    (the app writes, Bellman reads)
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

Safe precisely because the event log survives: "what fired, and when" is answerable forever
from `events.current.jsonl` regardless of which folders still exist.

Two cases that follow:

- **An open run at delete time.** Close it first — mark the run terminal (`cancelled`) in the
  event log, *then* remove the folder. An app whose `status.json` has vanished must read that
  as cancelled, not crash. Do not delete out from under a live run silently.
- **Orphan folders.** A crash between the database delete and the folder delete leaves a tree
  with no timer. The pruner already does orphan sweeps for slots — extend it here.

## No history in the folder — a new run wipes it

The folder holds the **current** run only. When a timer fires again, `status.json` and
`reply.json` are overwritten fresh; nothing from the previous run is kept there.

There is deliberately no `runs/` directory. History already has two homes — the append-only
`events.current.jsonl`, and the Run history page in the GUI (`ui/src/HistoryPage.svelte`). A
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
