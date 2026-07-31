> ARCHIVED 2026-07-31 — shipped; card bellman-ik5-live-run-state-in-the-gui, run 2026-07-31_0003, merge b37b7d3.

# IK5 — Live run state in the GUI

Repo: `~/bellman`. Depends on **IK3** (the data must exist first).
Design: `docs/todo/json_normalization.md`; `cards/IK3_reply_channel.md`.

## Why this card exists

IK2–IK4 are files, protocol and docs. Nothing puts a running run **on screen**. Today the
only way to see that an app is working is to open the timer's folder in a file manager.

And it is not recoverable from the log: per IK3, **heartbeats and progress are never logged**.
They live in `status.json` and nowhere else. So this card is the only place progress is ever
visible in the application — without it, the reply channel is correct and invisible.

There is a hole in the UI between "will fire" (All timers) and "has fired" (Run history).
Nothing covers "is running right now".

## Do NOT add a tab

The existing tabs stay as they are: `All timers · Week · Month · Run history · Settings`.

An "Active" tab would be empty almost all the time — most timers finish in milliseconds, and
a current non-terminal run exists only while delivery or app work is unresolved. A tab that
is blank 99% of the time teaches the operator to stop opening it, and this is the one thing
they would want to notice.

## Scope

**1. Live state on an integration-owned timer's row in `All timers`.** Where the operator
already looks, no navigation, noticed passively. An unowned action-only timer's
`status.json: fired` is a firing snapshot, not an app claiming to work; render its normal
next-fire/action outcome UI and do not pin or poll it as an active app run.

```
bulb-test          ● running · 7s · bulb on, 7s elapsed
morning-backup     next: tomorrow 06:00
weekly-report      ⚠ failed · timed out
report-mailer      ● running · 74m · overdue (expected ~10m)
```

**2. Absent optional fields render as NOTHING.** No "never", no "—", no greyed placeholder.
An app that sends no `heartbeat_at` or `progress` is a normal app and most are; its row shows
`running` and an elapsed time, and nothing else. Absence is not a state and must not look
like one.

**3. `overdue` is a label, not an ending.** Shown once the app's own plain `expected_secs`
has passed — **1×, not the watchdog's `× factor`**. Computed **at render time** by comparing
two numbers: no timer, no wakeup, no background task. The run is still `running` and may
still complete.

The two thresholds are deliberately different, and that is the whole value of the label:

| when | what happens | who sees it |
|---|---|---|
| `expected_secs` (15 min) | row reads `overdue` | a human, if looking |
| `expected_secs × factor` (30 min) | state becomes `failed` / `timed_out` | recorded, logged |

Firing the label at `× factor` too would make it land on the same second as the verdict, so
it would tell nobody anything they were not about to be told anyway. At 1× it is an early
warning; at 2× it is a judgement. **The label never fires an event** — no log line, no state
change, nothing but pixels.

**A re-sent `expected_secs` replaces the old one** (IK3): the label recalculates from the
unchanged `fired_at` with the latest accepted value — an app that learns mid-run the job is
bigger moves its own warning later, and that is correct, not drift.

A heartbeat with no new `expected_secs` does **not** move the overdue label: it restarts only
the watchdog countdown. The label always compares the latest estimate with the original
`fired_at`.

**The label's clock starts at `fired_at`.** `overdue` ⇔ `now − fired_at > expected_secs`,
both from `status.json`. This is deliberately a *different* anchor from the watchdog, which
counts on Bellman's monotonic clock from the moment it received the reply (R8) — and that is
fine, because the two answer different questions. The label answers the human one: "this
fired 20 minutes ago and said 15" — which is exactly R8's advisory example. The watchdog
answers a contractual one and needs the tamper-proof clock. `fired_at` is Bellman's own
stamp, exists on every run from T0, and needs no new field; anchoring the label on
`acknowledged_at` would mean a slow-to-ack app postpones its own warning.

For an app that never opted in, only the label exists. That is R8's advisory case, unchanged:
"running, overdue — 47m elapsed, expected 10m", and Bellman never acts on it.

**4. Current non-terminal runs pinned to the top of `Run history`.** "What is happening" above "what
happened" reads naturally, and the page already exists (`ui/src/HistoryPage.svelte`).

**5. A timer detail view** showing the current run in full: state, elapsed, `progress`,
`expected_secs`, `app_name`, and the run's `run_id` so it can be matched against the log.

**6. Every terminal state renders distinctly**: `completed`, `failed` (with `failure_kind`
telling `reported` from `timed_out`), `no_ack`, `superseded`. A human should be able to tell
"the app said it failed" from "the app went quiet" from "the timer fired again over it"
without opening anything.

## Where the data comes from

`status.json` — the mirror. Add a narrow Tauri read command (or extend `list_timers`) for this
view; no current run-status command exists. Do not invent a second source of truth and do not
parse the event log for this. The log has no progress in it.

## Idle cost

Bellman's design point is a small resident footprint (`docs/PERF.md`).

- **No polling when nothing is running.** If no timer has a current non-terminal run, this feature costs
  nothing.
- While a run is open, refresh at a human rate — seconds, not frames. `progress` is prose for
  a person, not telemetry.
- Extend the existing Tauri event plumbing (`timer-fired` is the current pattern) with one
  `run-status-changed` invalidation after every accepted status projection, including a
  terminal-but-current revision. The UI refetches the affected row; the event carries no
  second copy of state. The seconds-rate frontend timer exists only to advance
  elapsed/overdue text while an owned run is non-terminal; stopping it must not hide
  `completed → failed` or a late watchdog revision. Do not add a second polling loop.

## Exit gate

- An integration-owned timer with a current non-terminal run shows its live state in
  `All timers`, and the row updates as the app writes `progress`.
- An unowned action-only timer may have current `status.json: fired`, but it is not pinned or
  polled as an active app run and never displays `no_ack`.
- An app that sends **no** heartbeat and **no** progress shows `running` plus elapsed time and
  **no placeholder text anywhere** — asserted, since a stray "never" is the obvious slip.
- **`overdue` at 1×, `failed` at `× factor`** — a run with `expected_secs: 900` and a 2×
  factor reads `overdue` at 15 minutes while still `running`, and only becomes
  `failed`/`timed_out` at 30. Asserted at both thresholds; using one number for both is the
  obvious-looking wrong move.
- Crossing into `overdue` writes **nothing** — no event, no log line, no state change.
  Asserted by counting log lines across the boundary.
- The label anchors on `fired_at`: a run that fired at T, acked at T+5m, `expected_secs: 900`
  reads `overdue` at **T+15m**, not T+20m.
- A heartbeat at T+14m postpones an opted-in watchdog but the row still becomes overdue at
  T+15m; only a newly accepted `expected_secs` changes the label threshold.
- An app with no `error_detection` shows `overdue` and is **never** failed for it, however
  long it runs.
- The run can still reach `completed` after `overdue`, with the row updating to match.
- `completed`, `failed:reported`, `failed:timed_out`, `no_ack` and `superseded` are each
  visually distinguishable — screenshot-reviewed on WebKitGTK first.
- With no frontend polling active, revise the same current run `completed → failed`; the
  backend change notification refreshes the row/detail/history state.
- Current non-terminal runs appear at the top of `Run history`; past runs still list below them.
- **No new tab**; the five existing tabs are unchanged.
- With no current non-terminal runs, no polling occurs — proven by measurement, not assertion.
