# Contributing to Bellman

Bellman is public so the work can be followed in the open; it is **not yet
ready to install or to take feature requests** (see the README banner). Small,
well-scoped fixes are welcome anyway — read this file first.

## Ground rules

- **The output protocol is frozen by design.** One writer per file; the wire
  shapes `bellman-slot/1`, `bellman-reply/1`, `bellman-run/1`,
  `bellman-event/1`; a ten-line client in any language stays a complete
  client. Do not propose a required SDK, schema layer, shared output file, or
  extra handshake — see `docs/todo/CARD_INDEX.md` → *Standing decision* for
  the full ruling. Defects in the protocol's *implementation* or its *docs*
  are always wanted.
- **Prove it from the user's side, not only in tests.** This project has
  already shipped one fully-tested feature that did not work for a person.
  If you change behaviour, run the product and show what you saw.
- **Keep diffs scoped.** A bug fix does not include a cleanup of the
  surrounding file.

## Build and test

Everything is in the README's [Install](README.md#install) section
(system prerequisites per distro, Rust, Node 24, tauri-cli). From a checkout:

```sh
cargo test --workspace --all-targets   # Rust suite (full on Linux/macOS)
cargo clippy --workspace --all-targets -- -D warnings
cd ui && npm ci && npm test && npm run build   # UI unit tests + build
./tests/cli_roundtrip.sh               # CLI end-to-end smoke test
./launch.sh                            # freshness-aware GUI launch
```

CI (`.github/workflows/`) runs the Rust + UI suites and builds unsigned
packages on all three OSes; `linux.yml` is the full gate. One disclosed gap:
the `bellman-app` shell-crate unit tests do not run on the headless Windows
runner (excluded in `windows.yml`).

## Repository hygiene

- **Never commit absolute home paths** (`/home/<user>`, `/Users/<user>`) —
  `scripts/check_no_personal_paths.sh` runs in CI and fails the build. QA
  evidence uses the `/home/tester` placeholder; docs use `/home/you`.
- **Never commit your own scheduling data** — it lives in the per-OS data
  directory, not here. Private integrations go under the ignored patterns in
  `docs/LOCAL.md` (`local/`, `*.local`, …).
- Redact `bellman scan` output before pasting it anywhere public — it prints
  full crontab command lines.
- Commit messages: short imperative subject; what + why in the body. The
  history uses card-tagged subjects (`SHIP1-E: …`) — matching that style is
  fine but not required.

## Documentation changes

Docs are a product surface here — an integrator reads `docs/INTEGRATION.md`
and branches on it. If you change a wire shape or behaviour, update the doc
**and** the test that pins it (e.g.
`crates/bellman-core/tests/doc_fire_example.rs` for the fire-notification
example). A doc that drifts from the wire is a bug of the same severity as
the wire being wrong.

## Code of conduct

Participation is covered by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
