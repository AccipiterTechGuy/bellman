#!/bin/bash
# The desktop identity path: a NORMAL USER with sudo, not a root shell.
#
# The three readme_install_*.sh transcripts run in the images as they come —
# root, no sudo on two of them. That is one of the two cases §Install names,
# but it is not the one most people are in. This script covers the other:
# `builder` is an ordinary user who gets root only through `sudo`, exactly
# like someone on their own machine.
#
# ── Scaffolding, and it is NOT part of §Install ───────────────────────────
# Everything in this outer script exists only to make a bare container
# resemble a desktop: install sudo, create a user, give them the sudo group.
# A real desktop arrives with all three already true. It is spelled out here
# rather than hidden so nobody mistakes it for a step the README asks of
# anyone. The README's own commands are the inner script, and nothing else.
set -eux
apt-get update -qq
apt-get install -y -qq sudo >/dev/null
useradd -m -s /bin/bash builder
echo 'builder ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/builder
chmod 0440 /etc/sudoers.d/builder

# The inner script goes to a FILE rather than down `bash -s`'s stdin: apt's
# trigger processing reads stdin, and on the first attempt it swallowed the
# rest of the transcript, so the run ended after step 1 reporting success.
cat > /tmp/readme_steps.sh <<'INNER'
#!/bin/bash
# README §Install, Debian/Ubuntu, run as an ordinary user with sudo.
# `sudo` on step 1 and step 5 because those need root; NO `sudo` on steps
# 2-4, exactly as §Install says, because rustup and nvm install into the
# invoking user's $HOME.
#
# The one substitution: step 4's `git clone` reads /srcbundle instead of the
# public URL — the branch under test is not on public main yet. The bundle is
# a single self-contained file (`git bundle create`), so nothing outside it is
# needed.
set -eux
id -un
id -u                      # non-zero: this is not a root shell

echo "=== README step 1: system prerequisites ==="
sudo apt update
sudo apt install -y git libwebkit2gtk-4.1-dev libgtk-3-dev build-essential \
  curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev

echo "=== README step 2: Rust toolchain (no sudo — installs into ~) ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
test "$(command -v cargo)" = "$HOME/.cargo/bin/cargo"

echo "=== README step 3: Node 24 + Tauri CLI (no sudo) ==="
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash
source "$HOME/.nvm/nvm.sh"
nvm install 24
cargo install tauri-cli --locked

echo "=== README step 4: get the source, build the bundles ==="
git clone --branch train/2026-08-01_0005 /srcbundle bellman
cd bellman
git log --oneline -1
cd ui
npm ci
cd ..
cargo tauri build --bundles deb,appimage --ci --no-sign

echo "=== README step 5: install the deb ==="
sudo apt install -y ./target/release/bundle/deb/Bellman_*.deb

echo "=== what the README promises you get ==="
command -v bellman
command -v bellman-app
ls /usr/share/applications/ | grep -i bellman
ls -R /usr/share/bellman/testing_apps/
bellman --version
echo "--- the toolchain belongs to the user, not to root ---"
ls -ld "$HOME/.cargo" "$HOME/.nvm"
test ! -e /root/.cargo
test ! -e /root/.nvm
echo "OK-UBUNTU-NONROOT"
INNER
chmod 0755 /tmp/readme_steps.sh

exec sudo -u builder -i /tmp/readme_steps.sh
