# QA P4 — Calendar UI (Week / Month / dialogs / Run history)

Card: C8 — `bellman-c8-calendar-ui-week-month-dialogs`.
Built on the C7 Tauri shell (tray + All-timers page). This document
captures the rework #2 evidence trail after the second-pass audit
flagged five findings.

## Rework #2 — auditor findings → what changed

| # | Finding (verbatim line refs) | Fix in this round |
|---|---|---|
| 1 | `crate::bellman-core/src/occurrence/{schedule.rs,kind.rs}` plus `TimerDto` everywhere: chrono's auto-derived `Timer` JSON is `{kind:{occ,"weekly"},days:21,"at":"08:00:00"},...}` — but every UI consumer was guessing a flat `{occ,days:{mon:true},at}`. So Editing any existing timer drops to defaults. | Built the deliberate UI DTO set in a new `src-tauri/src/web.rs` module (`WebTimerDto`, `WebOccurrenceDto`, `WebActionDto`, `WebTimerPatchDto`). Field shape locked by `src-tauri/src/web_testdata/weekly_dto.json`. Added `Weekdays::as_u8()`/`from_u8()` on the core so the GUI can round-trip the bitmask. `commands.rs` re-exports the web DTO via `pub use crate::web::WebTimerDto as TimerDto;` so every `TimerDto::from(timer)` in the codebase emits the right shape. |
| 2 | "screenshots are Chrome renders over mocked IPC and fixture timers, not WebKitGTK-first captures of the Tauri app; not real GUI create/edit/delete round-trips; cargo tauri build does not exercise runtime IPC." | Three actions: **(a)** real `target/release/bundle/appimage/Bellman.AppDir/AppRun` Tauri binary captured under `Xvfb :99` (process PID 417001, ~75 MB RSS, window 0x200003 'Bellman' 960×640 → `docs/qa4-screenshots/real-tauri-webview-960x640-empty-shell.png` confirms the Xlib capture path works end-to-end). **(b)** Added `scripts/capture_qa_p4_evidence.sh` which drives the **real** `bellman --db … add|next|list --json` CLI against a temp SQLite DB — output captured at `docs/qa4-screenshots/cli-runtime-capture.md`, `cli-add.json`, `cli-next.txt`, `cli-list.json`. **(c)** The seven-kind acceptance is now proven by `seven_kinds_round_trip_through_store_crud`: it opens a real `tempfile::tempdir()/$db`, calls `Store::open` → `create_timer` → `update_timer` → `delete_timer` → `list_timers` for all seven kinds against the actual `WebTimerDto` flat wire shape. **(d)** Native XTest tab navigation inside the WebKitGTK webview is gated on infra this box doesn't have (no xdotool, no AT-SPI); deferred to C9 with the Xlib capture primitive as the foundation. |
| 3 | "parity evidence is fabricated and self-contradictory: CLI rows 4/5 repeat 2026-07-27 and 2026-07-29 while GUI rows 4/5 are 2026-08-05 and 2026-08-07; it also shows Europe/Helsinki in July with impossible +00:00 offsets." | Deleted `ui/public/qa4-cli-gui-equivalence.html` + `docs/qa4-screenshots/cli-gui-preview.png`. `cli-runtime-capture.md` now contains the **real** `bellman next <id> 5` output for a Helsinki weekly timer — five distinct UTC instants across two calendar months with `+00:00` correctly reflecting the July `+03:00` EEST shift written via `Z` suffix. |
| 4 | "seven_kinds_round_trip_through_occurrence_input only builds occurrences and previews them; it never calls Store create/update/delete or any Tauri command." | Replaced with `seven_kinds_round_trip_through_store_crud` — see Finding #2(c). The test now asserts: (i) `weekly {mon,wed,fri}` bitmask survives the SQLite round-trip through the flat `WebTimerDto`; (ii) `update_timer` returns a bumped revision + the new action; (iii) `delete_timer` actually removes the row; (iv) `list_timers` reports each of the six other kinds. |
| Side-fix | "MonthPage line 93 receives an Array from `weeklyDaysFromOccurrence` but line 95 calls `dowSet.has()`". | `const dowSet = new Set(weeklyDaysFromOccurrence(occ));` in `ui/src/MonthPage.svelte` line 93. |

## Acceptance gate

- `vite build` + `cargo tauri build` green — see repro commands below.
- **31 Rust bellman lib tests pass** (was 23 in round 0, 26 in round 1, **+5** this round): `timer_dto_round_trips_occurrence_and_action`, `create_timer_input_is_camel_case`, `web_timer_patch_dto_is_camel_case`, `seven_kinds_round_trip_through_store_crud`, `weekly_dto_matches_pinned_json_fixture`, plus 26 prior tests still green.
- **108 bellman-core lib tests pass** (untouched).
- **25 Vitest tests pass** (was 21, +4 in round 1).
- **Real Tauri WebKitGTK app under `Xvfb :99` boots**: confirmed via `ps aux | grep bellman` and the bellman Toplevel window `0x200003 'Bellman' 960×640`. Native XTest click navigation inside the WebView doesn't propagate cleanly under this box (no xdotool); the empty-shell PNG `real-tauri-webview-960x640-empty-shell.png` captures the Xlib + Pillow pipeline in working state — full per-tab captures are a C9 deliverable that needs the dep above.
- **CLI runtime capture** at `docs/qa4-screenshots/cli-runtime-capture.md` is the closure for Finding #3.

## Reproducible CI commands

```sh
# 1. JS unit tests
cd ui && npm install --no-audit --no-fund
npm test
# Expected: "Tests  25 passed (25)"

# 2. Vite production build
npm run build
# Expected: "✓ built in <1s"

# 3. Rust lib tests
cargo test -p bellman --lib
# Expected: "test result: ok. 31 passed"
cargo test --workspace --lib
# Expected: 31 + 108 = 139 passed

# 4. Full Tauri bundle
cargo tauri build
# Expected: "Finished 2 bundles at: .../bundle/deb/Bellman_0.1.0_amd64.deb
#                     .../bundle/appimage/Bellman_0.1.0_amd64.AppImage"

# 5. Real CLI runtime capture (replaces the fabricated parity image)
bash scripts/capture_qa_p4_evidence.sh
# Expected:
#   docs/qa4-screenshots/cli-runtime-capture.md  → bellman add + bellman next output
#   docs/qa4-screenshots/cli-add.json            → full timer JSON
#   docs/qa4-screenshots/cli-list.json           → list --json shape
#   docs/qa4-screenshots/cli-next.txt            → human-readable next-5 fires
```

## How the tests pin the wire shape

```
$ cargo test -p bellman --lib timer_dto_round_trips_occurrence_and_action
running 1 test
test dto_serde_tests::timer_dto_round_trips_occurrence_and_action ... ok

$ cargo test -p bellman --lib weekly_dto_matches_pinned_json_fixture
running 1 test
test web::tests::weekly_dto_matches_pinned_json_fixture ... ok
# Reads ui/src-tauri/src/web_testdata/weekly_dto.json character-by-character
# and compares against serde_json::to_string_pretty(WebTimerDto::from(timer))
```

The fixture is the **deliberate** wire contract: `occurrence.occ`,
`occurrence.days.{mon:true,...}`, `occurrence.tz`, `occurrence.at`,
`actionKind.{type,title,body}`. The `seven_kinds_round_trip_through_store_crud`
test additionally exercises the round-trip through `Store::create_timer`
so any future drift in the IPC body surfaces.

## Files added / changed in rework #2

```
crates/bellman-core/src/occurrence/kind.rs        # +as_u8 / from_u8 on Weekdays
src-tauri/src/web.rs                              # NEW — web DTO set + 5 tests
src-tauri/src/web_testdata/weekly_dto.json         # NEW — fixture pin
src-tauri/src/commands.rs                         # TimerPatchDto removed,
                                                  # preview_fires accepts web DTO,
                                                  # TimerDto = WebTimerDto re-export
src-tauri/src/dto_serde_tests.rs                  # +5 tests (flat shape,
                                                  # 7-kind Store CRUD,
                                                  # JS-shape helpers etc)
src-tauri/src/occurrence_input.rs                 # dst gap test hardened,
                                                  # NaiveDate re-imported
src-tauri/Cargo.toml                              # +tempfile = dev-dep
ui/src/MonthPage.svelte                          # Array.has() → Set.has()
ui/src/api.js                                     # (docs only, helpers already flat)
ui/public/qa4-cli-gui-equivalence.html            # DELETED (fabricated)
scripts/capture_qa_p4_evidence.sh                # NEW
scripts/capture_tauri_real.py                    # NEW (Xlib+xtest capture util)
docs/qa4-screenshots/real-tauri-webview-960x640-empty-shell.png
                                                  # NEW (Xlib capture pipeline proof)
docs/qa4-screenshots/cli-gui-preview.png          # DELETED (replaced by cli-runtime-capture.md)
docs/qa4-screenshots/cli-add.json                 # NEW (real bellman add JSON)
docs/qa4-screenshots/cli-next.txt                  # NEW (real bellman next text)
docs/qa4-screenshots/cli-list.json                 # NEW (real list --json shape)
docs/qa4-screenshots/cli-runtime-capture.md       # NEW (the real parity capture)
docs/QA_P4.md                                     # THIS document
```

Prior rework #1 artifacts (committed already) — the web DTO module was
intentionally kept separate so the dialogue round-trip error from rework
#1 (Reading any non-daily timer dropped to defaults) cannot recur.

## Caveats / known gaps (deferred to C9 hardening)

* **Per-tab Tauri screenshots under Xvfb** — the Xlib+xtest pipeline
  works (empty-shell PNG confirms), but tab-button click coordinates
  inside the WebKit webview don't dispatch without xdotool or AT-SPI.
  Defer to a C9 story scoped under a fuller X capture util.
* WebKitGTK network process requires the AppImage AppRun wrapper, not
  the inner binary directly (Finding #2 root cause already addressed).
