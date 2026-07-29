#!/usr/bin/env bash
# Safe local metadata hygiene for the Bellman git checkout.
#
# Only prunes *proven* stale records: git worktree registrations whose
# checkout directory is gone. Never deletes:
#   - active worktrees
#   - runtime data (~/.bellman)
#   - node_modules / ui dependencies
#   - target/ release or debug artifacts
#
# Usage:
#   scripts/repo_hygiene.sh           # prune + report
#   scripts/repo_hygiene.sh --dry-run # show what prune would do
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODE="prune"
for arg in "$@"; do
  case "$arg" in
    --dry-run) MODE="dry-run" ;;
    -h|--help)
      sed -n '2,14p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *)
      echo "repo_hygiene: unknown arg: $arg" >&2
      exit 2
      ;;
  esac
done

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "repo_hygiene: not a git checkout: $ROOT" >&2
  exit 1
fi

echo "repo_hygiene: root=$ROOT"
echo "repo_hygiene: worktrees before:"
git worktree list || true

case "$MODE" in
  dry-run)
    echo "repo_hygiene: dry-run — git worktree prune -n -v"
    git worktree prune -n -v || true
    ;;
  prune)
    echo "repo_hygiene: pruning absent worktree records (git worktree prune -v)"
    git worktree prune -v || true
    echo "repo_hygiene: worktrees after:"
    git worktree list || true
    ;;
esac

echo "repo_hygiene: preserved by design: target/, node_modules/, ~/.bellman/, active worktrees"
echo "repo_hygiene: done"
