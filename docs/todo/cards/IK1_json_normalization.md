# IK1 — Normalise the JSON shapes

Repo: `~/bellman`. Design: **`docs/todo/json_normalization.md`** rules R1–R6. Read it first.

No new features. This card makes the three existing JSON shapes agree, before anything is
built on top of them.

## The problem

Three shapes grew in three cards and drifted:

- **`kind` means two different things.** Event log: `"kind": "fired"` (the event). Fire
  notification (`WriteSlotPayload`): `"kind": "daily"` (the occurrence). Same field name,
  opposite meaning, in two files the same integrator reads. This is the real trap.
- **Four names for a moment**: `ts`, `fired_at`, `next_fire`, `claimed_at`.
- **No version on the event log.** Slot messages carry `schema`; `EventRecord` carries
  nothing, so a consumer cannot version-check.
- **Two overlapping status vocabularies**: `SlotStatus` and `EventKind`.

## Scope

- `schema` on **every** JSON Bellman writes, including `EventRecord` — `bellman-event/1`.
- Top-level `kind` means the **event kind** everywhere. `WriteSlotPayload`'s occurrence
  becomes `occurrence_kind`, and it gains a real top-level `kind` (`fired`).
- Rename `ts` → `logged_at`, `next_fire` → `next_fire_at`. Every timestamp ends `_at`;
  `scheduled_for` is the one deliberate exception — it is an intent, not an occurrence.
- One run-state vocabulary, per R5, shared by the log and (later) the reply channel.
- Update `docs/INTEGRATION.md` and the four copy-paste clients to match.

## Do NOT

- Do not add `deny_unknown_fields` anywhere. Tolerant readers are BUILD_PLAN rule 7 and are
  what let the shape grow — every later card in this series depends on it.
- Do not build a compatibility shim. Pre-1.0, the README says formats can change: this is a
  clean break with a version bump.
- No folder tree, no reply channel, no lightbulb. Those are IK2/IK3/IK4.

## Exit gate

- Every JSON Bellman emits carries `schema`; a test enumerates the emitters and proves it.
- A repo-wide search finds no remaining top-level `kind` that means an occurrence.
- Every timestamp field ends `_at`, except `scheduled_for`; asserted by a test over the
  serialised shapes.
- Round-trip tests pass for each shape; unknown fields are still ignored on read.
- `INTEGRATION.md` and all four clients reflect the new names, and the clients still run.
