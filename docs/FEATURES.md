# What Bellman does

Back to the [README](../README.md).

## Timers

- Give a timer a name, a time and how often it repeats: once, every N seconds
  (interval), daily, weekly, monthly, yearly, or a cron expression.
- Accurate to the second, with daylight-saving changes and short months handled
  for you.
- When a timer fires it can **start a program**, **show a desktop
  notification**, or **wake an app** that is connected to Bellman.
- Missed a run because the computer was off or asleep? Each timer decides what
  happens when it is back: catch up on every missed run, run once for all of
  them, or skip them.

## Two ways to use it

- **Desktop app:** a list of all timers with their next run, a week view, a
  month calendar and a run history, plus a menu-bar / tray icon. A first-run
  wizard sets it up, including start at login.
- **`bellman` command line:** add, list, edit, pause, resume, run now and
  delete timers, and preview their next runs. Every command can answer in JSON,
  so scripts and AI agents can drive it. See [CLI.md](CLI.md).

## Connect your own apps

- Any program, in any language, can create, change or delete its own timers by
  writing one JSON file. No SDK or library needed.
- When its timer fires, the app is told, and reports back: acknowledged,
  progress, completed or failed, over JSON files or a local socket.
- An optional watchdog notices when an app goes quiet for longer than it said
  it would.

How to do it: [Connect your own application](INTEGRATION.md#connect-your-own-application),
plus the [demo apps](../testing_apps/README.md).

## History

- Every firing and every reply goes into an event log. Old logs are compressed
  and cleaned up automatically (kept 30 days by default).
- Each timer also has a plain folder showing its current state, so you can see
  what happened without opening the app.

## Extra tools

- `bellman calendar` and `bellman agenda` draw a month or a day of timers as an
  image (SVG or PNG) or JSON.
- `bellman scan` lists every scheduled job on the computer (cron, systemd
  timers, `at` and Bellman); `bellman task` explains, enables, disables, edits
  or runs one, with a dry run first. Linux only for now.
- **Wake from sleep** (Linux and Windows): Bellman can ask the computer to wake
  up for a timer. Not available on macOS yet.

Settings are described in [CONFIG.md](CONFIG.md); where your timers and logs
are stored in [LOCAL.md](LOCAL.md).
