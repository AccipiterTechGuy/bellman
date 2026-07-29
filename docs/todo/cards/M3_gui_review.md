# M3 — Macro GUI: table, step editor, mandatory review

Design: `macro_recorder_security_plan.md` D-4, D-9, D-10, D-11.

## Scope

- Svelte 5 page, three-pane layout: macro table · step list · detail.
- Table columns: name, steps, duration, linked timer, trust level, last run, reviewed?
- Step editor: delete a step, insert a wait, edit a coordinate, **re-record one step**,
  undo/redo.
- **Mandatory post-authoring review** with Keep / Redact / Secret per step. A macro
  **cannot be saved without passing review.**
- The review screen states the **duration and the fact the machine is unusable during it**
  (D-10) — computed as `steps × repeat`, not per iteration.
- Repeat count field: a plain integer, bounded by the Settings ceiling (D-9, D-11).
- Provenance shown: who authored it, when, under which token, reviewed or not.
- Dry-run / step-through UI.

## Do NOT

- No expression field for the repeat count. An integer, never arithmetic (D-9).
- Do not let review be skippable "for now".

## Exit gate

- Screenshot review on WebKitGTK first (see the GUI QA card about display isolation).
- A test proves a macro **cannot** be saved without passing review.
- Redaction actually removes the literal from the saved blob — decrypt and grep to prove it.
- An agent-authored macro displays as `unreviewed` and the run/attach controls are disabled.
