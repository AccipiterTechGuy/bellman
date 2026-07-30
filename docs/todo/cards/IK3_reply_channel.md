# IK3 — The reply channel: `reply.json`

Repo: `~/bellman`. Design: **`docs/todo/json_normalization.md`** R5, R7–R9 and the run
lifecycle. Depends on **IK2**.

## What already exists — EXTEND it, do not duplicate it

Read these before writing anything. A parallel mechanism next to the real one is the failure
mode for this card:

- `SlotRunEvent` — the run-event feed with a monotonic `event_sequence`.
- `ack_through` — the durable un-acked cursor an app advances.
- `ack_grace` — **60s**, `DEFAULT_ACK_GRACE_SECS` in `app_config.rs`.
- The `no_ack` event kind, and `completed_at` already on run events.
- The slot watcher (notify + periodic rescan) — reuse it, do not write another.

What is missing is narrow: an app can say **"I received it"** but not **"I finished, here is
what happened."** Delivery is acknowledged; **outcome** is not reported.

## The file

One writer per file — that is the point. A shared file has a lost-update race that atomic
rename does not fix: the app reads, Bellman writes `no_ack`, the app writes back and Bellman's
change is gone.

| file | writer | reader |
|---|---|---|
| `status.json` | Bellman | everyone |
| `reply.json` | **the app** | Bellman |

`reply.json` is the entire app-facing surface.

### Bellman pre-creates the stub, pre-filled

At fire, Bellman writes **both** `status.json` and a `reply.json` stub. The stub already
carries every field the app should not have to know — matching the existing convention of
pre-generated free slot stubs.

**T0 — Bellman fires. `reply.json`:**

```json
{
  "schema": "bellman-reply/1",
  "run_id": "9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
  "app_name": "lightbulb",
  "state": null,
  "hint": "set state to acknowledged | running | completed | failed"
}
```

`app_name` is pre-filled from the timer's owner (`created_by.app_name`), so the app never
supplies it. When a timer was created by a human rather than an app there is no owner: leave
it `null` and let the first responder fill it in.

`state: null` is how Bellman tells "stub, untouched" from "the app answered". **Bellman writes
this file once and never again** — from T0 the app is its only writer.

### The app edits; it does not reconstruct

The app reads the stub, sets what changed, writes it back atomically. It never composes a
file from scratch and never has to carry `schema`, `run_id` or `app_name` in its own code.

| stays as Bellman wrote it | the app sets |
|---|---|
| `schema` `run_id` `app_name` | `state` (required) |
| | `acknowledged_at` `expected_secs` `error_detection` |
| | `heartbeat_at` `progress` |
| | `completed_at` `result` · `failed_at` `reason` |

**T1 — acknowledged:**

```json
{
  "schema": "bellman-reply/1",
  "run_id": "9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
  "app_name": "lightbulb",
  "state": "acknowledged",
  "acknowledged_at": "2026-07-30T05:00:00Z",
  "expected_secs": 15
}
```

**T2 — running (optional heartbeat):**

```json
{
  "…": "header unchanged",
  "state": "running",
  "acknowledged_at": "2026-07-30T05:00:00Z",
  "expected_secs": 15,
  "heartbeat_at": "2026-07-30T05:00:07Z",
  "progress": "bulb on, 7s elapsed"
}
```

**T3 — completed:**

```json
{
  "…": "header unchanged",
  "state": "completed",
  "acknowledged_at": "2026-07-30T05:00:00Z",
  "expected_secs": 15,
  "completed_at": "2026-07-30T05:00:15Z",
  "result": { "on_duration_secs": 15.02 }
}
```

Read-modify-write is safe here **only** because the app is the sole writer after T0. This is
the same property that made the split necessary in the first place — do not later "optimise"
it by letting Bellman touch this file again.

### Heartbeats are optional, and absence is not a state

`heartbeat_at` and `progress` are entirely optional. An app that never sends one is a normal,
fully-supported app — most will be. So:

- **Never require it.** No warning, no degraded status, no "last heartbeat: never".
- **Never display it when absent.** A missing `heartbeat_at` means the app does not do
  heartbeats; it does not mean the app is unhealthy. `status.json` and the GUI simply omit
  the field rather than showing an empty or placeholder value.
- **The watchdog works without it.** With `error_detection: true` and no heartbeats, the
  deadline is `expected_secs × factor` and never extends. That is correct behaviour, not a
  fallback.
- Absence is never a reason to fail, flag or escalate a run.

### Robustness rules

- **Only `state` is required from the app.** Everything else is optional and depends on the
  state being reported.
- **Never treat a missing field as a retraction.** If an app writes a minimal file rather than
  editing the stub, `status.json` retains what was folded in earlier. Both styles must work.
- The app must **not** be required to read `status.json`. Its whole interaction is one file.

Bellman watches it, validates, logs the transition, and folds the result into `status.json`.

### Every transition is appended to the event log

With no `runs/`, **`events.current.jsonl` is the only per-run record**. A transition that
never reaches it is gone permanently — so logging is not incidental here, it is the durability
story.

These event kinds are **new** and do not exist in `EventKind` today (the enum has
`Registered · Fired · FiredLate · SkippedMisfire · Coalesced · WakeDelivered · WakeFailed ·
NoAck · Pruned · YearRecalibrate · WakeCapability`). IK1 owns the vocabulary, so they are
added there and consumed here:

| kind | written when | carries |
|---|---|---|
| `acknowledged` | the app first answers | `app_name`, `expected_secs`, `error_detection` |
| `running` | the app first enters `running` — **once**, on the state change | — |
| `completed` | the app reports success | `duration_ms`, `result` |
| `failed` | the app reports failure, or a watchdog expires | `failure_kind`, `reason` |
| `superseded` | the timer fires again over an open run | the abandoned `run_id` |
| `reply_rejected` | validation refuses a reply | `reason` |

Every line carries `run_id` and `timer_id`, so one run's whole story is `grep`-able by id.

**One writer, and rotation goes through it** (`json_normalization.md` R11). `EventLog::open`
is called from five independent places outside tests today, and the pruner can rename
`events.current.jsonl` while another instance still holds it open. A line counts as durable
when it is flushed, not when it is created — hold it in SQLite, append, flush, mark published,
retry after a failed write or a restart. `event_id` makes the retry idempotent.

**Heartbeats and progress are NEVER logged.** Not the timestamps, not the text, not once.
They belong to the live view only — `status.json` and the GUI. A six-hour job with a
30-second heartbeat must add exactly **zero** lines to the log between `running` and
`completed`.

The log records **state transitions only**: one line each for `acknowledged`, `running`,
`completed`. Repeated writes inside a state append nothing.

Consequence worth knowing: because progress never reaches the log, the live view is the
**only** place it is ever visible. See the GUI card.

**Reconstruct what the watcher missed.** If the app moves `acknowledged` → `completed`
between two watcher ticks, Bellman sees only `completed` — but the accumulated
`acknowledged_at` is still in the file. Emit both lines, using the app's own timestamps
rather than the observation time, so the log reflects what happened rather than when we
noticed.

## `status.json` is the MIRROR — this is the point of the card

**`cat status.json` must always show the truth right now.** Not "what Bellman knows", not
"what fired" — the current state of the run including everything the app has reported.

This is the property most easily lost while satisfying every other line here. An
implementation that keeps only Bellman's own fields in `status.json` and leaves the app's
report in the event log would pass validation, pass the lifecycle tests, and still be wrong:
the file would read `state: "fired"` forever, and a human opening the folder would learn
nothing. The event log is the history; `status.json` is the **present**.

### Which fields land in `status.json`, and from where

| written directly by Bellman | folded in from `reply.json` |
|---|---|
| `schema` `run_id` `timer_id` `timer_name` | `app_name` `acknowledged_at` |
| `occurrence_kind` `scheduled_for` `fired_at` | `expected_secs` `error_detection` |
| `no_ack_at` | `heartbeat_at` `progress` |
| `failure_kind: "timed_out"` (watchdog) | `completed_at` `result` |
| | `failed_at` `reason` `failure_kind: "reported"` |

`state` comes from whichever side last moved it: Bellman writes `fired` and `no_ack` (and
`failed` on a watchdog expiry); the app's `acknowledged` / `running` / `completed` / `failed`
are folded in as they arrive.

**Bellman accumulates.** A field folded in at T1 stays in `status.json` even though the app's
T3 write no longer mentions it. That is why the app can write a complete-but-minimal
statement each time and never carry state.

## Lifecycle

`fired` (Bellman) → `acknowledged` → `running` → `completed` | `failed` (all app) ·
`no_ack` (Bellman, nobody picked it up).

### Which moves are legal

```
fired ─→ acknowledged ─→ running ─→ completed
            │              │    └──→ failed
            └──────────────┴───────→ completed | failed
```

Shorter paths are all valid — `fired → completed` is a normal app that did its work between
two watcher ticks. What is **not** valid:

- **A terminal report never moves backwards to a non-terminal one.** Once the app has said
  `completed` or `failed`, a later `running` or `acknowledged` for that run is rejected as
  `reply_rejected`. That is not a revised verdict — it reopens a closed run and restarts a
  watchdog, which is a bug in the writer every time.
- **Bellman never invents an app transition.** It records what the app wrote, plus its own
  `fired` / `no_ack` / watchdog `failed`. Nothing in between is inferred.

**The app may change its own verdict.** `failed` → `completed` on the current run is
**accepted**, and so is the reverse. Two reasons this is not the stale-writer hazard it looks
like:

- A stale process cannot reach here. Its `run_id` belongs to a previous firing and validation
  rejects it as `superseded` before any of this applies. What remains is the same app, on the
  run that is still open, correcting itself — far more likely a real correction than a bug.
- Nothing is lost either way. The log is append-only, so "failed 05:15, completed 05:22"
  survives as two lines whichever one the state ends on. Refusing the second would leave
  `status.json` reading `failed` for a run that succeeded, which is exactly the lie R7 exists
  to prevent.

So there is **one rule, not two**: for the run that is currently open, the app's latest
terminal report wins. It does not matter whether the state it replaces was the app's own
report or Bellman's watchdog inference — no `failure_kind` check, no special case.

The revision rule below is the same rule seen from the other side, and its one hard limit
still holds: **Bellman never overrides the app.** A watchdog cannot flip an app's `completed`
back to `failed`.

**Nothing ever auto-completes.** A run stays open until the app closes it, and **ages** — a
history reading "running for 3 days" is the truth, and obviously wrong to a human, without
Bellman pretending to know why. An unfinished run is not `failed`; `failed` means the app
said so.

## Timing — the asymmetry

- **Pickup has grace** (the existing 60s). Bellman wrote a file; whether anything read it is
  a fair, knowable question. Lapsed ⇒ `no_ack`.
- **Completion has no timeout, ever** — by default. How long another program takes is
  unknowable.

## The opt-in watchdog

An app may set `error_detection: true` with `expected_secs`. It is then **consenting** to be
held to a deadline **it declared itself** — not Bellman guessing. Default false.

- Deadline = `expected_secs × factor`, configurable, default forgiving (~2×). Failing at the
  exact stated second makes every app pad its estimate and the field becomes fiction.
- **The countdown is Bellman's monotonic clock, started at receipt** — never the app's
  timestamp (`json_normalization.md` R8). A skewed app clock would otherwise fail a
  healthy app, and a fast one would extend its own deadline indefinitely. Persist the
  wall-clock deadline too, but only to rebuild the countdown after a restart.
- **A heartbeat restarts the countdown** — this is what makes heartbeats worth sending, and
  the only way a long job can say "still alive, don't give up on me". Timed from Bellman's
  receipt of it, for the same reason.
- Outcome is `failed` with `failure_kind: "timed_out"` (vs `"reported"`).
- **Marking is not killing.** Flag the run; do not terminate the process.

Cheap to build because Bellman is a scheduler — a deadline is one entry in the existing heap.

### Where the failure is written — the two files diverge, on purpose

When the deadline expires the app is, by definition, silent. So **Bellman writes the failure
into `status.json` and the event log, and does not touch `reply.json`.**

```
status.json   state: "failed", failure_kind: "timed_out", failed_at: "…"
reply.json    state: "running"          ← unchanged, the app's last word
```

That divergence is correct and must not be "fixed":

- `reply.json` is **what the app said**. The app said `running` and then said nothing. Writing
  a failure into it would put words in the app's mouth — and would break the single-writer
  rule that makes the whole split safe.
- `status.json` is **the truth about the run**, which includes Bellman's own judgement.

`no_ack` has the same shape: the app never touched `reply.json` at all, so the stub still
reads `state: null` while `status.json` reads `no_ack`.

**A human browsing the folder must be told which to read.** The `README.txt` from IK2 has to
say it plainly: `status.json` is the answer; `reply.json` is only the app's side of the
conversation.

When the app eventually does reply `completed`, Bellman folds it in and `status.json` revises
— see the next section. At that point the two agree again.

## A late reply REVISES the state

`completed` arriving after a run was marked failed moves it to `completed`. Three properties
keep that safe:

- **The log does not flip.** `events.current.jsonl` is append-only, so "marked failed 05:15,
  completed 05:22" survives. That sequence is the interesting story.
- **One direction only.** An app's report beats Bellman's inference — Bellman *deduced*
  silence, the app *knows*. Bellman must never flip an app's `completed` back to failed.
- **Only while the run is still current.** A reply for a run the folder has already moved past
  (the timer fired again) is rejected as `superseded`, not applied. Revision reaches back
  through time, never across runs.

## Crash and restart — the two orderings that matter

Both come from `json_normalization.md` R10. Neither is visible in normal operation, which is
why they have to be specified rather than discovered.

### At fire: commit, then write the file

One SQLite transaction carries the previous run's final known state, its `superseded` event
if it was still open, the new `run_id`, the new `fired` event and any pending log lines.
`status.json` and the fresh `reply.json` stub are written **after** it commits.

| crash point | result |
|---|---|
| before the commit | the previous firing is still current — nothing was half-started |
| after the commit, before the file write | startup rebuilds both files from the database |
| after the file write | the database already holds the right generation; nothing to do |

Rebuilding is safe here precisely because `status.json` is Bellman's alone — regenerating it
cannot destroy anything an app said. That is a property the split buys and a shared file
would not.

Scheduling arithmetic does not change. This is persistence around a fire, not a new policy.

### At startup: read replies before firing anything

An app can answer while Bellman is stopped. If the scheduler runs first, the next fire
overwrites that reply and it is gone — **silently**, with nothing in the log to say a reply
ever existed. So before delivery starts:

1. Scan every `reply.json` under `timers/`.
2. Fold in each one that is valid and whose `run_id` is still the current run.
3. Emit the transitions that were missed, using the app's own timestamps.
4. Rebuild stale or missing `status.json` / stub files from the database.
5. Flush pending event-log lines.
6. **Then** start the scheduler.

A reply for a run the database has moved past is `superseded`, not applied — the same rule as
during normal operation, just discovered later.

## Validation on every read

**A parse failure is not a bad file — it is usually a file being written.** An app that
does not use atomic replacement will be caught mid-write. So the two failure classes are
handled differently:

| failure | response |
|---|---|
| JSON does not parse | wait the debounce window, re-read. Bytes changed ⇒ keep waiting. **Same invalid bytes still there** ⇒ `reply_rejected` |
| parses, but wrong `run_id` / `app_name` / reserved `state` / oversize | reject **immediately** — these are decidable on sight and waiting cannot change them |

Never quarantine on the first unparseable read. Never debounce a semantic rejection.

If the file is deleted or permanently corrupt: the database still identifies the current run,
Bellman reports the channel as missing, and the next fire (or startup) replaces it. A
hand-edited `run_id` is never trusted.

- `run_id` must match the run open in that folder — stale or unknown ⇒ quarantine to `bad/`.
- `app_name` must match the first acker; a second app cannot take over a run.
- `state` must be one an app may write — never `fired`, never `no_ack`.
- Bounded payload and bounded free text; oversize ⇒ quarantine, unread.
- **R9: a reply is data, never a command.** Bellman parses, validates, logs. It must never
  launch, execute, schedule or modify anything because an app wrote it. Worst case for a
  hostile reply is one bad log line.

## Exit gate

- Full chain observed: fired → acknowledged → running → completed, each transition in the log
  under one `run_id`, using the app's own timestamps.
- A transition the watcher never observed directly is still logged, reconstructed from the
  accumulated timestamps in `reply.json`.
- A long run with many heartbeats and changing `progress` appends **zero** log lines between
  `running` and `completed` — asserted by counting lines, not by inspection.
- **The mirror holds at every step**: after each app write, `cat status.json` shows that
  state and every field reported so far — asserted at all four points, not only at the end.
  A `status.json` still reading `fired` after the app acknowledged is a failure of this card
  even if the event log is perfect.
- Fields folded in earlier survive later writes that omit them (`expected_secs` from T1 is
  still in `status.json` after T3).
- An app that never replies stays `running` **indefinitely** — no auto-complete, no auto-fail.
- With `error_detection` on: the deadline marks `failed`/`timed_out`; a heartbeat before it
  prevents that; a late `completed` revises the state and the log retains both.
- **On watchdog expiry `reply.json` is byte-identical to what the app last wrote** — Bellman
  writes the failure to `status.json` and the log only. Asserted, because writing it into
  `reply.json` is the obvious-looking wrong move.
- On `no_ack`, `reply.json` is still the untouched stub while `status.json` reads `no_ack`.
- The stub exists at T0 with `state: null` and `app_name` pre-filled from the timer's owner;
  Bellman does not act on it.
- An app that edits the stub and an app that writes a minimal file both work.
- A reply whose `app_name` differs from the pre-filled owner is rejected.
- A reply omitting a field set earlier (e.g. `expected_secs` at T3) does **not** retract it —
  `status.json` retains the accumulated view.
- Duplicate reply is a no-op. Unknown `run_id`, wrong `app_name`, oversize, or a reserved
  `state` are each quarantined and change nothing.
- A test proves a reply cannot cause execution of anything.
- A run can be stopped/closed while nothing holds a token — the abort path is never gated.
- **A reply written while Bellman was stopped survives the restart** — stop Bellman, write
  `completed`, start it: the transition is folded in and logged **before** the next fire, and
  the stub is not overwritten first. The failure this catches is silent, so assert the log
  line exists rather than that nothing crashed.
- **Kill Bellman between the commit and the file write** — on restart `status.json` matches
  the database, and the run is neither duplicated nor lost.
- **The app's latest terminal report wins, from either source.** A watchdog `failed` followed
  by the app's `completed` revises; the app's **own** `failed` followed by its `completed`
  also revises. Both asserted, and asserted to take the same code path — a `failure_kind`
  check here would be the bug.
- The one direction that does not: Bellman's watchdog never flips an app's `completed` to
  `failed`.
- A terminal report followed by `running` or `acknowledged` **is** rejected — that reopens a
  closed run, which is a different thing from changing a verdict.
- **A mid-write file is not quarantined.** Write half a JSON document, then the rest: the
  reply is accepted. Write invalid bytes and leave them: `reply_rejected` after the debounce.
- **Watchdog arithmetic ignores the app's clock.** An app that stamps its heartbeat an hour in
  the future does not extend its deadline; one that stamps an hour in the past is not failed
  early. Both asserted against a fake monotonic source.
- A restart mid-countdown reconstructs the deadline from the persisted wall-clock value.
