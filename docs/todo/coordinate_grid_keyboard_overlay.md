# Coordinate Grid — screenshot-composited first, live overlay second

Repo: `~/bellman`

**Research complete.** Four-way study + synthesis:
`Research_from_Crew/transparent-desktop-coordinate-grid-virt_research_2026-07-29_194756/synthesis/synthesis.md`.
This card has been **re-scoped by that research**. Read the synthesis before starting; the
sections below record what it decided so a crew does not re-derive or re-argue it.

## The verdict that changed the plan

> **The grid does not need to be on the screen.**

Capture each monitor clean, draw the grid, labels and coordinates **into the captured image
buffer** in Rust, and hand the agent that image. The agent sees an identical labelled grid.
The desktop is never touched.

Two of four researchers reached this independently. It deletes, in one move: click-through,
focus theft, z-order, always-on-top, compositor detection, capture-inclusion, the
hide-then-click race, `macOSPrivateApi`, four open Tauri bugs — **and the Wayland blocker.**
It is the only version that works on GNOME/Wayland, where a live overlay is impossible
permanently, not temporarily.

Cost: days, versus weeks for the live overlay.

## Build order — do not invert this

**Tier A — `bellman grid screenshot` (this card).**
Capture per monitor → composite grid + labels in Rust → return the image **plus a coordinate
manifest**. No window, no overlay, no permissions beyond screen capture.

**Tier B — live overlay (a LATER card, only if a human needs to see it on the real desktop).**
Native, **WebView-free**, one surface per monitor, in the same Rust process. X11 / Windows /
macOS only. Do not start it as part of this card.

**Never a transparent WebView.** All four researchers agree.

## Per-OS reality

| Environment | Tier A (this card) | Tier B (later) |
|---|---|---|
| Linux / X11 | GO | GO |
| Windows 10 1803+ / 11 | GO | GO |
| macOS 13+ | GO — and needs **no private API** | GO-WITH-LIMITS: two TCC grants |
| Linux / Wayland — GNOME | **GO** (portal) | **NO-GO. Permanently.** |
| Linux / Wayland — KDE, wlroots | GO (portal) | NO-GO for v1; runtime-probe, never assume |

Never assume the environment. Probe at runtime and report honestly.

## The macOS coordinate correction — read this twice

The original brief for this feature asserted that macOS uses a bottom-left origin with Y
increasing upward. **That is wrong**, and it is wrong in a way that silently mirrors every
click vertically on one platform.

Bottom-left/Y-up is an **AppKit** fact. **CoreGraphics global display space — which is what
screen capture and click injection actually use — is top-left, Y-down, in points**, the same
as X11 and Windows.

Two researchers caught it against Apple's own documentation; two inherited the error from
the brief. Implement against CoreGraphics, and add a test that would fail if someone
"fixes" it back.

## The canonical coordinate space

One space, defined once, used by `grid`, by capture, and by any future click injection. The
synthesis §4 specifies it — implement that, document it in `docs/`, and unit-test the
conversion per OS. A grid that reports numbers the click API will not accept is worse than
no grid: it produces confident, wrong clicks.

## Dropped from the original spec

**The virtual keyboard — dropped, 3-1.** Keystrokes take keycodes, not coordinates. Drawing
a keyboard, screenshotting it, and having a model read a keycap back out of pixels is a
lossy round trip to information we already have exactly. Ship instead:

```
bellman type "hello"
bellman key ctrl+shift+p
```

Exact, instant, no overlay, no race — and they work on Wayland. The agent returns a key
*name*, validated against an enum.

**Note:** `bellman type` is gated. See `docs/todo/macro_recorder_security_plan.md` D-6 — it
is not exposed to agents in v1.

**Kept:** the red mode indicator. An unmistakable "you are in a dangerous mode" signal is
good design regardless of which modes survive.

## Where the grid actually sits

All four researchers ranked it **the weakest of the three targeting strategies**:

1. Accessibility tree — real element identities, not pixels
2. Anchored marks / image anchoring — survives things moving
3. The grid — the **universal floor** and the **calibration instrument**

Build it as the floor that always works and the tool that proves the other two are aimed
correctly. Do not build the product around it.

## Must ship with it

**The sentinel self-test** (synthesis §7). The feature must be able to prove its own
coordinates are right, rather than assert it. A round trip — read a labelled point off the
grid, feed it back, land within tolerance — is the real test. Drawing pretty lines is not.

## Acceptance

1. `bellman grid screenshot` returns an image with a readable coordinate grid composited in,
   plus a machine-readable coordinate manifest, on X11.
2. The same works on **GNOME/Wayland** through the portal.
3. Works with `DISPLAY` unset where the platform allows it; never requires a visible window.
4. Multi-monitor: correct coordinates on a second monitor, including one positioned left of
   or above the primary (negative coordinates), and under mixed DPI.
5. macOS conversion is tested against CoreGraphics top-left/Y-down, with a test that fails if
   an AppKit-style flip is reintroduced.
6. A coordinate read off the grid round-trips to within tolerance — the sentinel.
7. On an environment where capture is unavailable, it fails loudly with the reason, never a
   blank image.
8. No live overlay, no transparent window, and no `macOSPrivateApi` anywhere in this card.

## Prerequisites

The synthesis §8 carries a per-OS install list. Add it to `docs/BUILD_PLAN.md` beside the
existing toolchain blocks, and flag anything needing root — a crew cannot install those
unattended.
