# M6 — Capture authoring (opt-in) + secret awareness

Design: `macro_recorder_security_plan.md` **D-12**. This card is the *optional* second way
to author a macro. M2's compose path is the default and needs none of this.

## Read this before starting

Compose (M2) already delivers macro authoring on every platform with **no permissions**.
This card buys **speed** — recording a real workflow by doing it — at the cost of a
global-input-capture grant. That trade is the operator's to keep making; do not let this
card quietly become the default path.

## Blocked until two facts are checked

The research reports **contradict each other**. Resolve both before writing code:

1. **Is `rdev` maintained?** One report cites 2026 releases; three cite 2023 and a stalled
   maintenance issue. The whole write-our-own-capture recommendation turns on this.
2. **Do browsers expose `IsPassword` to UIA?** One says they deliberately do not; another
   says Chromium and Firefox do. This decides whether auto-pause can be *promised* or only
   *warned about*.

## Scope

- `trait InputCapture`, per-OS: Windows low-level hooks, macOS `CGEventTap` (with the TCC
  prompt and tap-disabled recovery), Linux/X11 XRecord.
- Coalescing raw events into `Step`s. Both recording modes; Steps stays the default.
- Wayland returns `ReplayOnly` / `Unavailable` with the honest sentence — never a silent
  half-capture.
- `Secret` step type backed by the keyring.
- Password-field awareness: Windows UIA `IsPassword`, macOS `AXSecureTextField` +
  `IsSecureEventInputEnabled()` with a `[secure input]` timeline marker.
- Manual pause hotkey; amber heuristics in the review screen.

## Exit gate

- Record a 20-step sequence on X11: assert step types, ordering, coalesced text, timing.
- Typing into a password field on Windows and macOS auto-pauses and records nothing.
- A `Secret` value never appears in `macros.enc`, the JSONL log, or `tracing` output —
  proven by grep over all three.
- The capability probe returns the honest answer on Wayland and X11, both observed live.
