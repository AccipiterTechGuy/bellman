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
(Linux: `~/.local/share/io.bellman.desktop`, macOS:
`~/Library/Application Support/io.bellman.desktop`, Windows:
`%APPDATA%\io.bellman.desktop`) — pass the matching `--db` and `--slots`
paths everywhere.

> **Use the directory your Bellman actually drives.** The two are separate
> stores. If the desktop app is what is running, `~/.bellman` is not the live
> one and a timer created there will never fire — substitute the desktop
> app's directory (`~/.local/share/io.bellman.desktop` on Linux,
> `~/Library/Application Support/io.bellman.desktop` on macOS) and pass both
> `--slots <dir>/slots` and `--db <dir>/timers.db` to `bellman slot-submit`.
> The GUI shows its own path under Settings → Data.

1. **Create the timer, owned by the app name `lightbulb`** (a one-shot
   `slot-submit`; the `app_name` on the add request becomes the integration
   owner — without an owner Bellman creates no reply channel):

   ```bash
   cat > /tmp/lightbulb-req.json <<EOF
   {"schema":"bellman-slot/1","request_id":"$(uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid)",
    "operation":"add",
    "payload":{"app_name":"lightbulb","timer_name":"lightbulb-demo","tz":"UTC",
    "occurrence":{"kind":"interval","every_secs":120}}}
   EOF
   bellman slot-submit /tmp/lightbulb-req.json --slots ~/.bellman/slots --db ~/.bellman/timers.db
   ```

   Two minutes is short enough that the **clock** shows you the loop; change
   `every_secs` to whatever suits once you have seen it once.

2. **Start the lightbulb** (leave it running in a terminal you can see):

   ```bash
   ./lightbulb.py --slots ~/.bellman/slots
   ```

3. **Wait for the interval.** Within two minutes the timer comes round and
   Bellman wakes the app — that is the whole point, so let the clock do it.

   If you would rather not wait, `bellman run-now lightbulb-demo` fires the
   same path by hand. It proves the app answers; it does **not** prove the
   scheduler works.

Watch the terminal: the bulb lights for 15 seconds and goes out. In the
meantime, in another terminal, watch the truth change:

```bash
# On macOS (where watch is not pre-installed):
# while :; do clear; cat ~/.bellman/timers/lightbulb-demo-*/status.json; sleep 1; done
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
