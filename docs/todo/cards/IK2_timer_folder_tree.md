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
- **Slug rules identical on all three platforms.** See the verified rules below — the
  original phrasing of this card was wrong on the most dangerous case.
- **Renaming a timer does not rename the folder** — integrations depend on the path. The live
  name lives in `timer.json`.
- `timer.json` is readable, **not authoritative**: Bellman writes, humans read, hand edits are
  ignored. It carries the `note` field saying so, because someone will edit the time and
  wonder why nothing happened.
- `status.json` written by Bellman at fire (`state: "fired"`). IK3 adds the app's side and
  folds it in. It is the **mirror**: `cat status.json` shows the truth right now, which is
  the reason a human opens this folder at all.
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

## Slug rules — verified against Microsoft Learn, 2026-07-30

The first version of this card said Windows *refuses* trailing dots. **It does not — it
silently strips them**, inconsistently across APIs (Explorer, `CopyFile`, `os.mkdir` all
trim; `NtCreateFile` does not). That is worse than an error: timers named `backup.` and
`backup` collapse into the **same folder**. Strip trailing dots and spaces yourself; never
rely on the OS to reject them.

**Reserved device names** apply to directories, not just files — Microsoft Learn states a
directory "must follow all the same naming rules as a regular file". The full current list:

```
CON PRN AUX NUL
COM1..COM9  COM¹ COM² COM³
LPT1..LPT9  LPT¹ LPT² LPT³
```

The superscript forms are real, not a documentation typo — Windows treats ISO-8859-1
superscript digits as digits. Matching is **case-insensitive** (`con` = `CON`).
`COM0`/`LPT0` are genuinely ambiguous: absent from the current canonical list, claimed
reserved elsewhere in Microsoft's docs, and creatable in practice (golang/go#67245). **Block
them anyway** — it costs nothing.

**The `-<hexid>` suffix already escapes reserved names.** The match is on the exact stem
(optionally + `.ext`), so `CON` is blocked but `CON-3f1a` is not, and `CONX` was never
reserved. **Detect and escape the reserved stem anyway** — do not let the suffix be the only
defence. Two reasons: the exemption depends on the separator never becoming a dot
(`CON.3f1a` *is* blocked), and any code path deriving a sibling file from the bare name
(`CON.json`) is exposed again.

**Illegal characters** — Windows-illegal but legal on Linux: `< > : " / \ | ? *`, plus ASCII
control characters `0x00`–`0x1F`. On macOS only `/` and NUL are illegal at the filesystem
layer, but **`:` must still be avoided** — APFS stores it, and Finder silently displays and
accepts it as `/`.

**Pipeline:** sanitise → strip trailing dots and spaces → escape a reserved stem → append
`-<hexid>`.

**Crate:** prefer `sanitize-filename` (0.6.0, Nov 2024, ~9.3M downloads, has a `windows`
flag). Avoid `sanitise-file-name` — single release, January 2022, no updates since. Either
way, own the reserved-name check rather than trusting a dependency with it.

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
- **Slug tests, unit-level and OS-independent** — real Windows validation belongs to M9:
  - `CON`, `con`, `COM1`, `COM¹`, `LPT3`, `COM0` each produce a safe folder name.
  - `backup.` and `backup` produce **different** folder names — the trailing-dot collision.
  - `a:b`, `a/b`, `a<b`, and a name containing `0x07` are each sanitised.
  - `CONX` is left alone — it was never reserved, and over-escaping is its own bug.
- Retention: exceeding any of the three limits prunes correctly and logs what went.
