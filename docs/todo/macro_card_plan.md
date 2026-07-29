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

| # | Card | Password needed to test? |
|---|---|---|
| M1 | Model + encrypted store + gate skeleton | **No** — DEK from OS keyring |
| M2 | Capture, per-OS | **No** |
| M3 | GUI: table + step editor + review screen | **No** |
| M4 | Replay engine + safety rails | No — dev bypass, shipped in M1 |
| M5 | Timer attachment + unattended trust | No |
| M6 | Secrets + password-field awareness | No |
| M7 | **Master password** — setup, recovery, lockout, audit polish | **Yes — this is where it arrives** |
| M8 | **Capability tokens** — the 10-token list, review gate, skill | **Yes** |
| M9 | Per-OS QA on real hardware + the ship guard | Yes |

M1–M6 are testable end to end with no password anywhere. M7 turns the gate real. M8 adds the
token system from D-2…D-5. M9 validates on real Windows/macOS/Wayland hardware.

**Do not collapse M1.** If encryption slips out of it while replay ships, the result is the
UI lock the whole design exists to avoid.

## Card notes

**M1** — `Macro`/`Step`/`Target`/`TrustLevel`; `macros.enc` with Argon2id KEK, wrapped DEK,
XChaCha20-Poly1305, generation ID; keyring-backed DEK; `gate` module with `RunToken` (private
field, so `replay()` cannot be called without `authorize()`); persisted lockout counter; the
dev-bypass cargo feature with all five ship guards; JSONL event types with hash chain.
**No capture, no injection.**

**M2** — `trait InputCapture` per OS. Wayland returns `ReplayOnly`/`Unavailable` honestly.
Both recording modes; Steps is the default. **Re-verify F2 first**: the reports disagree on
whether `rdev` is abandoned (2023 vs 2026 release dates) and the whole write-our-own
recommendation turns on it.

**M3** — Svelte 5 page; three-pane layout; step editor with undo/redo and re-record-one-step;
five recording indicators; **mandatory post-record review** with Keep/Redact/Secret. A
recording cannot be saved without passing review — assert it in a test.

**M4** — `enigo` behind a `pub(crate)` adapter; panic key; max time/steps; single-runner
mutex; modifier release on abort; dry run and step-through. First card that uses the dev
bypass — which by now has shipped and been exercised for three cards.

**M5** — `macro` action type; per-macro trust; `armed_until` decay; refuse-and-log when
locked; **forced `skip`** misfire policy, never coalesce.

**M6** — `Secret` step type; Windows UIA `IsPassword`; macOS `AXSecureTextField`. **Re-verify
F-conflict first**: reports disagree on whether browsers expose `IsPassword`, which decides
whether auto-pause can be promised at all.

**M7** — master password setup wizard with the data-loss sentence and opt-in recovery code;
lockout with backoff surviving a process kill; export/import; full audit event set including
`macro_hash`. **The gate becomes real here.** Everything before it ran open.

**M8** — capability tokens per D-2…D-5: 10 single-use tokens shown once at master-password
setup; configurable expiry; **record scope only, enforced by the type system**; stored as a
hash (verifiable while locked) *and* an encrypted copy (re-displayable after unlock); a
used-flag surviving restart; burn-all. Plus D-4's review gate — macros recorded under a token
are `agent-authored, unreviewed` and can neither run nor be attached to a timer until a human
approves them. Plus the agent skill file, which **must never contain a token**.

**M9** — fresh-VM validation per OS: record → review → save → dry-run → attach → fires
overnight → the audit log reconstructs it. The marker grep proven to fail a poisoned build.

## Open before M2 and M6 start

Three factual conflicts the synthesis flagged for re-verification, plus eight operator
questions in synthesis §6. M1 does not depend on any of them and can start now.
