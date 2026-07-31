> ARCHIVED 2026-07-31 — shipped; card bellman-ik4-lightbulb-app-connect-your-own-app-docs, run 2026-07-31_0002, merge 056e554.

# IK4 — The lightbulb app + "connect your own application"

Repo: `~/bellman`. Depends on **IK1, IK2, IK3 and SCH1**. This card proves the finished thing
and documents it; it must not build any of it.

## The lightbulb

A deliberately tiny app whose only job is to demonstrate the loop end to end, and to be the
example a third party copies.

1. A timer fires. Bellman writes `status.json` with `state: "fired"`.
2. The lightbulb accepts only notifications with its configured `app_name`, then writes the
   reply file at that notification's `reply_path` — `acknowledged`,
   `app_name: "lightbulb"`, `expected_secs: 15`.
3. The bulb turns **on** — visibly, on screen — and stays on for **15 seconds**.
4. The app overwrites its reply file — `completed`, with the measured on-duration.
5. Bellman validates, folds it into `status.json` and records the run terminal.

Keep it small and readable. It is documentation that happens to compile, and it must be
genuinely observable: a human watching the screen sees the bulb light and go out.

## The client must stay tiny

The whole point of one-writer-per-file is that integrating is trivial. If the lightbulb needs
more than roughly this, something in IK3 is wrong and should be fixed rather than papered over
here:

```python
if fire["app_name"] != APP_NAME:
    return
reply_path = fire["reply_path"]            # from the fire notification — never construct it
r = json.load(open(reply_path))            # schema, run_id, app_name already filled in
do_the_work()
r["state"] = "completed"
r["completed_at"] = now_utc()
tmp = reply_path + ".tmp"
json.dump(r, open(tmp, "w")); os.replace(tmp, reply_path)
```

One read, one atomic write, one file. The app never opens `status.json`, never composes a
document from scratch, and never copies `run_id` or writes identity fields into the reply —
Bellman pre-filled them. It knows only its configured `APP_NAME`, used to ignore other apps'
notifications. **The path comes from the notification's `reply_path`** (the filename is
per-run, IK3); a client with `reply.json` hardcoded is wrong under this protocol and the docs
must never show one.

## Documentation

A **"connect your own application"** section in `docs/INTEGRATION.md`:

- where the timer's folder is and what the three files are
- configure one integration `app_name` before enabling replies; an unowned human timer has
  no reply stub/notification and there is no first-responder ownership race
- how to notice a fire under `slots/fires/`, and that `slots/done/slot-<id>.json` is a
  request response, not a fire notification
- scan `slots/fires/` once at startup, then watch it; filesystem watch events are latency
  hints and `run_id` deduplication makes a rescan/redelivery safe
- what to write into the reply file, and the states an app may use
- the 60s pickup grace, that completion never auto-times out, and that an app may separately
  opt into the silence watchdog
- `error_detection` + `expected_secs`, and that a heartbeat extends the deadline
- what happens to a malformed reply
- that a late reply revises the state
- the size caps (R12), and the big-output convention: store the payload in a file the app
  owns, reply with `result: { summary, path, sha256 }` — Bellman displays the path as text
  and never opens it
- **deduplicate by `run_id`**: the same `run_id` seen twice is the same firing — act once,
  reply normally. Required of every app (IK6's transport fallback can deliver a fire twice),
  and cheap: remember the last `run_id` handled
- the reply file is per-run — take its exact path from the notification's `reply_path`,
  never construct or hardcode a filename; Bellman sends an absolute native path, not `~` or
  an environment-variable expression

Match the existing copy-paste style — short clients a reader lifts whole. Update the four
existing clients (Python, bash, PowerShell, Node) to cover the reply direction.

## Exit gate

- Full round trip observed live: fire → acknowledge → bulb visibly on for 15s → completed →
  validated and terminal. Every transition in `events.current.jsonl`.
- The lightbulb's reply logic is under ~10 lines in each documented language.
- Each client opens Bellman's absolute `reply_path` verbatim on its platform; no sample relies
  on shell tilde/environment expansion.
- **Someone (or something) that has not seen the code follows `INTEGRATION.md` and their
  client works.** That is the real test of this card — not that the bulb lights.
- A slot request response and a fire notification arrive concurrently; the lightbulb reads
  each from its own namespace and neither file replaces the other.
- Two different apps watch `slots/fires/`; only the configured `app_name` handles this timer,
  and Bellman never creates a null-owner stub both could legitimately write.
- The docs state the honest limits: completion never auto-times out, the opt-in silence
  watchdog is separate, nothing auto-completes, and a reply is data and never a command.
