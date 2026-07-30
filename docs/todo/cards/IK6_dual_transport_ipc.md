# IK6 — Dual transport: local IPC + JSON files, chosen per firing

Repo: `~/bellman`. Depends on **IK3** and **SCH1** shipped and **IK4** proven — the file
protocol and its durable publication path must be real and documented before a second
transport exists, because the file protocol is the fallback and the fallback must be the
thing that already works. Design:
`docs/todo/json_normalization.md` (all rules apply unchanged).

## Goal

An app may talk to Bellman over a local socket instead of files. **Both transports carry the
same logical messages and schema (with file-only `reply_path` optional), hit the same
validation, and produce the same records.** The operator and the app choose; Bellman is
indifferent.

```
                    ┌─ IPC adapter (socket / named pipe)
timer engine ───────┤                                      ──▶ ONE ingest path
                    └─ JSON folder adapter (IK3, unchanged)
```

## The adapter boundary — the actual point of the card

One internal seam: `deliver_fire(FireMessage)` out, `RunReply` in. `FireMessage.reply_path`
is required for file delivery and absent for IPC-only delivery; every other logical field is
the same. Everything downstream of that seam — validation, transition rules, watchdog,
folding into `status.json`, event log — neither knows nor cares which transport produced the
reply. **There is exactly one ingest function**, and both adapters call it. Two validation
paths is the failure mode of this card.

Everything already decided applies verbatim on both transports: R5 vocabulary, R7 grace
asymmetry, R8 watchdog on Bellman's monotonic clock, R9 reply-is-data, R12 size caps,
heartbeats never logged, terminal-revision rules, `superseded` on stale runs.

## Transport choice — per firing, never mid-firing

Per timer:

```json
"transport": { "mode": "auto" }        // auto | json | ipc
```

- `json` — files only, today's behaviour, the default.
- `ipc` — socket only; if no client is connected at fire time the run goes `no_ack` when
  grace lapses, exactly like an unwatched folder.
- `auto` — if a client holding this timer is connected at fire time, use IPC; otherwise
  files. **The choice is made at fire, recorded on the run, and never changes mid-firing.**
  The next firing chooses fresh.

**Selected mode vs delivery attempts — these are different things, and conflating them made
an earlier draft of this card contradict itself.** What is fixed per firing is the *selected
mode*. Within `auto`, delivery may still fall back:

- Fallback exists **only in `auto`**, and **only before delivery is confirmed**. Confirmation
  is the first valid `RunReply` accepted by the shared ingest path (normally
  `state: "acknowledged"`); a successful socket write alone proves nothing. After
  confirmation, the transport is settled for the firing and R7 pickup is satisfied. A client
  that confirms and then disconnects is **not** `no_ack`: it remains `acknowledged`
  indefinitely unless a prior reply explicitly opted into the silence watchdog. It is not a
  fallback case.
- On unconfirmed IPC failure: create the same run's file stub create-only, then publish the
  **same `run_id`** through the file adapter with its `reply_path`; record
  `transport: "ipc_fallback"` on the run. The file message is not eligible before that stub
  exists. Never mint a second run because a pipe broke.
- "Failure" here is an observed send error or disconnect before confirmation. Silence on an
  otherwise live connection is not proof of transport failure: retry IPC on the bounded pump
  schedule and let the normal pickup deadline reach `no_ack`; do not invent an independent
  fallback timer.
- Explicit `ipc` mode never falls back — no client, no confirmation ⇒ `no_ack`, as stated.
- Duplicate delivery is possible by construction (send confirmed lost ≠ send lost), so
  **apps must deduplicate by `run_id`** — this is now a stated requirement in IK4's
  integration docs, not an unstated assumption: same `run_id` seen twice = same firing, act
  once, reply normally.

The selected transport, immutable logical fire fields, deterministic per-adapter encoding and
confirmation state extend SCH1's existing transport-projection row; they do not create
another run ledger. The IPC encoding omits `reply_path`; a fallback encoding adds only the
create-only stub's exact path, never changes the identity or timing fields. IPC
send/confirmation uses a bounded nonblocking attempt on the fire-producer side. An
unconfirmed row remains durable for the publication pump: `auto` may project the same `run_id` into
`slots/fires/fire-<run_id>.json`, while explicit `ipc` retries IPC until R7 records pickup or
`no_ack`. After `no_ack`, periodic sends stop, but a later client claim triggers one replay
for the still-current explicit-IPC run; confirmation revises it to `acknowledged`, exactly
like late file pickup. A slow or wedged socket never queues the fire record behind action
work.

## NO generated `adapter.py` — connection info is data, not code

The proposal wanted Bellman to generate an `adapter.py` in every timer folder. Rejected, and
this must not creep back in another shape:

- **It is config wearing a code extension.** The file would contain a timer id, a socket
  path and a folder path — three strings. Strings belong in JSON, where they cannot execute.
- **Anything importable is an injection point.** The folder is user-writable by design; a
  `.py` that apps import means whatever edits that file runs inside every app that imports
  it. Bellman never executing it does not help — the *apps* execute it.
- **It is Python-only.** The file protocol works from bash, Node, Rust, anything. A `.py`
  connector demotes every other language to second class.
- **It duplicates per folder** what must live in one place — the proposal itself says the
  real logic belongs in one client package, which concedes the per-folder file is filler.

Instead: the fire notification and `timer.json` gain `"ipc": { "socket": "<path>" }` when
IPC is enabled. Data, not code. A separate `bellman-client` Python package is a **later,
optional** convenience card — the raw protocol must be documented well enough that any
language speaks it without a library, exactly like the file protocol.

## The socket

- One socket for all of Bellman — never a server per timer.
- Unix domain socket in a private **0700** directory, socket **0600**: on Linux prefer
  `$XDG_RUNTIME_DIR/bellman/bellman.sock`; on macOS use a private directory below
  `$TMPDIR` because `XDG_RUNTIME_DIR` is normally absent. If the preferred variable is
  missing, create an OS temp-directory fallback owned by the current user rather than
  placing a world-visible socket directly in `/tmp`. Windows uses a named pipe with an ACL
  restricted to the current user.
- Hold a server-instance lock before bind. Remove a stale Unix path only after `lstat`
  confirms it is a socket owned by the current user and a connection attempt proves no live
  server; never unlink an arbitrary file/symlink at the configured path.
- That makes the trust boundary **identical to the file protocol's**: same-user processes.
  No credentials, no secrets in any file — the OS is the gate, same as for the folders.
- Framing: newline-delimited JSON, same shapes as the files (`bellman-slot/1` out,
  `bellman-reply/1` in). The socket is a faster folder, not a second protocol.
- Enforce R12 while framing, before parsing: buffer at most 64 KB for one reply. A peer that
  sends more without a newline is rejected/disconnected without Bellman retaining the
  unbounded prefix. The next frame or another client must still be serviceable.

**Claiming a timer**: a client sends `{ "app_name": ..., "timer_id": ... }` on connect.
The claim is validated like a file reply — `app_name` must match the timer's explicit
integration owner. There is no first-acker ownership over IPC either. A local process
claiming someone else's timer gets its claim/replies rejected by the same rule the file path
already enforces. At fire, ownership is snapshotted on the run exactly as IK3 requires; a
timer owner change affects the next firing/claim and cannot invalidate a current run.
Nothing new to invent; the point is that nothing new *may* be invented.

## Files in IPC mode

- `status.json` is written **always**, both transports — the mirror is transport-independent,
  and the human browsing the folder must not care how the app talked.
- The `reply.json` stub is written only for firings that selected the file transport. The
  IK2 README explains a folder without one: "this run spoke over IPC; `status.json` is still
  the truth." Its IPC `FireMessage` therefore omits `reply_path`; it never advertises a file
  Bellman deliberately did not create.
- Heartbeats over IPC obey the same rule as heartbeats over files: live view only, never the
  log, R12 caps applied at ingest.

## Exit gate

- One ingest function, asserted structurally: the same reply bytes produce identical state,
  log lines and `status.json` whether they arrived by socket or by file.
- Selected mode and effective delivery are recorded separately on every run: `auto` may
  record an IPC-to-file fallback, but the selected mode never changes mid-firing.
- Kill the IPC client mid-run → watchdog / `no_ack` / aging rules behave exactly as for a
  file app with the same pickup history. Assert both sides: kill before confirmation ⇒
  `no_ack` when grace lapses; kill after confirmation ⇒ remains `acknowledged`, never
  `no_ack`, unless a later valid reply changes it.
- IPC delivery failure falls back to files with the **same `run_id`**, `ipc_fallback`
  recorded, and no duplicate run exists anywhere.
- A second local process claiming an owned timer's `app_name` over the socket is rejected by
  the same validation as a wrong-name file reply — one rule, asserted on both paths.
- A JSON-only app (IK4's lightbulb, unmodified) still passes its full exit gate with IPC
  enabled globally.
- `status.json` present and correct for an IPC run; no `reply.json` stub exists for it, and
  the README explains why. Its fire message has no `reply_path`; the JSON fallback for the
  same `run_id` creates the stub before adding that field to the fallback message.
- R12 caps enforced at the socket: an oversize IPC reply is truncated/rejected by the same
  numbers as an oversize file. A stream exceeding 64 KB without a newline is disconnected
  with bounded memory and does not block another client.
- Platform path tests assert private directory/socket permissions (or Windows ACL), macOS
  operation without `XDG_RUNTIME_DIR`, refusal to unlink a non-socket/symlink, and safe
  recovery of a genuinely stale owned socket.
- No `adapter.py` or any generated executable exists in any timer folder — asserted, so it
  cannot quietly return.
