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

### Bellman pre-creates the stub

At fire, Bellman writes **both** `status.json` and an empty `reply.json` stub. Matches the
existing convention of pre-generated free slot stubs, and it means the folder shows an app
exactly what to fill in.

**T0 — Bellman fires. `reply.json`:**

```json
{
  "schema": "bellman-reply/1",
  "run_id": "9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
  "state": null,
  "app_name": null,
  "hint": "set state to acknowledged | running | completed | failed, and app_name to your app"
}
```

`state: null` is how Bellman tells "stub, untouched" from "the app answered". Bellman writes
this once and **never writes the file again** — from T0 onward the app is its only writer.

**T1 — the app acknowledges.** Overwrites the whole file:

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
  "schema": "bellman-reply/1",
  "run_id": "9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
  "app_name": "lightbulb",
  "state": "running",
  "heartbeat_at": "2026-07-30T05:00:07Z",
  "progress": "bulb on, 7s elapsed"
}
```

**T3 — completed:**

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

### Each write is a COMPLETE statement, not a diff

Note that T3 does not repeat `acknowledged_at` or `expected_secs`. It does not have to —
**Bellman accumulates; the app does not.** Every earlier field is already folded into
`status.json` and appended to the event log.

This is the property that keeps the client trivial: an app never reads `reply.json` back,
never merges, never carries state between writes. It states where it is right now, in four
required fields (`schema`, `run_id`, `app_name`, `state`) plus whatever that state carries.

Consequence for the implementation: **do not treat a missing field as a retraction.**
`expected_secs` absent at T3 does not mean the app withdrew its estimate.

Bellman watches it, validates, logs the transition, and folds the result into `status.json`.

## Lifecycle

`fired` (Bellman) → `acknowledged` → `running` → `completed` | `failed` (all app) ·
`no_ack` (Bellman, nobody picked it up).

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
- **A heartbeat restarts the countdown** — this is what makes heartbeats worth sending.
- Outcome is `failed` with `failure_kind: "timed_out"` (vs `"reported"`).
- **Marking is not killing.** Flag the run; do not terminate the process.

Cheap to build because Bellman is a scheduler — a deadline is one entry in the existing heap.

## A late reply REVISES the state

`completed` arriving after a run was marked failed moves it to `completed`. Three properties
keep that safe:

- **The log does not flip.** `events.current.jsonl` is append-only, so "marked failed 05:15,
  completed 05:22" survives. That sequence is the interesting story.
- **One direction only.** An app's report beats Bellman's inference — Bellman *deduced*
  silence, the app *knows*. Bellman must never flip an app's `completed` back to failed.
- **`runs/` must not freeze at the deadline** or the archive is frozen wrong. Freeze on an app
  report, or let a late reply rewrite it.

## Validation on every read

- `run_id` must match the run open in that folder — stale or unknown ⇒ quarantine to `bad/`.
- `app_name` must match the first acker; a second app cannot take over a run.
- `state` must be one an app may write — never `fired`, never `no_ack`.
- Bounded payload and bounded free text; oversize ⇒ quarantine, unread.
- **R9: a reply is data, never a command.** Bellman parses, validates, logs. It must never
  launch, execute, schedule or modify anything because an app wrote it. Worst case for a
  hostile reply is one bad log line.

## Exit gate

- Full chain observed: fired → acknowledged → running → completed, each transition in the log
  under one `run_id`, `status.json` current throughout.
- An app that never replies stays `running` **indefinitely** — no auto-complete, no auto-fail.
- With `error_detection` on: the deadline marks `failed`/`timed_out`; a heartbeat before it
  prevents that; a late `completed` revises the state and the log retains both.
- The stub exists at T0 with `state: null`, and Bellman does not act on it.
- A reply omitting a field set earlier (e.g. `expected_secs` at T3) does **not** retract it —
  `status.json` retains the accumulated view.
- Duplicate reply is a no-op. Unknown `run_id`, wrong `app_name`, oversize, or a reserved
  `state` are each quarantined and change nothing.
- A test proves a reply cannot cause execution of anything.
- A run can be stopped/closed while nothing holds a token — the abort path is never gated.
