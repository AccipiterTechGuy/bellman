# Install Bellman on macOS

On Linux? See [Install on Linux](INSTALL_LINUX.md). Back to the
[README](../README.md).

Bellman supports **both Intel (`x86_64`) and Apple Silicon (`arm64`) Macs**
running macOS 11 (Big Sur) or later:

- **Intel (`x86_64`)** — verified end to end on real hardware (Intel Core
  i7-3615QM, macOS 14.8.9 Sonoma): full workspace test suite, 36-step CLI
  round-trip, native `.app` / `.dmg` and universal (`x86_64` + `arm64`)
  packaging, GUI launch + menu-bar tray, and the `testing_apps/lightbulb`
  handshake (`fired → acknowledged → completed`).
- **Apple Silicon (`arm64`)** — GitHub Actions CI (`macos-latest`) runs the
  full Rust test suite on `arm64` and packages a universal (`x86_64` + `arm64`)
  `Bellman.app` and `.dmg`; physical Apple Silicon desktop installation has not
  yet been run by hand.
- **Known macOS limitations in 0.1.0:** packages are unsigned (`--no-sign`),
  wake-from-sleep is disabled (the privileged wake helper daemon is not bundled
  or signed; timers due while the Mac is asleep are handled on resume by the
  misfire pass), and `bellman scan` does not yet discover `launchd` jobs.

Run every step below as your normal user (**never** with `sudo`). Building from
source takes about **5 GB** of disk.

## 1. Xcode Command Line Tools

Install Apple's C toolchain and SDK headers:

```sh
xcode-select --install
```

If a dialog appears, click **Install** and wait for it to finish (if already
installed, `xcode-select` prints `command line tools are already installed`).
Check it worked:

```sh
xcode-select -p && clang --version && echo "step 1 OK"
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

Any Node 24.x works (including Homebrew `node@24`). `cargo install` compiles
the Tauri CLI and takes a few minutes.

## 4. Get the source and build

```sh
git clone https://github.com/AccipiterTechGuy/bellman && cd bellman
cd ui && npm ci && cd ..
CI=true cargo tauri build --bundles app,dmg --ci --no-sign
```

**Why `CI=true` is required on macOS:** without `CI=true`, Tauri's `.dmg`
packaging script launches Finder via AppleScript to position the disk-image
window icons. In a normal terminal session without Finder Automation
permission, that AppleScript stalls and fails with
`Finder got an error: AppleEvent timed out (-1712)`. Setting `CI=true` skips
the Finder layout script and produces `target/release/bundle/macos/Bellman.app`
and `target/release/bundle/dmg/Bellman_0.1.0_*.dmg` reliably.

Want a **universal (`x86_64` + `arm64`)** `.app` and `.dmg` that runs on both
Intel and Apple Silicon Macs (the same bundle CI builds)? Add both Rust targets
and pass `--target universal-apple-darwin`:

```sh
rustup target add x86_64-apple-darwin aarch64-apple-darwin
CI=true cargo tauri build --target universal-apple-darwin --bundles app,dmg --ci --no-sign
```

The universal bundles are written under
`target/universal-apple-darwin/release/bundle/{macos,dmg}/`.

## 5. Install

Copy `Bellman.app` into `/Applications` (or open
`target/release/bundle/dmg/Bellman_0.1.0_*.dmg` and drag **Bellman.app** to
**Applications**), and optionally copy the `bellman` CLI sidecar onto your
`PATH` (`~/.local/bin`):

```sh
cp -R target/release/bundle/macos/Bellman.app /Applications/
mkdir -p "$HOME/.local/bin"
cp /Applications/Bellman.app/Contents/MacOS/bellman "$HOME/.local/bin/bellman"
```

Ensure `"$HOME/.local/bin"` is on your `PATH` (or invoke the bundled CLI at
`/Applications/Bellman.app/Contents/MacOS/bellman`).

**First run of an unsigned app (Gatekeeper):** because this build is unsigned
(`--no-sign`), macOS Gatekeeper blocks downloaded or externally built apps on
first open. Either **right-click (Control-click) `Bellman.app` in
`/Applications` → Open → Open**, or clear the quarantine attribute in Terminal:

```sh
xattr -dr com.apple.quarantine /Applications/Bellman.app
```

## 6. Check it works

```sh
/Applications/Bellman.app/Contents/MacOS/bellman --version
/Applications/Bellman.app/Contents/MacOS/bellman-app --version
```

They print `bellman 0.1.0` and `bellman-app 0.1.0`. Launch **Bellman** from
`/Applications` (or run `open -a /Applications/Bellman.app`) — the window and
menu-bar tray icon appear, and the first-run wizard opens on a fresh setup. The
demo apps for the [integration guide](INTEGRATION.md) are in
`testing_apps/` in your clone.

## Update

From your `bellman` folder, quit Bellman from the menu-bar icon (**Quit**),
then pull and rebuild:

```sh
git pull
cd ui && npm ci && cd ..
CI=true cargo tauri build --bundles app,dmg --ci --no-sign
rm -rf /Applications/Bellman.app
cp -R target/release/bundle/macos/Bellman.app /Applications/
cp /Applications/Bellman.app/Contents/MacOS/bellman "$HOME/.local/bin/bellman"
```

Your timers in `~/Library/Application Support/io.bellman.desktop/` and
`~/.bellman/` are kept.

## Uninstall

1. Quit **Bellman** from the menu-bar icon (**Quit**).
2. If you turned on **Start at login** (in the first-run wizard or Settings),
   turn it off in **Settings** before uninstalling. If you already removed the
   app, unload and delete the `Bellman` LaunchAgent plist written by
   `tauri-plugin-autostart` (and remove Bellman from
   **System Settings → General → Login Items** if listed):

   ```sh
   launchctl bootout "gui/$(id -u)/Bellman" 2>/dev/null || true
   rm -f "$HOME/Library/LaunchAgents/Bellman.plist"
   ```

3. Remove the app bundle and the CLI binary:

   ```sh
   rm -rf /Applications/Bellman.app "$HOME/.local/bin/bellman"
   ```

4. Optional — delete all your timers, logs and slots. **This cannot be undone:**

   ```sh
   rm -rf "$HOME/.bellman" "$HOME/Library/Application Support/io.bellman.desktop"
   ```

   If you used `bellman task new` to write cron entries, run `crontab -e` and
   delete the block between `# BEGIN bellman-managed` and
   `# END bellman-managed`.
