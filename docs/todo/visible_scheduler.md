# Visible Scheduler — one honest list of everything scheduled on this machine

Repo: `~/bellman`

## Goal

Bellman becomes the single place to see, understand and safely change **every** schedule on
the machine — not just Bellman's own timers. Nothing scheduled should be invisible, and
nothing Bellman creates should be hidden from this list.

## Sources to discover (Linux, v1)

| source | where |
|---|---|
| user crontabs | `crontab -l`, `/var/spool/cron/crontabs/<user>` |
| system crontab | `/etc/crontab` |
| drop-ins | `/etc/cron.d/*` |
| run-parts dirs | `/etc/cron.{hourly,daily,weekly,monthly}/` |
| anacron | `/etc/anacrontab` |
| systemd timers | `systemctl list-timers --all` (system **and** `--user`) |
| one-shot jobs | `atq` / `at -c <id>`, transient `systemd-run --on-*` units |
| Bellman | its own timer store |

## Per-task fields

`source` (exact file or unit), `owner user`, `command`, `schedule expression`,
`human explanation`, `next run`, `last run + result`, `logs`, `enabled/disabled`.

**Be honest about what is knowable.** systemd gives real exit status
(`systemctl show -p ExecMainStatus`) and real logs (`journalctl -u`). **cron does not
record exit status at all** — only that it invoked the job (journal / `/var/log/syslog`).
So `last result` for a cron job is best-effort and is often genuinely unknown.
Report `unknown`. Do NOT infer success from the absence of an error — that is the single
most tempting wrong move in this card.

## Parsing details that WILL bite

- `@reboot @yearly @annually @monthly @weekly @daily @midnight @hourly`
- 5-field (user crontab) vs 6-field (`/etc/crontab`, `/etc/cron.d` — extra user column)
- environment lines: `SHELL=`, `PATH=`, `MAILTO=`, `CRON_TZ=`
- **`%` in a cron command means newline** and everything after the first unescaped `%`
  is stdin, not arguments. Mis-handling this corrupts commands on rewrite.
- ranges/steps/names: `*/5`, `1-5`, `MON-FRI`, `JAN`, `1,15`
- cron runs in **system local time** unless `CRON_TZ` says otherwise; systemd timers carry
  their own. Reuse `bellman-core`'s existing occurrence engine (croner + chrono-tz) for
  next-run computation — do not hand-roll a second one, and do not lose the DST/clamp
  policies it already implements.

## Safety rules — non-negotiable

1. **Reading is free. Writing is dangerous.** A bad write to `/etc/cron.d` can break boot
   or silently kill a backup job.
2. **v1 is read-only for everything outside the invoking user's own crontab.** System
   files (`/etc/cron*`, `/etc/anacrontab`) and system systemd units are DISPLAY-ONLY.
   Wanting to change one prints exactly what would be run and stops.
3. **Never silently escalate.** If a change needs root, say which command needs it and
   stop. Do not call `sudo`, `pkexec` or a polkit agent from Bellman.
4. **Back up before every write.** Copy the current crontab into the Bellman data dir with
   a timestamp before applying, and keep the last N.
5. **Sentinel-fenced ownership.** Entries Bellman creates live between
   `# BEGIN bellman-managed` / `# END bellman-managed` with a stable id comment. Bellman
   may rewrite only inside its fence. **Never delete or reformat a line it did not
   create** — preserve hand-written lines, ordering, comments and whitespace byte-for-byte.
6. **Disable is reversible.** Disabling a cron entry comments it out and stores the exact
   original line; enable restores it byte-for-byte.
7. **`run now` needs explicit confirmation** and must never be implicit in a listing or
   an explain call. Capture stdout/stderr, record exit code, log it.
8. **Every write is a diff first.** `--dry-run` prints the before/after; no flag means no
   change.
9. Everything Bellman creates must appear in `bellman scan` — this card exists to remove
   hidden schedules, not add a new hiding place.

## CLI / API sketch

```
bellman scan [--source cron|cron.d|systemd|at|bellman|all] [--user U] [--json]
bellman task show <id>                 # fields above
bellman task explain <id>              # "every weekday at 06:00 Europe/Helsinki"
bellman task logs <id> [--lines N]
bellman task enable|disable <id> [--dry-run]
bellman task run <id> --confirm
bellman task new --command "..." --cron "..." [--source bellman|cron] [--dry-run]
bellman task edit <id> [...] --dry-run
bellman scan --diff                    # changed since last scan (drift detection)
```

`--json` on every read command — this is the surface an agent drives.

## Cross-platform

Bellman ships on three systems; this card implements **Linux fully**. Windows (Task
Scheduler) and macOS (launchd) get the same provider interface but return a clear
"not implemented on this platform yet", never an empty list that reads as "nothing
scheduled". Design the discovery layer as a per-OS provider so those slot in later.

## Acceptance

- A crontab line added by hand outside Bellman appears in `bellman scan` on next scan.
- A `/etc/cron.d` drop-in, a run-parts script, a system timer, a user timer and an `at`
  job each appear with the correct source path/unit.
- Next-run for systemd timers matches `systemctl list-timers` exactly.
- Next-run for a cron entry matches an independent calculation, including a `*/7` step
  and a `MON-FRI` range, across a DST boundary.
- A cron command containing `%` survives a disable→enable round trip byte-for-byte.
- Disable then enable leaves the crontab byte-identical to the original.
- Attempting to modify `/etc/crontab` refuses, explains, and changes nothing.
- Last result for a cron job with no journal evidence reports `unknown`, not `ok`.
- A crontab containing hand-written comments and blank lines is unchanged everywhere
  outside the bellman-managed fence after Bellman writes.
- `bellman scan --json` output is stable and schema-checked in a test.
