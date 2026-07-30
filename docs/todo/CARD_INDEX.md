# Bellman card index — all queued work, in order

Every card below is minted on the board and parked in **DESIGNING**. None are sealed or
railed: they are created only, until the cards already on the rails have shipped.

Design documents:
- `macro_recorder_security_plan.md` — decisions **D-1 … D-16** for the macro feature
- `json_normalization.md` — the JSON rules **R1 … R9**, the folder tree, retention
- `macro_card_plan.md` — the macro ladder and why it is ordered that way

## Grid

| card | scope | depends on |
|---|---|---|
| **OVL1** | Coordinate grid, **drawn into the screenshot** — not a live desktop overlay. Live overlay is a later card; the virtual keyboard was dropped 3-1 | research (done) |

## Integration kit — build in this order

| card | scope | depends on |
|---|---|---|
| **IK1** | Normalise the three JSON shapes. No new features | — |
| **IK2** | Per-timer folder tree: `timer.json`, `status.json`, `runs/` | IK1 |
| **IK3** | `reply.json` — outcome reporting, opt-in watchdog, revisable state | IK2 |
| **IK4** | Lightbulb app + "connect your own application" docs | IK1–IK3 |

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
