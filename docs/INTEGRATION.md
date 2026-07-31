# Bellman slot integration

External apps wake timers through a **directory of JSON slot files** — no SDK,
no socket, no shared library. Publish a complete request via temp-file + atomic
rename into `free/`; Bellman claims it into `work/`, applies the operation to
the timer store, and writes the answer into `done/`.

Slots root: `<data dir>/slots/` — the data dir is `~/.bellman/` on Linux,
`~/Library/Application Support/bellman/` on macOS, `%APPDATA%\bellman\` on
Windows (the desktop app uses its own app-data dir instead, Linux
`~/.local/share/io.bellman.desktop`). Full layout in
[LOCAL.md](LOCAL.md).

Layout: `slots/{free,work,done,bad,fires}/` — `free`/`work`/`done`/`bad`
carry **requests you make of Bellman**; `fires/` carries **notifications
Bellman makes of you** (see *Connect your own application*).

**Two directions, and most apps only need one:**

| you want to… | read |
|---|---|
| create / modify / delete timers from your app | *Protocol* + *Copy-paste clients*, below |
| **be woken by a timer and report the outcome** | **[Connect your own application](#connect-your-own-application)** |
| do that over a socket instead of files | [Talking over the local socket](#talking-over-the-local-socket-ipc) |
| just see it work first | `testing_apps/lightbulb/` — a ~130-line reference app |

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
`demo-app` — and then answers its fires. Set `BELLMAN_SLOTS` to the slots
root. `BELLMAN_DB` is optional — when unset, Bellman uses
`~/.bellman/timers.db` (CLI default). `bellman slot-submit` opens/replenishes
free stubs, so a pre-running daemon is not required.

The `app_name` on the add request becomes the timer's **integration owner**:
every firing publishes a notification under `fires/` and a pre-filled reply
stub, and only a reply carrying that same `app_name` is accepted. See
*Connect your own application* below for the full contract.

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

Answer its fires (scan once at startup, then keep rescanning — `run_id`
dedup makes redelivery safe):

```python
import json, os, time, pathlib
from datetime import datetime, timezone
fires, seen = pathlib.Path(os.environ["BELLMAN_SLOTS"]) / "fires", set()
while True:
    for f in sorted(fires.glob("fire-*.json")):
        fire = json.load(open(f))
        if fire.get("app_name") != "demo-app" or fire["run_id"] in seen: continue
        seen.add(fire["run_id"])
        p = fire["reply_path"]                    # absolute native path — open verbatim, never construct
        r = json.load(open(p))                    # stub: schema/run_id/app_name pre-filled by Bellman
        now = lambda: datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        r.update(state="acknowledged", acknowledged_at=now())
        json.dump(r, open(p + ".tmp", "w")); os.replace(p + ".tmp", p)
        do_the_work()                             # your job here
        r.update(state="completed", completed_at=now())
        json.dump(r, open(p + ".tmp", "w")); os.replace(p + ".tmp", p)
    time.sleep(1)
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

Answer its fires (no jq — the stub is JSON, edited with sed; write to a
temp file and rename, never edit in place):

```bash
declare -A seen
while :; do
  for f in "${BELLMAN_SLOTS}"/fires/fire-*.json; do
    [[ -e $f ]] || continue
    app=$(sed -n 's/.*"app_name": *"\([^"]*\)".*/\1/p' "$f" | head -1)
    run=$(sed -n 's/.*"run_id": *"\([^"]*\)".*/\1/p' "$f" | head -1)
    [[ $app == demo-app && -n $run && -z ${seen[$run]:-} ]] || continue
    seen[$run]=1
    p=$(sed -n 's/.*"reply_path": *"\([^"]*\)".*/\1/p' "$f" | head -1)  # absolute — open verbatim
    sed "s/\"state\": null/\"state\": \"acknowledged\", \"acknowledged_at\": \"$(date -u +%FT%TZ)\"/" "$p" > "$p.tmp" && mv "$p.tmp" "$p"
    do_the_work
    sed "s/\"state\": \"acknowledged\"/\"state\": \"completed\", \"completed_at\": \"$(date -u +%FT%TZ)\"/" "$p" > "$p.tmp" && mv "$p.tmp" "$p"
  done
  sleep 1
done
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

Answer its fires:

```powershell
$seen = @{}
while ($true) {
  Get-ChildItem (Join-Path $env:BELLMAN_SLOTS "fires") -Filter "fire-*.json" | ForEach-Object {
    $fire = Get-Content $_.FullName | ConvertFrom-Json
    if ($fire.app_name -ne "demo-app" -or $seen[$fire.run_id]) { return }
    $seen[$fire.run_id] = $true
    $p = $fire.reply_path                     # absolute native path — open verbatim, never construct
    $r = Get-Content $p | ConvertFrom-Json    # stub: schema/run_id/app_name pre-filled by Bellman
    $now = { (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ") }
    $r.state = "acknowledged"; $r | Add-Member -Force acknowledged_at (& $now)
    ($r | ConvertTo-Json -Depth 8) | Set-Content "$p.tmp"; Move-Item "$p.tmp" $p -Force
    Do-TheWork                                # your job here
    $r.state = "completed"; $r | Add-Member -Force completed_at (& $now)
    ($r | ConvertTo-Json -Depth 8) | Set-Content "$p.tmp"; Move-Item "$p.tmp" $p -Force
  }
  Start-Sleep 1
}
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

Answer its fires:

```javascript
const fs = require("fs"), path = require("path");
const fires = path.join(process.env.BELLMAN_SLOTS, "fires"), seen = new Set();
setInterval(() => {
  for (const n of fs.readdirSync(fires).filter(n => n.startsWith("fire-"))) {
    const fire = JSON.parse(fs.readFileSync(path.join(fires, n)));
    if (fire.app_name !== "demo-app" || seen.has(fire.run_id)) continue;
    seen.add(fire.run_id);
    const p = fire.reply_path;                // absolute native path — open verbatim, never construct
    const r = JSON.parse(fs.readFileSync(p)); // stub: schema/run_id/app_name pre-filled by Bellman
    const now = () => new Date().toISOString();
    Object.assign(r, {state: "acknowledged", acknowledged_at: now()});
    fs.writeFileSync(p + ".tmp", JSON.stringify(r, null, 2)); fs.renameSync(p + ".tmp", p);
    doTheWork();                              // your job here
    Object.assign(r, {state: "completed", completed_at: now()});
    fs.writeFileSync(p + ".tmp", JSON.stringify(r, null, 2)); fs.renameSync(p + ".tmp", p);
  }
}, 1000);
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

---

## Connect your own application

Everything an app needs is three JSON files and one rule: **one writer per
file**. Bellman writes its files, your app writes exactly one — the reply
file — and neither side ever touches the other's. The reference
implementation is `testing_apps/lightbulb/` in the repo: a stdlib-only terminal
app (~130 lines) whose reply logic is the six-line `reply()` function.

### The timer's folder

Each owned timer has a folder under `timers/` next to the database
(`~/.bellman/timers/<name>-<id>/` for the CLI default; the desktop app uses
its per-OS app-data dir, `~/.local/share/io.bellman.desktop` on Linux). You
never need to construct these paths — the fire notification carries them —
but a human browsing the folder sees:

| file | writer | reader | contents |
|---|---|---|---|
| `timer.json` | Bellman | everyone | what the timer IS; hand edits are ignored, the database wins |
| `status.json` | Bellman | everyone | the current run — **the truth, right now** (read this for "did it work?") |
| `reply-<run_id>.json` | **your app** | Bellman | the app's answer; a fresh file per run |

#### `status.json` — the mirror (`bellman-run/1`)

The current run, and the only place Bellman's view and your app's reports
are merged. Read it to answer *"did it work?"* without parsing the log.
Every fire notification carries its absolute path as `status_path`. It is
**Bellman-written and app-readable**; your app never writes it.

```json
{
  "schema": "bellman-run/1",
  "state": "completed",
  "run_id": "9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
  "timer_id": "3f1a2b9c-…",
  "timer_name": "demo-wake",
  "occurrence_kind": "interval",
  "scheduled_for": "2026-07-28T08:00:00Z",
  "fired_at": "2026-07-28T08:00:01Z",
  "app_name": "demo-app",
  "acknowledged_at": "2026-07-28T08:00:02Z",
  "expected_secs": 15,
  "completed_at": "2026-07-28T08:00:17Z",
  "result": {"ok": true},
  "transport": "json"
}
```

| field | always? | written from | notes |
|---|---|---|---|
| `schema` | yes | Bellman | `bellman-run/1` |
| `state` | yes | both | the R5 vocabulary — `fired`, `fired_late`, `coalesced`, `no_ack`, `superseded`, `cancelled` are Bellman's; `acknowledged`, `running`, `completed`, `failed` are your app's |
| `run_id`, `timer_id`, `timer_name` | yes | Bellman | identity; `run_id` matches the log and the reply filename |
| `occurrence_kind` | yes | Bellman | the **recurrence type**: `once`, `interval`, `daily`, `weekly`, `monthly`, `yearly`, `cron` |
| `scheduled_for`, `fired_at` | yes | Bellman | intended time, actual fire time |
| `app_name` | owned runs | Bellman | the integration owner snapshotted at fire; absent on unowned timers |
| `acknowledged_at`, `expected_secs`, `error_detection` | if reported | your app | pickup and the estimate driving the GUI label + opt-in watchdog |
| `heartbeat_at`, `progress` | if reported | your app | liveness; **never** in the event log — here is the only place they exist |
| `completed_at`, `result`, `result_truncated` | on success | your app | `result` is any JSON, capped at 32 KB here (`result_truncated: true` when trimmed) |
| `failed_at`, `reason`, `failure_kind` | on failure | both | `failure_kind` is `reported` (your app said so) or `timed_out` (watchdog expiry) |
| `no_ack_at` | on silence | Bellman | pickup grace lapsed with no valid reply |
| `transport` | yes | Bellman | `json`, `ipc` or `ipc_fallback` — how this run was delivered |

Optional fields are **omitted, not null** — absence means "not reported",
and fields accumulate across replies rather than being retracted. A run
that is still open simply has fewer keys.

> ⚠️ **`occurrence_kind` means two different things in two documents.** In
> `status.json` (above) it is the *recurrence type* — `interval`, `daily`,
> `cron`. In the **fire notification** it is the *timing of this
> particular firing* — `on_time`, `late`, `coalesced`, `catch_up_<n>`.
> Same key, different vocabularies; read it against the document you got
> it from.

`timer.json` beside it is the timer's definition (`bellman-timer/1`:
`timer_id`, `name`, `enabled`, `tz`, `occurrence`, `action`, `transport`,
`next_fire_at`, plus `integration.app_name` and `ipc.socket` when they
apply). It carries a `note` field saying what it is: readable, **not
authoritative** — hand edits are ignored and the database wins.

### Step 0 — own the timer

Set `payload.app_name` on the slot `add` request (any of the clients above).
That name becomes the timer's **integration owner**: it is snapshotted onto
every run, pre-filled into every reply stub and fire notification, and it is
the only `app_name` whose replies are accepted. An owner change applies to
the next firing; the run already in flight keeps its snapshot.

A timer created by a human (GUI or `bellman add`) has **no owner**, and
Bellman creates it no reply stub, no fire notification, no pickup deadline —
it never goes `no_ack`; its action simply runs and the result stays in the
event log. There is no null-owner stub anywhere, so two apps watching the
same directory can never both "claim" a timer by writing first. To give a
timer a reply channel, create it through the slot protocol with your
`app_name`.

### Step 1 — notice a fire

When the timer fires, Bellman writes `fires/fire-<run_id>.json` under the
slots root (atomically, after `status.json` and the reply stub exist):

```json
{"schema":"bellman-slot/1","kind":"fired","occurrence_kind":"on_time",
 "timer_id":"…","timer_name":"demo-wake","app_name":"demo-app",
 "run_id":"9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08",
 "scheduled_for":"2026-07-28T08:00:00Z","fired_at":"2026-07-28T08:00:01Z",
 "status_path":"/home/you/.bellman/timers/demo-wake-3f1a/status.json",
 "reply_path":"/home/you/.bellman/timers/demo-wake-3f1a/reply-9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08.json"}
```

- **Accept only your own `app_name`.** Other apps' notifications are not
  your work; two apps may watch the same directory safely.
- `slots/done/slot-<id>.json` is a **request response** (the answer to your
  add/modify/delete), not a fire notification. The two namespaces never
  overlap: fires live only under `fires/`, responses only under `done/`.
- **Scan `fires/` once at startup** and handle whatever is already there
  (a fire can arrive while your app is down), **then watch it**. Filesystem
  watch events are latency hints only — a plain rescan every second or so
  is a complete implementation, because `run_id` deduplication (below)
  makes any rescan or redelivery safe.

### Step 2 — answer

The reply file is **per-run**: take its exact path from the notification's
`reply_path` and open it **verbatim** — Bellman sends an absolute native
path, never `~` or an environment-variable expression, so no sample needs
shell expansion. Never construct or hardcode a filename.

Bellman pre-filled the stub with `schema`, `run_id`, `app_name` and
`state: null`. Read it, set what changed, write it back **atomically**
(temp file + rename onto the same path):

```json
{"schema":"bellman-reply/1","run_id":"9f2c1d77-…","app_name":"demo-app",
 "state":"acknowledged","acknowledged_at":"2026-07-28T08:00:02Z","expected_secs":15}
```

States an app may write: **`acknowledged` → `running` → `completed` |
`failed`** (shorter paths are fine — straight to `completed` is normal).
Never write `fired`, `no_ack` or `cancelled`; those are Bellman's.

| the app sets | when |
|---|---|
| `state` | always — the only required field |
| `acknowledged_at`, `expected_secs` | on pickup; the estimate powers the GUI label and the opt-in watchdog |
| `error_detection` | opt the run into the silence watchdog (below) |
| `heartbeat_at`, `progress` | optional liveness; each new value extends an armed watchdog |
| `completed_at` + `result` | success; `result` is any JSON value (size caps below) |
| `failed_at` + `reason` | the app itself decided it failed |

Only `state` is required. If you compose instead of editing the stub, the
minimal valid reply is `schema` + `run_id` + `app_name` + `state` — copy the
three identity fields from the stub or the notification; `{"state":"…"}`
alone cannot be matched to a run and is rejected. Fields set earlier are
never retracted by a later write that omits them — Bellman accumulates.

### Step 3 — deduplicate by `run_id`

Delivery is **at-least-once**: Bellman may republish the same notification
before your pickup is recorded, and your rescan sees the same file many
times. The same `run_id` seen twice is the same firing — **act once, reply
normally**. This is required of every app and it is cheap: remember the
last `run_id`(s) you handled.

### What Bellman does with your reply

Every accepted state transition is validated, appended to
`logs/events.current.jsonl` under the run's `run_id` (one line per
transition; heartbeats and progress are never logged), and folded into
`status.json` — the mirror that always shows the current truth, including
everything your app has reported so far.

- **Pickup grace: 60 s** (configurable). If by then no valid reply arrived
  and the slot-feed cursor did not ack the run, Bellman records `no_ack`.
  While the run is still current, a late reply **revises** the state —
  `completed` after `no_ack` (or after a watchdog `failed`) moves the state
  to `completed`; the append-only log keeps the whole story. Once the timer
  has fired again, the old run is over: its reply is logged `superseded`
  and not applied.
- **Completion never auto-times out.** An app that acknowledged but never
  reports an ending stays `running` forever — an unfinished run is the
  truth, not a failure. **Nothing auto-completes**; `completed` and
  `failed` are things only your app says.
- **The silence watchdog is opt-in and separate.** Set
  `error_detection: true` together with a positive `expected_secs` and the
  run is held to a deadline of `expected_secs × factor` (factor default
  2.0, configurable), counted on Bellman's clock from receipt of your
  latest distinct reply — every new heartbeat, progress change, state
  advance or new estimate **extends the deadline**. Expiry records
  `failed` / `timed_out` in `status.json` and the log (your reply file is
  left byte-identical — Bellman never puts words in the app's mouth, and
  marking is not killing: your process is never terminated). An explicit
  `error_detection: false` cancels the watchdog. `true` without any
  positive `expected_secs` is rejected.
- **Malformed replies.** A file caught mid-write is re-read after a short
  debounce, never condemned on sight. Stable invalid bytes, a wrong
  `app_name`, an unknown `run_id` or a reserved state are logged
  `reply_rejected` and a copy is quarantined under `timers/bad/`; the live
  file stays in place so your next write can fix it.
- **Size caps.** Whole reply file **64 KB** — over it the body is rejected
  unread. `result` is kept up to **32 KB** in `status.json` and **2 KB** on
  the log event, free text (`progress`, `reason`) **1 KB** — over those,
  values are truncated with `result_truncated: true`, never rejected. For
  big outputs, write the payload to a file your app owns and reply
  `result: {"summary": "…", "path": "/abs/path", "sha256": "…"}` — Bellman
  displays the path as text and never opens it.
- **A reply is data, never a command.** Bellman parses, validates and logs.
  It will never launch, execute, schedule or modify anything because of
  something an app wrote.

### What the GUI shows

Because heartbeats and progress are **never logged**, the live view is the
only place they are ever visible. An integration-owned timer's row in
**All timers** shows its current run as it happens —
`● running · 7s · bulb on, 7s elapsed` — and the row updates as the app
rewrites `progress`. If the run outlives its own `expected_secs` the row
adds `overdue (expected ~10m)` — a **label, not an ending**, computed at
render time as `now − fired_at > expected_secs` (1× the estimate, never
the watchdog's × factor; the run is still `running` and may still
complete). An app that sends no `heartbeat_at` or `progress` is normal:
its row shows `running` and an elapsed time, nothing else — absence never
renders as a placeholder. Current non-terminal runs also pin to the top of
**Run history** ("Happening now"), and every terminal state renders
distinctly: `completed`, `failed · reported`, `failed · timed out`,
`no ack`, `superseded`. A timer without an integration owner shows no live
run state — its `status.json: fired` is a firing snapshot, not an app
claiming to work.

Run it end to end with `testing_apps/lightbulb/`: its README shows the full
loop — fire → acknowledge → bulb visibly on for 15 s → completed →
validated and terminal — against a live Bellman.

## Talking over the local socket (IPC)

Everything above is the **file transport**, and it is the fallback that
always works. An app may instead talk to Bellman over **one local socket** —
a faster folder, not a second protocol. Both transports carry the same
logical messages and schemas (`bellman-slot/1` out, `bellman-reply/1` in),
hit the **same validation**, and produce the **same records** (state, log
lines, `status.json`). The operator and the app choose; Bellman is
indifferent — there is exactly one ingest path behind both adapters.

### Choosing the transport — per firing, never mid-firing

Each timer has `"transport": {"mode": …}` (set `"transport"` on the slot
`add`/`modify` payload, `bellman add --transport`, or `bellman edit
--transport`):

- `json` — files only, today's behaviour, **the default**.
- `ipc` — socket only; if no client is connected at fire time the run goes
  `no_ack` when the pickup grace lapses, exactly like an unwatched folder.
  It never falls back.
- `auto` — if a client holding this timer is connected at fire time, use
  IPC; otherwise files. The choice is made **at fire**, recorded on the run
  (`status.json` shows `transport: json | ipc | ipc_fallback`), and never
  changes mid-firing. The next firing chooses fresh.

Fallback exists **only in `auto`**, and **only before delivery is
confirmed** (confirmation = the first valid reply accepted, normally
`acknowledged`). On an unconfirmed IPC failure — a send error, or the
client disconnecting before confirming — Bellman creates the same run's
reply stub (create-only) and publishes the **same `run_id`** through the
file adapter with its `reply_path`; the run records
`transport: "ipc_fallback"`. No second run is ever minted because a pipe
broke, and silence on a live connection is not a failure: the bounded
retry pump keeps offering IPC until the normal pickup deadline rules apply.
A client that confirms and *then* disconnects is **not** `no_ack` — it
stays `acknowledged`, exactly like a file app that already answered.
Duplicate delivery is possible by construction, so the rule from step 3
applies doubled: **deduplicate by `run_id`** — same `run_id` seen twice is
the same firing, act once, reply normally.

### The socket

- **One socket for all of Bellman** — never a server per timer.
- Linux: `$XDG_RUNTIME_DIR/bellman/bellman.sock`; without `XDG_RUNTIME_DIR`,
  a user-owned private directory below the OS temp dir. macOS: a private
  directory below `$TMPDIR`. Windows: the named pipe
  `\\.\pipe\bellman-<username>` with an ACL restricted to you (the pipe
  name is what goes in `"ipc": {"socket": …}`). The directory is **0700**,
  the socket **0600** — the trust boundary is identical to the file
  protocol's (same-user processes; the OS is the gate, no credentials, no
  secrets).
- Find the path in `timer.json` (`"ipc": {"socket": "<path>"}`) or in any
  fire message. It is **data, not code** — Bellman never writes an
  `adapter.py` or any other generated, importable file into a timer folder;
  the raw protocol below is the whole contract, speakable from any language.
- The server runs while the desktop app runs (`"ipc_enabled": true` in
  `config.json`, the default). With it off, every firing resolves to files.

### The protocol — three frame kinds, newline-delimited JSON

1. **Claim** (client → Bellman, once, first frame):

   ```json
   {"schema":"bellman-claim/1","app_name":"demo-app","timer_id":"<uuid>"}
   ```

   `app_name` must match the timer's explicit integration owner — the same
   rule that rejects a wrong-`app_name` reply file, no first-acker
   ownership. A rejected claim gets
   `{"schema":"bellman-claim/1","ok":false,"error":"…"}` and a closed
   connection; an accepted one gets `{"schema":"bellman-claim/1","ok":true,…}`.

2. **Fire** (Bellman → client): the same `bellman-slot/1` message as the
   fire file, **without** `reply_path` (there is deliberately no stub for
   an IPC firing — the folder README explains: "this run spoke over IPC;
   `status.json` is still the truth"), with `"ipc": {"socket": …}` present:

   ```json
   {"schema":"bellman-slot/1","kind":"fired","run_id":"9f2c1d77-…",
    "timer_id":"…","timer_name":"demo-wake","app_name":"demo-app",
    "scheduled_for":"…","fired_at":"…",
    "status_path":"/…/status.json","ipc":{"socket":"/run/user/1000/bellman/bellman.sock"}}
   ```

   `status.json` is written **always**, on both transports. After `no_ack`
   periodic sends stop, but claiming later triggers **one replay** of the
   still-current explicit-`ipc` run — confirming it revises `no_ack` to
   `acknowledged`, exactly like late file pickup.

3. **Reply** (client → Bellman): the same `bellman-reply/1` document you
   would write into the reply file, as **one line**:

   ```json
   {"schema":"bellman-reply/1","run_id":"9f2c1d77-…","app_name":"demo-app","state":"acknowledged","expected_secs":15}
   ```

   Same states, same accumulation, same validation, same rejections
   (`reply_rejected` in the log), same caps: a frame is at most **64 KB**;
   a peer that sends more without a newline is disconnected, and every
   other client is unaffected. Heartbeats over IPC obey the file rule:
   live view only, never the log.

### A minimal IPC client (Python, stdlib)

```python
import json, socket, sys, time

sock_path, app_name, timer_id = sys.argv[1], "demo-app", sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock_path)
f = s.makefile("rw")
f.write(json.dumps({"schema": "bellman-claim/1", "app_name": app_name,
                    "timer_id": timer_id}) + "\n"); f.flush()
assert json.loads(f.readline())["ok"], "claim rejected"
seen = set()
for line in f:
    fire = json.loads(line)
    if fire["run_id"] in seen:
        continue                      # same run_id = same firing; act once
    seen.add(fire["run_id"])
    # … do the work …
    f.write(json.dumps({"schema": "bellman-reply/1", "run_id": fire["run_id"],
                        "app_name": app_name, "state": "completed",
                        "completed_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                        "result": {"ok": True}}) + "\n"); f.flush()
```
