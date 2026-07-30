# IK4 — The lightbulb app + "connect your own application"

Repo: `~/bellman`. Depends on **IK1, IK2, IK3**. This card proves the finished thing and
documents it; it must not build any of it.

## The lightbulb

A deliberately tiny app whose only job is to demonstrate the loop end to end, and to be the
example a third party copies.

1. A timer fires. Bellman writes `status.json` with `state: "fired"`.
2. The app writes `reply.json` — `acknowledged`, `app_name: "lightbulb"`, `expected_secs: 15`.
3. The bulb turns **on** — visibly, on screen — and stays on for **15 seconds**.
4. The app overwrites `reply.json` — `completed`, with the measured on-duration.
5. Bellman validates, folds it into `status.json` and closes the run.

Keep it small and readable. It is documentation that happens to compile, and it must be
genuinely observable: a human watching the screen sees the bulb light and go out.

## The client must stay tiny

The whole point of one-writer-per-file is that integrating is trivial. If the lightbulb needs
more than roughly this, something in IK3 is wrong and should be fixed rather than papered over
here:

```python
r = json.load(open(f"{d}/reply.json"))     # schema, run_id, app_name already filled in
do_the_work()
r["state"] = "completed"
r["completed_at"] = now_utc()
tmp = f"{d}/.reply.{uuid.uuid4()}"
json.dump(r, open(tmp, "w")); os.replace(tmp, f"{d}/reply.json")
```

One read, one atomic write, one file. The app never opens `status.json`, never composes a
document from scratch, and never carries `run_id` or `app_name` in its own code — Bellman
pre-filled them.

## Documentation

A **"connect your own application"** section in `docs/INTEGRATION.md`:

- where the timer's folder is and what the three files are
- how to notice a fire
- what to write into `reply.json`, and the states an app may use
- the 60s pickup grace, and that completion has no timeout unless you opt in
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
  never construct or hardcode a filename

Match the existing copy-paste style — short clients a reader lifts whole. Update the four
existing clients (Python, bash, PowerShell, Node) to cover the reply direction.

## Exit gate

- Full round trip observed live: fire → acknowledge → bulb visibly on for 15s → completed →
  validated and closed. Every transition in `events.current.jsonl`.
- The lightbulb's reply logic is under ~10 lines in each documented language.
- **Someone (or something) that has not seen the code follows `INTEGRATION.md` and their
  client works.** That is the real test of this card — not that the bulb lights.
- The docs state the honest limits: no completion timeout by default, nothing auto-completes,
  a reply is data and never a command.
