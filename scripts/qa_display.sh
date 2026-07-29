#!/usr/bin/env bash
# qa_display.sh — isolated X11 display for Bellman GUI QA.
#
# Starts Xvfb + metacity on a free display, prepares isolated XDG dirs and a
# private D-Bus session, and never attaches to the operator's :0.
#
# Usage:
#   source scripts/qa_display.sh          # load helpers into current shell
#   qa_display_start                      # export DISPLAY, XDG_*, write state
#   # … launch tauri-driver / capture scripts …
#   qa_display_stop                       # tear down Xvfb, metacity, state
#
# Or as a command:
#   scripts/qa_display.sh start|stop|status|env
#
# State file: ${BELLMAN_QA_STATE:-/tmp/bellman-qa-display.state}
# Session root: ${BELLMAN_QA_ROOT:-/tmp/bellman-qa-session}

set -euo pipefail

QA_STATE="${BELLMAN_QA_STATE:-/tmp/bellman-qa-display.state}"
QA_ROOT="${BELLMAN_QA_ROOT:-/tmp/bellman-qa-session}"
# Prefer high numbers; widen so crash leftovers do not exhaust the list.
QA_DISPLAY_CANDIDATES="${BELLMAN_QA_DISPLAY_CANDIDATES:-97 98 96 95 94 93 92 91 90 89 88 87 86 85}"

_qa_reap_stale_display() {
  # If a lock/socket exists but no live Xvfb owns it, remove the leftovers so
  # aborted runs do not permanently burn a display number.
  local n="$1"
  local lock="/tmp/.X${n}-lock"
  local sock="/tmp/.X11-unix/X${n}"
  local pid=""
  if [ -f "$lock" ]; then
    pid=$(tr -d ' \n' <"$lock" 2>/dev/null || true)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      # Live process — leave it alone (may not be Xvfb; still treat as busy).
      return 1
    fi
    # Stale lock.
    rm -f "$lock"
  fi
  if [ -e "$sock" ]; then
    # Socket with no lock and no live owner → remove.
    if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
      rm -f "$sock" 2>/dev/null || true
    fi
  fi
  return 0
}

_qa_is_display_free() {
  local n="$1"
  _qa_reap_stale_display "$n" || true
  # Refuse if lock file exists OR something already listens on that display.
  if [ -e "/tmp/.X${n}-lock" ] || [ -e "/tmp/.X11-unix/X${n}" ]; then
    return 1
  fi
  return 0
}

_qa_pick_display() {
  local n
  for n in $QA_DISPLAY_CANDIDATES; do
    if _qa_is_display_free "$n"; then
      echo "$n"
      return 0
    fi
  done
  echo "qa_display: no free display in candidates: $QA_DISPLAY_CANDIDATES" >&2
  echo "qa_display: refuse to attach to a busy server (that invalidated prior probes)." >&2
  echo "qa_display: free a display (check /tmp/.X*-lock) or set BELLMAN_QA_DISPLAY_CANDIDATES." >&2
  return 1
}

_qa_require_bins() {
  local missing=()
  for b in Xvfb metacity wmctrl; do
    command -v "$b" >/dev/null 2>&1 || missing+=("$b")
  done
  if [ ${#missing[@]} -gt 0 ]; then
    echo "qa_display: missing required binaries: ${missing[*]}" >&2
    return 1
  fi
}

qa_display_start() {
  _qa_require_bins

  if [ -f "$QA_STATE" ]; then
    # shellcheck disable=SC1090
    source "$QA_STATE"
    if [ -n "${QA_XVFB_PID:-}" ] && kill -0 "$QA_XVFB_PID" 2>/dev/null; then
      echo "qa_display: already running on DISPLAY=${DISPLAY:-?} (state $QA_STATE)" >&2
      return 0
    fi
    echo "qa_display: stale state file — cleaning" >&2
    qa_display_stop || true
  fi

  local n
  n="$(_qa_pick_display)"

  # Drop stale gvfs/portal mounts under a previous runtime dir (busy mounts
  # make `rm -rf` fail and leave the session unusable).
  if [ -d "$QA_ROOT/runtime" ]; then
    fusermount -uz "$QA_ROOT/runtime/gvfs" 2>/dev/null || true
    fusermount -uz "$QA_ROOT/runtime/doc" 2>/dev/null || true
  fi
  rm -rf "$QA_ROOT" 2>/dev/null || {
    # Last resort: keep the tree but wipe app data dirs.
    rm -rf "$QA_ROOT/share" "$QA_ROOT/config" "$QA_ROOT/cache" 2>/dev/null || true
  }
  mkdir -p \
    "$QA_ROOT/share/io.bellman.desktop/logs" \
    "$QA_ROOT/share/io.bellman.desktop/slots" \
    "$QA_ROOT/config" \
    "$QA_ROOT/runtime" \
    "$QA_ROOT/cache"
  chmod 700 "$QA_ROOT/runtime"

  # Wizard already completed; start_minimized=false so the main window maps.
  printf '%s\n' \
    '{"wizard_completed":true,"autostart_enabled":false,"start_minimized":false,"wake_enabled":false}' \
    > "$QA_ROOT/share/io.bellman.desktop/config.json"

  local disp=":${n}"
  # Xvfb: 1280x800 so large-layout shots fit; +extension GLX for WebKit.
  Xvfb "$disp" -screen 0 1280x800x24 -ac \
    +extension RANDR +extension GLX -nolisten tcp \
    >"$QA_ROOT/xvfb.log" 2>&1 &
  local xvfb_pid=$!
  sleep 0.4
  if ! kill -0 "$xvfb_pid" 2>/dev/null; then
    echo "qa_display: Xvfb failed to start on $disp — see $QA_ROOT/xvfb.log" >&2
    cat "$QA_ROOT/xvfb.log" >&2 || true
    return 1
  fi

  DISPLAY="$disp" metacity --sm-disable --replace \
    >"$QA_ROOT/metacity.log" 2>&1 &
  local meta_pid=$!
  sleep 0.4
  if ! kill -0 "$meta_pid" 2>/dev/null; then
    echo "qa_display: metacity failed — see $QA_ROOT/metacity.log" >&2
    kill "$xvfb_pid" 2>/dev/null || true
    rm -f "/tmp/.X${n}-lock"
    return 1
  fi

  # Export for the calling shell / child processes.
  export DISPLAY="$disp"
  export XDG_DATA_HOME="$QA_ROOT/share"
  export XDG_CONFIG_HOME="$QA_ROOT/config"
  export XDG_RUNTIME_DIR="$QA_ROOT/runtime"
  export XDG_CACHE_HOME="$QA_ROOT/cache"
  export XDG_CURRENT_DESKTOP="${XDG_CURRENT_DESKTOP:-GNOME}"
  export GDK_BACKEND=x11
  # Avoid gvfs FUSE mounts under our private XDG_RUNTIME_DIR (they block teardown).
  export GIO_USE_VFS=local
  export GTK_USE_PORTAL=0
  # Software GL under Xvfb — DRI may work (sami has ACL on /dev/dri) but
  # software is the reliable path and does not touch the operator's GPU session.
  export LIBGL_ALWAYS_SOFTWARE="${LIBGL_ALWAYS_SOFTWARE:-1}"
  export WEBKIT_DISABLE_COMPOSITING_MODE="${WEBKIT_DISABLE_COMPOSITING_MODE:-1}"
  export BELLMAN_QA_DATA="$QA_ROOT/share/io.bellman.desktop"
  export BELLMAN_QA_ROOT="$QA_ROOT"

  cat >"$QA_STATE" <<EOF
# bellman QA isolated display — generated by qa_display.sh
QA_XVFB_PID=$xvfb_pid
QA_META_PID=$meta_pid
QA_DISPLAY_NUM=$n
DISPLAY=$disp
XDG_DATA_HOME=$QA_ROOT/share
XDG_CONFIG_HOME=$QA_ROOT/config
XDG_RUNTIME_DIR=$QA_ROOT/runtime
XDG_CACHE_HOME=$QA_ROOT/cache
XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-GNOME}
GDK_BACKEND=x11
LIBGL_ALWAYS_SOFTWARE=${LIBGL_ALWAYS_SOFTWARE:-1}
WEBKIT_DISABLE_COMPOSITING_MODE=${WEBKIT_DISABLE_COMPOSITING_MODE:-1}
BELLMAN_QA_DATA=$QA_ROOT/share/io.bellman.desktop
BELLMAN_QA_ROOT=$QA_ROOT
EOF

  echo "qa_display: started DISPLAY=$disp (Xvfb pid=$xvfb_pid, metacity pid=$meta_pid)"
  echo "qa_display: data=$BELLMAN_QA_DATA state=$QA_STATE"
}

qa_display_stop() {
  local xvfb_pid="" meta_pid="" n=""
  if [ -f "$QA_STATE" ]; then
    # shellcheck disable=SC1090
    source "$QA_STATE" || true
    xvfb_pid="${QA_XVFB_PID:-}"
    meta_pid="${QA_META_PID:-}"
    n="${QA_DISPLAY_NUM:-}"
  fi

  if [ -n "$meta_pid" ]; then
    kill "$meta_pid" 2>/dev/null || true
  fi
  if [ -n "$xvfb_pid" ]; then
    kill "$xvfb_pid" 2>/dev/null || true
    # Wait briefly so the lock releases.
    for _ in 1 2 3 4 5; do
      kill -0 "$xvfb_pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -9 "$xvfb_pid" 2>/dev/null || true
  fi
  if [ -n "$n" ]; then
    rm -f "/tmp/.X${n}-lock"
  fi
  rm -f "$QA_STATE"
  echo "qa_display: stopped"
}

qa_display_status() {
  if [ ! -f "$QA_STATE" ]; then
    echo "qa_display: not running (no $QA_STATE)"
    return 1
  fi
  # shellcheck disable=SC1090
  source "$QA_STATE"
  local ok=0
  if [ -n "${QA_XVFB_PID:-}" ] && kill -0 "$QA_XVFB_PID" 2>/dev/null; then
    echo "qa_display: Xvfb pid=$QA_XVFB_PID DISPLAY=${DISPLAY:-?} OK"
  else
    echo "qa_display: Xvfb NOT running (stale state?)"
    ok=1
  fi
  if [ -n "${QA_META_PID:-}" ] && kill -0 "$QA_META_PID" 2>/dev/null; then
    echo "qa_display: metacity pid=$QA_META_PID OK"
  else
    echo "qa_display: metacity NOT running"
    ok=1
  fi
  echo "qa_display: BELLMAN_QA_DATA=${BELLMAN_QA_DATA:-?}"
  return $ok
}

# Print env assignments suitable for `eval "$(scripts/qa_display.sh env)"`.
qa_display_env() {
  if [ ! -f "$QA_STATE" ]; then
    echo "qa_display: not running" >&2
    return 1
  fi
  # shellcheck disable=SC1090
  source "$QA_STATE"
  cat <<EOF
export DISPLAY=$(printf %q "$DISPLAY")
export XDG_DATA_HOME=$(printf %q "$XDG_DATA_HOME")
export XDG_CONFIG_HOME=$(printf %q "$XDG_CONFIG_HOME")
export XDG_RUNTIME_DIR=$(printf %q "$XDG_RUNTIME_DIR")
export XDG_CACHE_HOME=$(printf %q "$XDG_CACHE_HOME")
export XDG_CURRENT_DESKTOP=$(printf %q "${XDG_CURRENT_DESKTOP:-GNOME}")
export GDK_BACKEND=x11
export GIO_USE_VFS=local
export GTK_USE_PORTAL=0
export LIBGL_ALWAYS_SOFTWARE=$(printf %q "${LIBGL_ALWAYS_SOFTWARE:-1}")
export WEBKIT_DISABLE_COMPOSITING_MODE=$(printf %q "${WEBKIT_DISABLE_COMPOSITING_MODE:-1}")
export BELLMAN_QA_DATA=$(printf %q "$BELLMAN_QA_DATA")
export BELLMAN_QA_ROOT=$(printf %q "$BELLMAN_QA_ROOT")
EOF
}

# Clean a known-stale crew Xvfb on :99 if present (idempotent, best-effort).
qa_display_cleanup_stale_99() {
  if [ -f /tmp/.X99-lock ]; then
    local pid
    pid=$(tr -d ' \n' </tmp/.X99-lock || true)
    if [ -n "$pid" ] && ps -p "$pid" -o cmd= 2>/dev/null | grep -q Xvfb; then
      echo "qa_display: killing stale Xvfb :99 pid=$pid"
      kill "$pid" 2>/dev/null || true
      sleep 0.3
      kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f /tmp/.X99-lock
  fi
}

# When executed (not sourced), dispatch on argv.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  cmd="${1:-}"
  case "$cmd" in
    start) qa_display_start ;;
    stop) qa_display_stop ;;
    status) qa_display_status ;;
    env) qa_display_env ;;
    cleanup-stale) qa_display_cleanup_stale_99 ;;
    *)
      echo "Usage: $0 start|stop|status|env|cleanup-stale" >&2
      exit 2
      ;;
  esac
fi
