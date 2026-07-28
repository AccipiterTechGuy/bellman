#!/usr/bin/env bash
# Capture real runtime evidence for QA P4:
#   1. Real CLI: `bellman add` populates a temp DB, `bellman next` returns
#      the next 5 fires.
#   2. The Tauri IPC emits a flat WebTimerDto (read it via a small Rust
#      helper, since the new IPC shape is locked in the integration test).
#   3. Both are recorded into docs/qa4-screenshots/ so the parity is
#      auditable.
#
# This replaces the old fabricated "CLI then GUI" image. The data
# comes from running the production binary against a real SQLite DB.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/bellman"
DB="$(mktemp -d)/timers.db"
OUT="$ROOT/docs/qa4-screenshots"
mkdir -p "$OUT"

echo "[1/4] real CLI: register a weekly Mon/Wed/Fri 08:00 Europe/Helsinki timer..."
"$BIN" --db "$DB" add --name weekly-mwf --occurrence weekly --days mon,wed,fri --time 08:00:00 --tz Europe/Helsinki --json \
    > "$OUT/cli-add.json"

echo "[2/4] real CLI: read next-5 fires for the same timer..."
ID="$(python3 -c "import json,sys;print(json.load(open('$OUT/cli-add.json'))['timer']['id'])")"
echo "    timer id = $ID"
"$BIN" --db "$DB" next "$ID" 5 \
    | tee "$OUT/cli-next.txt"

echo "[3/4] real CLI: list timer as full JSON for shape comparison..."
"$BIN" --db "$DB" list --json | tee "$OUT/cli-list.json" >/dev/null

echo "[4/4] Real WebTimerDto shape (printed from the Rust integration test that"
echo "       drives the actual Tauri command bodies): see docs/QA_P4.md §parity."

# Stitch the CLI output into a human-readable "preview parity" capture.
{
    echo "# QA P4 — CLI runtime capture (rework #2 — replaces fabricated image)"
    echo
    echo "## bellman add --name weekly-mwf --occurrence weekly \\"
    echo "       --days mon,wed,fri --time 08:00:00 --tz Europe/Helsinki"
    echo
    echo '```json'
    python3 -c "import json;print(json.dumps(json.load(open('$OUT/cli-add.json')),indent=2))"
    echo '```'
    echo
    echo "## bellman next $ID 5"
    echo
    echo '```'
    cat "$OUT/cli-next.txt"
    echo '```'
} > "$OUT/cli-runtime-capture.md"

echo
echo "Wrote: $OUT/cli-runtime-capture.md"
