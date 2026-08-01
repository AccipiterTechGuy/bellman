# DOC1 — rustdoc coverage on the public API

Raised by **C11** (full-system validation, 2026-08-02). C11's §6 asks for
"module docs on every public item"; measuring it turned up a job an order of
magnitude larger than a validation card should absorb, so it is written up
here instead of half-done. Nothing is broken — this is polish with a real
payoff, because `bellman-core` is what an integrator reads.

## What C11 measured

```sh
$ RUSTFLAGS="-W missing_docs" cargo build -p bellman-core 2>&1 \
    | grep -c '^warning: missing documentation'
593
```

Module-level docs are already **complete**: all 135 source files carry a
`//!` header (C11 added the last two, `src-tauri/src/main.rs` and
`src-tauri/src/dto_serde_tests.rs`). What is missing is item-level docs on
public functions, methods, struct fields and associated constants.

The count is not evenly spread — the wire-shape types (`store::models`,
`events::record`, `slots::envelope`, `reply::document`) are already well
documented because they are what INTEGRATION.md describes; the gaps are
mostly on helper methods and builder setters.

## Scope

- Turn `missing_docs` on **crate by crate**, starting with `bellman-core`,
  and clear it. `bellman-cli` and `bellman-app` follow.
- One sentence per item, saying **why it exists**, not restating the name.
  `/// Returns the name` on `fn name()` is worse than nothing; it is noise
  that hides the items that needed a sentence.
- Where a method has a non-obvious contract already living in a comment
  inside the body (there are several in `scheduler/` and `reply/`), move it
  to the doc comment rather than duplicating it.
- Land `#![warn(missing_docs)]` in each crate root as the last step of that
  crate, so the gap cannot come back.

## Do NOT

- Do not add `#[allow(missing_docs)]` to make the lint quiet. If an item does
  not deserve a sentence, it probably does not deserve to be `pub` — narrow
  the visibility instead, which is the more valuable outcome.
- Do not touch behaviour. This card is comments and visibility only; any
  behavioural change found on the way is a separate card.
- Do not do all three crates in one push if that makes an unreviewable diff.

## Exit gate

- `RUSTFLAGS="-W missing_docs" cargo build -p bellman-core` → 0 warnings, and
  `#![warn(missing_docs)]` in `crates/bellman-core/src/lib.rs`.
- `cargo doc --no-deps -p bellman-core` renders with no broken intra-doc
  links.
- `cargo test --workspace --all-targets` and `cargo clippy --workspace
  --all-targets -- -D warnings` still green; `cargo fmt --all --check` clean.
- The diff contains no behavioural change — every non-comment line is either
  a visibility narrowing or a moved comment, and the commit message says
  which.
