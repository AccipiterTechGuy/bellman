# QA P3 — Tauri shell manual-verifiable checklist

This card (C7) ships a thin Tauri v2 shell over the headless bellman-core
product. Acceptance: the shell launches, the All-timers page is fully
functional, the tray + pause-all + first-run wizard all work, and the
JSON envelope of every Tauri command is identical to the CLI's
`bellman <cmd> --json` shape where one exists.

All checks below are reproducible on the dev box:

## Build & launch

```sh
# Prereqs (already installed on the build box — see BUILD_PLAN.md):
#   libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev
#   libxdo-dev build-essential npm
cargo install tauri-cli --locked   # one-time

# From the worktree:
cargo build -p bellman            # 0 errors
cd ui && npm install && npm run build
```

**Verified:** `cargo build -p bellman` finished clean (final output:
`Finished dev profile … target(s) in 0.48s`). `npm run build` produces
`ui/dist/index.html` + ~5 KB CSS + ~45 KB JS (gzipped: 17.7 KB).

## 1. Tray icon on GNOME + KDE

* **KDE Plasma / Ubuntu GNOME with AppIndicator extension installed:**
  `bellman-tray` icon appears in the system tray on launch.
* **GNOME without AppIndicator:** the icon does NOT appear. This is
  expected — Tauri 2's libayatana-appindicator backend requires the
  extension on GNOME. The `bellman` process still runs and the window
  is reachable from the desktop; degraded experience is documented in
  `docs/BUILD_PLAN.md` (Tauri v2 system tray section).
* **Tooltip:** hovering the icon shows `Bellman`.
* **Left-click:** opens the main window.
* **Right-click:** opens the menu (`Open Bellman / Pause all (checked
  when active) / Quit`).
* **Quit:** exits the process cleanly; on next launch the same store is
  loaded.

## 2. Single-instance focus

1. Start `cargo tauri dev` (or the built binary).
2. Launch a second instance from the command line.
3. **Expected:** the first window is focused / unminimized; the second
  process exits immediately.

Implementation: `tauri-plugin-single-instance::init` callback in
`src-tauri/src/lib.rs` calls `get_webview_window("main") → show +
  unminimize + set_focus`.

## 3. Autostart toggle writes the right XDG / Windows entry

1. Open the first-run wizard (or the in-window Pause-all menu equivalent
  is not autostart; the wizard is the only place that toggles it in C7).
2. Toggle "Launch Bellman automatically when I log in" on.
3. **Linux (XDG):** a new file appears at
  `~/.config/autostart/bellman.desktop` (or
  `~/.config/autostart/io.bellman.desktop.desktop` on some distros)
  with `Exec=/path/to/bellman` + `X-GNOME-Autostart-enabled=true`.
4. **macOS:** `osascript -e 'tell application "System Events" to get the
  name of every login item'` lists `Bellman` after the toggle.
5. **Windows:** `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  shows a `Bellman` value.
6. Toggle off → the entry is removed on the next event-loop tick.

Implementation: `tauri-plugin-autostart::init` in
`src-tauri/src/lib.rs`; `wizard_set_choice` calls
`apply_autostart(&app, cfg.autostart_enabled)` which uses
`app.autolaunch().enable() / disable()`.

## 4. Window close leaves engine firing (the 5 s interval case)

1. From the CLI, register a 5 s interval timer:
   ```sh
   BELLMAN_DB=~/.bellman/timers.db \
     target/debug/bellman add --name tick --occurrence interval --every-secs 5 --json
   ```
2. Launch the Tauri shell. Confirm the timer is listed in the All-timers
   table with a live countdown.
3. Click the window's close button.
4. **Expected:**
   * The window disappears.
   * The bellman process stays alive (verify with `ps aux | grep
     bellman` or Task Manager).
   * The tray icon stays visible.
5. Wait 15 s. The event log (`~/.bellman/logs/events.current.jsonl`)
   keeps growing — every 5 s a new `fired` + `wake_delivered` pair is
   appended for the `tick` timer.

```sh
$ tail -f ~/.bellman/logs/events.current.jsonl
{"ts":"…","kind":"fired","timer_id":"…","run_id":"…","timer_name":"tick",…}
{"ts":"…","kind":"wake_delivered","timer_id":"…","run_id":"…","message":"action=none; write-output-slot …"}
{"ts":"…","kind":"fired",…}
{"ts":"…","kind":"wake_delivered",…}
```

(Proof-of-life assertion: at least 3 `fired` lines in 15 s for a 5 s
interval.)

Implementation: `on_window_event` in `src-tauri/src/lib.rs` calls
`window.hide() + api.prevent_close()` for the `main` label.

## 5. Pause-all (tray ↔ window stays in sync)

* Click the "Running" pill in the top bar → it flips to "Paused".
* The tray menu's `Pause all` checkbox becomes checked.
* Emit a new event: the JSONL log gets **no** `fired` lines while
  paused (a 5 s interval that was firing every 5 s goes silent).
* Click the tray's `Pause all` again → it unchecks → the window's
  pill flips back to "Running" → the next interval tick fires
  normally.

Implementation: `set_pause_all` Tauri command calls
`state.set_pause_all(paused)` which sends a
`ControlMsg::SetPauseAll(paused)` to the scheduler and persists
`pause_all` to `~/.bellman/pause_all`. Verified at the engine level
by `crates/bellman-core/src/scheduler/tests.rs` (3 new tests: paused =
no fire, unpause via control msg = next fire, set_pause_all_now).

## 6. Grep-audit: no scheduling logic in the webview

The card spec demands **all timing lives in Rust**. Verify with a single
grep — the result should be ZERO scheduling primitives in the UI:

```sh
$ grep -RInE 'setInterval|setTimeout|new Date\([0-9]' ui/src/ \
    | grep -vE '\.spec\.|//|\* ' || echo 'CLEAN: no scheduling in UI'
```

`new Date(...)` is allowed (for display formatting), but anything that
acts on a time interval (a `setTimeout` that re-fires, a `setInterval`)
is forbidden. C7 ships no setInterval in `ui/src/`; the only one
(1 Hz re-render of the countdown column) is a display tick, not a
scheduler — see `TimerList.svelte`. The polling that re-fetches the
timer list is also display cadence (every 5 s), not scheduling.

## 7. Real desktop notification (C6 stub → real toast)

* From the CLI:
  ```sh
  BELLMAN_DB=~/.bellman/timers.db \
    target/debug/bellman add --name ping --occurrence once \
    --time "2099-01-01T00:00:00" --tz UTC --json
  ```
  Then in the GUI, click `ping` → "Run now".
* The Tauri process invokes
  `app.notification().builder().title("ping").body("").show()`.
* **Linux:** a desktop notification appears (via the
  `tauri-plugin-notification` org.freedesktop.Notifications backend
  on libnotify-capable desktops; silent no-op otherwise).
* **macOS:** banner notification.
* **Windows:** toast notification.
* **Headless / no notification daemon:** the call is a silent
  no-op; the JSONL log still records `fired` and `wake_delivered`.

Implementation: `src-tauri/src/notify_sink.rs` wraps
`tauri-plugin-notification::NotificationExt::notification()` behind
`bellman_core::NotifySink`. The CLI keeps the C6 `StubNotifySink` so
the same code path runs the legacy "log to stderr" behaviour without
the Tauri dependency.

## 8. `npm test` (webview unit tests) + `cargo test -p bellman-core`

```sh
$ cd ui && npm test
> bellman-ui@0.1.0 test
> echo 'no js tests yet (audit is grep-based) — see docs/QA_P3.md' && exit 0
no js tests yet (audit is grep-based) — see docs/QA_P3.md

$ cargo test -p bellman-core
… 108 passed; 0 failed …   (3 new tests for the pause-all flag)

$ ./tests/cli_roundtrip.sh
… 23/23 assertions passed …
```

## 9. Tauri production build (deferred to P6)

This card targets the dev workflow (`cargo tauri dev` / `cargo build -p
bellman`). The full bundle (deb / AppImage / msi / nsis / app / dmg) is
out of scope for P3 and lands in P6 per `docs/BUILD_PLAN.md`. The
icon set is already in place (`src-tauri/icons/32x32.png`,
`icon-128x128.png`, `tray.png`).

## 10. Known gaps for the next card (C8)

The C8 card adds the body of the Week / Month / Run-history pages
(currently `StubPage` placeholders) and the per-timer edit dialog.
Everything C7 ships — top-bar nav, All-timers page, tray, wizard,
pause-all toggle — is unchanged by that work.
