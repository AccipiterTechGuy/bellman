# M4 — Replay engine + safety rails

Design: `macro_recorder_security_plan.md` D-9, D-10, D-11.

## Scope

- `enigo` behind a `pub(crate)` adapter — never called directly from anywhere else.
- `exec::replay(macro, RunToken)` — the single chokepoint from M1.
- **Stop key** that aborts the whole run including every remaining loop iteration (D-14).
  Default `Ctrl+Shift+G`, remappable, normalised per OS, registered globally for the run.
  Where it cannot be registered, execution is `Unavailable` — never "running with no abort".
- Evaluate a **pointer failsafe** (corner slam) as a path needing no hotkey registration.
- Caps enforced **twice**: refuse pre-flight on the estimate, hard-abort mid-run on actual
  elapsed time. One hung step defeats any estimate.
- Modifier release on abort — never leave a stuck Ctrl or Shift.
- Single-runner mutex: two macros never run at once.
- Screen-fingerprint check before injecting; refuse if the layout moved.
- Bounded repeat per D-9: caps multiply, delay between iterations, stop on failure, no
  nesting.
- On-screen countdown showing the abort key for the whole run (D-10).
- **Stop button** alongside the panic key (D-13). Both required: the key is the reliable
  path because the macro owns the pointer, the button is the discoverable one.
- **The stop UI must not be a click target** for injected events (D-13). Pick a mechanism
  explicitly — window-targeted injection preferred; synthetic-event marking or a reserved
  corner as fallbacks.
- A macro **only runs while Bellman is open**; quitting aborts the run rather than orphaning
  it (D-13).
- Dry run and step-through.

## First card that uses the dev bypass

By now it has shipped and been exercised for three cards.

## Do NOT

- **Do not put the abort path behind the gate** (D-15). Stopping needs no password, no token
  and no unlocked store. `replay()` takes a `RunToken`; `abort()` takes nothing.
- No conditionals. Ever. A fixed count only (D-9).
- No caller-supplied repeat count — it is reviewed macro content.

## Exit gate

- Replay into a scratch window and assert the result.
- **The panic key works during injection** — proven, not assumed. Our synthetic events must
  neither swallow the real keypress nor trigger it themselves. The research notes `enigo`'s
  event-marking is documented only on Windows and macOS.
- Abort mid-run leaves **no stuck modifiers** — assert by reading modifier state after.
- Exceeding the runtime cap hard-aborts and logs `aborted-on-cap`, never `completed`.
- Fingerprint mismatch refuses. Two concurrent requests → exactly one runs.
- The stop **button** halts a run that is actively moving the pointer — tested with the
  macro driving the mouse, not with it idle.
- An injected click whose coordinates fall on the stop control does **not** stop the run and
  does **not** get swallowed — whichever mechanism is chosen, prove this case.
- Quitting Bellman mid-run aborts cleanly with no stuck modifiers.
- **A macro can be stopped while the store is LOCKED**, and by a caller that holds no token —
  asserted by test. Every abort is attributed in the audit log.
- A remapped stop key works; a stop key that cannot be registered downgrades execution to
  `Unavailable` rather than running unstoppably.
