# Bellman card index — all queued work, in order

**2026-07-30: the integration kit is ON THE RAILS.** IK1 already SHIPPED
(`bellman-ik1-normalise-the-json-shapes` — R1–R6 landed in `4c2d7d7`/`03e0363`). IK2 departed
with crew `2026-07-30_0003`; IK3 → SCH1 → IK4 → IK5 → IK6 queued behind it in that order,
loadout `SU=claude:sonnet:medium · CO=kimi:K3:high · AU=codex:gpt-5.6-sol:high`. The design
was frozen after 8 adversarial audit rounds. The OVL1 and M1–M10 cards below remain parked
in **DESIGNING** — created only, not sealed, not railed.

Design documents:
- `macro_recorder_security_plan.md` — decisions **D-1 … D-16** for the macro feature
- `json_normalization.md` — the JSON/runtime rules **R1 … R12**, the folder tree, retention
- `macro_card_plan.md` — the macro ladder and why it is ordered that way

## Grid

| card | scope | depends on |
|---|---|---|
| **OVL1** | Coordinate grid, **drawn into the screenshot** — not a live desktop overlay. Live overlay is a later card; the virtual keyboard was dropped 3-1 | research (done) |

## Integration kit — build in this order

| card | scope | depends on |
|---|---|---|
| **IK1** | Normalise wire shapes and the shared vocabulary (**R1–R6 only**). No behaviour change | — |
| **IK2** | Per-timer folder tree: `timer.json`, `status.json`. **No `runs/`**; retained archive policy | IK1 |
| **IK3** | Per-run reply channel and runtime rules **R7–R12** — watchdog, revision, crash/startup, outbox/rotation, caps | IK2 |
| **IK4** | Lightbulb app + "connect your own application" docs | IK1–IK3, SCH1 |
| **IK5** | Live run state in the GUI — no new tab | IK3 |
| **IK6** | Dual transport: local IPC + files, chosen **per firing**. One ingest path; **no generated `adapter.py`** — connection info is data, not code. Files stay canonical | IK3 + SCH1 shipped, IK4 proven |

## Demos — the apps a visitor sees

| card | scope | depends on |
|---|---|---|
| **DEMO1** | `testing_apps/lightbulb_gui/` — the lightbulb with a window: set a time, watch the bulb light, watch the four-state handshake. stdlib **tkinter**, neon palette with a **golden** bulb, no Bellman imports. The terminal `lightbulb/` stays as-is | IK4 shipped |
| **WIZ1** | First-run wizard offers the demo — one tick, an explanation, a **Run the demo** button. Bellman **never creates the demo's timer** (the app claims its own owner); ships the demo files with the package so the path exists on an installed machine | DEMO1 + **SCH2** shipped |

Both demos live under `testing_apps/`. They share no code on purpose: the
terminal one is what a developer copies, the GUI one is what everyone else
watches. See `testing_apps/README.md`.

## Final gate

| card | scope | depends on |
|---|---|---|
| **C11** | 🏁 **Final gate** — full-system validation + originality sweep. Rewritten 2026-08-01: covers the integration surface (reply channel, watchdog, quarantine, both transports, both demo apps), install verbatim on Ubuntu/Fedora/Arch, and the wizard on a clean session. Two rules: **play the human, not the developer**, and **a clock must do the firing** (no `run-now` evidence) | everything else shipped |

## Ship readiness

| card | scope | depends on |
|---|---|---|
| **SHIP1** | 🔴 Fixes from the R9 ship-readiness swarm, all hand-verified: fire-notification docs describe a **deleted** schema (`bellman-fire/1`) and the wrong `occurrence_kind` vocabulary; the wizard **falsely** claims XDG autostart grants `CAP_WAKE_ALARM`; §Install lacks `apt update` and uses `curl` before installing it; dual data dirs half-documented; CLI cannot set a launch action; CI never runs tests on macOS/Windows; missing community files + 11 tracked `/home/sami` paths. **`patchelf`/`libgtk-3-dev` are refuted — do not add them** | — |

## Follow-ups raised by C11

| card | scope | depends on |
|---|---|---|
| **DOC1** | rustdoc coverage on the public API — `bellman-core` has **593** public items with no doc comment (module-level docs are already complete). One sentence each saying *why*, narrow anything that does not deserve one, then `#![warn(missing_docs)]` per crate. Comments and visibility only, no behaviour. Larger than C11 could absorb, so written up rather than half-done | C11 |

## Scheduler internals

| card | scope | depends on |
|---|---|---|
| **SCH1** | In-memory fire dispatcher + bounded action lanes. A slow launch currently stalls the whole heap | IK3 |
| **SCH2** | 🔴 **Slot-created timers never fire in a running Bellman** — `refill()` has no caller in `slots/`, so the horizon heap never learns of them. Fix both paths (watcher-processed *and* external `slot-submit`), and prove it with the lightbulb firing on its **own** schedule, no `run-now` | — |

### Deferred — worth doing, not now

**`bellman-client` convenience packages** (Python first) wrapping IK6's socket protocol.
Optional sugar only — the raw protocol must stay documented well enough that any language
speaks it without a library, exactly like the file protocol.

### 🔒 Standing decision — the output protocol is frozen by design

**The JSON output protocol and the small Python connector are not open for
redesign.** Owner decision, and it outranks any argument for elegance.

Protected: one writer per file (Bellman owns `timer.json` / `status.json`, the
app owns its per-run `reply-<run_id>.json`); the wire shapes `bellman-slot/1`,
`bellman-reply/1`, `bellman-run/1`, `bellman-event/1`; and a client that stays
one read, one atomic write, one file — about ten lines in any language.

Rejected in advance: a shared `output.json` with a lock file, a generated
`adapter.py` or any generated importable file, a **required** SDK / client
library / schema layer / code generator / build step, and extra handshakes,
negotiation or registration in the reply path.

The product claim is that a shell script is a valid client. Anything eroding
that is a downgrade wearing an improvement's clothes.

This freezes the **design**, not its defects: implementation bugs, inaccurate
docs, missing error handling, and genuine expressiveness gaps are all still
wanted. An **optional** convenience wrapper is fine — it is on the deferred
list — provided the raw protocol stays fully documented and usable without it.

### Decided against — do not re-propose

**One shared `output.json` per timer, guarded by a `.output.lock`.** Reviewed 2026-07-30 and
rejected. It reintroduces the lost-update race the split exists to prevent, then pays for it
with an advisory-lock protocol every integrating app must implement correctly — killing the
"a shell script can integrate" story, since `flock(1)` is not on macOS by default. It also
moves the stale-`run_id` check from Bellman (where it cannot be skipped) to the app (where it
can). The proposal reaches the same conclusion itself: Bellman's inferred states cannot live
in a file the app owns, so the mirror property needs "a separate Bellman-owned `status.json`".

Four sections of that review **were** adopted: R10 (fire transaction + startup ordering), R11
(single log writer), the monotonic watchdog clock in R8, and the transition/debounce rules in
IK3.

## Macro feature

| card | scope | password to test? |
|---|---|---|
| **M1** | Model + encrypted store + gate skeleton. **Atomic — do not split** | no |
| **M2** | **Compose** authoring — screenshot, pick, type | no |
| **M3** | GUI: table, step editor, mandatory review, stop-key verification | no |
| **M4** | Replay engine + safety rails | no |
| **M5** | Timer attachment + trust + idle defaults | no |
| **M6** | **Capture** authoring (opt-in) + secret awareness | no |
| **M7** | **Execution password** — the gate becomes real here | yes |
| **M8** | **Execution tokens** + review gate + agent skill | yes |
| **M9** | Per-OS QA on real hardware + ship guard | yes |
| **M10** | Wayland replay — **DEFERRED**, operator validates in a VM | yes |

## Cross-card dependencies

- **M2 depends on OVL1's screenshot core** — compose picks coordinates off a screenshot.
- **M6 is blocked** on two facts nobody has re-verified: is `rdev` maintained, and do
  browsers expose `IsPassword` to UIA.
- **M10 is blocked** on upstream keyboard-layout support, and needs a human VM sign-off.
- IK1 is the smallest and unblocks the most. It is the natural first departure.
