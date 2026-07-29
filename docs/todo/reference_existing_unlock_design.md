# Reference: an existing, tested unlock + encryption design (operator's own code)

**Status:** prior art from the operator's own trading-bot repo, offered as a starting point
for Bellman's macro-execution gate. It is the operator's code, so there is no licensing
question — but it was built for a **Linux systemd daemon with a human at an admin UI**, and
Bellman is a cross-platform desktop scheduler that must fire at 03:00 unattended. Take the
shape, not the whole thing. The differences are called out below and they matter.

Source (read-only reference, do not vendor):
`~/Trading_bot/Coding_section/User_Interface/exchange_gateway_ui/backend/middleware/`
and `~/Trading_bot/Coding_section/src/exchange/execution_gateway/common/`.

## Layer 1 — password verification (`password_store.py`)

- **scrypt** from the stdlib, no third-party dependency: `N=16384, r=8, p=1, dklen=32`,
  16-byte random salt. ~64 MB peak per call.
- Stored as ONE versioned line:
  `scrypt$N=16384,r=8,p=1$<b64-salt>$<b64-dk>`.
  The algorithm prefix is deliberate — it reserves room to add `argon2id$…` later without
  the parser guessing. The parser validates algorithm AND parameters and refuses anything
  it does not recognise rather than assuming.
- **Atomic write**: `O_CREAT|O_EXCL|O_WRONLY` to a `.tmp` → write → `fsync` → close →
  `os.replace` → `chmod 0600`, inside a `0700` directory, with `umask(0o077)` held across
  the whole operation as a belt against a loose ambient umask. Stale `.tmp` from a previous
  crash is removed first.
- **Constant-time compare** via `hmac.compare_digest` — timing leaks the verdict, not the
  contents.
- Minimum length 8. Verify-first ordering on change-password.

**Ports to Bellman almost unchanged.** In Rust, prefer `argon2id` over scrypt (it is the
current recommendation and the file format above already anticipates the migration), but
keep every other property: versioned line, parameter validation, atomic 0600 write in an
0700 dir, constant-time compare.

## Layer 2 — session unlock, key held in memory (`unlock_gate.py`)

- Correct password → `secrets.token_urlsafe(32)` bearer token.
- Token lives in a **module-level in-memory dict only**. Never written to disk.
- **15-minute TTL**, expired entries pruned on every check.
- Middleware **fails closed**: an explicit allowlist of protected routes, and anything on it
  without a valid token gets `401 NOT_UNLOCKED`. Not a denylist — a route is unprotected
  only if someone deliberately left it off the list.
- `revoke_all_tokens()` on password change, so every live session must re-unlock.

**Ports in shape, NOT in policy.** Two things must change for Bellman:

1. **The 15-minute TTL is wrong here.** It is correct for a human clicking an admin UI. A
   scheduler exists to act while nobody is present — a 15-minute window guarantees the
   03:00 timer finds itself locked. This is exactly the unattended paradox the research is
   chartered to solve; the answer is a deliberate policy decision, not this constant.
2. **One chokepoint.** The allowlist works because it is a single middleware every request
   passes through. Bellman needs the same property for macro execution: one function that
   every execution path goes through, so a future sixth path cannot quietly skip the gate.

## Layer 3 — encryption at rest (`runtime_bin.py`, `creds_writer_daemon.py`)

This is the paranoid part, and the part that does **not** port.

- Encryption is delegated to **`systemd-creds encrypt`**, with seal modes `tpm2`, `host`,
  or `host+tpm2` — so the key can be sealed to the machine's TPM, not stored by the app.
- Ciphertext is written atomically (`O_CREAT|O_EXCL`, 0600, fsync, `os.replace`, plus a
  **parent-directory fsync**).
- systemd decrypts at unit start into `$CREDENTIALS_DIRECTORY` (tmpfs). The application
  only ever *reads* plaintext from there and never writes it back.
- Where the service cannot encrypt for itself, a **root-owned unix-socket writer daemon**
  does it, authenticating the caller with `SO_PEERCRED`.
- Explicit rule in the module docstring: *never* print plaintext or ciphertext.

**The principle transfers; the mechanism does not.** systemd-creds is Linux-only. The idea
worth keeping is *the OS holds the key and the app never persists it*. Per-OS equivalents
to evaluate:

| OS | Equivalent |
|---|---|
| Linux | systemd-creds (as here), or Secret Service via the `keyring` crate |
| Windows | DPAPI / Credential Manager; TPM via CNG |
| macOS | Keychain, Secure Enclave on Apple Silicon |

## What to take

- Layer 1 essentially as-is, with argon2id in place of scrypt.
- Layer 2's **fail-closed allowlist through a single chokepoint**, and revoke-all on
  password change. Replace the TTL policy.
- Layer 3's **principle** — OS-held key, app never persists it, atomic 0600 writes with
  parent-dir fsync, never log either side of the encryption.

## What not to take

- The 15-minute session TTL.
- systemd-creds as *the* mechanism — it is one platform's implementation of the principle.
- The hardcoded absolute default path (`DEFAULT_HASH_FILE`). Bellman resolves its data dir
  per-OS already; use that.
