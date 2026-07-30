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
