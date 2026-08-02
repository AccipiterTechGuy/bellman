#!/bin/bash
# README §Install, Debian/Ubuntu, run LITERALLY in a clean ubuntu:24.04.
#
# The image gives you a root shell with no `sudo`, which is one of the two
# cases §Install names: step 1 drops the `sudo` prefix, steps 2 onward run as
# the invoking user (here, root) with no prefix. Nothing else is changed and
# nothing is added.
#
# The one substitution, stated here and in VALIDATION.md: step 4's `git clone`
# reads /srcbundle instead of the public URL, because the branch under test is
# not on public main yet. That bundle is a single self-contained file made on
# the host with
#     git bundle create <file> train/2026-08-01_0005
# so the container needs nothing but the file — no bind-mounted checkout, and
# in particular no dependency on a parent repository outside it. (Mounting a
# git *worktree* would not do: its .git is a pointer to a gitdir that lives
# elsewhere, so the clone fails inside the container.)
set -eux
id -u

echo "=== README step 1: system prerequisites ==="
apt update
apt install -y git libwebkit2gtk-4.1-dev libgtk-3-dev build-essential \
  curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev

echo "=== README step 2: Rust toolchain ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

echo "=== README step 3: Node 24 + Tauri CLI ==="
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash
source "$HOME/.nvm/nvm.sh"
nvm install 24
cargo install tauri-cli --locked

echo "=== README step 4: get the source, build the Debian/Ubuntu bundles ==="
git config --global --add safe.directory '*'
git clone --branch train/2026-08-01_0005 /srcbundle bellman
cd bellman
git log --oneline -1
cd ui
npm ci
cd ..
cargo tauri build --bundles deb,appimage --ci --no-sign

echo "=== README step 5: install the deb ==="
apt install -y ./target/release/bundle/deb/Bellman_*.deb

echo "=== what the README promises you get ==="
command -v bellman
command -v bellman-app
ls /usr/share/applications/ | grep -i bellman
ls -R /usr/share/bellman/testing_apps/
bellman --version
ls -l target/release/bundle/deb/*.deb target/release/bundle/appimage/*.AppImage
echo "OK-UBUNTU-LITERAL"
