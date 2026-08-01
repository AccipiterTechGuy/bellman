# Keeping private things private in a public repo

Bellman is a public, MIT-licensed repo. This note explains where your own data and
customisations live, so nothing personal is ever published by accident.

## Your scheduling data is NOT in this repo — by design

Everything Bellman knows about *your* timers lives in a per-OS data directory,
never in the git tree. **There are two of them**, and which one is live depends
on which interface you use — the CLI and the desktop app do **not** share a
store:

| OS | `bellman` CLI (default) | desktop app (GUI) |
|---|---|---|
| Linux | `~/.bellman/` | `~/.local/share/io.bellman.desktop/` |
| macOS | `~/.bellman/` | `~/Library/Application Support/io.bellman.desktop/` |
| Windows | `%USERPROFILE%\.bellman\` | `%APPDATA%\io.bellman.desktop\` (Roaming) |

So if you run the GUI but integrate against `~/.bellman/` (or vice versa), you
are looking at a store that is silently empty. Discover the live one without
reading this doc: the GUI shows its directory under **Settings → Data**, and
`bellman --help` names the CLI's (override with `--db <PATH>` / `BELLMAN_DB`,
which shifts the whole tree below to that database's parent directory).

⚠️ **The scheduler lives in the desktop app.** A timer created with plain
`bellman add` goes into the CLI store, and **nothing fires it** unless a
Bellman process is driving *that* store — running the desktop app does not,
because it drives its own. If you want a timer the desktop app will fire,
create it against the app's data directory (`--db` / `BELLMAN_DB`, or the
slot channel under its `slots/` root); the CLI store is for setups where
something else drives it.

```
<data dir>/
├─ timers.db (+ -wal, -shm)   your timers, runs, claim ledger
├─ logs/events.current.jsonl  what actually happened (rotates weekly or past 64 MB)
├─ logs/archive/*.jsonl.gz    30-day history (configurable), gzip-compressed
├─ slots/                     the JSON slot channel
│  ├─ free/ work/ done/ bad/  requests apps make of Bellman, and its answers
│  └─ fires/fire-<run_id>.json  notifications Bellman makes of apps
├─ timers/                    human-browsable view of state, one folder per timer
│  ├─ README.txt              explains the folder to whoever opens it
│  ├─ <name>-<id>/
│  │  ├─ timer.json           what the timer IS (readable, not authoritative)
│  │  ├─ status.json          the CURRENT run — the truth, right now
│  │  └─ reply-<run_id>.json  where an integrated app answers (owned timers only)
│  └─ bad/                    quarantined copies of rejected replies
└─ config.json                horizon, retention, concurrency cap, wake settings
```

The `timers/` tree is a **projection**, not a source of truth: the database
owns timers and the event log owns history, so it can be deleted or rebuilt
without losing either. Apps integrate against it — see
[INTEGRATION.md](INTEGRATION.md#connect-your-own-application).

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
