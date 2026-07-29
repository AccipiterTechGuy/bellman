# QA harness must never touch the operator's mouse, keyboard, or screen

Repo: `~/bellman`

## The problem (measured live, 2026-07-28)

`scripts/capture_qa_p4b.py` and `scripts/capture_qa_p4d.py` drive the GUI by injecting
**synthetic input into the operator's real X session**:

```python
DISPLAY_NAME = os.environ.get("DISPLAY", ":0")   # capture_qa_p4b.py:34
from Xlib.ext import xtest                        # :29
xtest.fake_input(d, X.MotionNotify, x=cx, y=cy)   # :316, :361  ← warps the REAL pointer
xtest.fake_input(d, X.ButtonPress, detail=1)      # :317, :362
xtest.fake_input(d, X.KeyPress, keycode)          # :152-156
```

`docs/QA_P4b.md` codifies `export DISPLAY=:0` in the runbook and its "paths tried" table
records the fallback as deliberate: "(a) Real `:0` + NVIDIA — **Works** — used for all
evidence".

Consequence: every GUI QA run steals the pointer and keyboard from whoever is using the
machine. This was observed live during the C8d run (a crew process driving
`capture_qa_p4d.py` against `DISPLAY=:0` while the operator was working).

**This is the requirement: GUI QA must run to completion while the operator keeps using
their mouse and keyboard, uninterrupted.**

## Findings — do NOT re-derive these, they cost a probe session

The QA_P4b "empty WebKit shell on Xvfb" conclusion is **not fully correct**. Two separate
causes were conflated, and both are fixable:

1. **`tauri_plugin_single_instance` silently kills every headless launch.**
   `src-tauri/src/lib.rs:45`. If any `bellman-app` is already running (and during QA one
   always is), a second launch forwards to the first and **exits immediately** — no
   window, no stdout, no stderr, exit 0. This is indistinguishable from "WebKit rendered
   nothing", and it is almost certainly what produced some of the C8-era blank results.
   Verified: launching under `dbus-run-session --` (private session bus) makes the second
   instance start normally and map a `bellman-app` toplevel on Xvfb.

2. **The window never maps on Xvfb — cause NOT yet identified. Start here.**
   `xwininfo -root -tree` shows only `bellman-app  10x10+10+10` (a placeholder); the real
   960x640 toplevel is never created, and the root screenshot is a single uniform colour.
   That is the blank PNG.

   A missing window manager was the first suspect and is **ruled out**: `openbox`,
   `matchbox-window-manager` and `i3` are absent, but **`metacity` and `muffin` ARE
   installed** (Mint ships them — no install needed). Repeating the probe with
   `metacity --sm-disable` running on the Xvfb display, a private bus, an 0700
   `XDG_RUNTIME_DIR` and isolated XDG dirs still produced the same 10x10 placeholder.

   State at that point: the app is genuinely alive (3 processes — main + WebKitNetwork +
   WebKitWeb), `RUST_LOG=debug RUST_BACKTRACE=1` stderr is clean apart from a benign
   `Ignoring invalid max threads value`, and metacity is managing the display. So the app
   starts and then declines to open a window.

   Untested leads, in order of suspicion:
   - **Tray-only startup.** `src-tauri/src/lib.rs:133` shows the window only when
     `start_minimized` is false. Confirm the app is actually reading the isolated
     `config.json` under `XDG_DATA_HOME` and not a different path — instrument it rather
     than assuming. There is no system tray on Xvfb; check what `tray.rs` does when tray
     creation fails.
   - Webview creation failing silently so the toplevel never gets realised.
   - First-run/wizard path taking a branch that skips window creation.

   Resolve this before designing around it. Do not re-run the whole matrix of
   `LIBGL_*`/`WEBKIT_DISABLE_*` env combinations — those were already tried across four
   probe runs and changed nothing while this root cause was in play.

3. **The DRI excuse in `docs/QA_P4.md` does not hold as written.** User `sami` has ACL
   read/write on both `/dev/dri/card1` and `/dev/dri/renderD128` (`getfacl` confirms
   `user:sami:rw-`), `swrast_dri.so` and `kms_swrast_dri.so` are present, WebKitGTK is
   2.52.3. Re-test before repeating that claim.

4. **The scripts are already display-agnostic.** `DISPLAY_NAME` reads `$DISPLAY`. No
   rewrite is needed to move them off `:0` — only an isolated display that renders.

## What to build

Two parts. Part A is the hard requirement; Part B is the durable fix and is the reason
this card exists rather than a one-line runbook edit.

### Part A — isolated display

GUI QA runs on its own display, never `:0`.

- A script (e.g. `scripts/qa_display.sh`) that brings up Xvfb + a window manager +
  a private DBus session, and tears them down cleanly.
- Pick a display number that is free; `:99` was found already occupied by a stale crew
  Xvfb, so **check and fail loudly rather than silently attaching to someone else's
  server** (attaching is what invalidated the first probe run).
- Launch the app under `dbus-run-session --` with isolated `XDG_DATA_HOME`,
  `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR` (the existing runbook already isolates the first
  two; add the rest).
- Update `docs/QA_P4b.md` and `docs/QA_P6.md`: `export DISPLAY=:0` must be gone, and the
  "paths tried" table corrected with the findings above.

### Part B — stop injecting global input

Replace XTEST pointer/keyboard injection with interaction dispatched **inside** the
webview, so the harness cannot touch a global pointer on any display.

Preferred: `tauri-driver` + `WebKitWebDriver` (the official Tauri v2 WebDriver path;
Linux needs the `webkit2gtk-driver` package). Clicks and typing become WebDriver commands
against elements, not screen coordinates — which also removes the fragile
"find the widget by accessibility tree, compute its centre, warp there" logic in
`focus_entry()` / `click_named()`.

If WebDriver proves unworkable for a given check, an acceptable fallback for that check
is dispatching DOM events via the existing Tauri IPC / injected JS — but **not** XTEST.
`xtest.fake_input` must not remain in the QA path.

Screenshots may still be taken from the isolated display; capture is read-only and does
not hijack anything.

## Prerequisites — what needs root and what does not

- **Window manager: no install needed.** `metacity` (and `muffin`) are already on the
  box. Use `metacity --sm-disable`.
- **`tauri-driver`: no root needed** — it is `cargo install tauri-driver`.
- **`WebKitWebDriver`: INSTALLED on this box (2026-07-29).** `/usr/bin/WebKitWebDriver`,
  `webkit2gtk-driver 2.52.3-0ubuntu0.24.04.1` — deliberately the same version as the
  installed `libwebkit2gtk-4.1` (a mismatch fails to attach with an unhelpful error).
  The operator installed it for this card; **do not ask for it again.**

Part A has **no known root dependency**. If Part A turns out to need one anyway, that is
a finding worth reporting on its own — do NOT fall back to `:0`.

### This card must also document the requirement for other people

These packages are now a real prerequisite for anyone running the GUI QA suite, not a
local quirk of this machine. `docs/BUILD_PLAN.md` has been updated with a
"to RUN the GUI test suite" block covering `webkit2gtk-driver`, `cargo install
tauri-driver`, the version-match rule, and the metacity note.

Keep that block truthful as you build: if the harness ends up needing anything else
(a package, a cargo tool, an env var, a permission), add it there in the same place.
The repo is public and the README already warns the project is unfinished — an
undocumented setup step is the difference between a contributor running the tests and
silently giving up.

**If a required package is missing: STOP and report exactly which package is needed.**
Do NOT fall back to `DISPLAY=:0` — that fallback is the entire bug this card exists to
remove, and it was chosen last time precisely because the alternative looked blocked.

## Do NOT break these

- The evidence bar stays where C8b/C8f set it: real WebKitGTK pixels, real CRUD through
  the GUI, per-shot `.meta.json`, raw app logs. Do not downgrade to mocked screenshots —
  that was already rejected once by an auditor (see `docs/QA_P4.md` rework #4).
- Do not delete existing evidence in `docs/qa4-screenshots/` or `docs/qa4-evidence/`.
- Do not weaken `tauri_plugin_single_instance` in shipping code to make testing easier.
  Isolate the *test* with a private bus; the plugin's production behaviour is correct.

## Verify

- Run the full GUI QA suite while moving the mouse continuously and typing in another
  window. The pointer must never jump; keystrokes must never land in Bellman.
- `grep -rn "fake_input\|DISPLAY.*:0" scripts/ docs/` returns nothing in the QA path.
- `pgrep -f bellman-app` before the run (a live instance present) must NOT prevent the
  QA instance from starting.
- Screenshots contain the real rendered UI (not a uniform colour) — check unique pixel
  colours > 1, and that the All/Week/Month pages are visually distinct.
- The display and DBus session are gone after teardown; no orphan Xvfb left behind
  (there is already one stale `:99` on this box — clean it up).
