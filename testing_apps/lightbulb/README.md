# The lightbulb — Bellman's smallest integrated app

`lightbulb.py` is the reference integration a third party copies: a
deliberately tiny app (stdlib-only Python) whose only job is to demonstrate
the full loop — a timer fires, the app acknowledges, the bulb lights
**visibly in the terminal** for 15 seconds, and the app reports completion
through the reply file. See `docs/INTEGRATION.md` → *Connect your own
application* for the protocol this implements.

*(Looking for a graphical demo to watch rather than code to copy? See [`testing_apps/lightbulb_gui/`](../lightbulb_gui/).)*

## Run the demo

You need a Bellman with the scheduler running (the desktop app, or any
process driving the same store). In the examples below the data dir is the
CLI default `~/.bellman`; for the desktop app use its app-data dir instead
(Linux: `~/.local/share/io.bellman.desktop`) — pass the matching `--db` /
`--slots` paths everywhere.

1. **Create the timer, owned by the app name `lightbulb`** (a one-shot
   `slot-submit`; the `app_name` on the add request becomes the integration
   owner — without an owner Bellman creates no reply channel):

   ```bash
   cat > /tmp/lightbulb-req.json <<EOF
   {"schema":"bellman-slot/1","request_id":"$(cat /proc/sys/kernel/random/uuid)",
    "operation":"add",
    "payload":{"app_name":"lightbulb","timer_name":"lightbulb-demo","tz":"UTC",
    "occurrence":{"kind":"interval","every_secs":3600}}}
   EOF
   bellman slot-submit /tmp/lightbulb-req.json --slots ~/.bellman/slots
   ```

2. **Start the lightbulb** (leave it running in a terminal you can see):

   ```bash
   ./lightbulb.py --slots ~/.bellman/slots
   ```

3. **Fire the timer now** (or wait for the interval):

   ```bash
   bellman run-now lightbulb-demo
   ```

Watch the terminal: the bulb lights for 15 seconds and goes out. In the
meantime, in another terminal, watch the truth change:

```bash
watch -n1 cat ~/.bellman/timers/lightbulb-demo-*/status.json
grep lightbulb-demo ~/.bellman/logs/events.current.jsonl | tail
```

You will see `fired` → `acknowledged` → `completed`, each folded into
`status.json` and appended to the event log under one `run_id`.

## What the app knows

Only its configured `APP_NAME` (to ignore other apps' notifications) and
the slots root it watches. The reply path arrives **inside the fire
notification** as an absolute native path — the app never constructs a
filename. The reply logic is the six-line `reply()` function: read the
pre-filled stub, set what changed, temp-write + `os.replace` back onto the
same path.
