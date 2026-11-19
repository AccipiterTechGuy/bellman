#!/usr/bin/env bash
# Stage the headless `bellman` CLI as a Tauri externalBin sidecar so deb /
# AppImage / NSIS / MSI / dmg all ship both:
#   - main binary  : bellman-app  (tray GUI shell)
#   - sidecar      : bellman      (AI-skill CLI, documented name)
#
# Invoked by tauri.conf.json build.beforeBundleCommand.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
if [[ -z "$TRIPLE" ]]; then
  echo "stage_cli_sidecar: could not determine host triple" >&2
  exit 1
fi

# Match tauri.conf.json bundle.macOS.minimumSystemVersion ("11.0") on Darwin so
# cargo's env fingerprint for C crates (libsqlite3-sys) stays identical across
# beforeBuildCommand, the main Tauri cargo build, and beforeBundleCommand.
if [[ "$(uname -s)" == "Darwin" ]]; then
  export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
fi

echo "stage_cli_sidecar: building bellman-cli for $TRIPLE"
cargo build -p bellman-cli --release

mkdir -p src-tauri/binaries
SRC="target/release/bellman"
# Windows cargo emits .exe
if [[ ! -f "$SRC" && -f "${SRC}.exe" ]]; then
  SRC="${SRC}.exe"
fi
if [[ ! -f "$SRC" ]]; then
  echo "stage_cli_sidecar: missing $SRC after cargo build" >&2
  exit 1
fi

# Tauri externalBin looks for binaries/<name>-<target-triple>[.exe]
DEST="src-tauri/binaries/bellman-${TRIPLE}"
if [[ "$SRC" == *.exe ]]; then
  DEST="${DEST}.exe"
fi
cp -a "$SRC" "$DEST"
chmod +x "$DEST" 2>/dev/null || true
echo "stage_cli_sidecar: staged $DEST"

# On macOS, if both Apple targets (x86_64-apple-darwin and aarch64-apple-darwin)
# are installed in rustup, also stage the cross-target sidecar and lipo a
# universal-apple-darwin sidecar so `cargo tauri build --target universal-apple-darwin`
# bundles a universal `bellman` CLI alongside the universal `bellman-app`.
if [[ "$(uname -s)" == "Darwin" ]] && command -v rustup >/dev/null 2>&1 && command -v lipo >/dev/null 2>&1; then
  OTHER_DARWIN=""
  case "$TRIPLE" in
    x86_64-apple-darwin)  OTHER_DARWIN="aarch64-apple-darwin" ;;
    aarch64-apple-darwin) OTHER_DARWIN="x86_64-apple-darwin" ;;
  esac
  if [[ -n "$OTHER_DARWIN" ]] && rustup target list --installed 2>/dev/null | grep -qx "$OTHER_DARWIN"; then
    echo "stage_cli_sidecar: building bellman-cli for $OTHER_DARWIN"
    cargo build -p bellman-cli --release --target "$OTHER_DARWIN"
    OTHER_SRC="target/${OTHER_DARWIN}/release/bellman"
    OTHER_DEST="src-tauri/binaries/bellman-${OTHER_DARWIN}"
    cp -a "$OTHER_SRC" "$OTHER_DEST"
    chmod +x "$OTHER_DEST" 2>/dev/null || true
    echo "stage_cli_sidecar: staged $OTHER_DEST"

    UNIV_DEST="src-tauri/binaries/bellman-universal-apple-darwin"
    lipo -create -output "$UNIV_DEST" \
      "src-tauri/binaries/bellman-x86_64-apple-darwin" \
      "src-tauri/binaries/bellman-aarch64-apple-darwin"
    chmod +x "$UNIV_DEST" 2>/dev/null || true
    echo "stage_cli_sidecar: staged universal $UNIV_DEST"
  fi
fi
