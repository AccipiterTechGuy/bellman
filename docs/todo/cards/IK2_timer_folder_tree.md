# IK2 — The per-timer folder tree

Repo: `~/bellman`. Design: **`docs/todo/json_normalization.md`**, "The per-timer folder tree".
Depends on **IK1**.

## Goal

A human-browsable view of state. Open a folder in a file manager and read what happened — no
CLI, no log parsing.

```
~/.bellman/timers/
├── README.txt
├── bulb-test-3f1a/
│   ├── timer.json      what the timer IS
│   ├── status.json     the CURRENT run
│   └── runs/           frozen past runs
```

## This tree is a VIEW, not the record

The database owns timers; `logs/events.current.jsonl` owns the history of what fired. The
tree can be deleted or rebuilt without losing anything permanent — that is exactly what makes
the deletion rule below safe. Keep it separate from `slots/`, which is the transient
request/response **channel**. Two trees, two jobs.

## Scope

- `timers/<slug>-<short-id>/` with `timer.json`, `status.json`, `runs/`, and a `README.txt`
  at the root explaining the layout to whoever opens it.
- **Slug rules identical on all three platforms.** Handle Windows reserved names (`CON`,
  `PRN`, `AUX`, `NUL`, …) and its refusal of trailing dots, or a timer that works on Linux
  breaks there. Collisions resolved by the short id.
- **Renaming a timer does not rename the folder** — integrations depend on the path. The live
  name lives in `timer.json`.
- `timer.json` is readable, **not authoritative**: Bellman writes, humans read, hand edits are
  ignored. It carries the `note` field saying so, because someone will edit the time and
  wonder why nothing happened.
- `status.json` written by Bellman at fire (`state: "fired"`). IK3 adds the app's side.
- `runs/<UTC-scheduled>.json` frozen at close, plus `closed_at` and `duration_ms`. The
  filename sorts chronologically in any file manager.
- **Deletion:** deleting a timer deletes its folder including `runs/`. No tombstone. Close any
  open run as `cancelled` in the event log **first**; an app whose `status.json` vanished must
  read that as cancelled, not crash.
- **Orphan sweep:** a crash between the database delete and the folder delete leaves a tree
  with no timer. Extend the pruner's existing orphan sweep.
- **Retention**, all configurable: **1 GB** total (hard ceiling, always wins) · **30 days** ·
  **50 runs per timer**. Prune by age, then per-timer count, then oldest-first across all
  timers if still over. Log every prune; never silent.

## Why the count cap is not redundant

A run file is ~600 bytes, so a per-minute interval timer makes 43,200 runs ≈ **26 MB in 30
days** — 2.6% of the ceiling. Size would never trigger, and the folder would hold tens of
thousands of files. Size protects the disk; only the count protects the browsable property
this whole card exists for.

## Exit gate

- Create a timer → its folder appears with `timer.json` and a `README.txt` at the root.
- Fire it → `status.json` shows `fired`; close it → a file lands in `runs/`.
- Rename the timer → folder path unchanged, `timer.json` shows the new name.
- Delete the timer → folder gone; the fire is still findable in `events.current.jsonl`.
- Delete a timer **with an open run** → the run is logged `cancelled` before removal.
- A timer named `CON` produces a valid folder on Windows.
- Retention: exceeding any of the three limits prunes correctly and logs what went.
