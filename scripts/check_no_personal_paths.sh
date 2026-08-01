#!/usr/bin/env bash
# Personal-path hygiene gate (SHIP1-G).
#
# The repository is public. Absolute home-directory paths (/home/<user>,
# /Users/<user>) and the author's personal identifiers (username, hostname)
# leak a name and machine layout to strangers — tracked QA-evidence files
# once carried the author's real home path, machine hostname, and file
# owner/group columns. This gate fails the build if any tracked file
# contains such a leak outside the explicit allowlists below, so it cannot
# silently return.
#
# Runs against TRACKED files only (git grep), so local scratch is unaffected.
#
# Two checks:
#   1. home-path check — any /home/<user> or /Users/<user> path component
#      outside ALLOWLIST is a leak;
#   2. personal-token check — any occurrence of a PERSONAL_TOKENS entry
#      (the author's username/hostname) outside TOKEN_ALLOWED_FILES is a
#      leak, wherever it appears (uname strings, ls -l owner/group, …).

set -euo pipefail
cd "$(dirname "$0")/.."

# ── Check 1: home-directory paths ────────────────────────────────────────

# Allowed placeholder roots (exact username component match only).
ALLOWLIST=(
  "/home/you"      # docs placeholder for "your home directory"
  "/home/tester"   # redacted QA-evidence placeholder
  "/home/me"       # crontab-parser test fixture placeholder
  "/home/alice"    # json_normalization.md example user
  "/home/u"        # demo/test fixture placeholder
  "/home/runner"   # GitHub Actions runner user (CI logs/scripts)
  "/Users/runner"  # GitHub Actions macOS runner user
)

# ── Check 2: personal tokens (username / hostname) ───────────────────────

# The repo author's personal identifiers. They must never appear in tracked
# files — not in paths, uname strings, owner columns, or comments. (This
# file names them by necessity and excludes itself from the scan.)
PERSONAL_TOKENS=(
  "sami"   # author's username; also the prefix of the machine hostname
)

# Pre-existing generic fixtures that use the token as an EXAMPLE cron/at
# owner name (not this machine): task-id determinism tests and the at(1)
# output parser fixture. Allowlisted by exact file; new hits elsewhere fail.
TOKEN_ALLOWED_FILES=(
  "crates/bellman-core/src/visible/id.rs"
  "crates/bellman-core/src/visible/providers/at.rs"
)

# Directories excluded from both scans: docs/todo and docs/archive are
# process/history records — they legitimately QUOTE past leaks (the SHIP1
# card itself names the leaked-home-path incident it mandated fixing).
# Shipping docs, code, tests and evidence are all still scanned.
EXCLUDES=(
  ':(exclude)docs/todo/**'
  ':(exclude)docs/archive/**'
)

fail=0

matches="$(git grep -nIEo '/(home|Users)/[A-Za-z0-9._-]+' -- . "${EXCLUDES[@]}" || true)"
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  token="${line##*:}"
  allowed=0
  for a in "${ALLOWLIST[@]}"; do
    if [[ "$token" == "$a" ]]; then
      allowed=1
      break
    fi
  done
  if [[ $allowed -eq 0 ]]; then
    echo "personal-path leak: $line" >&2
    fail=1
  fi
done <<< "$matches"

for token in "${PERSONAL_TOKENS[@]}"; do
  hits="$(git grep -nF "$token" -- . "${EXCLUDES[@]}" ':(exclude)scripts/check_no_personal_paths.sh' || true)"
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    file="${line%%:*}"
    allowed=0
    for f in "${TOKEN_ALLOWED_FILES[@]}"; do
      if [[ "$file" == "$f" ]]; then
        allowed=1
        break
      fi
    done
    if [[ $allowed -eq 0 ]]; then
      echo "personal-token leak ($token): $line" >&2
      fail=1
    fi
  done <<< "$hits"
done

if [[ $fail -ne 0 ]]; then
  cat >&2 <<'EOF'

Tracked files must not contain absolute home-directory paths or the
author's personal identifiers (username/hostname). Replace them with a
placeholder (/home/tester or 'tester' for QA evidence, /home/you in
documentation) or a relative path, or extend the allowlists in
scripts/check_no_personal_paths.sh if the hit is genuinely impersonal.
EOF
  exit 1
fi

echo "personal-path gate: clean"
