#!/usr/bin/env bash
# run_gui_qa.sh — launch Bellman GUI QA on an isolated display with WebDriver.
#
# Never attaches to the operator session. Never uses global-input-injection.
#
# Usage:
#   scripts/run_gui_qa.sh              # run p4b (default)
#   scripts/run_gui_qa.sh p4b
#   scripts/run_gui_qa.sh p4d
#   scripts/run_gui_qa.sh p4e
#   scripts/run_gui_qa.sh p4f
#   scripts/run_gui_qa.sh all
#
# Env:
#   BELLMAN_APP   path to bellman-app (default: search release + deb-extract)
#   BELLMAN_CLI   path to CLI binary
#   BELLMAN_QA_PYTHON  python with selenium (default: /tmp/bellman-qa-venv/bin/python or python3)
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

# Resolve app binary.
if [ -z "${BELLMAN_APP:-}" ]; then
  for c in \
    "$ROOT/target/release/bellman-app" \
    /tmp/bellman-deb-extract/usr/bin/bellman-app \
    "$HOME/bellman/target/release/bellman-app"
  do
    if [ -x "$c" ]; then
      export BELLMAN_APP="$c"
      break
    fi
  done
fi
if [ -z "${BELLMAN_APP:-}" ] || [ ! -x "${BELLMAN_APP}" ]; then
  echo "run_gui_qa: bellman-app not found. Build with:" >&2
  echo "  cd ui && npm ci && npm run build && cd .. && cargo tauri build --no-bundle" >&2
  echo "Or set BELLMAN_APP=/path/to/bellman-app" >&2
  exit 1
fi

if [ -z "${BELLMAN_CLI:-}" ]; then
  for c in \
    "$ROOT/target/release/bellman" \
    /tmp/bellman-deb-extract/usr/bin/bellman \
    /tmp/bellman-cli-schema3
  do
    if [ -x "$c" ]; then
      # Prefer the CLI (small) over a mistaken GUI binary
      if "$c" --help 2>&1 | head -1 | grep -qi 'scheduler\|Usage'; then
        export BELLMAN_CLI="$c"
        break
      fi
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

# selenium check
if ! "$PY" -c "import selenium" 2>/dev/null; then
  echo "run_gui_qa: selenium not importable from $PY" >&2
  echo "  python3 -m venv /tmp/bellman-qa-venv && /tmp/bellman-qa-venv/bin/pip install selenium pillow python-xlib" >&2
  exit 1
fi

cleanup() {
  "$DISPLAY_SH" stop 2>/dev/null || true
}
trap cleanup EXIT

# Best-effort: free known-stale :99 from prior crews (does not touch :0).
"$DISPLAY_SH" cleanup-stale 2>/dev/null || true

"$DISPLAY_SH" start
# shellcheck disable=SC1090
eval "$("$DISPLAY_SH" env)"

# Clean env inheritance for the capture process: avoid polluted operator vars
# (keyring paths, portal sockets) while keeping our isolated XDG + DISPLAY.
export GDK_BACKEND=x11
export LIBGL_ALWAYS_SOFTWARE="${LIBGL_ALWAYS_SOFTWARE:-1}"
export WEBKIT_DISABLE_COMPOSITING_MODE="${WEBKIT_DISABLE_COMPOSITING_MODE:-1}"

echo "run_gui_qa: DISPLAY=$DISPLAY BELLMAN_APP=$BELLMAN_APP PY=$PY suite=$SUITE"

run_one() {
  local name="$1"
  local script="$ROOT/scripts/capture_qa_${name}.py"
  if [ ! -f "$script" ]; then
    echo "run_gui_qa: unknown suite $name ($script missing)" >&2
    return 1
  fi
  echo "==== running $name ===="
  # Fresh session data between suites (except first start already prepared config).
  if [ "$name" != "p4b" ] || [ "${SUITE}" = "all" ]; then
    # Keep same display; reset data dir contents for isolation between suites.
    rm -rf "${BELLMAN_QA_DATA:?}"/*
    mkdir -p "$BELLMAN_QA_DATA/logs" "$BELLMAN_QA_DATA/slots"
    printf '%s\n' \
      '{"wizard_completed":true,"autostart_enabled":false,"start_minimized":false,"wake_enabled":false}' \
      > "$BELLMAN_QA_DATA/config.json"
  fi
  "$PY" "$script"
}

case "$SUITE" in
  p4b|p4d|p4e|p4f) run_one "$SUITE" ;;
  all)
    for s in p4b p4d p4e p4f; do
      run_one "$s"
    done
    ;;
  *)
    echo "Usage: $0 [p4b|p4d|p4e|p4f|all]" >&2
    exit 2
    ;;
esac

echo "run_gui_qa: suite $SUITE finished OK"
