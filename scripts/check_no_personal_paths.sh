#!/usr/bin/env bash
# Personal-path hygiene gate (SHIP1-G).
#
# The repository is public. Absolute home-directory paths (/home/<user>,
# /Users/<user>) leak a username and machine layout to strangers — 11
# tracked QA-evidence files once carried /home/sami. This gate fails the
# build if any tracked file contains such a path outside the explicit
# placeholder allowlist below, so the leak cannot silently return.
#
# Runs against TRACKED files only (git grep), so local scratch is unaffected.

set -euo pipefail
cd "$(dirname "$0")/.."

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

# Directories excluded from the scan: docs/todo and docs/archive are
# process/history records — they legitimately QUOTE past leaks (the SHIP1
# card itself names the /home/sami incident it mandated fixing). Shipping
# docs, code, tests and evidence are all still scanned.
matches="$(git grep -nIEo '/(home|Users)/[A-Za-z0-9._-]+' -- . ':(exclude)docs/todo/**' ':(exclude)docs/archive/**' || true)"

fail=0
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

if [[ $fail -ne 0 ]]; then
  cat >&2 <<'EOF'

Tracked files must not contain absolute home-directory paths.
Replace them with a placeholder (/home/tester for QA evidence, /home/you
in documentation) or a relative path, or extend the allowlist in
scripts/check_no_personal_paths.sh if the path is genuinely impersonal.
EOF
  exit 1
fi

echo "personal-path gate: clean"
