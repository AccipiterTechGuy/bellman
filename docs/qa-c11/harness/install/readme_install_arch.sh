#!/bin/bash
# README §Install, Arch, run VERBATIM in a clean archlinux:latest.
# NO shim and NO invented step. Step 1's `sudo` prefix is dropped because the
# README itself says to drop it when you are already root, which is what this
# image gives you. The pacman line carries no --noconfirm, so empty lines go
# in on stdin — exactly what a human pressing Enter supplies.
set -eux
id -u

echo "=== README step 1: system prerequisites (Arch) ==="
yes '' | pacman -Syu --needed git base-devel webkit2gtk-4.1 curl wget file \
  openssl xdotool libayatana-appindicator librsvg

echo "=== README step 2: Rust toolchain ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

echo "=== README step 3: Node 24 + Tauri CLI ==="
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash
source "$HOME/.nvm/nvm.sh"
nvm install 24
cargo install tauri-cli --locked

echo "=== README step 4: get the source, build the Arch way ==="
git config --global --add safe.directory '*'
git clone --branch train/2026-08-01_0005 /srcrepo bellman
cd bellman
git log --oneline -1
cd ui
npm ci
cd ..
cargo tauri build --no-bundle --ci

echo "=== README step 5: there is no package — run what you built ==="
ls -l target/release/bellman-app target/release/bellman
./target/release/bellman --version
./target/release/bellman --help | head -3
echo "OK-ARCH-VERBATIM"
