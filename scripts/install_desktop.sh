#!/usr/bin/env bash
# Install (or refresh) a *developer* XDG desktop entry for this Bellman checkout.
#
# Points the menu entry at repo-root launch.sh (freshness-aware). Uses a Bellman
# icon from src-tauri/icons — never mate-panel-clock. Categories use a single
# main category so desktop-file-validate stays clean (no multiple-main-category).
#
# Packaged installs (deb/AppImage) continue to use src-tauri/linux/bellman.desktop
# via Tauri's desktopTemplate — this script does not touch that template.
#
# Usage:
#   scripts/install_desktop.sh           # install / refresh
#   scripts/install_desktop.sh --print   # write desktop file to stdout
#   scripts/install_desktop.sh --validate-only  # validate generated file, no install
#   scripts/install_desktop.sh --uninstall
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LAUNCH="$ROOT/launch.sh"
ICON_SRC="$ROOT/src-tauri/icons/128x128.png"
# Fallback chain for the app icon.
if [ ! -f "$ICON_SRC" ]; then
  for c in \
    "$ROOT/src-tauri/icons/icon.png" \
    "$ROOT/src-tauri/icons/app-icon.png" \
    "$ROOT/src-tauri/icons/64x64.png"
  do
    if [ -f "$c" ]; then
      ICON_SRC="$c"
      break
    fi
  done
fi

APP_ID="bellman"
DESKTOP_NAME="Bellman.desktop"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/128x128/apps"
ICON_DST="$ICONS_DIR/${APP_ID}.png"
DESKTOP_DST="$APPS_DIR/$DESKTOP_NAME"
# Optional convenience symlink for PATH launchers.
BIN_LINK="${BELLMAN_BIN_LINK:-$HOME/.local/bin/bellman_launch.sh}"

MODE="install"
for arg in "$@"; do
  case "$arg" in
    --print) MODE="print" ;;
    --validate-only) MODE="validate" ;;
    --uninstall) MODE="uninstall" ;;
    -h|--help)
      sed -n '2,16p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *)
      echo "install_desktop: unknown arg: $arg" >&2
      exit 2
      ;;
  esac
done

[ -x "$LAUNCH" ] || { echo "install_desktop: launch.sh missing or not executable: $LAUNCH" >&2; exit 1; }
[ -f "$ICON_SRC" ] || { echo "install_desktop: no icon under src-tauri/icons" >&2; exit 1; }

# Single main category (Utility). Do NOT add System — that triggers
# desktop-file-validate's multiple-main-category hint.
render_desktop() {
  cat <<EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=Bellman
Comment=Bellman task scheduler — desktop cousin of cron
Exec=${LAUNCH}
Icon=${APP_ID}
Terminal=false
Categories=Utility;
Keywords=timer;scheduler;cron;alarm;task;
StartupNotify=false
X-GNOME-UsesNotifications=true
# Developer checkout launcher — managed by scripts/install_desktop.sh
X-Bellman-Repo=${ROOT}
EOF
}

uninstall() {
  rm -f "$DESKTOP_DST"
  rm -f "$ICON_DST"
  # Only remove the bin link if it points at this checkout's launch.sh.
  if [ -L "$BIN_LINK" ]; then
    local target
    target=$(readlink -f "$BIN_LINK" 2>/dev/null || true)
    if [ "$target" = "$(readlink -f "$LAUNCH")" ]; then
      rm -f "$BIN_LINK"
    fi
  fi
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPS_DIR" 2>/dev/null || true
  fi
  echo "install_desktop: removed $DESKTOP_DST"
}

case "$MODE" in
  print)
    render_desktop
    exit 0
    ;;
  uninstall)
    uninstall
    exit 0
    ;;
  validate|install)
    ;;
esac

TMP_DIR=$(mktemp -d)
TMP_DESKTOP="$TMP_DIR/Bellman.desktop"
trap 'rm -rf "$TMP_DIR"' EXIT
render_desktop >"$TMP_DESKTOP"

if command -v desktop-file-validate >/dev/null 2>&1; then
  if ! desktop-file-validate "$TMP_DESKTOP"; then
    echo "install_desktop: desktop-file-validate failed" >&2
    cat "$TMP_DESKTOP" >&2
    exit 1
  fi
  # Explicitly reject the multiple-main-category hint if validate is lenient.
  if desktop-file-validate "$TMP_DESKTOP" 2>&1 | grep -qi 'multiple main category\|more than one main category'; then
    echo "install_desktop: Categories produce multiple-main-category hint" >&2
    exit 1
  fi
else
  echo "install_desktop: desktop-file-validate not installed — syntax check skipped" >&2
fi

if [ "$MODE" = "validate" ]; then
  echo "install_desktop: OK (validate-only)"
  exit 0
fi

mkdir -p "$APPS_DIR" "$ICONS_DIR" "$(dirname "$BIN_LINK")"
cp -f "$ICON_SRC" "$ICON_DST"
cp -f "$TMP_DESKTOP" "$DESKTOP_DST"
chmod 644 "$DESKTOP_DST"
ln -sfn "$LAUNCH" "$BIN_LINK"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APPS_DIR" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" 2>/dev/null || true
fi

echo "install_desktop: installed $DESKTOP_DST"
echo "install_desktop: icon $ICON_DST (from $ICON_SRC)"
echo "install_desktop: bin link $BIN_LINK -> $LAUNCH"
echo "install_desktop: Exec=$LAUNCH"
