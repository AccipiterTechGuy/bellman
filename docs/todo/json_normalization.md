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

**R8 — the app's estimate is advisory only.** An app may declare `expected_secs` when it
acknowledges. Bellman displays it — "running, overdue: 47m elapsed, expected 10m" — and
**never acts on it**. It never kills, never marks failed, never closes a run. It is computed
at read time, so it costs no timer and no wakeup.

**R9 — a reply is data, never a command.** Bellman parses, validates and logs it. It must
never launch, execute, schedule or modify anything because an app said so. Worst case for a
hostile reply is one bad log line.

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
│   ├── timer.json      what the timer IS      (Bellman writes, you read)
│   ├── status.json     the CURRENT run        (Bellman + the app write)
│   └── runs/           one file per past run  (frozen at close)
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

**Deleting a timer deletes its folder, including `runs/`.** No tombstone, no orphan tree.

Safe precisely because the event log survives: "what fired, and when" is answerable forever
from `events.current.jsonl` regardless of which folders still exist.

Two cases that follow:

- **An open run at delete time.** Close it first — mark the run terminal (`cancelled`) in the
  event log, *then* remove the folder. An app whose `status.json` has vanished must read that
  as cancelled, not crash. Do not delete out from under a live run silently.
- **Orphan folders.** A crash between the database delete and the folder delete leaves a tree
  with no timer. The pruner already does orphan sweeps for slots — extend it here.

## `runs/` retention — OPEN

Needs a decision before build. The hazard is not disk, it is browsability: Bellman supports
interval timers at second resolution, so a per-minute timer produces **43,200 files in 30
days**. A folder like that is useless to the human this tree exists for.

Recommended default: **keep the last 50 runs per timer, and nothing older than 30 days,
whichever bites first.** Thirty days matches the event-log archive policy already in place;
the count cap is what keeps the folder readable. Both configurable.

## `timer.json` is readable, not authoritative

Bellman writes it, humans read it. Hand edits are ignored — the database wins. The file
carries a `note` field saying so, because someone will open it, change the time, and wonder
why nothing happened.
