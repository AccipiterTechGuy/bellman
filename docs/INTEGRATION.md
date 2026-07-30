# Bellman slot integration

External apps wake timers through a **directory of JSON slot files** — no SDK,
no socket, no shared library. Publish a complete request via temp-file + atomic
rename into `free/`; Bellman claims it into `work/`, applies the operation to
the timer store, and writes the answer into `done/`.

Default slots root (Linux): `~/.bellman/slots/`  
Layout: `slots/{free,work,done,bad}/`

CLI helper (one-shot, no daemon):

```bash
bellman slot-submit request.json --slots ~/.bellman/slots --db ~/.bellman/timers.db
```

## Protocol (`bellman-slot/1`)

### Rules

1. **Never edit a free stub in place.** Claim a free stub by exclusive rename,
   then write the *complete* request under the reserved `slot-NNNN.json` name
   via temp + same-directory rename. (Or use `SlotService::publish` /
   `bellman slot-submit`.)
2. **`request_id` is the idempotency key** (UUID string). Retries with the same
   id return the original response; they never double-create a timer.
3. **Ownership**: `modify` / `delete` require the same `app_name` that created
   the timer via slots.
4. **Watcher events are hints**; a periodic rescan is the source of truth. After
   publishing, either wait for the running Bellman process or call
   `bellman slot-submit` / drive `SlotService::poll` yourself.
5. Free stubs are pre-generated (≥5). After every claim Bellman replenishes.

### Request envelope

```json
{
  "schema": "bellman-slot/1",
  "slot_id": "0001",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "logged_at": "2026-07-27T12:00:00Z",
  "operation": "add",
  "payload": {
    "app_name": "my-app",
    "timer_name": "morning-wake",
    "tz": "UTC",
    "occurrence": { "kind": "daily", "time": "08:00:00" },
    "launch_command": "/usr/bin/true",
    "args": []
  }
}
```

| Field | Required | Notes |
|-------|----------|-------|
| `schema` | yes | must be `bellman-slot/1` (or major 1) |
| `slot_id` | filled by Bellman / free stub | reserved id; must match the filename |
| `request_id` | yes | UUID string; idempotency key |
| `logged_at` | optional | producer timestamp |
| `operation` | yes | `add` \| `modify` \| `delete` |
| `payload.app_name` | yes | ownership identity |
| `payload.timer_name` | add | display name |
| `payload.timer_id` / `id` | modify/delete | from earlier `done/` response |
| `payload.occurrence` | add | see below |
| `payload.tz` | optional | IANA tz (default UTC) |
| `payload.launch_command` + `args` | optional | convenience for `Action::Launch` |
| `payload.action` | optional | full action JSON (`type`: launch/notify/none) |
| `payload.ack_through` | optional | advance un-acked run-event cursor |

### Occurrence (simplified payload form)

```json
{ "kind": "interval", "every_secs": 60 }
{ "kind": "daily", "time": "08:00:00" }
{ "kind": "once", "time": "2030-01-01T09:00:00" }
{ "kind": "weekly", "time": "09:00:00", "days": ["mon", "wed"] }
{ "kind": "monthly", "time": "10:00:00", "day": 15 }
{ "kind": "yearly", "time": "10:00:00", "month": 7, "day": 4 }
{ "kind": "cron", "cron": "0 0 12 * * *" }
```

### Response envelope (`done/slot-NNNN.json`)

```json
{
  "schema": "bellman-slot/1",
  "slot_id": "0001",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "ok",
  "timer_id": "…",
  "next_fire_at": "2026-07-28T08:00:00Z",
  "events": []
}
```

On error: `"status": "error"` plus `"error": "…"`. Garbage input is quarantined
into `bad/` with a `.err.json` sidecar (not a normal response).

Each entry in `events` is one un-acked run:

```json
{
  "event_sequence": 3,
  "run_id": "…",
  "timer_id": "…",
  "scheduled_for": "2026-07-28T08:00:00Z",
  "status": "fired",
  "claimed_at": "2026-07-28T08:00:01Z",
  "completed_at": null
}
```

`status` uses the one run-state vocabulary shared with the event log
(`RunState`): `fired` means the run is open, `wake_delivered` / `wake_failed`
report how Bellman's wake action ended. The app-reported states
(`acknowledged`, `running`, `completed`, `failed`) arrive with the reply
channel — Bellman never writes them itself. Ack entries by sending any
request for that timer with `payload.ack_through` set to the highest
`event_sequence` seen.

---

## Copy-paste clients (< 10 lines each)

Each example publishes an **interval 60 s** timer named `demo-wake` for app
`demo-app`. Set `BELLMAN_SLOTS` to the slots root. `BELLMAN_DB` is optional —
when unset, Bellman uses `~/.bellman/timers.db` (CLI default).
`bellman slot-submit` opens/replenishes free stubs, so a pre-running daemon is
not required.

### Python 3

```python
import json, os, uuid, pathlib, subprocess
root, db = os.environ["BELLMAN_SLOTS"], os.environ.get("BELLMAN_DB")
req = {"schema":"bellman-slot/1","request_id":str(uuid.uuid4()),"operation":"add",
  "payload":{"app_name":"demo-app","timer_name":"demo-wake","tz":"UTC",
  "occurrence":{"kind":"interval","every_secs":60}}}
pathlib.Path("/tmp/bellman-req.json").write_text(json.dumps(req))
cmd = ["bellman","slot-submit","/tmp/bellman-req.json","--slots",root]
if db: cmd += ["--db", db]
subprocess.check_call(cmd)
```

### Shell (bash)

```bash
REQ=$(mktemp)
cat >"$REQ" <<EOF
{"schema":"bellman-slot/1","request_id":"$(uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid)","operation":"add",
 "payload":{"app_name":"demo-app","timer_name":"demo-wake","tz":"UTC",
 "occurrence":{"kind":"interval","every_secs":60}}}
EOF
args=(slot-submit "$REQ" --slots "${BELLMAN_SLOTS}")
[[ -n "${BELLMAN_DB:-}" ]] && args+=(--db "$BELLMAN_DB")
bellman "${args[@]}"
```

### PowerShell

```powershell
$req = @{ schema="bellman-slot/1"; request_id=[guid]::NewGuid().ToString();
  operation="add"; payload=@{ app_name="demo-app"; timer_name="demo-wake";
  tz="UTC"; occurrence=@{ kind="interval"; every_secs=60 } } } | ConvertTo-Json -Depth 5
$f = Join-Path $env:TEMP "bellman-req.json"; Set-Content $f $req
$args = @("slot-submit", $f, "--slots", $env:BELLMAN_SLOTS)
if ($env:BELLMAN_DB) { $args += @("--db", $env:BELLMAN_DB) }
bellman @args
```

### Node.js

```javascript
const fs = require("fs"), {execFileSync} = require("child_process"), {randomUUID} = require("crypto");
const f = "/tmp/bellman-req.json";
fs.writeFileSync(f, JSON.stringify({schema:"bellman-slot/1", request_id:randomUUID(),
  operation:"add", payload:{app_name:"demo-app", timer_name:"demo-wake", tz:"UTC",
  occurrence:{kind:"interval", every_secs:60}}}));
const args = ["slot-submit", f, "--slots", process.env.BELLMAN_SLOTS];
if (process.env.BELLMAN_DB) args.push("--db", process.env.BELLMAN_DB);
execFileSync("bellman", args, {stdio:"inherit"});
```

---

## Event log

Lifecycle events append to `logs/events.current.jsonl` (one JSON object per
line, `bellman-event/1`):

```json
{"schema":"bellman-event/1","logged_at":"2026-07-28T08:00:01Z","kind":"fired","event_id":"…","timer_id":"…","run_id":"…","timer_name":"morning-wake","scheduled_for":"2026-07-28T08:00:00Z"}
```

Top-level `kind` is always the **event kind**, from the one R5 run-state
vocabulary shared with the slot run-event feed: `registered`, `fired`,
`fired_late`, `skipped_misfire`, `coalesced`, `wake_delivered`, `wake_failed`,
`no_ack`, `pruned`, `year_recalibrate`, `cancelled`, `superseded` are written
by Bellman;
`acknowledged`, `running`, `completed`, `failed` are reserved for app reports
(reply channel). Timestamps end `_at` (`logged_at`); `scheduled_for` is the
one exception — it is an intent, not an occurrence. Rotation moves the
current file to `logs/archive/events-<ISO-week>.jsonl.gz` (gzip-compressed) —
weekly, or before an append would take it past 64 MB (configurable); archives
older than 30 days (configurable) are deleted, then oldest archives until
current + archives fit a 1 GB (configurable) retained-log budget. History is
therefore 30-day history (configurable), not forever.

## Fire notification (write-output-slot action)

A timer whose action is **write output slot** publishes one JSON per firing
into the configured output directory (`bellman-fire/1`):

```json
{"schema":"bellman-fire/1","kind":"fired","timer_id":"…","timer_name":"morning-wake","run_id":"…","scheduled_for":"2026-07-28T08:00:00Z","fired_at":"2026-07-28T08:00:01Z","occurrence_kind":"on_time"}
```

`kind` is the event kind (`fired` / `fired_late` / `coalesced`) — the same
vocabulary as the event log. `occurrence_kind` describes the occurrence:
`on_time` | `late` | `coalesced` | `catch_up_<n>`.

Wake actions: **launch** (arg array, no shell, `BELLMAN_RUN_ID` env, timeout
kill, output cap), **write-output-slot**, **desktop notification** (stub until
the GUI lands). Overlap default **skip**; failed wakes retry **1× after 30 s**,
then log `wake_failed` with message `FAILED`.
