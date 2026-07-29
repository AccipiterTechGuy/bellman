# M2 — Compose authoring: screenshot → pick → type

Design: `macro_recorder_security_plan.md` **D-12**. This is the default way to author a
macro, and the only one available on every platform.

## Depends on

The grid card's **screenshot core** (`bellman grid screenshot`). Build that first.

## Scope

- Take a screenshot (portal on Wayland, native elsewhere) and show it inside Bellman.
- The operator clicks **on the screenshot, inside Bellman's own window**, to pick a target
  point. Coordinates come from the grid card's canonical space.
- The operator types text into a Bellman field.
- Steps assemble into a macro in **Steps mode** (the default — see the research).
- Screen fingerprint recorded with the macro, so replay can detect that the layout moved.
- `MacroCapability` probe: report honestly per platform what authoring and replay can do.

## Why this exists

No permission is required to click inside your own window and type in your own text box, on
any operating system. So this path needs **no global input capture** — which means Bellman
is not keylogger-shaped, cannot record a password by accident, and works on Wayland.

## Do NOT

- Do not add global input capture here. That is M6, opt-in, and later.
- Do not invent a second coordinate space. Use the grid card's.

## Exit gate

- Compose a 20-step macro end to end on X11 **and** on GNOME/Wayland.
- Assert the macro contains no data that did not come from an explicit operator action.
- A coordinate picked off the screenshot round-trips to the same point the grid reports.
- The capability probe returns the honest answer on X11 and on Wayland, both observed live.
