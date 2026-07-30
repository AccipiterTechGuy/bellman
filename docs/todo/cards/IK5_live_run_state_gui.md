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
an open run only exists while an app is working. A tab that is blank 99% of the time teaches
the operator to stop opening it, and this is the one thing they would want to notice.

## Scope

**1. Live state on the timer's row in `All timers`.** Where the operator already looks, no
navigation, noticed passively.

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

**3. `overdue` is a label, not an ending.** Shown once the app's own
`expected_secs × factor` has passed. Computed **at render time** by comparing two numbers —
no timer, no wakeup, no background task. The run is still `running` and may still complete.

**4. Open runs pinned to the top of `Run history`.** "What is happening" above "what
happened" reads naturally, and the page already exists (`ui/src/HistoryPage.svelte`).

**5. A timer detail view** showing the current run in full: state, elapsed, `progress`,
`expected_secs`, `app_name`, and the run's `run_id` so it can be matched against the log.

**6. Every terminal state renders distinctly**: `completed`, `failed` (with `failure_kind`
telling `reported` from `timed_out`), `no_ack`, `superseded`. A human should be able to tell
"the app said it failed" from "the app went quiet" from "the timer fired again over it"
without opening anything.

## Where the data comes from

`status.json` — the mirror. Read it through an existing Tauri command; do not invent a second
source of truth and do not parse the event log for this. The log has no progress in it.

## Idle cost

Bellman's design point is a small resident footprint (`docs/PERF.md`).

- **No polling when nothing is running.** If no timer has an open run, this feature costs
  nothing.
- While a run is open, refresh at a human rate — seconds, not frames. `progress` is prose for
  a person, not telemetry.
- Reuse whatever update mechanism the GUI already has rather than adding a second loop.

## Exit gate

- A timer with an open run shows its live state in `All timers`, and the row updates as the
  app writes `progress`.
- An app that sends **no** heartbeat and **no** progress shows `running` plus elapsed time and
  **no placeholder text anywhere** — asserted, since a stray "never" is the obvious slip.
- `overdue` appears after the app's own deadline passes, and the run can still reach
  `completed` afterwards, with the row updating to match.
- `completed`, `failed:reported`, `failed:timed_out`, `no_ack` and `superseded` are each
  visually distinguishable — screenshot-reviewed on WebKitGTK first.
- Open runs appear at the top of `Run history`; past runs still list below them.
- **No new tab**; the five existing tabs are unchanged.
- With no open runs, no polling occurs — proven by measurement, not assertion.
