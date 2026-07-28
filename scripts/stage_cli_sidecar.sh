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
