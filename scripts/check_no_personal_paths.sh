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
#   2. personal-token check — ANY occurrence of a PERSONAL_TOKENS entry
#      (the author's username/hostname) is a leak, wherever it appears
#      (uname strings, ls -l owner/group, comments, fixtures). This one has
#      no per-file exemption, on purpose: see the note above PERSONAL_TOKENS.

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
# file names them by necessity: it splits the literal so a plain grep for
# the token does not match the gate itself, and excludes itself from the
# scan regardless.)
PERSONAL_TOKENS=(
  "sa""mi"   # author's username; also the prefix of the machine hostname
)

# There is deliberately NO per-file allowlist for the token scan. There used
# to be one, for two fixtures that used the token as an example cron/at owner
# name; C11 changed those fixtures to `alice`, which left the exemptions
# covering nothing while still permitting the token to come back in exactly
# the two files that had just been cleaned. An allowlist outliving its reason
# is a hole that reports "clean".
#
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
    echo "personal-token leak ($token): $line" >&2
    fail=1
  done <<< "$hits"
done

if [[ $fail -ne 0 ]]; then
  cat >&2 <<'EOF'

Tracked files must not contain absolute home-directory paths or the
author's personal identifiers (username/hostname). Replace them with a
placeholder (/home/tester or 'tester' for QA evidence, /home/you in
documentation, 'alice' for an example cron/at owner) or a relative path.

A genuinely impersonal PATH can be added to ALLOWLIST in
scripts/check_no_personal_paths.sh. There is no per-file exemption for the
TOKEN scan and adding one back is not the fix: a fixture that needs a
username needs a fictional one, not permission to use this machine's.
EOF
  exit 1
fi

echo "personal-path gate: clean"
