#!/usr/bin/env bash
# qa_wiz1_deb_inner.sh — runs INSIDE the bellman-wiz1-debqa container
# (see qa_wiz1_deb_docker.sh). Starts an isolated X server and runs the
# WIZ1 capture suite against the deb-installed /usr/bin/bellman-app.
set -euo pipefail

export DISPLAY=:99
Xvfb :99 -screen 0 1280x800x24 -ac +extension GLX +render -noreset &
sleep 1.2
openbox &
sleep 1.0

export XDG_DATA_HOME=/tmp/qa/share
export XDG_CONFIG_HOME=/tmp/qa/config
export XDG_RUNTIME_DIR=/tmp/qa/runtime
export XDG_CACHE_HOME=/tmp/qa/cache
mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"

export BELLMAN_QA_DATA="$XDG_DATA_HOME/io.bellman.desktop"
export BELLMAN_APP=/usr/bin/bellman-app
export BELLMAN_CLI=/usr/bin/bellman
export TAURI_DRIVER=/usr/local/bin/tauri-driver
export BELLMAN_QA_OUT=/out
export GDK_BACKEND=x11
export LIBGL_ALWAYS_SOFTWARE=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export GIO_USE_VFS=local
export GTK_USE_PORTAL=0
# The container has no user namespace for WebKit's bubblewrap sandbox; this
# is evidence capture, not a security boundary.
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1

echo "== deb payload =="
ls -l /usr/share/bellman/testing_apps/lightbulb_gui/lightbulb_gui.py \
      /usr/share/bellman/testing_apps/lightbulb/lightbulb.py \
      /usr/share/bellman/docs/INTEGRATION.md
command -v bellman bellman-app
bellman --help >/dev/null && echo "bellman CLI: OK"

/opt/qa-venv/bin/python /src/scripts/capture_qa_wiz1.py
