# M10 — Wayland replay (DEFERRED) — operator must validate in a VM

Design: `macro_recorder_security_plan.md` **D-16**, and D-12 / D-14 which bound it.

> **Status: DEFERRED. Do not start on a schedule.** This card is blocked on upstream work
> that is not ours, and it cannot be validated on the operator's own machine — their Mint
> desktop runs X11, so Wayland behaviour is invisible to them without a VM.
>
> **This card has a human gate.** A crew can prepare and instrument it. Only the operator can
> sign it off, by hand, in a virtual machine. Do not mark it shipped on a crew's word.

## What it delivers

Macro **replay** on Wayland. Authoring already works there today (compose, D-12, needs no
permission at all) — this card is only about actually clicking and typing.

## Preconditions — check these FIRST, and stop if either fails

Both must be true before writing a line of code. Re-verify at the time; they were false on
2026-07-29.

1. **Non-US keyboard layout support in the injection crate.** `enigo`'s Wayland/libei
   backend was experimental and **US-only**. On a Finnish layout it can type **silently wrong
   characters** — which is worse than refusing, and invisible to any test written in English.
   This is the whole reason the card is deferred.
2. **A registrable global stop key on the target compositor.** The research found **no
   panic-key path on wlroots/Wayland portals**. Per D-14, no verified stop key means
   execution stays `Unavailable` **regardless of whether injection works**.

Solving injection alone and shipping is the trap this card exists to prevent. Both, or
neither.

Also evaluate, since nobody has: the **XDG RemoteDesktop portal** as the sanctioned,
consent-prompted injection route — this is what remote-desktop applications use, and it may
be cleaner than `/dev/uinput`.

## Permission note

`/dev/uinput` grants **write** access — inject, cannot read. It is not the keylogger-grade
`input`-group grant, which D-12's compose path removed the need for entirely. If a permission
step is required, document exactly what it does and does not allow; do not let it be
mistaken for the read grant.

## What the operator must do — the VM checklist

The operator has KVM/libvirt available. Test in **disposable** VMs:

| VM | What it proves |
|---|---|
| Ubuntu (GNOME/Wayland) | the majority Wayland desktop |
| Fedora Workstation (GNOME/Wayland) | a different distro, same compositor |
| KDE Plasma on Wayland | a different compositor with a different portal implementation |
| A wlroots compositor (sway) | the no-stop-key case — must refuse, not run |

For each, by hand:

1. Set the VM's keyboard layout to **Finnish**, not US. This is the single most important
   step in the card.
2. Author a macro that types a string containing **`ä ö å`** and at least one symbol that
   moves between layouts (`@`, `-`, `/`).
3. Run it into a text editor inside the VM.
4. **Read what was actually typed, character by character.** Not "did it run" — *what did it
   produce.*
5. Press the stop key mid-run and confirm the run aborts and no modifier is left stuck.
6. Confirm a refusal is a clear `Unavailable` message with a reason, wherever it refuses.

## Exit gate

- Typed output on a **Finnish layout** matches the macro **exactly**, in every VM where
  replay is claimed to work. One wrong character = this card fails. There is no partial pass.
- Where the stop key cannot be registered, replay reports `Unavailable` and **does not run** —
  verified on a wlroots compositor.
- The permission step, if any, is documented with what it grants and what it does not.
- The operator has personally run the checklist above and said so. A crew may prepare and
  automate everything else; **this line is signed by a human.**

## If the preconditions still fail

Close the card as "still deferred", write down the date and what was checked, and re-mint it
later. That is a successful outcome, not a failure — shipping a macro engine that types `[`
where the operator typed `ä` would be considerably worse.
