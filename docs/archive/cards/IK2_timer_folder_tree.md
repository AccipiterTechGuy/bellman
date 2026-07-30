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
│   ├── timer.json           what the timer IS
│   ├── status.json          the CURRENT run
│   └── reply-<run_id>.json    where the app answers — per-run filename   (IK3)
```

**The folder holds the current run only.** There is no `runs/` directory and no history here:
a new fire overwrites `status.json`; IK3 creates a fresh per-run reply file when the timer
has an integration owner (the previous run's is deleted after ingest). History lives in `events.current.jsonl` plus its
archives; the GUI's existing Run history page is only a reader of that log.

## This tree is a VIEW, not the record

The database owns timers; `logs/events.current.jsonl` plus its archives own the retained
history of what fired. The tree can be deleted or rebuilt without losing that retained
history — that is exactly what makes the deletion rule below safe. Keep it separate from
`slots/`, which is the transient
request/response **channel**. Two trees, two jobs.

## Scope

- `timers/<slug>-<short-id>/` with `timer.json`, `status.json`, optional
  `reply-<run_id>.json` (IK3, integration-owned timers), and a `README.txt`
  at the root explaining the layout to whoever opens it. The README must state which file
  answers the question: **`status.json` is the truth; the reply file is only the app's side.**
  They diverge whenever Bellman judged a run (`no_ack`, watchdog expiry) and the app did not
  speak — see IK3.
- **Slug rules identical on all three platforms.** See the verified rules below — the
  original phrasing of this card was wrong on the most dangerous case.
- **Renaming a timer does not rename the folder** — integrations depend on the path. The live
  name lives in `timer.json`.
- `timer.json` is readable, **not authoritative**: Bellman writes, humans read, hand edits are
  ignored. It carries the `note` field saying so, because someone will edit the time and
  wonder why nothing happened.
- `status.json` written by Bellman at fire (`state: "fired"`). IK3 adds the app's side and
  folds it in. Optional fields the app never sent (`heartbeat_at`, `progress`, `expected_secs`)
  are simply absent — never rendered as empty or "never". It is the **mirror**: `cat status.json` shows the truth right now, which is
  the reason a human opens this folder at all.
- **Deletion:** deleting a timer deletes its folder. No tombstone. Mark any current
  unresolved run `cancelled` in the event log **first**; an app whose `status.json`
  vanished must read that as cancelled, not crash.
- **Orphan sweep:** a crash between the database delete and the folder delete leaves a tree
  with no timer. Extend the pruner's existing orphan sweep.
- **The folders need no retention.** Each holds at most three small files for the current run
  (an unowned action-only timer has no IK3 reply file), and
  nothing accumulates — a "50 runs per timer" or size cap here would be pruning history that
  this tree, by design, does not keep. Retention belongs to the **event-log archives**
  (`rotate_and_retain` already exists): **1 GB** retained-log budget · **30 days**, both
  configurable. Current rotates weekly or before crossing a configurable **64 MB** default;
  crash-safe rotation has two 64 MB extents plus small temporary overhead at defaults. Log every prune;
  never silent. Archives are gzip-compressed on rotation (R12) —
  `events.current.jsonl` stays plain, and `log_query` + the GUI read both forms.
- **State the window honestly.** History is findable for the retention window, not forever.
  Anywhere the README or docs describe history, say "30-day history (configurable)".

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

## Exit gate

- Create a timer → its folder appears with `timer.json` and a `README.txt` at the root.
- Fire it → `status.json` shows `fired`; the app completes it → `state: "completed"` with
  `completed_at`, per the `bellman-run/1` shape. (`duration_ms` is on the **log event** only —
  a `duration_ms` appearing in `status.json` means a field IK1 never defined was invented.)
- Fire an integration-owned timer **again** → `status.json` is rewritten for the second
  firing and a new per-run reply file is created; the first firing's reply path is never
  overwritten. If the first firing was non-terminal, `superseded` is logged.
- Rename the timer → folder path unchanged, `timer.json` shows the new name.
- Delete the timer → folder gone; every fire within the retention window is still findable in
  `events.current.jsonl` / its archives.
- Fill current to its 64 MB threshold before the week changes: it rotates/compresses before
  the next line, and age/budget pruning keeps current plus final archives within 1 GB while
  never deleting the live file.
- Delete a timer **with a current unresolved run** → the run is logged `cancelled` (in the
  R5 vocabulary) before removal.
- **Slug tests, unit-level and OS-independent** — real Windows validation belongs to M9:
  - `CON`, `con`, `COM1`, `COM¹`, `LPT3`, `COM0` each produce a safe folder name.
  - `backup.` and `backup` produce **different** folder names — the trailing-dot collision.
  - `a:b`, `a/b`, `a<b`, and a name containing `0x07` are each sanitised.
  - `CONX` is left alone — it was never reserved, and over-escaping is its own bug.
