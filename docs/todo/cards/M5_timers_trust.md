# M5 — Timer attachment + trust levels + idle defaults

Design: `macro_recorder_security_plan.md` D-2, D-4, D-10; synthesis D1.

## Scope

- A `macro` action type in Bellman's existing action list.
- Per-macro `trust` level. **An unreviewed macro cannot be attached to a timer** (D-4).
- Keyring second-wrap for unattended runs + `armed_until` lease decay.
- **Refuse-and-log** when the store is locked. Never run late, never queue.
- **Forced `skip` misfire policy** — a macro that missed its slot does NOT fire later. This
  overrides Bellman's normal coalesce behaviour and is deliberate: a macro firing at an
  unexpected moment is the failure case.
- **Timer-triggered macros default to idle-only** (D-10) — the scenario to prevent is one
  firing at 3pm in the middle of an email.
- Per-macro `allow_slot_trigger` flag, **off by default** (D-8 Q2), plus the target-window
  check before any input is sent.
- Settings status line + fix-it buttons.

## The honest limit

Unattended means *after the operator has logged in*, never from a cold boot — on all three
platforms the credential store is gated on interactive login. Say this in the UI at the
toggle; do not imply otherwise.

## Exit gate

- Timer fires while unlocked → runs. Same timer with the app locked → refuses, logs,
  notifies, and **does not run late**.
- Keyring wrap survives a restart within a session; a simulated fresh-boot-no-login refuses.
- Migration (new `install_id`) drops every macro back to `attended`.
- An unreviewed macro cannot be attached — asserted by test.
- A slot cannot trigger a macro whose `allow_slot_trigger` is off.
