# SCH1 — In-memory fire dispatcher and bounded action lanes

Repo: `~/bellman`. **Internal scheduler work — no JSON-shape change.** Depends on **IK3**:
the publication side uses its per-run reply path, R10 gate and R11 outbox. SCH1 must preserve
those contracts while moving action execution off the scheduler loop.

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

### The limiter exists; the worker pool does not

`actions/concurrency.rs` is a real semaphore — `ActionLimiter`, fair-ish, overflow queue,
peak/wait stats, `DEFAULT_MAX_CONCURRENT_ACTIONS = 16`. `runner.rs:194` already wraps
`execute_once` in `limiter.run(…)`.

But the only caller is the scheduler thread, so in-flight never exceeds **1**. A cap of 16
over a single-threaded producer caps nothing. `ActionLimiter` blocks callers on a `Condvar`;
it does not own threads or execute queued jobs. (`run_parallel_under_cap` spawns threads only
as a test helper.) **This card therefore adds the bounded worker executor that is missing.**
Reuse the existing `ActionLimiter` as the shared global permit/statistics gate; do not write
a second semaphore and do not mistake its waiters for the durable fire queue.

The executor starts exactly `max_concurrent_actions` workers (minimum 1). Its in-memory hint
queue holds `2 × max_concurrent_actions` run ids (minimum 1); this is derived from the
existing setting, so SCH1 adds no config/JSON field. Overflow leaves the durable claim
`pending` for the pump rather than blocking a producer.

## The shape

```
timer heap  ──due──▶  claim in SQLite  ──▶  bounded fire queue  ──▶  worker lanes  ──▶  action
     ▲                                                                                    │
     └──────────────── advance next_fire immediately, do not wait ─────────────────────────┘
```

**On the fire-producer side, only short work:** whether the producer is the scheduler loop,
GUI `run_now` or CLI `run-now`, mint `run_id`, persist the claim, **publish the fire** — the
fresh `status.json` and, for an integration-owned run, the per-run reply stub and slot fire
notification, all small local writes — enqueue a job hint, compute `next_fire`, move on. It never waits for an action.
Publication lives *here* precisely so it can never queue behind a slow action (see the
publication rule below).

**On the workers: configured `Action` execution only** — `Launch`, `Notify` or `None`,
including launch output capture and retries. Workers never publish firing records; if a
worker is writing `status.json` **or a slot fire notification**, this card has been
implemented wrong.

### `ActionRunner` must split; a mutex around it fails the card

`FireAction::on_fire(&mut self)` and today's `ActionRunner` are deliberately single-caller:
the runner owns mutable `in_flight`, `last_message` and an `EventLog` handle. Putting that
whole value behind `Arc<Mutex<_>>` would make every worker hold the mutex through launch and
sleep, restoring global serial execution under a different name.

Split the responsibilities:

- the shared **claim service** makes the durable overlap-admission decision in the fire
  transaction; the **dispatcher** owns its in-memory queued/active reservations,
  cancellation tokens and bounded queue and enforces that decision;
- a thread-safe or per-worker **action executor** owns only the immutable action inputs,
  notification sink and shared `Arc<ActionLimiter>`, and returns an action result;
- each worker uses its own SQLite connection to commit the durable result/outbox transaction;
  no `Store`, mutable `ActionRunner`, or JSONL handle is shared behind a long-held mutex;
- `last_message` remains a `run_now` response concern, assembled from the returned result,
  not shared scheduler state.

The current `ActionRunner::in_flight` / `overlap_blocks` logic moves to the dispatcher. It
cannot represent `Parallel { cap > 1 }` with its one-bit `HashSet`, and keeping it in both
places would apply overlap twice.

### Overlap is decided at fire commit, not later at dequeue

GUI, scheduler and standalone CLI can all produce a fire while only one process owns the
dispatcher. Therefore the fire transaction examines older executable claims for that timer
(`pending` or `active`) and stores an internal lane disposition; queue timing is never allowed
to change policy:

- `Skip`: if any older executable claim is unfinished, the new claim is immediately
  `finished/skipped_misfire(overlap_skip)` in the fire transaction. Otherwise it is
  `pending`.
- `QueueOne`: admit at most one executable follow-up beyond the oldest active/pending action.
  Excess new claims finish `skipped_misfire(overlap_queue_full)` in the fire transaction.
- `Parallel { cap }`: admit the new claim only while fewer than `cap` older executable claims
  are unfinished; excess finishes `skipped_misfire(overlap_parallel_cap)`. `cap: 0` admits
  none.
- `Replace`: finish every older **pending** action
  `wake_failed(overlap_replace_before_start)`, mark every older **active** action
  `cancel_requested`, and leave only the newest claim pending. It is not eligible until those
  active predecessors finish. The dispatcher observes the durable request and signals their
  cancellation tokens; a CLI does not need access to another process's memory.

SQLite commit order settles races with worker completion. If the worker commits success
before the replace transaction, there is nothing left to cancel. If replacement commits
first, the worker must observe `cancel_requested` before its final result: an action actually
interrupted is `wake_failed/overlap_replace`; one that had already completed successfully
before cancellation could take effect is truthfully `wake_delivered`. In both cases the
newest action waits for predecessor completion, so they never overlap. These are internal
dispatch fields, not additions to any JSON shape.

### `run_now` is a producer, not a bypass

`service/run_now.rs` currently constructs an `ActionRunner` and calls `on_fire` directly;
the GUI and the standalone CLI both use that service. Leaving this path intact would bypass
the lanes, overlap policy, slot publisher and R11 outbox — and after `write_fire_slot()` moves
out of the runner, it would also stop publishing manual-fire notifications.

Scheduled delivery, GUI `run_now` and CLI `run-now` therefore enter one
`pre-fire barrier → claim/commit → publish → dispatch` service. A manual fire keeps today's
`FireKind::OnTime`; it is a different producer of the same claim, not a second executor:

- inside the scheduler process, `run_now` submits the claim and waits for that claim's durable
  result while the dispatcher continues serving other timers. Its short R10 per-timer gate
  is released immediately after post-commit projection, before this wait;
- a standalone CLI never executes the action beside a live GUI dispatcher. It commits the
  request and performs the same short post-commit file projection under the shared locks,
  then relies on the dispatcher's periodic DB pump and waits/polls for the claim result;
- when no dispatcher owns the OS lock, the CLI may acquire it, run the same bounded
  dispatcher until its claim finishes, then release it;
- if the current owner dies while a CLI waits, lock acquisition plus the normal `active`
  recovery rule continues the same `run_id`.

No new local-IPC protocol is required for this card; SQLite is the durable handoff and the
periodic pump is the wakeup backstop.

### The slot notification MOVES — this is a semantic change, stated on purpose

There is no `write_slot` *action* — `Action` is `Launch · Notify · None`
(`store/models.rs`). The fire notification is written by `ActionRunner::write_fire_slot()`
(`runner.rs:244`), today **inside the worker path, after the primary action succeeds**.
Leaving it there under this card resurrects the defect: with a configured fixed
`write_slot_file`, a slow first firing finishing late would write its notification **over**
the second firing's — or, kept strictly ordered, the second firing's notification would wait
out the first firing's whole action. Either way the folder lies.

There is a second current writer to remove: standalone CLI `run-now` calls
`publish_fire_slot_response()` after the action and overwrites `slots/done/slot-<id>.json`
with a request-response envelope. That path already belongs to `SlotService`; using it for a
fire notification gives one file two schemas and lets a late first action replace a newer
response or notification.

Ownership after this card is exact:

- `slots/done/slot-<id>.json` is written only as the response to a slot request, by
  `SlotService`. The unsolicited post-action `publish_fire_slot_response()` overlay is
  removed; unacknowledged run events appear in the next normal response.
- file fire notifications are written only under `slots/fires/`: the default is
  `fire-<full-run_id>.json`; a configured fixed filename is allowed there only as the
  at-least-once wake hint described below.
- pickup or superseding removes the matching per-run notification; a current one-shot with
  no recorded pickup may remain until timer deletion or the normal run-retention window,
  when its transport projection and file are pruned together. Cleanup takes the same per-target
  interprocess lock as publication, re-reads the file, and deletes it only if its `run_id`
  still matches the projection being cleaned; pickup of an older firing must never remove a
  newer fixed-path wake hint.

So: ownership of `write_fire_slot()` / the `write_slot_*` config **moves into
fire-producer-side publication** and is **removed from the worker and the retry-success
path**.
Do **not** add a second semantic run-event feed: the committed run claim, its existing
per-timer `event_sequence`, and the durable `ack_through` cursor remain the one app-visible
feed (IK3's explicit reuse rule).

The current `write_slot_*` route is runtime configuration, however, and cannot be reconstructed
after its caller exits or the timer is renamed. The same fire transaction therefore stores
one **transport projection** keyed by `run_id`: the immutable target path, serialized
notification payload and a database-wide monotonic `publication_order`. It is retry/routing
state for the existing run, not another event or history copy, and is pruned with that run.
For a configured fixed target, a separate durable target cursor stores the greatest
`publication_order` ever assigned while any projection for that target remains. Advancing it
and inserting the projection happen in the same transaction. An older projection below that
cursor is permanently obsolete as a fixed-path hint even after the newer file is consumed;
it must never reappear merely because the path became empty. Prune the cursor only when no
timer configuration or retained projection still references that canonical target.
The fire producer attempts it immediately; a bounded local-write failure remains pending for
the live publication pump and startup recovery, never for an action worker. The attempt is
eligible only after R10 has successfully projected `status.json` and, for file transport,
the create-only reply stub. Publishing a path before its channel exists is not "immediate";
it is a broken notification.

Filesystem publication is **at-least-once, not physically exactly-once**. After the atomic
replace succeeds but before pickup is recorded, Bellman may publish the firing again, so every
consumer must deduplicate by `run_id` (already an IK4 requirement). Pickup has R7's existing
definition: either `ack_through` advances past this firing **or** Bellman ingests any valid
reply for it. File presence alone proves neither. Recovery checks the target before retrying:

- the same known `run_id` already present suppresses an immediate recovery rewrite, but stays
  eligible for a later bounded retry until pickup is recorded;
- a known firing with a higher global `publication_order` already present at a fixed
  `write_slot_file` means the older pending firing is obsolete as a wake hint — never
  overwrite the newer notification, even when the two firings belong to different timers;
- a fixed target's durable cursor higher than this projection also makes it obsolete,
  whether or not a file is currently present;
- a missing file, an older known firing, or a file the app consumed is written again;
- malformed bytes or an unknown `run_id` under Bellman's `slots/fires/` namespace are
  surfaced and atomically replaced by the newest pending projection. This is Bellman-owned
  output, not the app-owned reply channel, so IK3's copy-only quarantine rule does not apply.

An interprocess lock serialises publishers per canonical target path. Use a bounded stable
lock shard set under the data root, keyed by the canonical target, rather than a lock beside
a file that may be deleted/replaced. Fire publication acquires the R10 timer shard before the
target shard; no path ever acquires them in reverse order. This makes GUI, CLI, recovery and
retries safe for fixed filenames without pretending SQLite and a filesystem replace are one
transaction. A fixed file is a **wake hint, not the queue**: if several
firings pass before the app reads it, each timer's existing unacknowledged `SlotRunEvent` feed
carries its `event_sequence`s in order. The publication pump runs on the immediate attempt, a
bounded backoff trigger, and startup. Retries stop when R7 pickup succeeds or its deadline
records `no_ack`; the durable feed and existing file still allow a late pickup to revise
`no_ack`.

The observable semantics change from *"notify only after the action succeeded"* to
*"notify at fire — even while the action is queued, skipped, retrying, or ultimately
failing."* That is intentional, not collateral: the notification reports the **firing**;
the action's outcome is recorded where delivery outcomes live — the run claim and event log
(`wake_delivered` / `wake_failed`). `SlotRunEvent::from_claim` exposes the claim's current
status only while that `event_sequence` remains unacknowledged; it is a delivery feed, not a
second history log. An integration that needs "only notify on action success" is conflating
two facts this design deliberately separates.

### Worker completion returns through the database, never through the run files

A worker does not write `status.json` or append JSONL. On completion it commits the claim's
`active → finished` result and the corresponding R11 event-outbox row to SQLite in one
transaction, then signals the dispatcher pump that a lane is free. The R11 publisher drains
the event row; an unacknowledged `SlotRunEvent` projection reflects the updated claim on its
next read.

It does **not** project `wake_delivered` or `wake_failed` into `status.json.state`. IK3 owns
that current app lifecycle (`fired` / `acknowledged` / `running` / `completed` / `failed`,
plus Bellman's `no_ack` and watchdog inference). Immediate notification means an app can
legitimately report `completed` before the worker finishes; the later action result must not
move that file backwards to a wake-delivery state. If Bellman crashes after the completion
transaction but before JSONL publication, the claim remains finished and the R11 outbox
drains on restart without rerunning the action.

This is the return arrow missing from the diagram: workers produce durable claim results;
they do not become parallel file writers.

### Rules

- **No thread per timer.** One shared pool, bounded by the existing
  `max_concurrent_actions`. A thousand timers must not mean a thousand threads.
- **Publication and execution are different things, and only execution queues.** When a timer
  fires again while its previous action is still running, the *record* of the new fire — the
  claim, any IK3 `superseded` line for an unresolved owned app run, the fresh `status.json`
  and owned-run reply stub/notification
  — is published
  **immediately**, at fire time, on the fire-producer side. It is short local work and it is
  the IK contract ("a new firing always proceeds"). Only the configured wake action
  (`Launch` / `Notify` / `None`) waits its turn in the timer's lane. Queuing the publication
  behind a 15-minute action would leave the folder claiming the old run is current for
  15 minutes, which breaks IK3's mirror.
- **Same-timer dispatch is OVERLAP-POLICY-AWARE, not blanket-serial.** `OverlapPolicy`
  already exists (`store/models.rs`): `Skip` (default) · `QueueOne` · `Parallel { cap }` ·
  `Replace`. A lane that hard-serialises every timer silently overrides `Parallel` and
  `Replace` — a configured policy the dispatcher ignores is a lie in the settings UI. Per
  policy, when a fire arrives while a previous action runs:
  - `Skip` — the new action is not started. Finish that firing's claim as an overlap skip and
    emit the existing `skipped_misfire` event with reason `overlap_skip`; do **not** mark it
    `wake_delivered`.
  - `QueueOne` — the first follow-up waits in the lane. Further firings are still published
    but their actions are finished as overlap skips (`skipped_misfire`,
    `overlap_queue_full`); they do not silently disappear and do not replace the queued
    follow-up.
  - `Parallel { cap }` — up to `cap` actions of this timer run concurrently, still inside
    the global `max_concurrent_actions`;
  - `Replace` — signal cancellation and start the new action **only after the old worker
    confirms it stopped**, preserving the no-concurrency rule. For `Launch`, add a
    cancellation token to the existing 20 ms `try_wait` loop and kill/reap the child; the
    same token interrupts retry backoff and prevents another attempt. Record an action that
    was actually interrupted as `wake_failed` with reason `overlap_replace`; if it completed
    before cancellation took effect, retain truthful `wake_delivered`. This is action-policy
    cancellation, unrelated to IK3's watchdog rule that never kills an app.
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
  dispatcher pump runs on **worker dequeue/completion**, **enqueue failure**, **startup**, and
  a **periodic safety tick no slower than once per second**. The tick is the backstop for a lost in-process wakeup;
  SQLite, not a notification, remains the source of truth.
- **All startup pumps wait for R10.** Reply scanning/ingest, outbox recovery and folder
  reconciliation complete before the dispatcher or transport-publication pumps begin.
  Otherwise an old notification can be replayed — or an already-due fire can supersede the
  current run — before Bellman reads a valid reply written while it was stopped.
- **Do not persist an `enqueued` fiction.** SQLite and an in-memory channel cannot change
  atomically. Claims use durable `pending → active → finished`; queue entries are disposable
  `run_id` hints. A worker must compare-and-set `pending → active` before executing. Duplicate
  or stale hints lose that transition and do nothing, so a live process cannot execute one
  claim concurrently twice. A full queue merely drops the hint; the claim remains `pending`
  for the next pump pass.
- **The dispatcher reserves lane order before enqueue.** It scans each timer by
  `event_sequence`, keeps an in-memory set of queued run ids, and sends only the oldest
  policy-eligible pending claim. A successful channel send keeps that reservation until the
  worker reports its `pending → active` result; a failed send releases it immediately.
  Worker completion/CAS failure releases the reservation and wakes the pump. This prevents
  two workers from overtaking a serial timer while retaining the rule above that no
  `enqueued` state is persisted.
- **Dispatch state and delivery outcome are separate internal fields.** `pending` / `active`
  have no outcome. `finished` carries exactly one existing R5 delivery outcome:
  `wake_delivered`, `wake_failed`, or `skipped_misfire`. Migrate today's
  `ClaimStatus::{Claimed, Completed, WakeFailed}` into that split rather than treating
  `finished` as success. `SlotRunEvent::from_claim` and the R11 event row project the same
  outcome; a `Skip` or collapsed `QueueOne` claim can never appear as `wake_delivered`.
- **Policy disposition is durable too.** The fire transaction, not whichever worker happens
  to dequeue later, records execute/skip/cancel intent and any reason above. The pump only
  enqueues claims admitted for execution; it never re-decides overlap from a later snapshot.
- **Worker execution is at-least-once across a crash.** A `pending` claim is safe to re-queue.
  An `active` claim is ambiguous: the external side effect may have happened before Bellman
  could commit `finished`. Only the process holding a dedicated dispatcher OS lock may pump
  scheduled claims; after acquiring that lock at startup, it knows the previous dispatcher
  is gone, returns that owner's `active` claims to `pending`, and re-queues the **same
  `run_id`**. An arbitrary CLI process must not reset them. Inside a live process, the worker
  supervisor catches a worker exit/panic, returns its unfinished claim to `pending`, and
  triggers the pump. No local state can make a process launch and SQLite one transaction;
  commands that require logical exactly-once use `BELLMAN_RUN_ID` as their idempotency key.
  A `finished` claim is never re-run.
- **The event log keeps R11's interprocess outbox/publisher discipline.** Several lanes
  finishing at once commit claim results plus outbox rows to SQLite; they do not become
  several JSONL or run-file writers.
- **Shutdown drains.** Stop accepting new jobs, let in-flight lanes finish or time out, then
  drain and sync pending outbox rows through the R11 publisher before exit.
- **Crash recovery is unchanged in principle** (R10): claims are in SQLite before the job
  exists in memory, so restart re-queues every unfinished claim with its original `run_id`.
  The in-memory queue is a fast path, never the record; the at-least-once caveat above is
  explicit.

## What does NOT change

`FireKind`, misfire and catch-up logic, coalescing, overlap policy, the retry counts, the
JSON shapes and the per-timer folder tree. SCH1 does make the already-defined publication
ownership executable: it records internal routing/retry state and gives fire notifications
their own `slots/fires/` namespace, without adding a wire field or another run-event feed.

## Exit gate

- **A timer whose action takes 30 s does not delay a timer due 1 s later** — asserted on
  wall-clock, with the second timer recorded `on_time`, not `Late`. This single test is the
  card; it fails today.
- Peak in-flight actually reaches the cap under a mass-fire, and never exceeds it —
  `LimiterStats::peak_in_flight` already reports this.
- Two fires of the **same** timer execute their actions in order, never concurrently —
  **asserted for `Skip`/`QueueOne`/`Replace` only.** A `Parallel { cap: 2 }` timer runs two
  actions concurrently (asserted), never three (asserted), and stays inside the global cap.
- Race two pump triggers while two claims for one serial timer are pending; the dispatcher
  enqueues only the oldest `event_sequence`, and the second becomes eligible only after the
  first releases its lane.
- Assert every overlap outcome, not just process counts: `Skip` and excess `QueueOne`
  firings finish as `skipped_misfire`; the retained `QueueOne` follow-up executes once;
  `Parallel` excess is `skipped_misfire/overlap_parallel_cap`; `Replace` interrupts a running
  launch and retry backoff, records the first firing `wake_failed/overlap_replace`, then
  starts the second firing; none is mislabeled `wake_delivered`.
- Hold the dispatcher queue before firing through a standalone CLI, then release it. Assert
  `Skip`, `QueueOne`, `Parallel` and `Replace` outcomes match the state at each fire commit,
  not the later dequeue time. Race worker completion against `Replace` on both SQLite commit
  orders and assert the predecessor is truthfully `wake_delivered` or
  `wake_failed/overlap_replace` while the replacement never overlaps it.
- **A full queue drains without a restart:** fill the queue, persist one more claim, let a
  worker finish — the pending claim is dispatched by the pump, with Bellman never restarted
  during the test.
- **Slot notification ownership:** with a **fixed** configured `write_slot_file`, a slow
  first firing and a second firing that publishes while it runs — the first firing's late
  completion neither overwrites nor duplicates the second's notification. Asserted with the
  first action still running when the second's notification is checked.
- **The two slot namespaces never collide:** a normal slot request owns
  `done/slot-<id>.json`; a fire owns `fires/fire-<run_id>.json` (or a fixed name under
  `fires/`). Complete a slow CLI `run-now` after both files contain newer data and assert
  there is no post-action `publish_fire_slot_response()` write at all.
- **Slot publication crash windows:** crash once before the atomic replace and once after the
  replace but before pickup. Both remain eligible for bounded redelivery; an unchanged file
  suppresses only the immediate startup rewrite, not every later retry. If the app
  consumed/deleted the file without pickup being recorded, redelivery is allowed and its
  `run_id` dedupe keeps it one logical firing. An older unacknowledged firing never replaces
  a newer firing already at a fixed path; both remain ordered in the durable `SlotRunEvent`
  feed until acknowledged.
- **Cleanup is compare-before-delete under the publisher lock:** publish the second firing
  into a fixed path, then record late pickup/superseding of the first firing. Cleanup of the
  first projection leaves the second firing's file byte-identical. Then consume and clean up
  the second file and trigger a retry of the still-unacknowledged first projection; the
  durable target cursor prevents the first notification from resurfacing.
- **Run files precede delivery:** fail `status.json`, then separately fail stub creation.
  In both cases the transport projection stays pending and no notification containing
  `reply_path` appears until R10 reconciliation has completed the required files.
- A firing whose action is skipped (`Skip` overlap) or ultimately fails still produced its
  fire notification — the semantic change above, asserted so it is load-bearing, not prose.
- **Worker completion ownership:** let the app report the second firing `completed`, then let
  its worker finish `wake_delivered` (and repeat with `wake_failed`). The claim and event log
  record the action outcome while `status.json` remains the app's `completed`; no worker
  result regresses the lifecycle. Crash after the completion transaction and before JSONL
  publication; restart drains the event without rerunning the action.
- **A second fire publishes immediately even while the first action still runs**: fire a
  integration-owned timer whose action takes 30 s, provide no app reply, and fire it again
  at 10 s — at ~10 s the folder already shows the new `run_id` and `superseded` is already
  logged, while the first action is still executing in its lane. Asserted on wall-clock,
  because "queue the whole fire" passes every ordering test and still breaks this.
- **Manual fire uses the same dispatcher:** while the GUI owns the dispatcher lock and the
  first firing's action is still running, invoke the second firing through the standalone
  CLI `run-now`. Its publication appears immediately, its action obeys the configured
  overlap lane, and the CLI receives that claim's durable result without constructing a
  second `ActionRunner` or slot writer.
- A full queue under a resume mass-fire loses nothing: every claim is eventually delivered,
  and the count matches.
- **Kill mid-action:** restart re-queues the unfinished claim with the same `run_id`; the
  action may execute twice, while a `finished` claim never executes again. Assert both sides
  so the at-least-once boundary cannot be mistaken for exactly-once.
- **Startup ordering includes every pump:** while Bellman is stopped, write `completed` for
  the first firing and make the second firing due. Restart and assert the first reply is
  ingested before any dispatcher recovery, notification replay or scheduler delivery can
  publish the second firing.
- Shutdown with lanes busy drains rather than truncating the log.
- The scheduler thread's own time per fire stays bounded — measure it, since "we moved the
  work off the loop" is easy to believe and easy to get wrong.
