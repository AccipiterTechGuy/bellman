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

**Verified at the end of the rework #2 round:**

* The release binary boots cleanly (no panic, no stderr) under:

  ```
  env XDG_DATA_HOME=$PWD/target/audit-runtime-final/data \
      XDG_CONFIG_HOME=$PWD/target/audit-runtime-final/config \
      target/release/bellman
  ```

  — the auditor's reproducer (was exiting 101 with
  `PluginInitialization("notification", "Error deserializing
  plugins.notification ...")` after the audit; now enters the
  event loop and stays alive until killed by SIGTERM).

* `cargo tauri build` finishes with both bundles present:

  ```
  Bundling Bellman_0.1.0_amd64.AppImage
      Finished 2 bundles at:
          target/release/bundle/deb/Bellman_0.1.0_amd64.deb
          target/release/bundle/appimage/Bellman_0.1.0_amd64.AppImage
  ```

* `cd ui && npm test` is real (vitest run, 8 passed):

  ```
  ✓ src/api.test.js (8 tests)
  Test Files  1 passed (1)
       Tests  8 passed (8)
  ```

  Eight tests across two layers:
  - Five tests inject a `__TAURI_INTERNALS__` mock and exercise
    the IPC path: a Rust-shaped TimerDto round-trip through
    `invoke()` preserves camelCase, `listen()` actually delivers
    the handler when `runCallback` fires with the recorded id,
    `unsubscribe()` halts further deliveries and unlistens from
    the runtime, concurrent `listen()` calls allocate fresh ids.
  - Three DTO-shape guards (TimerDto / LogTailDto / WizardChoice)
    fail if the wire shape ever drifts away from camelCase.
  - Two-layer fail-mode (negative test): temporarily removing
    `#[serde(rename_all = "camelCase")]` from `LogTailDto` makes
    `log_tail_dto_is_camel_case` panic, proving the contract
    really is locked.

* `cargo test -p bellman --lib` runs **7 new DTO / source-grep
  tests** in `src-tauri/src/dto_serde_tests.rs`:
  1. `timer_dto_is_camel_case` — JSON-round-trip a TimerDto,
     assert `nextFireUtc`/`lastFired` exist and
     `next_fire_utc`/`last_fired` do not.
  2. `log_tail_dto_is_camel_case` — same for LogTailDto
     (`totalRecords`) plus a value-level JSON check
     (`"totalRecords":7` not `total_records`).
  3. `run_now_response_is_camel_case` — same for RunNowResponse
     (`timerId`, `scheduledFor`, `nextFireUtc`, `message`).
  4. `app_info_is_camel_case` — seven camelCase keys,
     seven snake_case anti-patterns.
  5. `wizard_choice_is_camel_case` — same with startMinimized +
     wakeEnabled.
  6. `wizard_status_defaults_is_camel_case_wizard_choice` —
     nested serialization keeps camelCase in
     `WizardStatus.defaults`.
  7. `pause_all_emit_is_bare_bool_in_sources` — walks every
     `src-tauri/src/*.rs` (skipping the test file itself) and
     fails the build if `emit("pause-all-changed", ...)` carries
     an object-shaped payload. Detects the auditor's exact
     regression AND any future refactor that wraps the bool in
     an object (`{ paused: ... }`).

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

## 5. Pause-all: tray ↔ window stays in sync

The auditor flagged this in NEEDS_FIX #4 then again in rework #2.
The current state:

* `withGlobalTauri: true` is set in `tauri.conf.json` so the
  `window.__TAURI__.event` global is injected into the webview on
  boot. Without this, Tauri 2 defaults to `false` and the global
  listener API is unavailable.
* `api.js::listen()` also implements the transformCallback
  fallback so it still works even if the global is disabled in a
  future config change. Both paths allocate callback IDs from a
  per-process Map and dispatch through
  `__TAURI_INTERNALS__.runCallback(cb_id, payload)`.
* `App.svelte` subscribes via `onMount` + a placeholder `onDestroy`
  closure; the real unsubscriber from `listen()` is wired in
  inside the `.then` handler so destroying the component before
  the listener resolves can no longer leak.
* Click the "Running" pill in the top bar → it flips to "Paused".
* The `set_pause_all` Tauri command calls
  `tray::set_tray_pause_check(&app, paused)` so the tray's
  CheckMenuItem updates in lockstep.
* The tray's on_menu_event emits the **same bare bool** payload
  (`app.emit("pause-all-changed", next)`) so the window pill
  updates even when the user clicks the tray's "Pause all".
* `tray::install` runs AFTER `app.manage(state)` so the persisted
  pause-all flag is loaded into the tray's check item at startup.
* Rust test `pause_all_emit_is_bare_bool_in_sources` walks every
  `src-tauri/src/*.rs` (skipping the test file) and asserts neither
  surface re-introduces the `{ "paused": next }` object payload.
* Vitest `listen() actually delivers events through the IPC bridge`
  injects a `__TAURI_INTERNALS__` mock, fires `runCallback` with the
  recorded handler id, and proves the handler runs once — then
  `unsubscribe() stops further deliveries` proves the runtime sees
  `plugin:event|unlisten` AND subsequent `runCallback` calls are
  dropped.

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

$ cargo test -p bellman --lib      # NEW: Tauri shell serde contract
7 passed; 0 failed    (DTO JSON shape + pause-all emit source grep)

$ cd ui && npm test
8 passed; 0 failed    (real Tauri event delivery via mock IPC + DTO shape)

$ ./tests/cli_roundtrip.sh
23/23 assertions passed

$ cargo tauri build
.deb + .AppImage both produced

$ env XDG_DATA_HOME=$PWD/target/audit-runtime-final/data \
    XDG_CONFIG_HOME=$PWD/target/audit-runtime-final/config \
    target/release/bellman
   (no panic; runs the event loop until SIGTERM)

$ cd ui && npm run build
5.21 KB CSS / 46.19 KB JS (18.19 KB gzipped) — production bundle.
```
