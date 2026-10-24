#!/bin/bash
# checks.sh installed|removed DISTRO — test scaffolding, not part of the guide.
set -exo pipefail
what=$1 d=$2
case $what in
installed)
  bellman --version; bellman-app --version
  bellman-app --help | head -3
  if [ "$d" != arch ]; then
    test -f /usr/share/applications/Bellman.desktop
    test -d /usr/share/bellman/testing_apps/lightbulb
    test -d /usr/share/bellman/testing_apps/lightbulb_gui
  fi
  BELLMAN_BIN="$(command -v bellman)" bash "$HOME/bellman/tests/cli_roundtrip.sh"
  ;;
removed)
  ! command -v bellman; ! command -v bellman-app
  test ! -e /usr/share/bellman; test ! -e /usr/share/applications/Bellman.desktop
  test ! -e "$HOME/.bellman"; test ! -e "$HOME/.local/share/io.bellman.desktop"
  ;;
esac
echo "CHECK_OK $what $d"
