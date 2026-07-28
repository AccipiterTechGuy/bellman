#!/usr/bin/env bash
# Measure engine idle footprint (P5 acceptance).
#
# Usage:
#   scripts/perf_idle.sh              # 600 s (10 min) acceptance window
#   PERF_SECS=60 scripts/perf_idle.sh # smoke
#
# Evidence lands in docs/qa4-evidence/perf-idle/ (VmRSS samples + JSONL + report).

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

SECS="${PERF_SECS:-600}"
OUT="${PERF_OUT:-$ROOT/docs/qa4-evidence/perf-idle}"
mkdir -p "$OUT"
DATA="$OUT/run-data"
rm -rf "$DATA"
mkdir -p "$DATA"

echo "== building release perf_idle example =="
cargo build -p bellman-core --example perf_idle --release

echo "== running ${SECS}s idle window (engine + 1s timer + EventLog) =="
export BELLMAN_PERF_OUT="$OUT"
./target/release/examples/perf_idle --secs "$SECS" --data-dir "$DATA" | tee "$OUT/perf_idle.stdout"

# Snapshot final VmRSS of this shell is not the subject; the report is.
echo "== evidence =="
ls -la "$OUT"
if [[ -f "$OUT/perf_idle_report.json" ]]; then
  echo "--- report ---"
  cat "$OUT/perf_idle_report.json"
fi
if [[ -f "$OUT/events.current.jsonl" ]]; then
  FIRED=$(grep -cE '"kind":"fired"|"kind":"fired_late"' "$OUT/events.current.jsonl" || true)
  LINES=$(wc -l < "$OUT/events.current.jsonl")
  echo "event_log_lines=$LINES fired_count=$FIRED"
  echo "--- first 3 / last 3 event lines ---"
  head -n 3 "$OUT/events.current.jsonl" || true
  echo "..."
  tail -n 3 "$OUT/events.current.jsonl" || true
fi

# Try Tauri shell RSS only if a release binary already exists (C10 packaging
# builds it). Do NOT claim a measurement we did not take.
if [[ -x "$ROOT/target/release/bellman" ]] && file "$ROOT/target/release/bellman" | grep -qi 'ELF'; then
  # The workspace also names the CLI binary `bellman`; distinguish by size/path.
  echo "note: target/release/bellman exists — identify whether it is CLI or Tauri before claiming shell RSS"
else
  echo "note: Tauri shell release binary not present; resident-shell RSS deferred (see PERF.md)"
fi

echo "done."
