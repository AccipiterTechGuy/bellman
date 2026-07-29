# M8 — Execution tokens + review gate + agent skill

Design: `macro_recorder_security_plan.md` **D-2 … D-5, D-8 (Q3)**. This is the operator's own
design; implement it as written.

## Scope

- Setting the execution password mints **one list of 10 tokens, displayed exactly once**
  (the 2FA backup-code pattern). Expiry configurable in Settings. Each token **single-use**.
- **Scopes, enforced by the type system:**

  | scope | permits | limits |
  |---|---|---|
  | `record` | one authoring session | single-use, expiry from Settings |
  | `run:<macro-id>` | **one named macro**, once | single-use, short TTL |

  **There is no `run:*` token, ever.** A blanket run token is the execution password with a
  shorter life. Three macros means three tokens.
- **Stored in two forms** (D-5): a **hash** so a token can be verified while the store is
  **locked** — an agent presents one when nobody is there to unlock anything — and an
  **encrypted copy** so the list can be re-displayed after unlock. Plus scope, expiry, and a
  **used-flag that survives a restart**, or a crash lets a spent token replay.
- Expiry must not trust the wall clock alone — reuse the scheduler's clock-jump detector.
- **Burn-all**: one action revokes every outstanding token.
- Minting more requires the execution password.
- **The review gate (D-4):** a macro authored under a token is stamped
  `agent-authored, unreviewed` and can **neither run nor be attached to a timer** until a
  human opens and approves it. Granting a token authorises a *session*, not its output.
- **The agent skill file** — documents how to author a macro and in what order. It
  **must never contain a token.** The operator pastes one in at the moment of use.

## Do NOT

- **Do not make the tooling fetch a token for itself.** That collapses documentation and
  credential back into one thing and destroys the entire property this card exists for: an
  agent that reads the skill gains knowledge and no capability.
- Do not add a run scope that names more than one macro.

## Accepted, not a bug

A token pasted into a chat is in that transcript on disk forever. That is exactly why they
are single-use. Treat every token as public the moment it is used.

## Exit gate

- A `record` token cannot start a replay — proven by the type system, i.e. it does not
  compile.
- A `run:<id>` token cannot run a different macro.
- A used token is refused after a process restart.
- A token verifies correctly while the store is locked.
- Burn-all invalidates every outstanding token.
- A macro authored under a token cannot be run or attached until approved — asserted.
- A repo-wide grep proves no token literal exists in the skill file or any docs.
