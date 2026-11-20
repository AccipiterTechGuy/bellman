# Install Bellman on Linux

For Debian / Ubuntu, Fedora and Arch. There is no tagged release yet, so you
build Bellman from source: six steps and about **5 GB** of disk (2.6 GB build,
~2.5 GB toolchains).

On a Mac? See [Install on macOS](INSTALL_MACOS.md). Back to the
[README](../README.md).

**Who runs what.** Steps 1 and 5 install system packages and need `sudo`.
Steps 2–4 install into your home directory — run them as yourself, **never**
with `sudo`. Already root (a container)? Leave out `sudo`.

## 1. System packages

Debian / Ubuntu:

```sh
sudo apt update
sudo apt install -y git libwebkit2gtk-4.1-dev libgtk-3-dev build-essential \
  curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Fedora:

```sh
sudo dnf install -y git gcc gcc-c++ make webkit2gtk4.1-devel openssl-devel \
  curl wget file libxdo-devel libayatana-appindicator-gtk3-devel librsvg2-devel
```

Arch:

```sh
sudo pacman -Syu --needed git base-devel webkit2gtk-4.1 curl wget file \
  openssl xdotool libayatana-appindicator librsvg
```

`pacman -Syu` upgrades the whole system and asks before it starts; add
`--noconfirm` only for unattended runs.

Check it worked — you should see three version numbers and `step 1 OK`:

```sh
pkg-config --modversion webkit2gtk-4.1 gtk+-3.0 ayatana-appindicator3-0.1 && echo "step 1 OK"
```

## 2. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

## 3. Node 24 and the Tauri CLI

```sh
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash
source "$HOME/.nvm/nvm.sh"
nvm install 24
cargo install tauri-cli --locked
```

Any Node 24.x works, including a distro package. `cargo install` compiles the
Tauri CLI and takes a few minutes.

## 4. Get the source and build

```sh
git clone https://github.com/AccipiterTechGuy/bellman && cd bellman
cd ui && npm ci && cd ..
```

Then build for your distro (this takes several minutes):

Debian / Ubuntu:

```sh
cargo tauri build --bundles deb,appimage --ci --no-sign
```

Fedora:

```sh
cargo tauri build --bundles rpm --ci --no-sign
```

Arch:

```sh
cargo tauri build --no-bundle --ci
```

Stay in the `bellman` folder for step 5.

## 5. Install

Debian / Ubuntu:

```sh
sudo apt install -y ./target/release/bundle/deb/Bellman_*.deb
```

Fedora:

```sh
sudo dnf install -y ./target/release/bundle/rpm/Bellman-*.rpm
```

Arch:

```sh
sudo install -Dm755 -t /usr/local/bin target/release/bellman target/release/bellman-app
```

The deb and rpm add a **Bellman** entry to your app launcher; on Arch, start
the app with `bellman-app`.

## 6. Check it works

```sh
bellman --version
bellman-app --version
```

Both print `0.1.0`. Now open **Bellman** from your app launcher (Arch: run
`bellman-app`) — the first-run wizard appears. The demo apps for the
[integration guide](INTEGRATION.md) are in
`/usr/share/bellman/testing_apps/` (deb/rpm) or `testing_apps/` in your clone.

## Update

From your `bellman` folder:

```sh
git pull
cd ui && npm ci && cd ..
```

Then repeat your distro's command from step 4 and from step 5. Your timers are
kept.

## Uninstall

1. If you chose **Start at login** (in the first-run wizard or Settings), turn
   it off in **Settings** first. Already uninstalled? Delete
   `~/.config/autostart/Bellman.desktop` instead.
2. Remove the program:

   Debian / Ubuntu:

   ```sh
   sudo apt remove -y bellman
   ```

   Fedora:

   ```sh
   sudo dnf remove -y bellman && sudo rm -rf /usr/share/bellman
   ```

   Arch:

   ```sh
   sudo rm -f /usr/local/bin/bellman /usr/local/bin/bellman-app
   ```

3. Optional — delete all your timers, logs and slots. **This cannot be undone:**

   ```sh
   rm -rf ~/.bellman ~/.local/share/io.bellman.desktop
   ```

   If you used `bellman task new` to write cron entries, run `crontab -e` and
   delete the block between `# BEGIN bellman-managed` and
   `# END bellman-managed`.

## Install notes

- **Arch package names.** Arch ships the `libxdo` library in the `xdotool`
  package; there is no `libxdo` package.
- **Why `libgtk-3-dev` is listed** although WebKitGTK pulls it in today: so the
  list stays complete if that dependency ever changes.
- **AppImage (Debian/Ubuntu only).** Step 4 also builds
  `target/release/bundle/appimage/Bellman_*.AppImage`, which runs without
  installing but needs FUSE 2 (`libfuse2t64` on Ubuntu 24.04). Fedora and Arch
  cannot build it: the `linuxdeploy` tool Tauri uses fails on their newer
  shared libraries, which is an upstream limitation.
- **`npm ci` warnings.** It ends with an audit summary ("12 vulnerabilities …")
  and an `esbuild` install-scripts notice. Both concern development-only
  packages that are not part of the app. Do not run `npm audit fix --force`: it
  upgrades the front-end past the tested versions.
- **What `cargo tauri build` does.** It builds the UI and the `bellman` CLI
  itself; `npm ci` is the only front-end step you run by hand.
- **Fuller checks.** On Debian/Ubuntu, `scripts/smoke_install_deb.sh` tests the
  installed package (`SMOKE_MODE=docker` runs it in a container); the manual
  VM checklist is [QA_P6.md](QA_P6.md).
- **How this guide is tested.** `tests/readme_install/run.sh ubuntu|fedora|arch`
  extracts every command block above from this file and runs it verbatim in a
  clean container — install, update, then uninstall — and checks that nothing
  is left behind (`--user` runs Ubuntu as a normal user with `sudo`). Last
  verified 2026-10-24 with rustc 1.99.1, tauri-cli 2.12.4 and Node 24.22.1.

Working on Bellman itself? See [DEVELOPMENT.md](DEVELOPMENT.md).
