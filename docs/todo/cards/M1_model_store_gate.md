# M1 — Macro model + encrypted store + gate skeleton

Design: `docs/todo/macro_recorder_security_plan.md` (D-1…D-12). Read it first.

## Scope

- `Macro` / `Step` / `Target` / `TrustLevel` types.
- `macros.enc`: header, Argon2id KEK, wrapped DEK, XChaCha20-Poly1305, generation ID.
- **DEK's primary home is the OS keyring** (`keyring-core` + one platform-native store), so
  the store is encrypted from day one with **no password anywhere**. The execution password
  wraps a *second* copy for migration only — and that arrives in M7, not here.
- `gate` module: `RunToken` with a **private field**, `unlock()`, `authorize()`. `replay()`
  takes a `RunToken` **by value**, so no execution path can compile without going through
  `authorize()`. D-7 of the synthesis: a type, not an `if`.
- Persisted lockout counter.
- The dev-bypass cargo feature and **all five ship guards**: `compile_error!` under
  `not(debug_assertions)`, `build.rs` panic on `PROFILE=release`, feature-unification check,
  a poison marker grepped out of every shipped artifact, undismissable UI banner.
- JSONL event types + audit hash chain, including `macro_hash`.

## Do NOT

- No capture. No injection. Not one line.
- **Do not split this card.** If encryption slips to a later card while replay ships, the
  result is the UI lock the whole design exists to prevent: a padlock over a plaintext file.

## Exit gate

- Round trips: wrong password fails, right password opens, password change re-wraps without
  re-encrypting, a tampered header byte fails the AEAD, rollback to an older generation is
  detected.
- `cargo build --release --features dev-open-gate` **fails to compile**.
- `cargo tree` feature-unification check passes; CI marker-grep proves a clean release binary.
- `authorize()` refusal matrix table-tested.
- A test **enumerates every execution source** and proves they all converge on `authorize()`.
- Everything above runs with no password entered anywhere.
