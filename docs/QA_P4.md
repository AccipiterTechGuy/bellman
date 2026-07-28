# QA P4 — Calendar UI (Week / Month / dialogs / Run history)

Card: C8 — `bellman-c8-calendar-ui-week-month-dialogs`.
Built on the C7 Tauri shell (tray + All-timers page). This document
captures the rework #3 evidence trail (and the rework #4 follow-up
fix). Earlier rework #1 / #2 audits closed wire-shape, real-Store-CRUD,
and CLI-runtime-capture findings; rework #3 closed IPC-key alignment
plus onceAt/anchor/workdir preservation; rework #4 closed two real
GUI-build bugs the prior tests did not exercise.

## Rework #4 audit findings → what changed

| # | Finding (verbatim line refs) | Fix |
|---|---|---|
| A | `ui/src/TimerDialog.svelte:147-152`: anchor dropped on every Save because `buildInput` checked `form.isEdit`, but `form` has no `isEdit` property — only the top-level `$derived(!!timer)` declares it. Regression I introduced in rework #3. | `buildInput` now reads the top-level `isEdit` ($derived). New JS-side test in `ui/src/dialog-build.test.js::interval edit+save: anchor preserved verbatim (rework #3 + #4)` exercises both branches (`isEdit=false` ⇒ anchor null, `isEdit=true` ⇒ verbatim). |
| B | `ui/src/TimerDialog.svelte:127-145` + `src-tauri/src/web.rs:168-179`: blank tz advertised as system-local (PLAN.md:87) but serialized as "UTC" so the GUI default diverged from CLI default. | JS dialog now resolves `Intl.DateTimeFormat().resolvedOptions().timeZone` whenever the field is blank — that returns "Europe/Helsinki" on this box, "America/Los_Angeles" on the auditor's. The Rust web DTO's "UTC" fallback stays as a belt-and-braces safety for CLI paths. |
| C | Static-harness Chrome fixtures + empty WebKitGTK shell: required WebKitGTK-first captures still deferred. | Removed the rejected mocked-Chrome screenshots (`docs/qa4-screenshots/{all,week,month,history,dialog}.png`, `ui/public/qa4-harness.html`). Kept `real-tauri-webview-960x640-empty-shell.png` as proof of the Xlib capture pipeline. Documented the WebKitGTK-Xvfb limitation (WebKitWebProcess sleeps — needs DRM/DRI/GBM this dev box doesn't have). Closing proof is the JS-side `dialog-build.test.js` (12 tests, executed by the existing vitest harness) + the Rust `tauri_create_update_via_real_ipc_json` test that runs against a real `Store::open(…)` DB. |

## Acceptance gate (current state)

- **JS tests**: `cd ui && npm test` → 37 passed (25 prior + 12 new in `dialog-build.test.js`).
- **Rust tests**: `cargo test --workspace --lib` → 32 + 108 = **140 passed** (round #3) + unchanged in this round (Rust side untouched).
- **Vite build**: green.
- **`cargo tauri build`** → deb + AppImage green.

```
cargo test -p bellman --lib --              → 32 passed
cargo test -p bellman --lib tauri_create_update_via_real_ipc_json -- → ok
cargo test -p bellman --lib seven_kinds_round_trip_through_store_crud -- → ok
cargo test --workspace --lib               → 140 passed
cd ui && npm test                            → 37 passed
cd ui && npm run build                       → 73.92 kB index-*.js
cargo tauri build                            → deb + AppImage
```

## Reproducible CI commands

```sh
# 1. UI tests + Vite build
cd ui && npm install --no-audit --no-fund
npm test
npm run build
cd ..

# 2. Rust tests (lib + integration in src-tauri/src/dto_serde_tests.rs)
cargo test --workspace --lib

# 3. Full Tauri bundle (proves the IPC command bodies compile + link
#    against the production binary).
cargo tauri build

# 4. Real CLI runtime capture (rework #2 replacement for the
# fabricated parity image — runs the same Rust binary the GUI calls
# and captures its actual `bellman next` output for one persisted timer).
bash scripts/capture_qa_p4_evidence.sh
```

## How the tests pin the IPC contract

The two key tests for this rework round:

```
cargo test -p bellman --lib tauri_create_update_via_real_ipc_json
cargo test -p bellman --lib seven_kinds_round_trip_through_store_crud
cd ui && npm test -- --reporter=verbose
```

Both use the same data shape the production binary consumes. The Rust
test deserializes a hand-written JSON snapshot of what `buildInput()`
in `TimerDialog.svelte` emits (camelCase `occ`/`at`/`onceAt`/`days`/
`anchor` + `actionKind` for patches) and drives the real
`CreateTimerInput → into_new_timer → Store::create_timer` /
`WebTimerPatchDto → into_core_patch → Store::update_timer` chain.
The JS test exhaustively exercises `buildInput()`'s shape for every
kind + the auditor-flagged regression (interval anchor must round-trip
on Edit+Save, not drop to `null`).

## WebKitGTK captures — environment note

The spec asks for per-tab WebKitGTK screenshots of the live Tauri app.
The dev box has:

- `Xvfb :99` running in the background.
- A bellman Tauri binary (PIDs 425415, 662194/97, etc.) with both
  WebKitNetworkProcess and WebKitWebProcess alive and bound to
  that display.
- A Toplevel window `0x200003 'Bellman' 960×640` mapped and viewable.
- Xlib + Pillow capture pipeline proves out — the empty-shell PNG
  `real-tauri-webview-960x640-empty-shell.png` was captured against
  this exact process.

What does **not** work in this env: WebKitWebProcess remains sleeping
(MESA-LOADER or DRM/GBM gate fails) so the WebView never paints to
the Xvfb display. Tried `LIBGL_ALWAYS_SOFTWARE=1 + GALLIUM_DRIVER=swrast
+ GDK_BACKEND=x11 + WEBKIT_DISABLE_DMABUF_RENDERER=1`: the WebKit
process boots but the page renders as the same 44376-byte uniform
background. This is a known limitation of WebKitGTK on headless Xvfb
without DRI/GBM; resolving it requires either:

- running the binary on a Wayland session (no Xvfb),
- installing WebKitGTK's sister pipeline `WebKitWebProcess --renderer`
  in software-fallback mode via `webkit2gtk-5.0`,
- or driving Tauri via WebKit WebDriver (`tauri-driver` + `webkit2gtk-4.1-launcher`).

None of those are available in this dev box without root apt-get.
Deferring to a C9 follow-up that requires dev-env upgrades is the
honest path — fabricating screenshots here would repeat the exact
mistake the auditor flagged.

## Files added / changed in rework #4

```
ui/src/TimerDialog.svelte                        # Fix 1 (isEdit binding)
                                                # + Fix 2 (blank → system-local tz)
ui/src/dialog-build.test.js                       # NEW 12 tests for buildInput
docs/qa4-screenshots/                             # Cleanup: removed static-harness
                                                # Chrome PNGs, kept real-tauri shell
ui/public/qa4-harness.html                        # DELETED (rejected mocked harness)
docs/QA_P4.md                                     # THIS update
```

## Caveats / known gaps

- **Per-tab WebKitGTK screenshots** — see above. Pipeline proven via
  empty-shell; full screenshots gated on environment.
- The 7-kind GUI CRUD is exercised at the IPC body level
  (`tauri_create_update_via_real_ipc_json`) rather than via a
  WebKitGTK session. The test uses the same `CreateTimerInput → Store`
  command code-path as `cargo tauri build`'s compiled binary, so any
  drift there surfaces as a test failure.
- GUI preview-vs-CLI parity is asserted indirectly: the dialog's
  preview pane calls the same `preview_fires` Tauri command the
  binary's CLI preview path calls; both run `Occurrence::preview(...)`,
  and `Occurrence::preview` is unit-tested in bellman-core.
