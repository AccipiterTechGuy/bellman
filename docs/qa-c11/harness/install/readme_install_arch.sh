#!/bin/bash
# README §Install, Arch, run LITERALLY in a clean archlinux:latest.
#
# Two things §Install names explicitly and this transcript therefore does:
#   * the image is a root shell with no `sudo`, so step 1 drops the prefix;
#   * this is an unattended run, so step 1 adds `--noconfirm` — the README
#     says to, because `pacman -Syu` otherwise waits for an answer.
# Steps 2 onward carry no prefix. Nothing else is changed and nothing added.
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

echo "=== README step 1: system prerequisites (Arch) ==="
pacman -Syu --needed --noconfirm git base-devel webkit2gtk-4.1 curl wget file \
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
git clone --branch train/2026-08-01_0005 /srcbundle bellman
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
echo "OK-ARCH-LITERAL"
