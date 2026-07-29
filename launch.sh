#!/usr/bin/env bash
# Bellman desktop launcher (repo root).
#
# Selects a GUI binary only when it is *fresh* relative to source inputs.
# Never silently launches a stale target/release or target/debug binary.
#
# Freshness rule (deterministic):
#   A candidate is FRESH iff it is executable and its mtime is >= the newest
#   mtime among GUI-affecting inputs: Cargo manifests/lock, crates/,
#   src-tauri/{src,capabilities,icons,linux,Cargo.toml,tauri.conf.json,build.rs},
#   ui/{src,public,index.html,vite/svelte configs,package files}.
#   Missing stamp roots are ignored; if no stamp files exist, any executable
#   candidate is treated as fresh (fixture / empty tree edge case).
#
# Priority among fresh candidates:
#   1. target/release/bellman-app
#   2. target/debug/bellman-app
#   3. src-tauri/target/release/bellman-app
#   4. src-tauri/target/debug/bellman-app
#
# When no fresh binary exists:
#   - BELLMAN_ALLOW_STALE=1 (or BELLMAN_APP_ALLOW_STALE=1): use the best stale
#     binary with a stderr warning (explicit opt-in only).
#   - else: rebuild via `cargo tauri build --no-bundle` when cargo+tauri-cli
#     are available; on rebuild failure or missing tools, fall through to
#     `cargo tauri dev` (or a status dialog if src-tauri is absent).
#
# Env:
#   BELLMAN_ALLOW_STALE / BELLMAN_APP_ALLOW_STALE  opt-in stale reuse
#   BELLMAN_LAUNCH_DRY_RUN=1   print selection decision, do not exec/rebuild
#   BELLMAN_SKIP_REBUILD=1     dry-run helper: never attempt rebuild (tests)
#
# Desktop entry / ~/.local/bin should point here (see scripts/install_desktop.sh).
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
export PATH="$CARGO_BIN:$PATH"
# nvm-installed node is needed by `tauri dev` for the Svelte frontend.
# shellcheck source=/dev/null
[ -s "$HOME/.nvm/nvm.sh" ] && . "$HOME/.nvm/nvm.sh" >/dev/null 2>&1

die() {
  local msg="$1"
  if [ "${BELLMAN_LAUNCH_DRY_RUN:-}" != "1" ]; then
    if command -v zenity >/dev/null 2>&1; then
      zenity --error --width=420 --title="Bellman" --text="$msg" 2>/dev/null || true
    fi
    if command -v notify-send >/dev/null 2>&1; then
      notify-send "Bellman" "$msg" 2>/dev/null || true
    fi
  fi
  echo "bellman: $msg" >&2
  exit 1
}

allow_stale() {
  [ "${BELLMAN_ALLOW_STALE:-}" = "1" ] || [ "${BELLMAN_APP_ALLOW_STALE:-}" = "1" ]
}

# Newest mtime (epoch seconds) among GUI-affecting sources. Prints 0 if none.
# Includes Rust/core crates, Tauri shell (src, conf, capabilities, icons), and
# the full frontend inputs that embed into the GUI binary (ui/src + entry/config).
source_stamp() {
  local root="$1"
  local newest=0
  local f mt
  local -a paths=(
    "$root/Cargo.toml"
    "$root/Cargo.lock"
    "$root/src-tauri/Cargo.toml"
    "$root/src-tauri/Cargo.lock"
    "$root/src-tauri/tauri.conf.json"
    "$root/src-tauri/build.rs"
    "$root/ui/package.json"
    "$root/ui/package-lock.json"
    "$root/ui/index.html"
    "$root/ui/vite.config.js"
    "$root/ui/vite.config.ts"
    "$root/ui/svelte.config.js"
    "$root/ui/jsconfig.json"
    "$root/ui/tsconfig.json"
  )
  local p
  for p in "${paths[@]}"; do
    if [ -f "$p" ]; then
      mt=$(stat -c '%Y' "$p" 2>/dev/null || stat -f '%m' "$p" 2>/dev/null || echo 0)
      [ "$mt" -gt "$newest" ] && newest=$mt
    fi
  done
  # Directory trees (null-delimited; tolerate empty).
  while IFS= read -r -d '' f; do
    mt=$(stat -c '%Y' "$f" 2>/dev/null || stat -f '%m' "$f" 2>/dev/null || echo 0)
    [ "$mt" -gt "$newest" ] && newest=$mt
  done < <(
    {
      [ -d "$root/src-tauri/src" ] && find "$root/src-tauri/src" -type f -print0
      [ -d "$root/src-tauri/capabilities" ] && find "$root/src-tauri/capabilities" -type f -print0
      [ -d "$root/src-tauri/icons" ] && find "$root/src-tauri/icons" -type f -print0
      [ -d "$root/src-tauri/linux" ] && find "$root/src-tauri/linux" -type f -print0
      [ -d "$root/crates" ] && find "$root/crates" -type f \( -name '*.rs' -o -name 'Cargo.toml' \) -print0
      [ -d "$root/ui/src" ] && find "$root/ui/src" -type f -print0
      [ -d "$root/ui/public" ] && find "$root/ui/public" -type f -print0
    } 2>/dev/null
  )
  echo "$newest"
}

is_fresh() {
  local bin="$1"
  local stamp="$2"
  [ -x "$bin" ] || return 1
  # No source inputs → treat as fresh (empty fixture / pure binary tree).
  [ "$stamp" -eq 0 ] && return 0
  local bmt
  bmt=$(stat -c '%Y' "$bin" 2>/dev/null || stat -f '%m' "$bin" 2>/dev/null || echo 0)
  [ "$bmt" -ge "$stamp" ]
}

# Emit a single machine-readable decision line on stdout for tests / dry-run.
emit_decision() {
  # shellcheck disable=SC2145
  echo "$*"
}

candidates() {
  local root="$1"
  printf '%s\n' \
    "$root/target/release/bellman-app" \
    "$root/target/debug/bellman-app" \
    "$root/src-tauri/target/release/bellman-app" \
    "$root/src-tauri/target/debug/bellman-app"
}

# Select path: prints decision, sets global SELECT_PATH / SELECT_ACTION.
select_gui() {
  local root="$1"
  local stamp
  stamp=$(source_stamp "$root")
  local best_stale=""
  local c

  while IFS= read -r c; do
    [ -x "$c" ] || continue
    if is_fresh "$c" "$stamp"; then
      SELECT_PATH="$c"
      SELECT_ACTION="exec"
      SELECT_FRESHNESS="fresh"
      emit_decision "action=exec path=$c freshness=fresh stamp=$stamp"
      return 0
    fi
    # First existing (priority order) becomes best stale fallback.
    if [ -z "$best_stale" ]; then
      best_stale="$c"
    fi
  done < <(candidates "$root")

  if [ -n "$best_stale" ] && allow_stale; then
    SELECT_PATH="$best_stale"
    SELECT_ACTION="exec-stale"
    SELECT_FRESHNESS="stale"
    emit_decision "action=exec path=$best_stale freshness=stale allow_stale=1 stamp=$stamp"
    return 0
  fi

  if [ -n "$best_stale" ]; then
    SELECT_PATH="$best_stale"
    SELECT_ACTION="rebuild-or-dev"
    SELECT_FRESHNESS="stale"
    emit_decision "action=rebuild-or-dev path=$best_stale freshness=stale stamp=$stamp"
    return 0
  fi

  SELECT_PATH=""
  SELECT_ACTION="tauri-dev"
  SELECT_FRESHNESS="missing"
  emit_decision "action=tauri-dev path= freshness=missing stamp=$stamp"
  return 0
}

run_tauri_dev() {
  local root="$1"
  command -v cargo >/dev/null 2>&1 || die "cargo not on PATH — cannot start the dev GUI."
  cargo tauri --version >/dev/null 2>&1 || die "tauri-cli missing.\n\nInstall with:\ncargo install tauri-cli --locked"

  # First `tauri dev` compiles for a few minutes; keep a terminal so progress is visible.
  if command -v x-terminal-emulator >/dev/null 2>&1; then
    exec x-terminal-emulator -e bash -lc "cd '$root' && cargo tauri dev; echo; echo '[bellman exited — press enter]'; read"
  fi
  exec cargo tauri dev
}

try_rebuild() {
  local root="$1"
  command -v cargo >/dev/null 2>&1 || return 1
  cargo tauri --version >/dev/null 2>&1 || return 1
  echo "bellman: binary stale or missing — rebuilding GUI (cargo tauri build --no-bundle)…" >&2
  (cd "$root" && cargo tauri build --no-bundle) || return 1
  # Re-select after rebuild — only exec a *fresh* binary.
  # Never fall back to a still-stale candidate without explicit BELLMAN_ALLOW_STALE=1.
  local stamp
  stamp=$(source_stamp "$root")
  local c
  while IFS= read -r c; do
    if is_fresh "$c" "$stamp"; then
      echo "bellman: rebuilt $c" >&2
      exec "$c"
    fi
  done < <(candidates "$root")
  if allow_stale; then
    while IFS= read -r c; do
      if [ -x "$c" ]; then
        echo "bellman: WARNING rebuild left binary stale; launching with BELLMAN_ALLOW_STALE=1: $c" >&2
        exec "$c"
      fi
    done < <(candidates "$root")
  fi
  echo "bellman: rebuild finished but no fresh GUI binary found — refusing silent stale launch" >&2
  return 1
}

pre_gui_status() {
  local root="$1"
  phase_line() { [ -e "$1" ] && echo "✅ $2" || echo "⬜ $2"; }

  local STATUS
  STATUS="$(
    echo "The Bellman GUI has not been built yet (src-tauri missing)."
    echo ""
    echo "Built so far:"
    phase_line "$root/crates/bellman-core/src/occurrence" "occurrence engine"
    phase_line "$root/crates/bellman-core/src/store"      "SQLite store + run claims"
    phase_line "$root/crates/bellman-core/src/scheduler"  "scheduler loop + misfire"
    phase_line "$root/crates/bellman-cli/src"             "CLI"
    phase_line "$root/crates/bellman-core/src/slots"      "slot IPC"
    phase_line "$root/crates/bellman-core/src/visible"    "visible scheduler (scan/task)"
    phase_line "$root/crates/bellman-core/src/calendar"   "calendar snapshot"
    phase_line "$root/src-tauri"                          "GUI (Tauri)"
    echo ""
    echo "Build:  cd $root && cargo tauri build --no-bundle"
    echo "Watch:  http://127.0.0.1:8031/train"
  )"

  if [ "${BELLMAN_LAUNCH_DRY_RUN:-}" = "1" ]; then
    emit_decision "action=status freshness=no-src-tauri"
    echo "$STATUS" >&2
    return 0
  fi

  if command -v zenity >/dev/null 2>&1; then
    zenity --info --width=460 --title="Bellman — not built yet" --text="$STATUS" 2>/dev/null || echo "$STATUS"
  else
    echo "$STATUS"
    if command -v notify-send >/dev/null 2>&1; then
      notify-send "Bellman" "GUI not built yet." 2>/dev/null || true
    fi
  fi
}

# ---------------------------------------------------------------- main
[ -d "$REPO" ] || die "Bellman repo not found at:\n$REPO"
cd "$REPO" || die "Cannot enter $REPO"

SELECT_PATH=""
SELECT_ACTION=""
SELECT_FRESHNESS=""

if [ ! -d "$REPO/src-tauri" ]; then
  pre_gui_status "$REPO"
  exit 0
fi

select_gui "$REPO"
# SELECT_FRESHNESS is set by select_gui for callers/tests that source decisions.
: "${SELECT_FRESHNESS:=}"

if [ "${BELLMAN_LAUNCH_DRY_RUN:-}" = "1" ]; then
  # Decision already printed by select_gui.
  exit 0
fi

case "$SELECT_ACTION" in
  exec)
    exec "$SELECT_PATH"
    ;;
  exec-stale)
    echo "bellman: WARNING using stale binary $SELECT_PATH (BELLMAN_ALLOW_STALE=1; freshness=$SELECT_FRESHNESS)" >&2
    exec "$SELECT_PATH"
    ;;
  rebuild-or-dev)
    if [ "${BELLMAN_SKIP_REBUILD:-}" = "1" ]; then
      die "stale binary at $SELECT_PATH — rebuild refused (BELLMAN_SKIP_REBUILD=1). Set BELLMAN_ALLOW_STALE=1 to force."
    fi
    if try_rebuild "$REPO"; then
      :
    fi
    echo "bellman: rebuild unavailable or failed — entering cargo tauri dev" >&2
    run_tauri_dev "$REPO"
    ;;
  tauri-dev)
    if [ "${BELLMAN_SKIP_REBUILD:-}" = "1" ]; then
      die "no GUI binary found and BELLMAN_SKIP_REBUILD=1"
    fi
    run_tauri_dev "$REPO"
    ;;
  *)
    die "internal launcher error: unknown action '$SELECT_ACTION' (freshness=$SELECT_FRESHNESS)"
    ;;
esac
