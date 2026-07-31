#!/usr/bin/env bash
# run_gui_qa.sh — launch Bellman GUI QA on an isolated display with WebDriver.
#
# Never attaches to the operator session. Never uses global pointer/keyboard injection.
#
# Usage:
#   scripts/run_gui_qa.sh              # run p4b (default)
#   scripts/run_gui_qa.sh p4b
#   scripts/run_gui_qa.sh p4d
#   scripts/run_gui_qa.sh p4e
#   scripts/run_gui_qa.sh p4f
#   scripts/run_gui_qa.sh wiz1
#   scripts/run_gui_qa.sh all
#
# Env:
#   BELLMAN_APP   path to bellman-app (MUST be from this tree for p4f/Settings)
#   BELLMAN_APP_ALLOW_STALE=1  allow /tmp/bellman-deb-extract fallback (discouraged)
#   BELLMAN_CLI   path to CLI binary
#   BELLMAN_QA_PYTHON  python with selenium
#   BELLMAN_QA_ROOT    session root (default /tmp/bellman-qa-session)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

SUITE="${1:-p4b}"
DISPLAY_SH="$ROOT/scripts/qa_display.sh"

# Prefer a venv that has selenium; fall back to python3.
if [ -n "${BELLMAN_QA_PYTHON:-}" ]; then
  PY="$BELLMAN_QA_PYTHON"
elif [ -x /tmp/bellman-qa-venv/bin/python ]; then
  PY=/tmp/bellman-qa-venv/bin/python
elif command -v python3 >/dev/null; then
  PY=python3
else
  echo "run_gui_qa: no python3" >&2
  exit 1
fi

# Resolve app binary — prefer this worktree / CARGO_TARGET_DIR, then HOME.
# The /tmp/bellman-deb-extract fallback is gated: it is often an older binary
# without Settings (breaks p4f) and must not be used silently.
if [ -z "${BELLMAN_APP:-}" ]; then
  for c in \
    "${CARGO_TARGET_DIR:-}/release/bellman-app" \
    "$ROOT/target/release/bellman-app" \
    "$HOME/bellman/target/release/bellman-app"
  do
    [ -n "$c" ] && [ -x "$c" ] || continue
    export BELLMAN_APP="$c"
    break
  done
fi
if [ -z "${BELLMAN_APP:-}" ] || [ ! -x "${BELLMAN_APP}" ]; then
  if [ "${BELLMAN_APP_ALLOW_STALE:-}" = "1" ] && [ -x /tmp/bellman-deb-extract/usr/bin/bellman-app ]; then
    export BELLMAN_APP=/tmp/bellman-deb-extract/usr/bin/bellman-app
    echo "run_gui_qa: WARNING using stale /tmp/bellman-deb-extract binary (BELLMAN_APP_ALLOW_STALE=1)" >&2
  else
    echo "run_gui_qa: bellman-app not found in this tree. Build with:" >&2
    echo "  cd ui && npm ci && npm run build && cd .." >&2
    echo "  cargo build -p bellman-app --release --features custom-protocol --manifest-path src-tauri/Cargo.toml" >&2
    echo "Or: cargo tauri build --no-bundle" >&2
    echo "Or set BELLMAN_APP=/path/to/current/bellman-app" >&2
    echo "(Refusing silent /tmp/bellman-deb-extract fallback — set BELLMAN_APP_ALLOW_STALE=1 only if intentional.)" >&2
    exit 1
  fi
fi

if [ -z "${BELLMAN_CLI:-}" ]; then
  for c in \
    "${CARGO_TARGET_DIR:-}/release/bellman" \
    "$ROOT/target/release/bellman" \
    /tmp/bellman-deb-extract/usr/bin/bellman \
    /tmp/bellman-cli-schema3
  do
    [ -n "$c" ] && [ -x "$c" ] || continue
    if "$c" --help 2>&1 | head -1 | grep -qi 'scheduler\|Usage'; then
      export BELLMAN_CLI="$c"
      break
    fi
  done
fi

# Preconditions
command -v WebKitWebDriver >/dev/null || {
  echo "run_gui_qa: WebKitWebDriver missing — sudo apt install -y webkit2gtk-driver" >&2
  exit 1
}
command -v tauri-driver >/dev/null || [ -x "$HOME/.cargo/bin/tauri-driver" ] || {
  echo "run_gui_qa: tauri-driver missing — cargo install tauri-driver --locked" >&2
  exit 1
}
export TAURI_DRIVER="${TAURI_DRIVER:-$HOME/.cargo/bin/tauri-driver}"
export PATH="$HOME/.cargo/bin:/usr/bin:/bin:${PATH:-}"

if ! "$PY" -c "import selenium" 2>/dev/null; then
  echo "run_gui_qa: selenium not importable from $PY" >&2
  echo "  python3 -m venv /tmp/bellman-qa-venv && /tmp/bellman-qa-venv/bin/pip install selenium pillow python-xlib" >&2
  exit 1
fi

_reap_qa_drivers() {
  # Kill any leftover tauri-driver / WebKitWebDriver whose env marks QA ownership,
  # and any still-listening auto ports from this session. Best-effort.
  local p cmd
  while read -r p cmd; do
    case "$cmd" in
      *tauri-driver*|*WebKitWebDriver*)
        if [ -r "/proc/$p/environ" ] && \
           tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -qE 'BELLMAN_QA|bellman-qa|qa-p4'; then
          kill -TERM "$p" 2>/dev/null || true
        fi
        ;;
    esac
  done < <(ps -eo pid=,args= 2>/dev/null || true)
  sleep 0.3
  while read -r p cmd; do
    case "$cmd" in
      *tauri-driver*|*WebKitWebDriver*)
        if [ -r "/proc/$p/environ" ] && \
           tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -qE 'BELLMAN_QA|bellman-qa|qa-p4'; then
          kill -KILL "$p" 2>/dev/null || true
        fi
        ;;
    esac
  done < <(ps -eo pid=,args= 2>/dev/null || true)
}

cleanup() {
  # Prefer Python-side stop_session (process group); then display; then sweep.
  "$PY" -c "
import sys
sys.path.insert(0, r'$ROOT/scripts')
try:
    import qa_webdriver as q
    q.stop_session()
except Exception:
    pass
" 2>/dev/null || true
  "$DISPLAY_SH" stop 2>/dev/null || true
  _reap_qa_drivers
}
trap cleanup EXIT

# Best-effort: free known-stale :99 from prior crews (does not touch operator display).
"$DISPLAY_SH" cleanup-stale 2>/dev/null || true

"$DISPLAY_SH" start
# shellcheck disable=SC1090
eval "$("$DISPLAY_SH" env)"

export GDK_BACKEND=x11
export LIBGL_ALWAYS_SOFTWARE="${LIBGL_ALWAYS_SOFTWARE:-1}"
export WEBKIT_DISABLE_COMPOSITING_MODE="${WEBKIT_DISABLE_COMPOSITING_MODE:-1}"
export GIO_USE_VFS=local
export GTK_USE_PORTAL=0

echo "run_gui_qa: DISPLAY=$DISPLAY BELLMAN_APP=$BELLMAN_APP PY=$PY suite=$SUITE"

run_one() {
  local name="$1"
  local script="$ROOT/scripts/capture_qa_${name}.py"
  if [ ! -f "$script" ]; then
    echo "run_gui_qa: unknown suite $name ($script missing)" >&2
    return 1
  fi
  echo "==== running $name ===="
  # Fresh session data between suites.
  if [ "$name" != "p4b" ] || [ "${SUITE}" = "all" ]; then
    rm -rf "${BELLMAN_QA_DATA:?}"/*
    mkdir -p "$BELLMAN_QA_DATA/logs" "$BELLMAN_QA_DATA/slots"
    printf '%s\n' \
      '{"wizard_completed":true,"autostart_enabled":false,"start_minimized":false,"wake_enabled":false}' \
      > "$BELLMAN_QA_DATA/config.json"
  fi
  "$PY" "$script"
  # Ensure driver group is gone between suites (not only at EXIT).
  "$PY" -c "
import sys
sys.path.insert(0, r'$ROOT/scripts')
try:
    import qa_webdriver as q
    q.stop_session()
except Exception:
    pass
" 2>/dev/null || true
  _reap_qa_drivers
}

case "$SUITE" in
  p4b|p4d|p4e|p4f|wiz1) run_one "$SUITE" ;;
  all)
    for s in p4b p4d p4e p4f; do
      run_one "$s"
    done
    ;;
  *)
    echo "Usage: $0 [p4b|p4d|p4e|p4f|wiz1|all]" >&2
    exit 2
    ;;
esac

echo "run_gui_qa: suite $SUITE finished OK"
