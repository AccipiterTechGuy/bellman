# Coordinate Grid and Virtual Keyboard Overlay

Repo: `~/bellman`

**GATED ON RESEARCH.** A four-way feasibility study is running
(`Transparent desktop coordinate-grid + virtual-keyboard overlay — cross-OS feasibility`).
Do not start until its `synthesis.md` lands. The study may come back NO-GO for a platform,
or recommend image anchoring over a grid entirely — either outcome rewrites this card.

## Goal

Give Bellman and connected AI agents a visual targeting system for the desktop: a
transparent overlay that labels screen positions, so an agent can look at a screenshot and
name an exact place to click or a key to press.

## The loop this exists to serve

1. Bellman shows the overlay.
2. Bellman screenshots the desktop **with the overlay composited in**.
3. The agent reads the screenshot and returns a cell, a coordinate, or a key.
4. Bellman **hides the overlay**, then performs the real click or keystroke.

Step 4 is the one with a hidden trap — see "Do not fake the hide" below.

## Mode 1 — grid only

Transparent, always-on-top grid over the whole desktop. Readable X/Y coordinates or
labelled cells. Light neon colour, soft glow, faded — the applications underneath must
stay legible. This is an aid, not a curtain.

## Mode 2 — grid + virtual keyboard

The same grid plus a drawn keyboard. The grid turns **red** so the active mode is
unmistakable at a glance. Each key is labelled with its screen coordinates so an agent can
reference, select or inspect a specific key.

## Required behaviour

- Transparent full-screen overlay, always on top, never steals focus
- Optional click-through mode
- Light neon grid in normal mode; red grid when the keyboard is active
- Soft faded glow, not heavy solid lines
- Readable X/Y coordinates in both modes
- One or several monitors
- Quick show / hide / mode-switch
- Screenshot capture that includes the overlay
- Hide before executing the final click or key action

## Commands

```
bellman grid show
bellman grid show --keyboard
bellman grid hide
bellman grid screenshot [--out PATH]
bellman grid mode normal
bellman grid mode keyboard
```

`--json` on every one of these. This surface exists to be driven by an agent, not typed.

## Correctness rules — these are where the feature lives or dies

**One coordinate space, and it is documented.** A grid reporting numbers that the click
API does not accept is worse than no grid: it produces confident, wrong clicks. Logical vs
physical pixels, DPI scale, and **macOS's bottom-left origin with Y increasing upward**
(against top-left/Y-down on X11 and Windows) must all be resolved into a single space that
`bellman grid` reports and `bellman mouse move` consumes. The research synthesis names
that space — implement it, document it in `docs/`, and test the conversion per OS.

**Do not fake the hide.** Sleeping 100 ms after `hide()` and hoping the compositor caught
up is how the overlay ends up in the click, or in the next screenshot. Use the real
per-OS synchronisation the research identifies; if a timeout genuinely is the only honest
mechanism on some platform, say so in the code comment and make the value configurable
rather than a magic number.

**A screenshot must actually contain the overlay.** Some capture APIs silently omit
always-on-top layered windows. Assert it in a test: capture with the overlay up and prove
the grid pixels are present, rather than trusting the API.

**Report what it cannot do.** There are surfaces where the overlay cannot draw or the
capture returns black — lock screens, UAC/secure desktop, full-screen exclusive apps. When
Bellman is on one of those, say so; never return a screenshot that silently lacks the
overlay and let an agent target from it.

## Platform honesty

Per the research verdict, some platforms will be GO, some GO-WITH-LIMITS, and Wayland may
be NO-GO outside wlroots compositors. Implement what is GO. On anything else,
`bellman grid show` must fail with a clear reason — "overlay unsupported on GNOME/Wayland:
no layer-shell protocol" — and **never** an empty window or a silent success. An agent
that thinks the grid is up when it is not will click blind.

## Idle cost

Bellman's design point is a small resident footprint (`docs/PERF.md`). A full-screen
always-on-top transparent window is a real compositing burden. When hidden, the cost must
be genuinely zero — not an invisible window still being composited. Measure it and add it
to the perf gates.

## Acceptance

1. Transparent neon coordinate grid shows over the desktop.
2. Grid + virtual keyboard shows.
3. Normal mode is light neon; keyboard mode is red. Distinguishable in a screenshot test.
4. Coordinates readable in both modes.
5. Click-through works: a click at a gridded point reaches the app underneath, proven by
   the app reacting, not by inspection.
6. `bellman grid screenshot` saves an image that demonstrably contains the overlay.
7. Multi-monitor: correct coordinates on a second monitor, including one positioned left
   of or above the primary (negative coordinates), and under mixed DPI if the research
   says that is supported.
8. A coordinate read off the grid, fed to the mouse-move API, lands within 1 px of the
   labelled point — the round trip, not the drawing, is the real test.
9. On an unsupported platform, `grid show` fails loudly with the reason.
10. Hidden overlay costs no measurable idle CPU.

## Prerequisites

The research report produces a per-OS install list. Add it to
`docs/BUILD_PLAN.md` alongside the existing toolchain and GUI-test blocks, and flag
anything needing root — the crew cannot install those unattended.
