# Bellman CLI

AI-skill and human-facing command surface for Bellman. Talks to `bellman-core`
directly (headless — no daemon required). Binary name: **`bellman`**.

Primary consumer: AI agents. Pass **`--json`** on every command and parse the
stable envelope described below.

## Global options

| Flag | Env | Description |
|------|-----|-------------|
| `--json` | | Emit the JSON envelope on stdout (one object, one line unless `BELLMAN_JSON_PRETTY=1`). |
| `--db <PATH>` | `BELLMAN_DB` | Path to the timers SQLite database. Default: `~/.bellman/timers.db`. |

Exit status: `0` on success, `1` on error. With `--json`, errors still print a
JSON object on **stdout** (`ok: false`) so agents can parse a single stream —
including **clap parse-time failures** (missing required flags, unknown
subcommands, bad value types). Those use `error.code = "invalid_args"` and a
best-effort `command` field from argv. Help/version output stays human-readable
even when `--json` is present.

## JSON envelope (stable)

### Success

```json
{
  "ok": true,
  "command": "<command-name>",
  ...command-specific fields...
}
```

### Error

```json
{
  "ok": false,
  "command": "<command-name>",
  "error": {
    "code": "<machine_code>",
    "message": "<human message>"
  }
}
```

### Error codes

| Code | Meaning |
|------|---------|
| `invalid_args` | Missing/invalid flags or values |
| `not_found` | Timer name/id not found |
| `ambiguous_name` | Multiple timers share the name; use id |
| `stale_revision` | Optimistic concurrency conflict (retry) |
| `invalid_occurrence` | Occurrence failed core validation |
| `already_claimed` | Claim ledger already has this fire slot |
| `network_filesystem` | DB path is on a refused network FS |
| `action_failed` | Fire action returned an error |
| `store_error` | Other store / SQLite failure |

### Timer object

Timers serialize as the core `Timer` struct:

| Field | Type | Notes |
|-------|------|-------|
| `id` | UUID string | Stable primary key |
| `name` | string | Display name |
| `enabled` | bool | `false` when paused |
| `occurrence` | object | See occurrence schema |
| `tz` | string | IANA tz (mirrors occurrence) |
| `next_fire_utc` | RFC3339 string \| null | Next scheduled fire (UTC) |
| `last_fired` | RFC3339 string \| null | Last delivered fire (UTC) |
| `misfire` | object | Policy (`skip` / `coalesce` / `catch_up`) |
| `overlap` | string \| object | `skip` \| `queue_one` \| `parallel` \| `replace` |
| `retry` | object | `{ max_retries, delay_secs }` |
| `valid_from` / `valid_until` | RFC3339 \| null | Validity window |
| `max_runs` | u64 \| null | Cap on delivered runs |
| `tags` | string[] | Free-form tags |
| `action` | object | Wake action (`type`: `none` / `launch` / `notify`) |
| `revision` | i64 | Optimistic concurrency token |

### Occurrence object

Tagged by `kind.occ` (serde tag `occ`):

```json
{ "kind": { "occ": "daily", "at": "09:30:00" }, "tz": "Europe/Helsinki", ... }
```

Kinds: `once` | `interval` | `daily` | `weekly` | `monthly` | `yearly` | `cron`.

---

## Commands

### `bellman add`

Create a timer.

```text
bellman add --name <NAME> --occurrence <KIND> [kind flags] [--tz TZ] [--tag T]...
```

| Flag | Required for | Description |
|------|--------------|-------------|
| `--name` | always | Timer name |
| `--occurrence` | always | `once` \| `interval` \| `daily` \| `weekly` \| `monthly` \| `yearly` \| `cron` |
| `--time` | once, daily, weekly, monthly, yearly | See time formats below |
| `--every-secs` | interval | Period in seconds (≥ 1) |
| `--days` | weekly | Comma-separated weekdays (`mon,wed` or `monday,…`) |
| `--day` | monthly, yearly | Day of month 1–31 |
| `--month` | yearly | Month 1–12 |
| `--cron` | cron | Cron expression (seconds optional; croner) |
| `--tz` | optional | IANA timezone (default: system local if detectable, else `UTC`) |
| `--tag` | optional | Repeatable free-form tags |

**Time formats**

- once: `YYYY-MM-DDTHH:MM:SS` (interpreted in `--tz`) or full RFC3339
- daily / weekly / monthly / yearly: `HH:MM` or `HH:MM:SS`

**JSON success**

```json
{
  "ok": true,
  "command": "add",
  "timer": { /* Timer object */ }
}
```

**Example**

```bash
bellman add --name backup --occurrence daily --time 03:00 --tz UTC --json
bellman add --name tick --occurrence interval --every-secs 30 --json
bellman add --name standup --occurrence weekly --days mon,wed,fri --time 09:30 --json
bellman add --name payroll --occurrence monthly --day 1 --time 08:00 --json
bellman add --name ny --occurrence yearly --month 1 --day 1 --time 00:00 --json
bellman add --name once-job --occurrence once --time 2030-06-15T12:00:00 --tz UTC --json
bellman add --name noon --occurrence cron --cron "0 0 12 * * *" --tz UTC --json
```

---

### `bellman list`

List all timers (any enabled state), ordered by name then id.

```text
bellman list [--json]
```

**JSON success**

```json
{
  "ok": true,
  "command": "list",
  "count": 2,
  "timers": [ /* Timer, … */ ]
}
```

---

### `bellman edit`

Patch a timer identified by **name or id**.

```text
bellman edit <name-or-id> [--name NEW] [--time TIME] [--enabled true|false]
                           [--every-secs N] [--days …] [--day N] [--month N]
                           [--cron EXPR] [--json]
```

- `--time` updates the wall-clock component of the existing kind (or once-at).
- Kind-specific flags only apply when the timer already has that kind.
- At least one patch flag is required.
- Uses optimistic revision internally (single writer; agents need not pass revision).

**JSON success** — same shape as `add` with `"command": "edit"`.

---

### `bellman rm`

Delete a timer by name or id.

```text
bellman rm <name-or-id> [--json]
```

**JSON success**

```json
{
  "ok": true,
  "command": "rm",
  "id": "<uuid>",
  "name": "<name>",
  "deleted": true
}
```

---

### `bellman next`

Preview the next **N** fire times (default **5**) without mutating the schedule.

```text
bellman next <name-or-id> [N] [--json]
```

Fires are returned as UTC RFC3339 strings. Skip-next / exclusions / max_runs
apply the same way as live `next_fire`, but pending skips are **not** consumed.

**JSON success**

```json
{
  "ok": true,
  "command": "next",
  "id": "<uuid>",
  "name": "<name>",
  "n": 5,
  "fires": ["2030-06-15T12:00:00+00:00", "..."]
}
```

---

### `bellman run-now`

Execute the timer's action **immediately** through the core fire path:

1. `claim_run(timer_id, now)` on the store claim ledger  
2. invoke the [`FireAction`] callback  
3. `complete_run`  
4. advance `last_fired` + `record_run` (same bookkeeping as the scheduler)

Until C6 ships real launch/notify actions, the injected action is a **stub**:
one log line on stderr, also returned in the JSON `message` field.

```text
bellman run-now <name-or-id> [--json]
```

**JSON success**

```json
{
  "ok": true,
  "command": "run-now",
  "id": "<uuid>",
  "name": "<name>",
  "run_id": "<uuid>",
  "scheduled_for": "<RFC3339 UTC>",
  "message": "bellman: run-now stub action …",
  "timer": { /* Timer after advance */ }
}
```

---

### `bellman pause` / `bellman resume`

Disable or re-enable a timer (`enabled = false` / `true`). Paused timers are
omitted from the scheduler horizon query.

```text
bellman pause  <name-or-id> [--json]
bellman resume <name-or-id> [--json]
```

**JSON success** — same shape as `add` with `"command": "pause"` or `"resume"`.

---

## Name or id resolution

Arguments accepting `<name-or-id>`:

1. If the string parses as a UUID → lookup by id.  
2. Else exact **case-sensitive** name match.  
3. Zero matches → `not_found`. Multiple matches → `ambiguous_name` (use id).

---

## Round-trip smoke test

```bash
./tests/cli_roundtrip.sh
```

Adds all seven occurrence kinds, then exercises list / edit / next / pause /
resume / run-now / rm against `--json` output.
