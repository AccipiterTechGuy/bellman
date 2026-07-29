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
    "$fx/src-tauri/capabilities" \
    "$fx/src-tauri/linux" \
    "$fx/crates/bellman-core/src" \
    "$fx/ui/src" \
    "$fx/ui/public" \
    "$fx/target/release" \
    "$fx/target/debug" \
    "$fx/src-tauri/target/release"
  cp "$LAUNCH_SRC" "$fx/launch.sh"
  chmod +x "$fx/launch.sh"
  # Minimal source stamp inputs (mirror real GUI-affecting roots).
  echo 'fn main() {}' >"$fx/src-tauri/src/main.rs"
  echo '[package] name="x"' >"$fx/src-tauri/Cargo.toml"
  echo '{}' >"$fx/src-tauri/tauri.conf.json"
  echo '{}' >"$fx/src-tauri/capabilities/default.json"
  echo 'png' >"$fx/src-tauri/icons/128x128.png"
  echo '[Desktop Entry]' >"$fx/src-tauri/linux/bellman.desktop"
  echo '[workspace]' >"$fx/Cargo.toml"
  echo '# lock' >"$fx/Cargo.lock"
  echo 'export let x = 1;' >"$fx/ui/src/app.js"
  echo '{}' >"$fx/ui/package.json"
  echo '<div>ui</div>' >"$fx/ui/index.html"
  echo 'export default {}' >"$fx/ui/vite.config.js"
  echo 'export default {}' >"$fx/ui/svelte.config.js"
  # Fake binaries (shell stubs — never executed under dry-run).
  printf '#!/bin/sh\necho fake-release\n' >"$fx/target/release/bellman-app"
  printf '#!/bin/sh\necho fake-debug\n' >"$fx/target/debug/bellman-app"
  printf '#!/bin/sh\necho fake-src-tauri-release\n' >"$fx/src-tauri/target/release/bellman-app"
  chmod +x \
    "$fx/target/release/bellman-app" \
    "$fx/target/debug/bellman-app" \
    "$fx/src-tauri/target/release/bellman-app"
}

# Age every known stamp input so a later binary mtime counts as fresh.
age_all_sources() {
  local fx="$1"
  local epoch="$2"
  local f
  for f in \
    "$fx/Cargo.toml" \
    "$fx/Cargo.lock" \
    "$fx/src-tauri/Cargo.toml" \
    "$fx/src-tauri/tauri.conf.json" \
    "$fx/src-tauri/src/main.rs" \
    "$fx/src-tauri/capabilities/default.json" \
    "$fx/src-tauri/icons/128x128.png" \
    "$fx/src-tauri/linux/bellman.desktop" \
    "$fx/ui/src/app.js" \
    "$fx/ui/package.json" \
    "$fx/ui/index.html" \
    "$fx/ui/vite.config.js" \
    "$fx/ui/svelte.config.js"
  do
    [ -e "$f" ] && touch_epoch "$f" "$epoch"
  done
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
trap 'rm -rf "$FX" "$FX2" "$FX3" "$FX4" "$FX5" "$FX6" "$FX7" "$FX8" "$FX9" 2>/dev/null || true' EXIT
make_fixture "$FX"
# Source old, binaries newer.
age_all_sources "$FX" 1000000000
touch_epoch "$FX/target/release/bellman-app" 2000000000
touch_epoch "$FX/target/debug/bellman-app" 2000000000
OUT=$(run_select "$FX")
assert_contains "$OUT" "action=exec" "fresh release: action=exec"
assert_contains "$OUT" "freshness=fresh" "fresh release: freshness=fresh"
assert_contains "$OUT" "path=$FX/target/release/bellman-app" "fresh release: prefers target/release"

# --- 2. Stale release refused without opt-in (rebuild-or-dev) ----------
FX2=$(mktemp -d)
make_fixture "$FX2"
age_all_sources "$FX2" 3000000000
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
age_all_sources "$FX3" 3000000000
touch_epoch "$FX3/target/release/bellman-app" 1000000000
OUT=$(run_select "$FX3" BELLMAN_ALLOW_STALE=1)
assert_contains "$OUT" "action=exec" "allow_stale: action=exec"
assert_contains "$OUT" "freshness=stale" "allow_stale: freshness=stale"
assert_contains "$OUT" "allow_stale=1" "allow_stale: flag in decision"
assert_contains "$OUT" "path=$FX3/target/release/bellman-app" "allow_stale: uses release path"

# --- 4. Prefer release over debug when both fresh ----------------------
FX4=$(mktemp -d)
make_fixture "$FX4"
age_all_sources "$FX4" 1000000000
touch_epoch "$FX4/target/debug/bellman-app" 2000000000
touch_epoch "$FX4/target/release/bellman-app" 2000000000
OUT=$(run_select "$FX4")
assert_contains "$OUT" "path=$FX4/target/release/bellman-app" "priority: release over debug"

# --- 5. Fall through to debug when release missing, debug fresh --------
FX5=$(mktemp -d)
make_fixture "$FX5"
rm -f "$FX5/target/release/bellman-app"
age_all_sources "$FX5" 1000000000
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

# --- 8. ui/index.html (and other non-src UI inputs) affect stamp -------
# Auditor finding 1: index.html newer than binary must force stale, not fresh.
FX7=$(mktemp -d)
make_fixture "$FX7"
age_all_sources "$FX7" 1000000000
touch_epoch "$FX7/target/release/bellman-app" 2000000000
touch_epoch "$FX7/ui/index.html" 3000000000
OUT=$(run_select "$FX7")
assert_contains "$OUT" "action=rebuild-or-dev" "ui/index.html newer: rebuild-or-dev"
assert_contains "$OUT" "freshness=stale" "ui/index.html newer: stale"

FX8=$(mktemp -d)
make_fixture "$FX8"
age_all_sources "$FX8" 1000000000
touch_epoch "$FX8/target/release/bellman-app" 2000000000
touch_epoch "$FX8/src-tauri/icons/128x128.png" 3000000000
OUT=$(run_select "$FX8")
assert_contains "$OUT" "freshness=stale" "src-tauri/icons newer: stale"

touch_epoch "$FX8/src-tauri/icons/128x128.png" 1000000000
touch_epoch "$FX8/src-tauri/capabilities/default.json" 3000000000
OUT=$(run_select "$FX8")
assert_contains "$OUT" "freshness=stale" "src-tauri/capabilities newer: stale"

touch_epoch "$FX8/src-tauri/capabilities/default.json" 1000000000
touch_epoch "$FX8/ui/vite.config.js" 3000000000
OUT=$(run_select "$FX8")
assert_contains "$OUT" "freshness=stale" "ui/vite.config.js newer: stale"

# --- 9. Post-rebuild must not silently exec still-stale binary ---------
# Auditor finding 2: fake cargo that succeeds without refreshing the binary.
FX9=$(mktemp -d)
make_fixture "$FX9"
age_all_sources "$FX9" 2000000000
touch_epoch "$FX9/target/release/bellman-app" 1000000000
mkdir -p "$FX9/fakebin" "$FX9/home" "$FX9/cargo-home"
# cargo tauri --version and cargo tauri build --no-bundle both succeed as /bin/true.
ln -s /bin/true "$FX9/fakebin/cargo"
# Intercept x-terminal-emulator so tauri-dev path cannot open a real terminal.
printf '#!/bin/sh\necho XTERM_WOULD_RUN "$@"; exit 99\n' >"$FX9/fakebin/x-terminal-emulator"
chmod +x "$FX9/fakebin/x-terminal-emulator"
# Also stub notify-send/zenity to keep die() quiet.
ln -sf /bin/true "$FX9/fakebin/notify-send" 2>/dev/null || true
set +e
OUT9=$(
  HOME="$FX9/home" \
  CARGO_HOME="$FX9/cargo-home" \
  PATH="$FX9/fakebin:/usr/bin:/bin" \
  BELLMAN_SKIP_REBUILD= \
  bash "$FX9/launch.sh" 2>&1
)
RC9=$?
set -e
# Must NOT execute the stale binary (which would print fake-release).
if printf '%s' "$OUT9" | grep -q 'fake-release'; then
  echo "  FAIL: post-rebuild silently executed stale binary"
  echo "    out: $OUT9"
  FAIL=$((FAIL + 1))
else
  echo "  PASS: post-rebuild did not exec stale binary"
  PASS=$((PASS + 1))
fi
if printf '%s' "$OUT9" | grep -Eq 'refusing silent stale launch|entering cargo tauri dev|tauri-cli missing|cannot start|XTERM_WOULD_RUN'; then
  echo "  PASS: post-rebuild falls through (dev/error), not silent stale"
  PASS=$((PASS + 1))
else
  echo "  FAIL: post-rebuild falls through (dev/error), not silent stale"
  echo "    out: $OUT9 (rc=$RC9)"
  FAIL=$((FAIL + 1))
fi

echo "=== results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
