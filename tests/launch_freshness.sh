#!/usr/bin/env bash
# Headless launcher selection / freshness tests (isolated temp fixture).
# Never opens a GUI; uses BELLMAN_LAUNCH_DRY_RUN=1.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LAUNCH_SRC="$ROOT/launch.sh"
PASS=0
FAIL=0

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"
  if printf '%s' "$haystack" | grep -q -- "$needle"; then
    echo "  PASS: $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $label"
    echo "    expected to contain: $needle"
    echo "    got: $haystack"
    FAIL=$((FAIL + 1))
  fi
}

assert_eq() {
  local got="$1"
  local want="$2"
  local label="$3"
  if [ "$got" = "$want" ]; then
    echo "  PASS: $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $label (got='$got' want='$want')"
    FAIL=$((FAIL + 1))
  fi
}

# Portable touch for epoch seconds.
touch_epoch() {
  local path="$1"
  local epoch="$2"
  if touch -d "@${epoch}" "$path" 2>/dev/null; then
    return 0
  fi
  # BSD fallback
  touch -t "$(date -r "$epoch" '+%Y%m%d%H%M.%S' 2>/dev/null || date -d "@$epoch" '+%Y%m%d%H%M.%S')" "$path"
}

make_fixture() {
  local fx="$1"
  mkdir -p \
    "$fx/src-tauri/src" \
    "$fx/src-tauri/icons" \
    "$fx/crates/bellman-core/src" \
    "$fx/ui/src" \
    "$fx/target/release" \
    "$fx/target/debug" \
    "$fx/src-tauri/target/release"
  cp "$LAUNCH_SRC" "$fx/launch.sh"
  chmod +x "$fx/launch.sh"
  # Minimal source stamp inputs.
  echo 'fn main() {}' >"$fx/src-tauri/src/main.rs"
  echo '[package] name="x"' >"$fx/src-tauri/Cargo.toml"
  echo '{}' >"$fx/src-tauri/tauri.conf.json"
  echo '[workspace]' >"$fx/Cargo.toml"
  echo '# lock' >"$fx/Cargo.lock"
  echo 'export let x = 1;' >"$fx/ui/src/app.js"
  echo '{}' >"$fx/ui/package.json"
  # Fake binaries (shell stubs — never executed under dry-run).
  printf '#!/bin/sh\necho fake-release\n' >"$fx/target/release/bellman-app"
  printf '#!/bin/sh\necho fake-debug\n' >"$fx/target/debug/bellman-app"
  printf '#!/bin/sh\necho fake-src-tauri-release\n' >"$fx/src-tauri/target/release/bellman-app"
  chmod +x \
    "$fx/target/release/bellman-app" \
    "$fx/target/debug/bellman-app" \
    "$fx/src-tauri/target/release/bellman-app"
}

run_select() {
  local fx="$1"
  shift
  # shellcheck disable=SC2086
  env -i \
    PATH="/usr/bin:/bin:${CARGO_HOME:-$HOME/.cargo}/bin" \
    HOME="$HOME" \
    BELLMAN_LAUNCH_DRY_RUN=1 \
    BELLMAN_SKIP_REBUILD=1 \
    "$@" \
    bash "$fx/launch.sh" 2>/dev/null
}

echo "=== launch freshness tests ==="

# --- 1. Fresh release binary preferred ---------------------------------
FX=$(mktemp -d)
trap 'rm -rf "$FX" "$FX2" "$FX3" "$FX4" "$FX5" "$FX6" 2>/dev/null || true' EXIT
make_fixture "$FX"
# Source old, binaries newer.
touch_epoch "$FX/src-tauri/src/main.rs" 1000000000
touch_epoch "$FX/Cargo.toml" 1000000000
touch_epoch "$FX/Cargo.lock" 1000000000
touch_epoch "$FX/src-tauri/Cargo.toml" 1000000000
touch_epoch "$FX/src-tauri/tauri.conf.json" 1000000000
touch_epoch "$FX/ui/src/app.js" 1000000000
touch_epoch "$FX/ui/package.json" 1000000000
touch_epoch "$FX/target/release/bellman-app" 2000000000
touch_epoch "$FX/target/debug/bellman-app" 2000000000
OUT=$(run_select "$FX")
assert_contains "$OUT" "action=exec" "fresh release: action=exec"
assert_contains "$OUT" "freshness=fresh" "fresh release: freshness=fresh"
assert_contains "$OUT" "path=$FX/target/release/bellman-app" "fresh release: prefers target/release"

# --- 2. Stale release refused without opt-in (rebuild-or-dev) ----------
FX2=$(mktemp -d)
make_fixture "$FX2"
touch_epoch "$FX2/src-tauri/src/main.rs" 3000000000
touch_epoch "$FX2/Cargo.toml" 3000000000
touch_epoch "$FX2/Cargo.lock" 3000000000
touch_epoch "$FX2/src-tauri/Cargo.toml" 3000000000
touch_epoch "$FX2/src-tauri/tauri.conf.json" 3000000000
touch_epoch "$FX2/ui/src/app.js" 3000000000
touch_epoch "$FX2/ui/package.json" 3000000000
touch_epoch "$FX2/target/release/bellman-app" 1000000000
touch_epoch "$FX2/target/debug/bellman-app" 1000000000
touch_epoch "$FX2/src-tauri/target/release/bellman-app" 1000000000
OUT=$(run_select "$FX2")
assert_contains "$OUT" "action=rebuild-or-dev" "stale without allow: rebuild-or-dev"
assert_contains "$OUT" "freshness=stale" "stale without allow: freshness=stale"
assert_contains "$OUT" "path=$FX2/target/release/bellman-app" "stale without allow: still reports best path"

# --- 3. Stale reuse only with explicit opt-in --------------------------
FX3=$(mktemp -d)
make_fixture "$FX3"
touch_epoch "$FX3/src-tauri/src/main.rs" 3000000000
touch_epoch "$FX3/Cargo.toml" 3000000000
touch_epoch "$FX3/Cargo.lock" 3000000000
touch_epoch "$FX3/src-tauri/Cargo.toml" 3000000000
touch_epoch "$FX3/src-tauri/tauri.conf.json" 3000000000
touch_epoch "$FX3/ui/src/app.js" 3000000000
touch_epoch "$FX3/ui/package.json" 3000000000
touch_epoch "$FX3/target/release/bellman-app" 1000000000
OUT=$(run_select "$FX3" BELLMAN_ALLOW_STALE=1)
assert_contains "$OUT" "action=exec" "allow_stale: action=exec"
assert_contains "$OUT" "freshness=stale" "allow_stale: freshness=stale"
assert_contains "$OUT" "allow_stale=1" "allow_stale: flag in decision"
assert_contains "$OUT" "path=$FX3/target/release/bellman-app" "allow_stale: uses release path"

# --- 4. Prefer release over debug when both fresh ----------------------
FX4=$(mktemp -d)
make_fixture "$FX4"
touch_epoch "$FX4/src-tauri/src/main.rs" 1000000000
touch_epoch "$FX4/Cargo.toml" 1000000000
touch_epoch "$FX4/Cargo.lock" 1000000000
touch_epoch "$FX4/src-tauri/Cargo.toml" 1000000000
touch_epoch "$FX4/src-tauri/tauri.conf.json" 1000000000
touch_epoch "$FX4/ui/src/app.js" 1000000000
touch_epoch "$FX4/ui/package.json" 1000000000
touch_epoch "$FX4/target/debug/bellman-app" 2000000000
touch_epoch "$FX4/target/release/bellman-app" 2000000000
OUT=$(run_select "$FX4")
assert_contains "$OUT" "path=$FX4/target/release/bellman-app" "priority: release over debug"

# --- 5. Fall through to debug when release missing, debug fresh --------
FX5=$(mktemp -d)
make_fixture "$FX5"
rm -f "$FX5/target/release/bellman-app"
touch_epoch "$FX5/src-tauri/src/main.rs" 1000000000
touch_epoch "$FX5/Cargo.toml" 1000000000
touch_epoch "$FX5/Cargo.lock" 1000000000
touch_epoch "$FX5/src-tauri/Cargo.toml" 1000000000
touch_epoch "$FX5/src-tauri/tauri.conf.json" 1000000000
touch_epoch "$FX5/ui/src/app.js" 1000000000
touch_epoch "$FX5/ui/package.json" 1000000000
touch_epoch "$FX5/target/debug/bellman-app" 2000000000
OUT=$(run_select "$FX5")
assert_contains "$OUT" "action=exec" "debug-only fresh: exec"
assert_contains "$OUT" "path=$FX5/target/debug/bellman-app" "debug-only fresh: debug path"

# --- 6. No binaries → tauri-dev ----------------------------------------
FX6=$(mktemp -d)
make_fixture "$FX6"
rm -f \
  "$FX6/target/release/bellman-app" \
  "$FX6/target/debug/bellman-app" \
  "$FX6/src-tauri/target/release/bellman-app"
OUT=$(run_select "$FX6")
assert_contains "$OUT" "action=tauri-dev" "missing binaries: tauri-dev"
assert_contains "$OUT" "freshness=missing" "missing binaries: freshness=missing"

# --- 7. BELLMAN_APP_ALLOW_STALE alias ----------------------------------
OUT=$(run_select "$FX3" BELLMAN_APP_ALLOW_STALE=1)
assert_contains "$OUT" "allow_stale=1" "BELLMAN_APP_ALLOW_STALE alias works"

echo "=== results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
