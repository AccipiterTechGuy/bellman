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

**On the scheduler thread, only short work:** mint `run_id`, persist the claim, **publish the
fire** — the fresh `status.json`, the per-run reply stub, the slot fire notification, all
small local writes — enqueue the job, compute `next_fire`, move on. It never waits for an
action. Publication lives *here* precisely so it can never queue behind a slow action (see
the publication rule below).

**On the workers: action execution only** — process launch, output capture of the launched
process, retries. Workers never publish firing records; if a worker is writing `status.json`
**or a slot fire notification**, this card has been implemented wrong.

### The slot notification MOVES — this is a semantic change, stated on purpose

There is no `write_slot` *action* — `Action` is `Launch · Notify · None`
(`store/models.rs`). The fire notification is written by `ActionRunner::write_fire_slot()`
(`runner.rs:244`), today **inside the worker path, after the primary action succeeds**.
Leaving it there under this card resurrects the defect: with a configured fixed
`write_slot_file`, a slow first firing finishing late would write its notification **over**
the second firing's — or, kept strictly ordered, the second firing's notification would wait
out the first firing's whole action. Either way the folder lies.

So: `write_fire_slot()` / the `write_slot_*` config **move into scheduler-side publication**,
run **exactly once per firing**, and are **removed from the worker and the retry-success
path**.

The observable semantics change from *"notify only after the action succeeded"* to
*"notify at fire — even while the action is queued, skipped, retrying, or ultimately
failing."* That is intentional, not collateral: the notification reports the **firing**;
the action's outcome is reported where outcomes live — the event log and `status.json`
(`wake_delivered` / `wake_failed`). An integration that needs "only notify on action
success" is conflating two facts this design deliberately separates.

### Rules

- **No thread per timer.** One shared pool, bounded by the existing
  `max_concurrent_actions`. A thousand timers must not mean a thousand threads.
- **Publication and execution are different things, and only execution queues.** When a timer
  fires again while its previous action is still running, the *record* of the new fire — the
  claim, the `superseded` line, the fresh `status.json` and `reply.json` stub — is published
  **immediately**, at fire time, on the scheduler side. It is short local work and it is the
  IK contract ("a new firing always proceeds"). Only the *action* — the process launch —
  waits its turn in the timer's lane. Queuing the publication behind a 15-minute action would
  leave the folder claiming the old run is current for 15 minutes, which breaks IK3's mirror.
- **Same-timer dispatch is OVERLAP-POLICY-AWARE, not blanket-serial.** `OverlapPolicy`
  already exists (`store/models.rs`): `Skip` (default) · `QueueOne` · `Parallel { cap }` ·
  `Replace`. A lane that hard-serialises every timer silently overrides `Parallel` and
  `Replace` — a configured policy the dispatcher ignores is a lie in the settings UI. Per
  policy, when a fire arrives while a previous action runs:
  - `Skip` — the new action is not started (existing behaviour, now enforced at dispatch);
  - `QueueOne` — at most one follow-up waits in the lane; further fires collapse into it;
  - `Parallel { cap }` — up to `cap` actions of this timer run concurrently, still inside
    the global `max_concurrent_actions`;
  - `Replace` — the in-flight action is cancelled, then the new one starts.
  "Never concurrently, in order" is the rule **for the serial policies only** (`Skip`,
  `QueueOne`, `Replace`); asserting it for `Parallel` would assert the bug. **Publication is
  immediate for every policy** — the policy governs the action, never the record.
- **Different timers run in parallel**, up to the cap.
- **The queue is bounded, and a full queue never drops a fire.** The claim is already durable
  in SQLite before enqueue, so backpressure means "stays pending", never "lost". If the queue
  is full the scheduler still records the firing and keeps computing times.
- **Pending claims have a LIVE feeder, not just a boot-time one.** Today pending-claim
  scanning happens only at scheduler startup (`delivery.rs`) — under this card, a claim
  persisted while the queue was full would wait for a restart that may never come. The
  dispatcher pump runs on three triggers: **worker completion** (a slot freed), **enqueue
  failure** (retry path), and **startup** (crash recovery). Claims carry an explicit state —
  `pending → enqueued → active → finished` — so the pump can tell what needs feeding and a
  crash can tell what needs re-queuing, without double-running anything.
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
- Two fires of the **same** timer execute their actions in order, never concurrently —
  **asserted for `Skip`/`QueueOne`/`Replace` only.** A `Parallel { cap: 2 }` timer runs two
  actions concurrently (asserted), never three (asserted), and stays inside the global cap.
- **A full queue drains without a restart:** fill the queue, persist one more claim, let a
  worker finish — the pending claim is dispatched by the pump, with Bellman never restarted
  during the test.
- **Slot notification ownership:** with a **fixed** configured `write_slot_file`, a slow
  first firing and a second firing that publishes while it runs — the first firing's late
  completion neither overwrites nor duplicates the second's notification, and each firing's
  notification was written exactly once, at fire time. Asserted with the first action still
  running when the second's notification is checked.
- A firing whose action is skipped (`Skip` overlap) or ultimately fails still produced its
  fire notification — the semantic change above, asserted so it is load-bearing, not prose.
- **A second fire publishes immediately even while the first action still runs**: fire a
  timer whose action takes 30 s, fire it again at 10 s — at ~10 s the folder already shows
  the new `run_id` and `superseded` is already logged, while the first action is still
  executing in its lane. Asserted on wall-clock, because "queue the whole fire" passes every
  ordering test and still breaks this.
- A full queue under a resume mass-fire loses nothing: every claim is eventually delivered,
  and the count matches.
- Kill mid-flight: on restart the undelivered claims re-queue, with no duplicate delivery of
  a claim already completed.
- Shutdown with lanes busy drains rather than truncating the log.
- The scheduler thread's own time per fire stays bounded — measure it, since "we moved the
  work off the loop" is easy to believe and easy to get wrong.
