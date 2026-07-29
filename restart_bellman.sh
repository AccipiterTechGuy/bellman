#!/usr/bin/env bash
# Restart only Bellman GUI processes launched from this checkout.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$REPO/launch.sh"

die() {
  echo "restart_bellman: $*" >&2
  exit 1
}

is_repo_app() {
  local pid="$1"
  local exe
  exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
  case "$exe" in
    "$REPO/target/release/bellman-app" | \
    "$REPO/target/release/bellman-app (deleted)" | \
    "$REPO/target/debug/bellman-app" | \
    "$REPO/target/debug/bellman-app (deleted)" | \
    "$REPO/src-tauri/target/release/bellman-app" | \
    "$REPO/src-tauri/target/release/bellman-app (deleted)" | \
    "$REPO/src-tauri/target/debug/bellman-app" | \
    "$REPO/src-tauri/target/debug/bellman-app (deleted)")
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

[ -x "$LAUNCHER" ] || die "launcher is missing or not executable: $LAUNCHER"

repo_pids=()
while IFS= read -r pid; do
  if is_repo_app "$pid"; then
    repo_pids+=("$pid")
  fi
done < <(pgrep -x bellman-app 2>/dev/null || true)

if ((${#repo_pids[@]} > 0)); then
  kill -TERM "${repo_pids[@]}"

  for _ in {1..50}; do
    still_running=false
    for pid in "${repo_pids[@]}"; do
      if kill -0 "$pid" 2>/dev/null && is_repo_app "$pid"; then
        still_running=true
        break
      fi
    done
    "$still_running" || break
    sleep 0.1
  done

  for pid in "${repo_pids[@]}"; do
    if kill -0 "$pid" 2>/dev/null && is_repo_app "$pid"; then
      kill -KILL "$pid"
    fi
  done
fi

restart_log="$(mktemp "${TMPDIR:-/tmp}/bellman-restart-XXXXXX.log")"
nohup "$LAUNCHER" >"$restart_log" 2>&1 </dev/null &
launched_pid=$!

for _ in {1..30}; do
  if kill -0 "$launched_pid" 2>/dev/null && is_repo_app "$launched_pid"; then
    echo "Bellman restarted: pid=$launched_pid log=$restart_log"
    exit 0
  fi
  sleep 0.1
done

if [ -s "$restart_log" ]; then
  sed -n '1,80p' "$restart_log" >&2
fi
die "replacement process did not stay running (log: $restart_log)"
