#!/usr/bin/env bash
# qa_wiz1_deb_docker.sh — WIZ1 exit-gate verification against a REAL deb install.
#
# Builds a clean Ubuntu 24.04 image, installs target/release/bundle/deb/
# Bellman_*.deb with apt (real dpkg install, deps resolved from the distro
# archive — not an extract), then runs the full scripts/capture_qa_wiz1.py
# suite inside the container against /usr/bin/bellman-app. The wizard's demo
# panel therefore resolves the INSTALLED /usr/share/bellman/testing_apps
# payload (the source tree is mounted read-only at /src but CARGO_MANIFEST_DIR
# baked into the binary still points at the build tree — which does not exist
# in the container, so only the installed location can resolve).
#
# Usage:
#   scripts/qa_wiz1_deb_docker.sh [path/to/Bellman_*.deb]
#
# Output: screenshots + meta land in /tmp/bellman-wiz1-deb-out (see -v /out).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DEB="${1:-}"
if [ -z "$DEB" ]; then
  DEB="$(find target/release/bundle/deb -name '*.deb' 2>/dev/null | head -1 || true)"
fi
if [ -z "$DEB" ] || [ ! -f "$DEB" ]; then
  echo "qa_wiz1_deb_docker: no .deb found — run cargo tauri build first" >&2
  exit 1
fi
DEB="$(cd "$(dirname "$DEB")" && pwd)/$(basename "$DEB")"
echo "qa_wiz1_deb_docker: deb=$DEB"

CTX="$(mktemp -d /tmp/bellman-wiz1-deb-ctx.XXXXXX)"
trap 'rm -rf "$CTX"' EXIT
cp "$DEB" "$CTX/Bellman_0.1.0_amd64.deb"
cp "$HOME/.cargo/bin/tauri-driver" "$CTX/tauri-driver"

cat > "$CTX/Dockerfile" <<'EOF'
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      xvfb openbox wmctrl x11-utils dbus-x11 \
      python3 python3-tk python3-venv \
      webkit2gtk-driver ca-certificates \
 && rm -rf /var/lib/apt/lists/*
# Real package install (dpkg via apt) — proves the payload layout lands.
COPY Bellman_0.1.0_amd64.deb /tmp/bellman.deb
RUN apt-get update && apt-get install -y /tmp/bellman.deb \
 && rm -rf /var/lib/apt/lists/* /tmp/bellman.deb \
 && test -f /usr/share/bellman/testing_apps/lightbulb_gui/lightbulb_gui.py \
 && test -f /usr/share/bellman/testing_apps/lightbulb/lightbulb.py \
 && test -f /usr/share/bellman/docs/INTEGRATION.md
COPY tauri-driver /usr/local/bin/tauri-driver
RUN python3 -m venv /opt/qa-venv \
 && /opt/qa-venv/bin/pip install --no-cache-dir selenium pillow python-xlib
EOF

echo "qa_wiz1_deb_docker: building image (apt pulls webkit — can take minutes)"
docker build -t bellman-wiz1-debqa "$CTX"

OUT=/tmp/bellman-wiz1-deb-out
rm -rf "$OUT"
mkdir -p "$OUT"

echo "qa_wiz1_deb_docker: running WIZ1 suite against /usr/bin/bellman-app"
docker run --rm --shm-size=1g \
  -v "$ROOT":/src:ro \
  -v "$OUT":/out \
  bellman-wiz1-debqa bash /src/scripts/qa_wiz1_deb_inner.sh

echo "qa_wiz1_deb_docker: OK — evidence in $OUT"
