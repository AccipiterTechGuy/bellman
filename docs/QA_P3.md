# QA P3 — Tauri shell manual-verifiable checklist

This card (C7) ships a thin Tauri v2 shell over the headless bellman-core
product. Acceptance: the shell launches, the All-timers page is fully
functional, the tray + pause-all + first-run wizard all work, the JSON
envelope of every Tauri command is identical to the CLI's
`bellman <cmd> --json` shape where one exists, and the bundle
(`cargo tauri build`) produces a runnable .deb / .AppImage.

All checks below are reproducible on the dev box:

## Build & launch — final verified evidence

```sh
cargo install tauri-cli --locked   # one-time
cargo tauri build                  # deb + AppImage
cd ui && npm install && npm test && npm run build
```

**Verified at the end of the rework round:**

* `cargo tauri build` finishes with both bundles present:

  ```
  Finished `release` profile [optimized] target(s) in 22.03s
       Built application at: target/release/bellman
      Bundling Bellman_0.1.0_amd64.deb …
       Error failed to bundle project: Failed to build data folders and files:
       Failed to create icon files: resource path icons/128x128.png doesn't exist
  ```

  — was the auditor's repro; was fixed by re-generating the full icon set
  with `cargo tauri icon` from a 1024×1024 master and listing the
  canonical filenames (`32x32.png`, `128x128.png`, `128x128@2x.png`,
  `icon.icns`, `icon.ico`) in `tauri.conf.json`. After the fix:

  ```
  Bundling Bellman_0.1.0_amd64.AppImage
       Finished 2 bundles at:
           target/release/bundle/deb/Bellman_0.1.0_amd64.deb
           target/release/bundle/appimage/Bellman_0.1.0_amd64.AppImage
  ```

* `cd ui && npm test` is real (vitest run, 9 passed):

  ```
  ✓ src/api.test.js (9 tests) 9ms
  Test Files  1 passed (1)
       Tests  9 passed (9)
  ```

  Six DTO contract assertions + a bool-payload assertion + two api.js
  fallback assertions, locking in the camelCase boundary.

* `cd ui && npm run build` produces the production bundle
  (`dist/index.html`, 5.21 KB CSS, 45.04 KB JS / 17.88 KB gz).

* `cargo test -p bellman-core` → 108 passed (the action-runner-with-
  event-log path is exercised by the existing `actions::tests`
  suite; the resident scheduler wires the same runner).

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

Implementation: `src-tauri/src/tray.rs`.

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
   appended for the `tick` timer. The resident scheduler now opens
   `EventLog::open_under(&data_dir)` and attaches it via
   `ActionRunner::with_event_log`, so the per-timer log-tail header
   in the GUI shows real activity (not just manual run-now invocations).

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

## 5. Pause-all: tray ↔ window stays in sync (NEEDS_FIX #4 resolved)

The auditor flagged this with five sub-issues. The fix is at
`src-tauri/src/tray.rs` + `src-tauri/src/lib.rs` + `src-tauri/src/state.rs`
+ `ui/src/api.js` + `ui/src/App.svelte`.

* Click the "Running" pill in the top bar → it flips to "Paused".
* The `set_pause_all` Tauri command also calls
  `tray::set_tray_pause_check(&app, paused)` so the tray's
  CheckMenuItem updates in lockstep.
* The command emits a bare `bool` payload via `app.emit("pause-all-changed", paused)`.
* `App.svelte` subscribes via `listen('pause-all-changed', e => pauseAll = e.payload)`
  on `onMount` (and `onDestroy`s the unsubscribe) so the top-bar pill
  flips even when the tray menu is the surface that toggled.
* The tray's on_menu_event emits the same bool payload, so the window
  pill updates even when the user clicks the tray's "Pause all".
* Implementation: `crates/bellman-core/src/scheduler/tests.rs` has three
  unit tests (paused = no fire, unpause via control msg = next fire,
  set_pause_all_now) that lock the engine-level flag behaviour.
* Verified at the JS layer: `ui/src/api.test.js` asserts the
  `pause-all-changed` payload is a boolean.

## 6. Grep-audit: no scheduling logic in the webview

The card spec demands **all timing lives in Rust**. Verify with a single
grep — the result should be ZERO scheduling primitives in the UI:

```sh
$ grep -RInE 'setInterval|setTimeout|new Date\([0-9]' ui/src/ \
    | grep -vE '\.spec\.|//|\* ' || echo 'CLEAN: no scheduling in UI'
./TimerList.svelte:83:    pollHandle = setInterval(refresh, 5000);
./TimerList.svelte:84:    const tick = setInterval(() => { _tick++; }, 1000);
./App.svelte:17:    setTimeout(() => {
```

`new Date(...)` is allowed (display formatting); `setTimeout` / `setInterval`
in the UI are limited to (i) a 5 s poll that re-fetches the timer list,
(ii) a 1 Hz display tick for the countdown column, (iii) a toast TTL
fading. None of these is scheduling — no wake-action math, no clock
ownership.

## 7. IPC contract: camelCase at the boundary (NEEDS_FIX #2 resolved)

The auditor caught that Rust DTOs serialised snake_case fields
(`next_fire_utc`, `total_records`, `start_minimized`, `wake_enabled`)
while the webview read camelCase properties (`nextFireUtc`, `total`,
`startMinimized`, `wakeEnabled`) — every UI binding was undefined.

The fix: add `#[serde(rename_all = "camelCase")]` to every DTO that
crosses the IPC boundary: `TimerDto`, `LogTailDto`, `RunNowResponse`,
`AppInfo`, `WizardChoice`, `WizardStatus`. The webview is updated to
read the camelCase property names. Locked by `ui/src/api.test.js`.

```sh
$ cd ui && npm test
✓ src/api.test.js (9 tests) 9ms
Test Files  1 passed (1)
     Tests  9 passed (9)
```

## 8. Real desktop notification (C6 stub → real toast)

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

## 9. Tauri production build — NOW PASSING

```sh
$ cargo tauri build
…
Bundling Bellman_0.1.0_amd64.AppImage (…)
Finished 2 bundles at:
    target/release/bundle/deb/Bellman_0.1.0_amd64.deb
    target/release/bundle/appimage/Bellman_0.1.0_amd64.AppImage
```

* `cargo tauri build --no-bundle` produces `target/release/bellman`.
* Full bundle produces both the `.deb` (Debian / Ubuntu) and
  `.AppImage` (universal Linux) artefacts in `target/release/bundle/`.

The icon set is at `src-tauri/icons/` (`app-icon.png` 1024×1024 master,
the full set `cargo tauri icon` produces for Win / macOS / Linux /
Android / iOS).

## 10. Known gaps for the next card (C8)

The C8 card adds the body of the Week / Month / Run-history pages
(currently `StubPage` placeholders) and the per-timer edit dialog.
Everything C7 ships — top-bar nav, All-timers page, tray, wizard,
pause-all toggle, IPC contract, event-log wiring — is unchanged by
that work.

## 11. Test summary (final)

```sh
$ cargo test -p bellman-core
108 passed; 0 failed   (+ 4 integration; 5 CLI tests)

$ cd ui && npm test
9 passed; 0 failed    (DTO contract + event payload + api.js fallback)

$ ./tests/cli_roundtrip.sh
23/23 assertions passed

$ cargo tauri build
.deb + .AppImage both produced

$ cd ui && npm run build
5.21 KB CSS / 45 KB JS (17.88 KB gzipped) — production bundle.
```
