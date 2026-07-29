# Macro feature — card ladder and build order

Design decisions: `macro_recorder_security_plan.md` (operator, D-1…D-7).
Research: `Research_from_Crew/macro-recorder-gui-the-security-model-th_research_2026-07-29_195350/synthesis/synthesis.md`.

## The ordering rule

> Build and test the machine first. Add the master password once everything works.
> Add capability tokens last. Otherwise nothing can be tested.

That is the operator's instruction and it is what this ladder does. It also happens to match
the research: its own card list says **cards 1–3 need no password anywhere**. "Security
first" there means the *shape* — encrypted store, one chokepoint — not a password prompt.

### The one thing that does NOT move late

**Encryption ships in card 1**, keyed from the OS keyring, with **no password involved**.

This costs testing nothing — nothing prompts, the keyring hands the key over silently — and
it avoids the failure the research names explicitly: a card-1 plaintext store means a later
migration, a window where macros sit unencrypted, and a padlock icon over a plain file. The
chokepoint has the same property: it is a *type* everything is built against, and
retrofitting a type after four cards of code exist is exactly the failure mode.

So: **encryption and the chokepoint are structural and come first. The password and the
tokens are policy and come last.** They are different things, and only the second kind
blocks testing.

## The ladder

Re-cut after D-12 (compose/capture). All nine are minted on the board, **created only** —
none are sealed or railed until the cards already on the board have shipped.

| # | Card | Password to test? | Notes |
|---|---|---|---|
| **M1** | Model + encrypted store + gate skeleton | **No** | DEK from OS keyring. Atomic — do not split. |
| **M2** | **Compose** authoring — screenshot, pick, type | **No** | Default path. Needs no permissions on any OS. Depends on the grid card's screenshot core. |
| **M3** | GUI — table, step editor, mandatory review | **No** | |
| **M4** | Replay engine + safety rails | No | First user of the dev bypass. |
| **M5** | Timer attachment + trust + idle defaults | No | |
| **M6** | **Capture** authoring (opt-in) + secrets | No | Optional. Blocked on two facts. |
| **M7** | **Execution password** — setup, lockout, audit | **Yes** | The gate becomes real here. |
| **M8** | **Execution tokens** + review gate + agent skill | **Yes** | |
| **M9** | Per-OS QA on real hardware + ship guard | Yes | |

M1–M6 are testable end to end with no password anywhere.

### What D-12 changed

- **M2 is now compose, not capture.** Authoring inside Bellman's own window needs no
  permission on any platform, works on Wayland, and cannot record a password by accident.
- **Capture moved to M6 and became optional.** It buys speed, at the cost of a
  global-input-capture grant. It must not quietly become the default path.
- The old M6 (password-field awareness) **folded into M6** — it is only needed once capture
  ships, and only on the platforms capture supports.
- Two of the three factual conflicts the research flagged are now **moot** for M1–M5. They
  block M6 only.

## Open before M2 and M6 start

Three factual conflicts the synthesis flagged for re-verification, plus eight operator
questions in synthesis §6. M1 does not depend on any of them and can start now.
