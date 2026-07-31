> ARCHIVED 2026-07-31 — shipped; card bellman-demo1-lightbulb-gui-demo-app-neon-golden-bulb, run 2026-07-31_0006, merge 6a46408.

# DEMO1 — The lightbulb, with a window

Repo: `~/bellman`. Depends on **IK4 shipped** (the protocol and
`docs/INTEGRATION.md` exist and are correct). Touches **no** Bellman code:
this card adds `testing_apps/lightbulb_gui/` (one Python file plus a README)
and edits three existing READMEs. If it needs to change anything in
`crates/`, `src-tauri/` or `ui/`, stop — that is a different card and a sign
something in the protocol is wrong.

Both demos live under `testing_apps/`, whose own README states the rule they
are both held to: they integrate exactly as a stranger's application would.

Design: `docs/INTEGRATION.md` → *Connect your own application* is the
specification. This card implements a client of it, nothing more.

## Why this card exists

`testing_apps/lightbulb/lightbulb.py` proves the loop, but only to someone
willing to open a terminal, create a timer with a CLI command, and read
ANSI art. That is the right artefact for a **developer** — it is the thing
they copy into their own app.

It is the wrong artefact for everyone else. A person evaluating Bellman
wants to see a scheduler wake an application, and wants to see it without
learning a CLI first. Today there is nothing to show them.

So: **two demos, two audiences, both kept.**

| | audience | what it is for |
|---|---|---|
| `testing_apps/lightbulb/lightbulb.py` (exists) | developers | the thing you copy — 130 lines, the six-line `reply()` is the lesson |
| `testing_apps/lightbulb_gui/` (this card) | everyone else | the thing you watch — set a time, see the bulb light, see the handshake |

**The terminal version is not replaced, not deprecated, and not refactored
into a shared library.** Two files that each stand alone beats one clever
abstraction; the copy-paste story is the reason the terminal one exists.

## The hard constraint — this is a FOREIGN app

The demo's whole evidential value is that it is a stranger's application.
Therefore:

- **No imports from Bellman.** Not `bellman-core`, not a generated client,
  not a shared schema module. It speaks JSON over the documented file
  protocol exactly as a third party would.
- **No reading the database.** `timers.db` does not exist as far as this
  app is concerned.
- **No private knowledge.** Every path it opens either comes from a fire
  notification or is the documented slots root. It never constructs a
  reply filename.
- **Python standard library only.** No pip install, no venv, no build step.
  `python3 lightbulb_gui.py` and a window appears.

If any of these has to bend, the finding is about `docs/INTEGRATION.md`,
not about this app. Report it rather than working around it.

## Why tkinter and not Tauri

tkinter ships with CPython on all three platforms, needs no toolchain, and
keeps the demo one readable file. A Tauri demo would add a second Rust
build to the repo and bury a six-line protocol under thousands of lines of
scaffolding — a demo whose build is harder than the thing it demonstrates
has failed at being a demo. If tkinter is genuinely missing on a target
(some minimal Linux distributions package it separately as `python3-tk`),
say so in the README; do not add a dependency to work around it.

## Scope — both directions in one window

The terminal lightbulb only *answers* fires. This one also *creates* the
timer, so a visitor sees the entire integration without touching a shell.

**1. Set a time.** A minimal picker: **in N seconds** (default 10 — a demo
nobody waits for is not a demo), **at HH:MM**, or **every N minutes**. The
app publishes a `bellman-slot/1` add request with `app_name:
"lightbulb-gui"` and the chosen occurrence.

Publish it **the documented way, from Python** — claim a free stub by
exclusive rename, then write the complete request under the reserved
`slot-NNNN.json` name via temp + same-directory rename (`docs/INTEGRATION.md`
→ *Protocol* → *Rules*). Do **not** shell out to `bellman slot-submit`:
requiring Bellman's own CLI to demonstrate that no Bellman code is required
defeats the point, and a visitor may not have it on `PATH`. Read the
outcome from `slots/done/slot-NNNN.json` and show the resulting
`timer_id` and `next_fire_at` on screen.

Use `app_name: "lightbulb-gui"` — distinct from the terminal demo's
`lightbulb`, so the two can run side by side and neither answers the
other's timers.

**2. Wait, visibly.** Show the timer's name, its next fire time, and a
live countdown. This is the part that makes a scheduler comprehensible:
the thing is going to happen, and you can watch it approach.

**3. Answer the fire.** Watch `slots/fires/`, accept only notifications
carrying `app_name: "lightbulb-gui"`, dedupe by `run_id`, and reply
through the notification's `reply_path` exactly as the terminal version
does — `acknowledged` with `expected_secs`, then `completed` with the
measured duration.

**4. Show the handshake, not just the bulb.** A row of state chips lights
up in sequence as the protocol advances:

```
   ○ fired        ○ acknowledged     ○ running      ○ completed
```

Each chip illuminates when that state is actually written, with its
timestamp beneath. A viewer sees a four-step conversation between two
programs, which is the thing being demonstrated. The bulb alone would only
prove that Python can draw a circle.

**5. A "make it fail" button.** While a run is in flight, one button
replies `failed` with a `reason` instead of completing. The `failed` chip
lights red and the reason is displayed. The error path is half the
protocol and is otherwise invisible.

**6. Clean up after itself.** A **Remove timer** button publishes a
`delete` request for the timer it created, and the app offers this on
close if its timer still exists. A demo that silently accumulates timers
in a stranger's install is a bad guest.

## The look — neon, with a golden bulb

Deliberate and specific, because "make it look nice" produces grey Tk
defaults every time.

**Palette:**

| role | colour | where |
|---|---|---|
| background | `#0a0a12` near-black indigo | the window |
| panel | `#12121f` | grouped controls, the log strip |
| neon cyan | `#00e5ff` | headings, borders, the countdown |
| neon magenta | `#ff2fb9` | the active/running accent, focus rings |
| neon violet | `#8b5cf6` | secondary chrome, the timer's next-fire line |
| **bulb OFF** | `#3a3326` dim bronze | the filament at rest |
| **bulb ON** | `#ffc94a` warm gold | the lit bulb — **golden light, not neon** |
| glow | `#ffc94a` → background, stepped | the halo around a lit bulb |
| success | `#39ff88` neon green | the `completed` chip |
| failure | `#ff3b5c` neon red | the `failed` chip |

**The bulb is the one warm thing on the screen.** Everything else is cold
neon on near-black; the light itself is golden. That contrast is the whole
visual idea — the room is electric blue, the lamp is warm — and it should
read instantly in a screenshot.

**Drawing the glow.** tkinter's Canvas has **no alpha channel** — do not
attempt transparency, it will silently do nothing or throw. Build the halo
from concentric ovals painted back-to-front, each a solid colour stepped
between `#ffc94a` and the background (roughly 8–12 rings, computed by
linear interpolation, largest and darkest first). Animate the lit bulb with
a subtle brightness pulse — a few percent, on a slow cycle. It should look
like a filament, not a disco.

**Typography:** one monospace family throughout (`TkFixedFont` is the
portable choice), sizes for hierarchy, no bold-italic decoration. Thin
1-pixel neon rules to separate regions. Generous dark space — the point is
that the golden bulb dominates.

## The trap that will break this app

**tkinter is single-threaded and its event loop must never block.** A
`while True: … time.sleep()` watch loop — which is exactly what the
terminal lightbulb does and what a coder will copy — freezes the window
solid: no repaint, no button, an OS "application not responding" badge.

Poll with `root.after(250, …)` instead, rescanning `fires/` on each tick
and returning immediately. The 15-second bulb-on period is likewise a
sequence of `after()` callbacks driving an animation frame and a countdown,
never a sleep. Do the file reads inline (they are microseconds); if any
work ever genuinely blocks, it belongs on a worker thread that hands
results back through a `queue.Queue` drained by an `after()` tick — never
by touching widgets from the thread.

Assert this: the exit gate requires the window to stay responsive
*throughout* a run, not merely to display the right thing at the end.

## Documentation

- `testing_apps/lightbulb_gui/README.md` — what it shows, how to run it
  (`python3 lightbulb_gui.py --slots <dir>`), how to find the slots root
  for both the CLI default and the desktop app, the `python3-tk` note, and
  a screenshot of the lit bulb.
- `testing_apps/lightbulb/README.md` — one line pointing at the GUI version
  for people who want to watch rather than copy.
- The repo `README.md` "Connect your own application" section — name both
  demos and say plainly which is which: copy the terminal one, watch the
  GUI one.
- `docs/INTEGRATION.md` — a sentence in the same place. No protocol text
  changes; if the protocol needs a change, that is a finding, not an edit.

## Exit gate

- A human with no prior knowledge runs `python3 lightbulb_gui.py`, sets
  "in 10 seconds", presses the button, and **sees the bulb light** — with
  no terminal, no CLI, and no Bellman source knowledge.
- The timer is created through the **documented slot claim from Python**;
  `bellman slot-submit` is not invoked, and the app runs with the `bellman`
  binary absent from `PATH`.
- Every one of the four chips — `fired`, `acknowledged`, `running`,
  `completed` — illuminates at the moment that state is written, each with
  its real timestamp.
- **The window stays responsive for the entire run**: buttons work and the
  countdown repaints while the bulb is on. Asserted deliberately, because
  the copied `while True` loop is the predicted failure.
- "Make it fail" produces a real `failed` reply with a `reason`; the red
  chip lights and `status.json` shows `failure_kind: "reported"`.
- Running **both** demos at once, each with its own timer: neither answers
  the other's fires, and both runs reach `completed` independently.
- The same `run_id` delivered twice is acted on **once** — assert by
  re-dropping a fire notification.
- **No Bellman imports anywhere**, asserted structurally by grepping the
  file: no `bellman`, no database access, no constructed reply filename.
  Every path opened comes from a notification or the slots root.
- Standard library only: it runs on a clean Python 3 with no site-packages.
- "Remove timer" deletes the timer it created; after a full run and
  removal, the install is in the state it started in.
- The terminal `lightbulb.py` is **byte-identical** to before this card,
  and still passes its own IK4 exit gate.
- A screenshot of the lit golden bulb on the neon field is committed with
  the README — the visual spec is a deliverable, not a suggestion, and is
  reviewed rather than assumed.
