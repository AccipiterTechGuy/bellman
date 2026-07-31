> ARCHIVED 2026-07-31 — shipped; card bellman-sch2-slot-created-timers-must-actually-fire-fix-lightbulb-proof, run 2026-07-31_0007, merge 7a2163e.

# SCH2 — A timer created through the slot channel must actually fire

Repo: `~/bellman`. Fix **and** the end-to-end proof, in one card: the fix is
small and the reason it went unnoticed is that nothing ever tested the whole
loop on a real clock. Shipping one without the other would leave the same
hole.

Design: `docs/INTEGRATION.md`; `cards/SCH1_fire_dispatcher_lanes.md`.
Uses the existing `testing_apps/lightbulb/` (and `lightbulb_gui/` if DEMO1
has shipped) as the test client — do not write a new demo app for this.

## The defect

**A timer created through the slot channel never fires in a running Bellman.**
The slot channel is the documented way for an external application to create
its own wake-up timer, so this silently breaks the entire integration story:
every instruction in `INTEGRATION.md` can be followed correctly and the timer
still never goes off.

Observed 2026-07-31 against the running desktop app:

| timer | created via | result |
|---|---|---|
| `lightbulb-demo` | `bellman slot-submit` (separate CLI process) | 38 min overdue, never fired |
| `probe-live-claim` | published to `free/`, claimed by the **running** app | two 120 s intervals missed, never fired |
| `qa-collide-*`, `qa-nearby-two-min` | GUI | fired on time, same process, same minute |

Neither slot-created timer's `next_fire_utc` ever advanced; both stayed
`enabled: 1` with no `fired` event in the log.

**Mechanism.** The engine keeps a near-horizon `BinaryHeap` in memory.
`next_sleep()` is `min(next_fire − now, max_sleep)` with `max_sleep` 30 s, so
the loop does wake regularly — but `tick()` only drains the heap it already
has. Rebuilding the horizon from the store happens on an explicit `Refill`
control message. Grep the tree: `.refill()` is called from
`src-tauri/src/commands.rs` (GUI add/edit/delete) and `state.rs:385`
(`cli_run_now`), and from nowhere in `crates/bellman-core/src/slots/`. The
slot watcher *holds* a `ControlHandle` but uses it only to arm IK3 reply
deadlines (`slots/watcher.rs:311-313`). So a slot mutation lands in the
database and the running scheduler never learns of it — until a GUI edit, a
clock jump, or a restart happens to rebuild the horizon for other reasons.

## Two distinct paths — and one fix does not cover both

This is the part to get right; an obvious one-line patch fixes only half.

**Path A — the running app processes the request.** The app writes into
`free/`, the running Bellman's watcher claims and applies it. The watcher
knows a mutation happened, so it can refill directly. **Fix: refill after any
watcher-processed slot request that added, modified or deleted a timer.**
Refill only when something actually changed — a rebuild is a store query and
must not run on every idle poll.

**Path B — another process applies the request.** `bellman slot-submit` (the
helper the docs recommend, and the one used in `INTEGRATION.md`'s copy-paste
clients) claims and applies the request *itself*, writing to the same
database. The running app never sees a slot request at all, so no watcher
refill can help it. This is the path the first observation above took.

Path B needs the running scheduler to notice that **someone else** changed
the store. Pick one and state why in the commit:

1. **A periodic horizon rebuild** — a bounded safety net (say every 60 s, or
   the existing `max_sleep` tick when the heap is empty). Simplest, covers
   any external writer forever, costs one store query per interval. Note that
   it also subsumes Path A's promptness problem, though Path A should still
   refill immediately rather than wait for the tick.
2. **Detect external writes cheaply** — SQLite's `PRAGMA data_version`
   changes when another connection commits, so the loop can rebuild only when
   the store actually moved. Cheaper than an unconditional rebuild, and more
   precise.
3. **Make `slot-submit` defer to a running instance** — publish only, and let
   the daemon claim it. Rejected unless the other two prove unworkable:
   detecting "is Bellman running" is exactly the kind of ambient guess this
   codebase has avoided, and it would make the CLI's behaviour depend on
   invisible state.

Option 2 with option 1 as the floor is the recommendation. Whatever is
chosen, **both paths must be covered by tests**, because they fail
independently.

Also verify the sibling operations, which have the same shape and were never
exercised either: a slot **modify** that moves a fire time must take effect
without a restart, and a slot **delete** must remove a timer from the heap so
it does not fire as a ghost after deletion.

## The proof — use the lightbulb, on a real clock

Automated Rust tests are required, but they are not sufficient on their own:
this bug survived a full integration kit precisely because every test drove
the fire path directly. The acceptance evidence is an application waking on
its own schedule with nobody touching it.

1. Start a real Bellman (desktop app or the same store-driving process).
2. Start `testing_apps/lightbulb/lightbulb.py`.
3. Create its timer **only** through the slot channel, on a short interval —
   both paths, one run each: once published to `free/` for the running app to
   claim, once via `bellman slot-submit`.
4. **Do not call `run-now`.** Wait for the timer's own scheduled second.
5. The bulb lights, the app replies, the run reaches `completed`.

That last point is the whole card. The demo that "proved" the loop on
2026-07-31 was fired by hand; had it been left to its own interval it would
have shown nothing. Any future demo evidence must state whether the fire was
scheduled or manual.

If DEMO1 has shipped, run the same two paths through `lightbulb_gui/` as
well — it creates its own timer through the slot protocol, so it exercises
Path A by construction.

## Exit gate

- A timer created by publishing to `free/` and claimed by the **running**
  Bellman fires at its scheduled second, with **no restart** — asserted in an
  automated test, not only observed.
- A timer created by `bellman slot-submit` while a Bellman is running fires
  within the documented bound of its scheduled second, with no restart.
- A slot **modify** that moves the fire time takes effect without a restart;
  a slot **delete** stops the timer firing, with no ghost fire from a stale
  heap entry.
- The heap is **not** rebuilt on idle polls where nothing changed — asserted
  by counting store queries across an idle period, so the fix does not become
  a busy loop.
- **Live lightbulb evidence**: `testing_apps/lightbulb/` completes a run that
  was fired by the scheduler on its own interval, with the event log showing
  `fired → acknowledged → completed` under one `run_id` and no `run-now` in
  the transcript. Recorded in the card's evidence notes.
- The bug's own reproduction — create through the slot channel, wait past two
  intervals, assert it fired — exists as a regression test that fails against
  the current `main`.
- `docs/INTEGRATION.md` states plainly what an app can expect: after a
  successful slot response, the timer is live in the running scheduler within
  a bounded time. If a bound is chosen rather than immediacy, the doc names
  the number.

## Why this is also a validation finding

Nothing here is exotic. It was missed because every existing test drives the
fire path directly rather than letting a clock do it, and because the demo
was always fired by hand. Full-system validation should carry a standing
item: **at least one test where nobody touches anything and the schedule does
the work.**

## Evidence notes (2026-07-31, train run 2026-07-31_0007)

Fix: option 2 + option 1 floor from the card. The scheduler loop polls
SQLite's `PRAGMA data_version` on every wake (a foreign commit — e.g.
`bellman slot-submit` applying a request on its own connection — bumps it;
own commits do not) and rebuilds the horizon heap when it moves, with an
unconditional rebuild every `external_rebuild_interval` (default **60 s**) as
the floor. The watcher refills immediately after any poll that processed a
slot request (Path A). Idle ticks cost one pragma read and **zero** horizon
queries (asserted via a store query counter). Detection bound for foreign
writers: one `max_sleep` tick (**30 s**), 60 s worst case — now stated in
`docs/INTEGRATION.md` rule 5.

Automated tests (all in `crates/bellman-core`): both paths, slot modify,
slot delete (no ghost fire), idle-no-rebuild, floor. The two regression
tests were verified RED against pre-fix behaviour (fix temporarily disabled)
and GREEN with it.

Live lightbulb evidence — **every fire below was scheduled by the running
Bellman desktop app itself on a real clock; `run-now` was never invoked**
(full transcript: `docs/qa4-evidence/sch2-live-lightbulb-transcript.log`,
harness: `docs/qa4-evidence/sch2-live-orchestrate.py`, isolated Xvfb +
private D-Bus + XDG data dir, app built from this branch):

| run | created via | scheduled_for | fired (lateness) | run completed |
|---|---|---|---|---|
| lightbulb `sch2-live-slot-submit` | `bellman slot-submit` (separate process) | 19:09:19.009 | 19:09:19.011 (**2 ms**) | fired→acknowledged→completed, one run_id |
| lightbulb `sch2-live-free-publish` | publish to `free/`, running app claimed | 19:10:10.077 | 19:10:10.080 (**4 ms**) | fired→acknowledged→completed, one run_id |
| lightbulb_gui (DEMO1) `lightbulb-gui-demo` | GUI's own slot-protocol create | 19:10:56 | 19:10:56 (**3 ms**) | fired→acknowledged→completed, one run_id |
| lightbulb_gui `sch2-gui-slot-submit` | `bellman slot-submit`, GUI answered | 19:11:49 | 19:11:49 (**3 ms**) | fired→acknowledged→completed, one run_id |
