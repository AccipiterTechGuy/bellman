#!/usr/bin/env bash
# Fresh-install smoke for the Linux .deb produced by `cargo tauri build`.
#
# Modes:
#   host   (default) — install the deb on this machine (sudo when not root,
#                      directly when already root), verify
#                      launcher entry + CLI on PATH, exercise autostart
#                      write/remove, then optionally leave installed.
#   docker           — install inside an ephemeral Ubuntu container (no
#                      tray/GUI; still checks file layout + CLI).
#
# Usage:
#   scripts/smoke_install_deb.sh [path/to/Bellman_*.deb]
#   SMOKE_MODE=docker scripts/smoke_install_deb.sh
#   SMOKE_KEEP=1 scripts/smoke_install_deb.sh   # do not apt-remove at end
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="${SMOKE_MODE:-host}"

# Host mode installs with elevated privileges. When already root -- a clean
# container, a CI image, a root shell -- `sudo` is often not installed at all,
# so calling it unconditionally fails with "sudo: command not found" rather
# than doing the obvious thing. Resolve it once: empty when root.
if [[ "$(id -u)" -eq 0 ]]; then
  SUDO=""
elif command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
else
  echo "smoke_install_deb: not root and sudo is unavailable — cannot install" >&2
  exit 1
fi
KEEP="${SMOKE_KEEP:-0}"

pick_deb() {
  if [[ $# -ge 1 && -n "${1:-}" ]]; then
    echo "$1"
    return
  fi
  local found
  found="$(find target/release/bundle/deb -name '*.deb' 2>/dev/null | head -1 || true)"
  if [[ -z "$found" ]]; then
    echo "smoke_install_deb: no .deb under target/release/bundle/deb — run cargo tauri build first" >&2
    exit 1
  fi
  echo "$found"
}

DEB="$(pick_deb "${1:-}")"
DEB_ABS="$(cd "$(dirname "$DEB")" && pwd)/$(basename "$DEB")"
echo "smoke_install_deb: mode=$MODE deb=$DEB_ABS"

assert_deb_layout() {
  local deb="$1"
  echo "== dpkg-deb contents =="
  dpkg-deb -c "$deb" | tee /tmp/bellman-deb-contents.txt
  grep -E 'usr/share/applications/.*\.desktop' /tmp/bellman-deb-contents.txt
  grep -E 'usr/bin/bellman-app' /tmp/bellman-deb-contents.txt
  # CLI sidecar
  grep -E 'usr/bin/bellman' /tmp/bellman-deb-contents.txt
  # At least one hicolor icon
  grep -E 'usr/share/icons/' /tmp/bellman-deb-contents.txt
  echo "layout: OK"
}

smoke_host() {
  assert_deb_layout "$DEB_ABS"

  echo "== install =="
  $SUDO dpkg -i "$DEB_ABS" || $SUDO apt-get install -f -y

  echo "== PATH + launcher =="
  command -v bellman
  command -v bellman-app
  bellman --help >/dev/null
  bellman list --json | head -c 200 || true
  echo
  test -f /usr/share/applications/Bellman.desktop \
    || test -f /usr/share/applications/bellman.desktop \
    || ls /usr/share/applications/*[Bb]ellman*.desktop
  # Desktop database refresh (best-effort)
  if command -v update-desktop-database >/dev/null; then
    $SUDO update-desktop-database /usr/share/applications || true
  fi
  echo "launcher entry present"

  echo "== autostart XDG write (plugin-equivalent) =="
  # Simulate what tauri-plugin-autostart writes so we can document the path
  # without requiring a full GUI session in this smoke.
  local auto_dir="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
  mkdir -p "$auto_dir"
  local auto_file="$auto_dir/Bellman.desktop"
  cat >"$auto_file" <<EOF
[Desktop Entry]
Type=Application
Name=Bellman
Exec=$(command -v bellman-app)
X-GNOME-Autostart-enabled=true
Comment=Bellman autostart smoke entry
EOF
  test -f "$auto_file"
  grep -q 'bellman-app' "$auto_file"
  echo "autostart entry written: $auto_file"
  echo "NOTE: full 'survives relog' check is manual — see docs/QA_P6.md §Autostart."

  if [[ "$KEEP" != "1" ]]; then
    echo "== cleanup =="
    rm -f "$auto_file"
    $SUDO dpkg -r bellman || $SUDO apt-get remove -y bellman || true
  else
    echo "SMOKE_KEEP=1 — leaving package + autostart entry installed"
  fi
  echo "smoke_host: PASS"
}

smoke_docker() {
  assert_deb_layout "$DEB_ABS"
  if ! command -v docker >/dev/null; then
    echo "smoke_install_deb: docker not available" >&2
    exit 1
  fi
  local img="ubuntu:24.04"
  echo "== docker install ($img) =="
  docker run --rm \
    -v "$DEB_ABS:/tmp/bellman.deb:ro" \
    "$img" \
    bash -lc '
      set -euo pipefail
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -qq
      apt-get install -y -qq libwebkit2gtk-4.1-0 libayatana-appindicator3-1 ca-certificates >/dev/null
      dpkg -i /tmp/bellman.deb || apt-get install -f -y
      command -v bellman
      command -v bellman-app
      bellman --help >/dev/null
      ls /usr/share/applications/*[Bb]ellman*.desktop
      test -x /usr/bin/bellman
      test -x /usr/bin/bellman-app
      echo "docker smoke: PASS"
    '
}

case "$MODE" in
  host) smoke_host ;;
  docker) smoke_docker ;;
  *)
    echo "unknown SMOKE_MODE=$MODE (host|docker)" >&2
    exit 2
    ;;
esac
