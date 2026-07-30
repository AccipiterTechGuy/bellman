# Bellman JSON — the normalised shape

Locked design. Every integration/run document defined here obeys these rules. IK1 migrates
the R1–R6 wire shapes/vocabulary where today's code differs; the card index assigns the later
runtime and persistence rules to IK2/IK3. Internal operator configuration such as
`config.json` is not a protocol message and is outside these shapes.

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

**R1 — every protocol/view JSON defined here carries `schema`.** `bellman-event/1` for log
lines, `bellman-slot/1` for slot messages, and the named timer/run/reply schemas below. No
exceptions among these integration surfaces, including the event log and quarantine
metadata. This rule does not retrofit unrelated internal `config.json`.

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
| `no_ack` | Bellman — **no acknowledgement was received** (a filesystem read leaves no trace, so "nobody read it" is unknowable — say what was observed) | **provisional terminal** — a late valid reply or a late `ack_through` supersedes it while the run is still current |
| `cancelled` | Bellman — the timer was deleted while its run was open | yes |
| `failed` (`failure_kind: "timed_out"`) | Bellman — only if the app set `error_detection` | **revisable** — a late app reply supersedes it |
| `skipped_misfire`, `coalesced`, `pruned`, `wake_*`, `year_recalibrate` | Bellman | as today |

**What counts as pickup:** any valid reply state (`acknowledged` / `running` / `completed` /
`failed`) **or** the existing slot-feed cursor (`ack_through`) advancing past this run's
event. The slot feed is a real, durable acknowledgement path that predates the reply channel
— declaring `no_ack` while it shows the app acked would be Bellman contradicting its own
records. The pickup deadline is its own persisted deadline; the existing `ack_grace` constant
may seed its default, but the two jobs (pickup timeout vs pruning grace) stay separately
named and separately configurable.

Pickup/app lifecycle exists only for a run that snapshots an integration owner. An unowned
human timer has no reply stub or fire notification, no pickup deadline/`no_ack`, and no
watchdog; its configured `Action` still runs and its delivery outcome remains in the claim
and event log. Its `status.json` is the current firing snapshot (`fired`/`fired_late`), not a
claim that an app is working. IK5 must not treat that unowned snapshot as an active app run.
For `superseded`/deletion only, "unresolved" therefore has two exact tests: an owned run's
app lifecycle is non-terminal, or an unowned run's action claim is not `finished`.

**R6 — readers stay tolerant.** Unknown fields ignored, never `deny_unknown_fields`
(BUILD_PLAN rule 7). This is what lets the shape grow without breaking old consumers.

**R7 — a deadline on pickup, never on completion.**

- **Pickup** has a deadline (default 60s, its own persisted setting — `ack_grace` may seed
  the default, but pickup timeout and pruning grace are different jobs with separate names).
  Its active countdown starts on Bellman's monotonic clock when the fire transaction commits;
  persist the corresponding wall-clock deadline for restart recovery, with the same explicit
  clock-jump limitation as the watchdog fallback. Action queueing, transport retries and
  `expected_secs` never move it.
  Pickup is satisfied by any valid reply state **or** the slot-feed `ack_through` advancing
  past this run — see the pickup definition under R5. Whether a file was merely *read* is
  not knowable and is never claimed. Lapsed ⇒ `no_ack` ("no acknowledgement was received"),
  revisable while the run is still current by either pickup signal: a late valid reply or
  `ack_through` advancing past this run. A late cursor revision records `acknowledged`; it
  does not invent `running` or completion. This is one instance of the allowed
  Bellman-inference-to-app-state revision: `no_ack` is Bellman's provisional observation,
  not an app-authored closing verdict.
- **Completion has no timeout, ever.** How long the other program takes is unknowable —
  seconds, minutes, hours. A run stays non-terminal until the app reports an ending.
  **Nothing ever auto-completes.**

An unfinished run is not `failed` — `failed` means the app *said* it failed. It stays
`running` and **ages**, so the history reads "running for 3 days", which is the truth and is
obviously wrong to a human without Bellman pretending to know why.

**R8 — the estimate is advisory unless the app OPTS IN to a watchdog.**

By default `expected_secs` is display-only: "running, overdue — 47m elapsed, expected 10m".
Bellman never acts on it. Guessing another program's duration is not Bellman's business.

But an app may set **`error_detection: true`** in its reply, and that changes the contract:
the app is **consenting** to be watched against a deadline **it declared itself**. That is
not Bellman guessing — it is the app asking. Default is `false`; silence means advisory.
`true` is valid only when the accumulated reply has a positive `expected_secs`.
`error_detection` follows the normal accumulation rule: omission retains the last value,
while an explicit `false` cancels any pending watchdog without removing the advisory
estimate.

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
- **A heartbeat restarts the countdown.** More exactly, while the watchdog is enabled, each
  distinct accepted non-terminal reply arms/rearms it at Bellman's receipt time; this covers
  a new `heartbeat_at`, changed progress, a state advance or a new estimate. An exact duplicate
  is a no-op and cannot extend the deadline merely because a watcher rescanned it. The app's
  `heartbeat_at` value is display data, never the arithmetic anchor.
- **The outcome is `failed` with `failure_kind: "timed_out"`.** One state to reason about,
  and the distinction is preserved where it matters: `reported` means the app said it
  failed, `timed_out` means the app went quiet past its own deadline. Those need different
  reactions from a human. (`no_ack` stays its own state — Bellman observed no pickup signal.)
- **Marking is not killing.** Bellman flags the run; it does not terminate the process. If
  Bellman launched a configured wake action, that action's existing 60s launch timeout (and
  SCH1 `Replace` cancellation) remains a separate execution policy. Watchdog expiry never
  invokes either path and adds no kill of its own.
- **A late reply REVISES the state.** `completed` arriving after the run was marked failed
  moves it to `completed`. The state always shows the best available truth; nothing stays a
  lie. Three properties make that safe:

  - **The log does not flip.** `status.json` holds the latest state; `events.current.jsonl`
    is append-only and keeps both facts, so "marked failed 05:15, completed 05:22" survives.
    That sequence is the interesting story and would be lost if only the state remained.
  - **One direction only.** An app's own report always beats Bellman's inference — Bellman
    *deduced* silence, the app *knows*. Bellman must never flip an app's `completed` back to
    failed. Bellman's guesses are overridable; the app's claims are not.

    The app may also revise **itself**: `failed` then `completed` on the still-current run is
    accepted, exactly like revising a watchdog verdict. One rule covers both — the app's
    latest terminal report wins, whatever it replaces. (After an **app-authored** terminal
    report, moving back to a non-terminal state is different and is refused. A provisional
    Bellman `no_ack` or watchdog `timed_out` may move to a valid app-authored state while
    current; those are inference revisions, not app verdicts moving backwards. See IK3.)
  - **Only while the run is still current.** A reply for a run the folder has already moved
    past — the timer fired again — is rejected as `superseded`, not applied. Revision reaches
    back through time, never across runs.

Cheap to implement precisely because Bellman is a scheduler — a watchdog deadline is one
entry in the heap it already runs.

**R9 — a reply is data, never a command.** Bellman parses, validates and logs it. It must
never launch, execute, schedule or modify anything because an app said so. Worst case for a
hostile reply is one bad log line.

Open reply paths no-follow and validate the opened handle is a regular file (reject Unix
symlinks/FIFOs/devices and Windows reparse points). Size-check and read the capped bytes from
that same handle, never from a second path lookup. Otherwise a path swap can turn "parse
64 KB of data" into blocking on a pipe or copying an unrelated file.

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
  That serialization point is an **interprocess per-timer gate**, not an in-process mutex.
  Every mutation of the current app lifecycle uses it: file or IPC reply ingest,
  `ack_through`, pickup/watchdog deadlines, timer deletion, folder reconciliation, scheduled
  delivery, GUI `run_now` and standalone CLI `run-now`.
  A fire producer holds it from the barrier read through the SQLite fire transaction and the
  short post-commit `status.json` / create-only reply projection. A crash releases the gate;
  startup then reconstructs the database-owned view before delivery resumes. The producer
  releases the gate **before** enqueueing or waiting for action execution; a long `run_now`
  must never block the reply watcher.
  Every other lifecycle mutator likewise re-reads the current `run_id`, state and deadline
  inside the gate, commits its transition/outbox rows, then projects `status.json` before
  releasing it. A reconciler may not write a snapshot captured before taking the gate.
  Worker claim outcomes do not use this lifecycle path because SCH1 forbids them from
  changing `status.json`.
  Lock identity must survive rename/deletion: use a bounded stable shard set under the data
  root (for example 256 lock files selected by the timer UUID), never a lock file inside the
  timer folder. Deleting a folder while another process holds a lock on an inode inside it
  would let a third process create a new inode at the same path and enter concurrently.
  Hash collisions may serialize unrelated timers but do not change correctness.
- **A partial writer cannot hold firing forever.** Normal watching releases the gate between
  debounce reads and keeps retrying while that run remains current. A pre-fire/startup
  barrier waits at most one existing 200 ms debounce window: re-read under the gate, ingest
  if complete, reject only if the identical invalid bytes are stable, and otherwise let the
  new firing proceed without quarantining the changing file. A valid reply completed after
  that final barrier read is the accepted true-simultaneity race and becomes superseded; the
  "new firing always proceeds" rule forbids an unbounded parse wait.
- **One transaction per fire.** The previous run's final known state — including anything the
  barrier just ingested — its `superseded` event if the prior run is unresolved by the exact
  owner/claim test above, the new `run_id`,
  the `fired` event and any pending log lines commit to SQLite **together**. Only then is
  `status.json` rewritten. Crash before the commit and the previous firing is still current;
  crash after it and startup rebuilds the file. There is no window where the folder claims
  something the database never recorded.
- **A file error is recoverable without a restart.** A failed post-commit `status.json` write
  or reply-stub create leaves the database truth intact, surfaces the error, and signals a
  bounded periodic folder reconciler. It rewrites database-owned `status.json` and creates a
  stub only when its per-run path is absent, using `O_EXCL`; an existing reply path is never
  rebuilt or overwritten. This is the same recovery startup performs, not a second state
  machine.
- **Projection order is observable.** After commit, project `status.json`, then create the
  per-run stub, then make the fire notification/transport eligible. A file-transport app
  must never receive a `reply_path` that Bellman has not successfully created (or lost
  `O_EXCL` because the app already created a real reply there). If either required run-file
  projection fails, the transport row stays pending until the live reconciler repairs it;
  IPC skips only the stub step, never `status.json`. This also defines the crash between the
  two run-file writes: startup/live reconciliation finishes the missing second projection
  before any delivery pump publishes the fire.
- **Startup reads replies before the scheduler fires anything.** An app can answer while
  Bellman is stopped. If the scheduler runs first, that run is superseded before its reply
  was ever read, and the outcome is recorded unknown **silently** — the worst kind of loss,
  because nothing anywhere says a reply existed. So, in order: scan every `reply-*.json`,
  fold in what is valid and still current, drain pending outbox rows through R11, rebuild `status.json`
  from the database, recreate **missing** stubs create-only (`O_EXCL`) — and only then start
  delivery. **Never rebuild or overwrite an existing reply path**: startup obeys the same
  never-write-over-a-live-reply-file law as the watcher, because an app can be writing at
  this exact moment; a lost `O_EXCL` race to a real reply is the correct outcome.

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
- **The publisher has a live feeder.** It drains after local enqueue, after recovery/rotation,
  and on a periodic safety tick no slower than once per second. A different process's SQLite commit cannot rely on
  an in-process wakeup; the tick guarantees a CLI-enqueued row does not wait forever while an
  otherwise idle GUI owns the lease.
- **Append errors surface.** `emit()` currently discards them (`let _ = log.append(&rec)`,
  `actions/runner.rs:128`) — under this rule a failed append leaves the outbox row in place
  for retry and updates an operator-visible publisher-health error; it is never silently
  dropped.

A line is durable when it is **synced, not flushed**: enqueue, append, flush, **fdatasync
(`sync_data`)**, then mark published, and retry after a failed write or a restart. A flush
moves bytes into the OS cache and survives a process crash only — marking published at flush
would let a machine crash lose the line with the outbox row already cleared, which is why
the mark comes after the sync. Events are low-rate, so the sync cost is irrelevant — and if
it ever is not, batch the sync, never skip it. If a platform cannot sync, the guarantee is
explicitly downgraded to process-crash-only and documented as such, not silently assumed.

**Delivery is at-least-once, and the file is honest about it.** The duplicate window is a
crash **after a successful sync but before mark-published**: the line is durably on disk,
the outbox row still says pending, so the retry appends the same event **again**. (A crash
between flush and sync is the opposite case — the line may be *gone*, and the retry is a
first append, not a duplicate; a test expecting a duplicate there tests the wrong window.)
`event_id` identifies the duplicate, it does not prevent it; nothing about an id makes a
blind append idempotent. Two duties follow:

- **The publisher checks before retrying**: on startup, scan the current file's tail for the
  pending `event_id`s and skip the ones already physically present.
- **Every reader dedupes by `event_id` anyway** — GUI, `log_query`, anything counting. The
  publisher check shrinks the window; the reader rule is the guarantee.

**Rotation cannot jump ahead of recovery.** Before rotating, the lease holder reconciles
every appended-but-unmarked outbox row against the current tail and marks those already
present. It appends/marks in order and never starts a rotation with such a row outstanding.
Rotation itself has a small durable SQLite journal naming the source, `.rotating` file,
compressed temporary file and final archive. Under the same publisher lease:

1. sync current, record the rotation intent, rename it to `.rotating`, and sync the parent
   directory where the platform supports directory sync;
2. compress to a temporary archive, sync it, rename to the final `.jsonl.gz`, sync the archive
   directory, then delete the `.rotating` source and sync its parent directory;
3. create/open the new current file only after the final archive is durable and the source
   cleanup is recorded, clear the journal, then resume draining the outbox. Do not append
   through an old handle or let another process rotate any phase.

After a crash or newly acquired lease, recover that journal **before** appending or starting
another rotation. Reconcile pending `event_id`s against `events.current.jsonl` **and** the
newest `.rotating` / temporary / final archive named by the journal; finish or roll forward
the interrupted rotation, delete redundant source/temp artifacts only after verifying the
final archive, create the new current file, then drain the outbox. This is why a synced line renamed just
before a crash is not missed merely because it is no longer in the current tail. If a
platform cannot provide the directory-sync guarantee, document the same explicit
process-crash-only downgrade as file sync.

While the journal is active, readers include its plain `.rotating` source in addition to
current/final archives and still deduplicate by `event_id`; they never parse the partial gzip
temporary. Rotation therefore does not create a temporary hole in Run history.

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
| `reply.json`, whole file | **64 KB** | reject without reading the body; leave it in place and write bounded diagnostic metadata to `bad/` |
| `result` as stored in `status.json` | **32 KB** | truncated, `result_truncated: true` |
| `result` as carried on the log event | **2 KB** | truncated, `result_truncated: true` |
| `reason` / `progress` free text | **1 KB** | truncated |
| one JSONL event line, total | **4 KB** | must not happen if the above hold — assert it |

The asymmetry between 32 KB and 2 KB is deliberate: `status.json` is the current run and is
overwritten next fire, so it can afford detail; the log is append-only and keeps everything
for the retention window, so every byte is multiplied by history. The log line keeps the
head of the result plus the truncation flag — enough to grep, not enough to bloat.

**Quarantine is idempotent and retained.** Leaving rejected current bytes in place must not
copy them again on every periodic scan. Name the artifact deterministically from the source
path plus a digest of the rejected bytes. For an unread oversize file use timer/run identity,
source path and observed length; same-length replacement is intentionally deduplicated
because Bellman refuses to read it. Create the artifact once. Unchanged input is one `reply_rejected` event and
one artifact; changed input may produce a new one. Prune `bad/` with the existing configurable
30-day retention window and a separate configurable **64 MB default aggregate ceiling**,
oldest artifacts first and with payload/sidecar pairs removed together. A capped individual
file without these two rules is still an unbounded disk leak.

Quarantine creation and `bad/` pruning share one stable interprocess lock under the data
root. Write/sync a temporary artifact, then install each immutable payload/sidecar at its
deterministic final name with create-new/no-replace semantics while holding the lock. Startup
removes stale temporaries and an orphan half-pair. A `content_copied: false` oversize sidecar
is a complete single artifact,
not an orphan. When quarantine is reached from reply ingest, lock order is R10 timer shard
then `bad/` lock, never the reverse.

**A large output is the app's to store, not Bellman's.** The documented convention for big
results: write the payload somewhere the app owns and reply with a summary —
`result: { "summary": "…", "path": "/app/owned/file", "sha256": "…" }`. Under R9 the path is
**data**: displayed as text, never opened, followed or executed by Bellman.

**`duration_ms` has one formula.** Bellman's own clock, both ends: monotonic elapsed from
the fire transaction commit (the same anchor as pickup) to **ingesting** the terminal reply.
App timestamps are never subtracted
— an app may skip `acknowledged_at` entirely (legal), and a skewed app clock must not produce
a negative or absurd duration. One anchor pair, computed by Bellman, clamped at zero, present
on the terminal event only. (`fired → completed` directly, no ack: same formula, no special
case.)

A monotonic anchor **does not survive a restart**. When Bellman restarts mid-run, the
original anchor is gone; the fallback is Bellman's own **wall-clock** stamps — ingest wall
time minus `fired_at` (both Bellman's, still no app arithmetic), clamped at zero, and the
event carries `duration_source: "wall_clock"` so a jumped clock's number is identifiable as
the estimate it is. Same-process runs carry no marker — monotonic is the default, the
fallback is the exception.

**Archives are compressed and current is size-bounded.** `events.current.jsonl` stays plain
text — grep-ability of the live log is a feature. Rotate at the ISO-week boundary **or before
an append would take current past a configurable 64 MB default**, whichever comes first.
Rotated archives are compressed; JSONL compresses hard because every line repeats the same
field names. Retention first removes archives older than 30 days, then oldest archives until
`current + final archives` fits the configurable 1 GB retained-log budget. The R11
`.rotating` source and gzip temp may temporarily require two 64 MB extents plus small
codec/filesystem overhead at defaults; they are journal-owned working space, not falsely
counted as retained history, and recovery cleans them. Use gzip via `flate2`, which is already in the dependency tree — do not add a
compression crate for this. `log_query` and the Run-history GUI must read both plain and
compressed archives. The R11 journal/recovery order above owns the crash-safe rename and
compression mechanics.

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
  "integration": { "app_name": "lightbulb" },
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
`integration.app_name` is the single writer authorized for this timer's per-run reply
channel. It is explicit before a reply stub/fire notification exists; a human-created timer
with no integration owner has no app reply channel. The fire transaction snapshots it onto
the run; changing timer ownership affects only later firings and never invalidates the
current app mid-run.

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

### `reply-<run_id>.json` — `bellman-reply/1`

**The only file an integrating app writes.** Overwritten at each step; never read back.

**The filename is per-run** — `reply-` + the **full** `run_id` (`reply-9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08.json`). Full id, not a prefix: a truncated name can collide, and a colliding reuse hands a new run the path a still-alive old app holds — rebuilding the very clobber this exists to kill.
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

Fire notifications and slot request responses never share a path. `SlotService` alone owns
`slots/done/slot-<id>.json`; fire notifications live under `slots/fires/`, normally as
`fire-<full-run_id>.json`. A configured fixed filename under `fires/` is only an at-least-once
wake hint; the durable `SlotRunEvent` feed is the queue. No action-completion path rewrites
either notification.

```json
{
  "schema": "bellman-slot/1",
  "kind": "fired",
  "occurrence_kind": "daily",
  "timer_id": "3f1a…",
  "timer_name": "bulb-test",
  "app_name": "lightbulb",
  "run_id": "9f2c…",
  "scheduled_for": "2026-07-30T05:00:00Z",
  "fired_at": "2026-07-30T05:00:00Z",
  "status_path": "/home/alice/.local/share/bellman/timers/bulb-test-3f1a/status.json",
  "reply_path": "/home/alice/.local/share/bellman/timers/bulb-test-3f1a/reply-9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08.json"
}
```

Changes from today: top-level `kind` becomes the **event** (`fired`), the occurrence moves to
`occurrence_kind`; `app_name` identifies the one configured consumer; and `reply_path` is
added so that app never has to guess where to reply.
Both path fields are absolute native paths as serialized by Bellman — never `~`, an
environment variable or a URI. The Linux example above is illustrative; Windows carries an
absolute Windows path.
`reply_path` is required when the selected delivery uses files. IK6 keeps the same
`bellman-slot/1` message with that field omitted for an IPC-only firing, because no reply
stub exists in that mode; `status_path` remains present for every transport.

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
│   └── reply-9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08.json  where the app answers    (the app writes, Bellman reads)
└── morning-backup-7b22/
```

**This tree is a VIEW, not the record.** The database is the source of truth for timers, and
`logs/events.current.jsonl` plus its archives are the durable retained history of everything
that fired. The folders can be deleted, rebuilt or lost without losing that retained history
— that is what makes the rules below safe.

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
log's retention window, not forever**. Archives are pruned (default 30 days / 1 GB retained
budget, with 64 MB current-file rotation), so every
place the docs or GUI describe history must say "30-day history", never "permanent". A
retention window nobody states reads as a data-loss bug the first time someone looks for a
31-day-old run.

Two cases that follow:

- **A current unresolved run at delete time.** Mark it terminal (`cancelled`) in the event
  log *before* removing the folder. An app whose `status.json` has vanished must read that
  as cancelled, not crash. Do not delete out from under a live run silently.
- **Orphan folders.** A crash between the database delete and the folder delete leaves a tree
  with no timer. The pruner already does orphan sweeps for slots — extend it here.

## No history in the folder — a new run replaces the current view

The folder holds the **current** run only. When an integration-owned timer fires again,
`status.json` is overwritten fresh and a **new** per-run reply file is created; the previous
run's reply file is deleted after its final ingest (never overwritten — different runs never
share a path). An unowned action-only timer rewrites only `status.json`. Nothing from the
previous run is kept there.

There is deliberately no `runs/` directory. History has exactly **one durable home** — the
append-only `events.current.jsonl` and its archives. The Run history page in the GUI
(`ui/src/HistoryPage.svelte`) is a **reader** of that home, not a second copy — there is no
independent GUI store, and wording that implies two durable homes overstates what exists. A
third copy in the folder would buy only "browse past runs in a file manager", and would cost
size caps, age caps, per-timer count caps, pruning and a freeze-before-wipe ordering rule.
Not worth it.

**Consequence, accepted:** if the first firing is unresolved when the second firing occurs,
mark the first `superseded`; for an integration-owned run its final app outcome may remain
unknown. The second firing replaces it as current and rewrites `status.json`, but never
overwrites the first firing's reply path. Log `superseded` **loudly** — it means the interval
is shorter than the app takes, which is a misconfiguration worth seeing.

## `timer.json` is readable, not authoritative

Bellman writes it, humans read it. Hand edits are ignored — the database wins. The file
carries a `note` field saying so, because someone will open it, change the time, and wonder
why nothing happened.
