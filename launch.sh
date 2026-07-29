#!/usr/bin/env bash
# Bellman desktop launcher (repo root).
#
# Picks the best available surface:
#   1. release GUI binary (target/release/bellman-app)
#   2. debug GUI binary   (target/debug/bellman-app)
#   3. cargo tauri dev    (needs cargo + tauri-cli + node)
#
# Safe to run at any point; never mutates the working tree.
# Desktop entry / ~/.local/bin should point here or re-exec this file.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
export PATH="$CARGO_BIN:$PATH"
# nvm-installed node is needed by `tauri dev` for the Svelte frontend.
[ -s "$HOME/.nvm/nvm.sh" ] && . "$HOME/.nvm/nvm.sh" >/dev/null 2>&1

die() {
  local msg="$1"
  if command -v zenity >/dev/null 2>&1; then
    zenity --error --width=420 --title="Bellman" --text="$msg" 2>/dev/null || true
  fi
  command -v notify-send >/dev/null 2>&1 && notify-send "Bellman" "$msg" 2>/dev/null || true
  echo "bellman: $msg" >&2
  exit 1
}

[ -d "$REPO" ] || die "Bellman repo not found at:\n$REPO"
cd "$REPO" || die "Cannot enter $REPO"

# ---------------------------------------------------------------- GUI path
if [ -d "$REPO/src-tauri" ]; then
  for candidate in \
    "$REPO/target/release/bellman-app" \
    "$REPO/target/debug/bellman-app" \
    "$REPO/src-tauri/target/release/bellman-app" \
    "$REPO/src-tauri/target/debug/bellman-app"
  do
    if [ -x "$candidate" ]; then
      exec "$candidate"
    fi
  done

  command -v cargo >/dev/null 2>&1 || die "cargo not on PATH — cannot start the dev GUI."
  cargo tauri --version >/dev/null 2>&1 || die "tauri-cli missing.\n\nInstall with:\ncargo install tauri-cli --locked"

  # First `tauri dev` compiles for a few minutes; keep a terminal so progress is visible.
  if command -v x-terminal-emulator >/dev/null 2>&1; then
    exec x-terminal-emulator -e bash -lc "cd '$REPO' && cargo tauri dev; echo; echo '[bellman exited — press enter]'; read"
  fi
  exec cargo tauri dev
fi

# ------------------------------------------------- pre-GUI status dialog (should not hit today)
phase_line() { [ -e "$1" ] && echo "✅ $2" || echo "⬜ $2"; }

STATUS="$(
  echo "The Bellman GUI has not been built yet (src-tauri missing)."
  echo ""
  echo "Built so far:"
  phase_line "$REPO/crates/bellman-core/src/occurrence" "occurrence engine"
  phase_line "$REPO/crates/bellman-core/src/store"      "SQLite store + run claims"
  phase_line "$REPO/crates/bellman-core/src/scheduler"  "scheduler loop + misfire"
  phase_line "$REPO/crates/bellman-cli/src"             "CLI"
  phase_line "$REPO/crates/bellman-core/src/slots"      "slot IPC"
  phase_line "$REPO/crates/bellman-core/src/visible"    "visible scheduler (scan/task)"
  phase_line "$REPO/crates/bellman-core/src/calendar"   "calendar snapshot"
  phase_line "$REPO/src-tauri"                          "GUI (Tauri)"
  echo ""
  echo "Build:  cd $REPO && cargo tauri build --no-bundle"
  echo "Watch:  http://127.0.0.1:8031/train"
)"

if command -v zenity >/dev/null 2>&1; then
  zenity --info --width=460 --title="Bellman — not built yet" --text="$STATUS" 2>/dev/null || echo "$STATUS"
else
  echo "$STATUS"
  command -v notify-send >/dev/null 2>&1 && notify-send "Bellman" "GUI not built yet." 2>/dev/null || true
fi
