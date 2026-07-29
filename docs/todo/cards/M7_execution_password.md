# M7 — Execution password: setup, lockout, audit polish

Design: `macro_recorder_security_plan.md` D-2, D-8 (Q4, Q5).

**This is where the gate becomes real.** M1–M6 all ran open, behind the dev bypass. Nothing
before this card required a password to test, by design.

## Scope

- Setup wizard for the **execution password** (that name — not "the Bellman password", so a
  second gated feature later does not force a rename).
- Argon2id `m=64MiB, t=3, p=4` (RFC 9106 second recommended). **Measure the memory cost
  against Bellman's idle-footprint exit gates in `docs/PERF.md`** — do not assume it fits.
- The password wraps a **second copy** of the DEK for migration. The keyring copy from M1
  remains the primary.
- **No recovery code** (D-8 Q4). The wizard must state plainly: changing the code can remove
  the gate but cannot decrypt data, and the password is what moves macros to another
  machine. Losing it does not lose the macros while this machine's keyring is intact.
- Lockout with backoff, **surviving a process kill**.
- Export / import via `age`.
- The full audit event set incl. `macro_hash` and the chain root; run-history filtering.
- Cap changes audited (D-11) — "the runtime ceiling was raised" must be findable.

## Exit gate

- Setup cannot complete without acknowledging the data-loss/migration sentence.
- 10 failed unlocks → 15-minute lockout that survives a process kill.
- An overnight fixture log answers "what ran, who asked, how was it authorised, did it change
  since I reviewed it" for every run **and every refusal**.
- Chain verification detects an edited line.
- Argon2id memory measured against the P3/P5 idle gates, with the number written down.
