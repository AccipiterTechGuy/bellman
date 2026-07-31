# WIZ1 — Offer the demo in the first-run wizard

Repo: `~/bellman`. Depends on **DEMO1 shipped** — the wizard cannot point at
an app that does not exist yet. Touches `ui/src/Wizard.svelte`,
`src-tauri/src/first_run.rs` / `config.rs`, and the Linux packaging manifest.

Design: `docs/INTEGRATION.md` → *Connect your own application*;
`cards/DEMO1_lightbulb_gui.md`.

## Why this card exists

Bellman's whole purpose is waking *other applications*, and a new user has
no way to see that happen. They install it, they schedule a launch command,
and the integration surface — the thing that makes Bellman more than cron —
is invisible. `testing_apps/lightbulb_gui/` exists to show it, but only
someone who browses the repository will ever find it.

The first-run wizard is the one moment we know a person is paying attention
and asking "what does this do?".

## Scope

**One tick on the wizard's first step**, beside the existing autostart and
tray options:

```
☐ Show me the demo — watch a timer wake a real application
```

With one hint line under it, in the same voice as the existing hints:

> A tiny example app (a lightbulb) that Bellman wakes on a schedule. It
> talks to Bellman exactly the way your own applications can — over plain
> JSON files, no plugin and no shared code. Optional, and it changes
> nothing about your setup.

**When ticked**, the wizard's final "Setup complete" step gains a panel:

- what the demo is, in two sentences
- the exact command to run it, in a copyable field
- a **Run the demo** button when `python3` is present (see *Launching*)
- a link to `docs/INTEGRATION.md` for "connect your own application"

**When not ticked**, nothing changes anywhere. The panel does not appear,
no files are touched, no timer exists.

The choice is remembered in config so Settings can offer the same panel
later — someone who declined at first run must be able to find it again
without reinstalling.

## What this card must NOT do

**Bellman must never create the demo's timer.** This looks like the obvious
implementation and it is wrong twice over:

1. **It requires a capability that deliberately does not exist.** An
   integration owner is established by the *app* claiming the timer through
   the slot protocol with its own `app_name`. A timer created from the GUI
   has no owner and gets no reply channel — that is documented behaviour
   (`docs/INTEGRATION.md`, *Step 0*), and it is what keeps one-writer-per-file
   safe. Adding "assign an owner from the GUI" so the wizard can pre-make the
   demo timer would let a human attach any app name to any timer, producing
   fire notifications nobody answers and `no_ack` noise. If that feature is
   ever wanted it is its own card with its own argument — not a side effect
   of a demo.
2. **It breaks what the demo proves.** The lightbulb is persuasive precisely
   because it is a foreign application doing what any third party could do.
   The moment Bellman sets it up from the inside, the demonstration weakens
   to "Bellman can talk to itself".

So: the demo app creates its own timer, exactly as DEMO1 specifies. The
wizard's entire job is **explaining and launching**, never provisioning.

**No demo tab, and no demo code in the product.** Reviewed and rejected: a
tab is useful once per user and then permanent furniture, the same reason
IK5 refused one for live run state. The wizard panel and a Settings entry
are the whole surface.

## Launching — the packaging problem

`testing_apps/` lives in the repository. A user who installed the `.deb` has
no repository, so a wizard that prints `python3 testing_apps/lightbulb_gui/…`
is telling them to run a file they do not have. **Fix the packaging, not the
message:** ship both demos with the Linux package (for example under
`/usr/share/bellman/testing_apps/`) and have the wizard resolve the path at
runtime — the installed location first, then the source tree when running
from a dev checkout.

- Two small Python files and two READMEs; a negligible package size cost.
- The **Run the demo** button appears only when a demo directory was
  actually resolved *and* `python3` is on `PATH`. Otherwise show the copyable
  command and a plain note about the `python3` / `python3-tk` requirement.
  Never show a button that cannot work.
- Launching spawns the demo as an ordinary detached child process with the
  correct `--slots` path for this install. Bellman does not supervise it,
  does not restart it, and closing Bellman does not kill it — it is a
  separate application and must behave like one.
- If the platform packaging cannot carry the files (Windows/macOS bundles are
  unvalidated), degrade honestly: show the explanation and the documentation
  link without the command. Do not fabricate a path.

## Exit gate

- A fresh profile runs the wizard; the tick is present on the first step with
  its hint, and defaults to **unticked**.
- Ticking it and finishing shows the demo panel on the completion step;
  leaving it unticked shows the completion step exactly as it is today —
  asserted, so the default path is provably unchanged.
- **No timer is created by the wizard under either choice.** Asserted against
  the store: the timer count after the wizard equals the count before.
- Pressing **Run the demo** launches the demo app against this install's
  slots root; the user sets a time in it, the bulb lights, and the run
  reaches `completed` — the full loop from a fresh install, with no terminal.
- Closing Bellman leaves the demo running; the demo is unaffected.
- With `python3` absent from `PATH`, the button is not shown and the copyable
  command plus the requirement note are shown instead.
- The installed `.deb` contains the demo files, and the path the wizard shows
  resolves to a file that exists on that machine — verified on a real deb
  install, not in the source tree.
- The preference is persisted and the same panel is reachable from Settings
  afterwards.
- No new tab, and no demo code in `crates/` or `ui/` beyond the wizard panel
  and its Settings entry.
