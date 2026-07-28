# Keeping private things private in a public repo

Bellman is a public, MIT-licensed repo. This note explains where your own data and
customisations live, so nothing personal is ever published by accident.

## Your scheduling data is NOT in this repo — by design

Everything Bellman knows about *your* timers lives in the per-OS data directory, never
in the git tree:

| OS | data dir |
|---|---|
| Linux | `~/.bellman/` |
| macOS | `~/Library/Application Support/bellman/` |
| Windows | `%APPDATA%\bellman\` |

```
<data dir>/
├─ timers.db (+ -wal, -shm)   your timers, runs, claim ledger
├─ logs/events.current.jsonl  what actually happened
├─ logs/archive/*.jsonl       rotated history
├─ slots/{free,work,done,bad} the JSON slot IPC channel
└─ config.json                horizon, retention, concurrency cap, wake settings
```

So: the commands your timers launch, when they run, what they produced, and any app
integrating over the slot channel — none of it is in this repository, and cloning the
repo tells nobody anything about your schedule. Publishing the code publishes the code.

## Local code you don't want published

If you add private integrations, machine-specific scripts, or experiments, keep them
under one of the ignored patterns (see `.gitignore`):

```
local/            ← put anything private in here
*.local           e.g. notes.local
*.local.*         e.g. config.local.json, run.local.sh
.env, .env.*      secrets for local dev (.env.example IS tracked)
secrets.*         (secrets.example.* IS tracked)
```

Convention: **a tracked `*.example.*` file documents the shape, the untracked real one
holds the values.** Anyone cloning gets the template; only you have the filled-in copy.

## Credentials

Never commit real credentials, even to a private repo — git history is forever and
visibility can change. Bellman itself needs no API keys to run. For packaging/signing,
CI reads named repository secrets (`APPLE_CERTIFICATE`, `APPLE_PASSWORD`, …); the
workflow files reference the names only, never the values.

If something sensitive is ever committed, rotate it first and rewrite history second —
rewriting alone does not un-leak a value that was pushed.
