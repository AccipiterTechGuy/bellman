# SCH1 — In-memory fire dispatcher and bounded action lanes

Repo: `~/bellman`. **Internal scheduler work — no protocol change.** Independent of IK1–IK5;
neither blocks the other. Keep it that way: this card must not touch the JSON shapes, the
folder tree or the reply channel.

## The defect, verified in the code

The scheduler thread runs the action **synchronously and to completion**:

- `scheduler/engine/delivery.rs` — `finish_claimed_run` calls `self.action.on_fire(&ctx)`
  inline, on the loop thread, and only then advances the timer.
- `actions/launch.rs:89–110` — `run_launch` blocks in a `try_wait` loop until the child
  exits, sleeping 20 ms between polls. `DEFAULT_TIMEOUT` is **60 s** (`launch.rs:14`).
- `actions/runner.rs:328` — retries sleep **inline** on the same thread, so the worst case is
  `max_retries × (timeout + delay_secs)`.

**One slow launch stalls every other timer for the whole duration.** A 60-second command with
two retries holds the heap for three minutes; every timer due in that window fires late, and
`FireKind::Late` is what they will be recorded as. The bug is invisible in tests because test
actions return immediately.

### The limiter already exists and currently does nothing

`actions/concurrency.rs` is a real semaphore — `ActionLimiter`, fair-ish, overflow queue,
peak/wait stats, `DEFAULT_MAX_CONCURRENT_ACTIONS = 16`. `runner.rs:194` already wraps
`execute_once` in `limiter.run(…)`.

But the only caller is the scheduler thread, so in-flight never exceeds **1**. A cap of 16
over a single-threaded producer caps nothing. **This card is not "add a worker pool" — it is
"give the pool that already exists something to do."** Reuse `ActionLimiter`; do not write a
second concurrency primitive next to it.

## The shape

```
timer heap  ──due──▶  claim in SQLite  ──▶  bounded fire queue  ──▶  worker lanes  ──▶  action
     ▲                                                                                    │
     └──────────────── advance next_fire immediately, do not wait ─────────────────────────┘
```

**On the scheduler thread, only short work:** mint `run_id`, persist the claim, enqueue the
job, compute `next_fire`, move on. It never waits for an action.

**On the workers:** everything that can block — process launch, output capture, retries,
slot/file writes.

### Rules

- **No thread per timer.** One shared pool, bounded by the existing
  `max_concurrent_actions`. A thousand timers must not mean a thousand threads.
- **Same timer stays ordered.** Two jobs for one timer never run out of order. The existing
  `in_flight` set in `runner.rs:301` and the overlap policy are the seed of this — extend
  them, do not duplicate.
- **Different timers run in parallel**, up to the cap.
- **The queue is bounded, and a full queue never drops a fire.** The claim is already durable
  in SQLite before enqueue, so backpressure means "stays pending", never "lost". If the queue
  is full the scheduler still records the firing and keeps computing times.
- **The event log and `status.json` keep single-writer discipline** (`json_normalization.md`
  R11). Several lanes finishing at once must not become several writers — this is the most
  likely way to break IK3 while implementing this card.
- **Shutdown drains.** Stop accepting new jobs, let in-flight lanes finish or time out, flush
  pending log lines, then exit.
- **Crash recovery is unchanged in principle** (R10): claims are in SQLite before the job
  exists in memory, so restart re-queues what never ran. The in-memory queue is a fast path,
  never the record.

## What does NOT change

`FireKind`, misfire and catch-up logic, coalescing, overlap policy, the retry counts, the
JSON shapes, the folder tree. If a diff in this card touches `docs/todo/json_normalization.md`
the scope has slipped.

## Exit gate

- **A timer whose action takes 30 s does not delay a timer due 1 s later** — asserted on
  wall-clock, with the second timer recorded `on_time`, not `Late`. This single test is the
  card; it fails today.
- Peak in-flight actually reaches the cap under a mass-fire, and never exceeds it —
  `LimiterStats::peak_in_flight` already reports this.
- Two fires of the **same** timer are delivered in order, never concurrently.
- A full queue under a resume mass-fire loses nothing: every claim is eventually delivered,
  and the count matches.
- Kill mid-flight: on restart the undelivered claims re-queue, with no duplicate delivery of
  a claim already completed.
- Shutdown with lanes busy drains rather than truncating the log.
- The scheduler thread's own time per fire stays bounded — measure it, since "we moved the
  work off the loop" is easy to believe and easy to get wrong.
