#!/bin/bash
# README §Install, Fedora, run LITERALLY in a clean fedora:latest.
#
# This image DOES ship `sudo`, so step 1 keeps the prefix exactly as §Install
# writes it. Steps 2 onward carry no prefix, per the same section. Nothing is
# added.
#
# The one substitution, stated here and in VALIDATION.md: step 4's `git clone`
# reads /srcrepo (a read-only bind mount of the checkout) instead of the
# public URL, because the branch under test is not on public main yet.
set -eux
id -u
command -v sudo

echo "=== README step 1: system prerequisites (Fedora) ==="
sudo dnf install -y git gcc gcc-c++ make webkit2gtk4.1-devel openssl-devel \
  curl wget file libxdo-devel libayatana-appindicator-gtk3-devel librsvg2-devel

echo "=== README step 2: Rust toolchain ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

echo "=== README step 3: Node 24 + Tauri CLI ==="
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash
source "$HOME/.nvm/nvm.sh"
nvm install 24
cargo install tauri-cli --locked

echo "=== README step 4: get the source, build the Fedora bundle ==="
git config --global --add safe.directory '*'
git clone --branch train/2026-08-01_0005 /srcrepo bellman
cd bellman
git log --oneline -1
cd ui
npm ci
cd ..
cargo tauri build --bundles rpm --ci --no-sign

echo "=== README step 5: install the rpm ==="
sudo dnf install -y ./target/release/bundle/rpm/Bellman-*.rpm

echo "=== what the README promises you get ==="
command -v bellman
command -v bellman-app
ls /usr/share/applications/ | grep -i bellman
ls -R /usr/share/bellman/testing_apps/
bellman --version
echo "OK-FEDORA-LITERAL"
