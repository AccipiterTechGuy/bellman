<!-- Thanks! Before submitting, please confirm the checkboxes that apply.
     Delete this comment when done. -->

## What and why

<!-- One paragraph: what changed and why it is needed. Link the issue if any. -->

## Out of scope

<!-- Anything a reviewer might expect in this diff that you deliberately
     left out (and why). -->

## Verification (required)

<!-- This project has shipped fully-tested features that did not work for a
     person. "Tests pass" alone is not enough — show the change working from
     the user's side: the command you ran, what you saw, a screenshot, a
     captured file. -->

- [ ] `cargo test --workspace --all-targets` passes (or said why not run)
- [ ] `cd ui && npm test && npm run build` passes (if `ui/` touched)
- [ ] `./tests/cli_roundtrip.sh` passes (if the CLI was touched)
- [ ] I ran the product and saw the change working (describe above)
- [ ] Docs updated where behaviour or the wire changed (and their pinning tests)
- [ ] No absolute home paths (`/home/<user>`, `/Users/<user>`) added —
      `bash scripts/check_no_personal_paths.sh` is clean

## Frozen protocol

- [ ] This PR changes **no** wire shape (`bellman-slot/1`, `bellman-reply/1`,
      `bellman-run/1`, `bellman-event/1`) and introduces no required SDK /
      handshake — or it explicitly proposes changing the standing decision in
      `docs/todo/CARD_INDEX.md` (expect a long discussion).
