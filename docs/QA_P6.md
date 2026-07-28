# QA P6 — packaging + CI + fresh-install

Card: packaging (C10 / P6). Feature-complete app is frozen — this document
covers **build artefacts, installers, launcher entry, CLI on PATH, autostart,
and CI**. No feature work lives here.

Related: `docs/BUILD_PLAN.md` §P6, `docs/PERF.md`, `docs/QA_P3.md` (shell
behaviour), `docs/QA_P4.md` / `docs/QA_P4b.md` (GUI).

## Artefacts produced

| OS | Bundle targets | Notes |
|---|---|---|
| Linux | `.deb` + `.AppImage` | **No Flatpak / Snap** (breaks tray, autostart, single-instance) |
| Windows | NSIS `.exe` + WiX `.msi` | WebView2 **evergreen download bootstrapper** (`webviewInstallMode: downloadBootstrapper`) |
| macOS | `.app` + `.dmg` | Signing + notarization **stubbed** (see CI secrets below) |

Local (this machine):

```sh
cd ui && npm ci && npm run build && cd ..
cargo tauri build --bundles deb,appimage --ci --no-sign
# → target/release/bundle/deb/Bellman_0.1.0_amd64.deb
# → target/release/bundle/appimage/Bellman_0.1.0_amd64.AppImage
```

After install of the deb:

| Path | Role |
|---|---|
| `/usr/bin/bellman-app` | Tray GUI shell (main binary) |
| `/usr/bin/bellman` | Headless AI-skill CLI (`add\|list\|edit\|rm\|next\|…`) |
| `/usr/share/applications/Bellman.desktop` | Launcher entry (Name=Bellman, Exec=bellman-app) |
| `/usr/share/icons/hicolor/*/apps/…` | Freedesktop icons |

Dual binary names resolve the pre-existing cargo clash (both the Tauri package
and `bellman-cli` wanted the binary name `bellman`). The GUI binary is
`bellman-app` because `src-tauri/Cargo.toml` renames the package to
`bellman-app` and declares `[[bin]] name = "bellman-app"` (there is no
`mainBinaryName` key in `tauri.conf.json`). The CLI is staged as a Tauri
`externalBin` sidecar named `bellman` (`scripts/stage_cli_sidecar.sh`).

## Fresh-install smoke

Automated (layout + install + PATH + launcher file):

```sh
# After a green `cargo tauri build --bundles deb,appimage`:
scripts/smoke_install_deb.sh                          # host install (sudo)
SMOKE_MODE=docker scripts/smoke_install_deb.sh        # container-only
SMOKE_KEEP=1 scripts/smoke_install_deb.sh             # leave package installed
```

What the host smoke proves:

1. `dpkg -i` succeeds (or `apt-get install -f` repairs deps).
2. `bellman` and `bellman-app` are on `PATH`.
3. `bellman --help` / `bellman list --json` run (CLI is the real sidecar).
4. A `*.desktop` entry exists under `/usr/share/applications/`.
5. An XDG autostart file can be written for `bellman-app` (plugin-equivalent
   path used by `tauri-plugin-autostart`).

### Manual VM checklist (install → autostart → timer after restart)

Use a clean Ubuntu 24.04 GNOME (Wayland) **or** KDE Plasma (X11) VM / second
user session. AppImage path is analogous (mark executable, run once, enable
autostart from the first-run wizard).

| # | Step | Expected |
|---|---|---|
| 1 | `sudo dpkg -i Bellman_0.1.0_amd64.deb` (or `apt install ./…`) | Package installs; no unpack errors |
| 2 | Open **Activities / app launcher**, search **Bellman** | Icon appears; launching opens the main window |
| 3 | First-run wizard: enable **autostart** | `~/.config/autostart/` gains a Bellman `.desktop` with `Exec=…/bellman-app` |
| 4 | From a terminal: `bellman add --name p6-smoke --occurrence interval --every-secs 30 --json` | `ok: true` JSON envelope |
| 5 | Confirm timer listed in GUI All-timers tab | Next-fire countdown updates |
| 6 | **Log out and log back in** (or reboot) | Bellman tray returns without manual start; autostart `.desktop` still present |
| 7 | Wait ≥30 s after session is up | Timer fires (JSONL under `~/.bellman/logs/events.current.jsonl` gains `fired` / action lines; notification if Notify action) |
| 8 | `bellman list --json` still works post-relog | CLI on PATH unchanged |

**Autostart survives relog (Linux XDG):** the desktop entry under
`~/.config/autostart/` is plain XDG and is re-read by the session manager on
login. It is **not** a systemd user unit. Removing the entry (wizard toggle
off, or delete the file) stops autostart on the next login. Observed path from
`tauri-plugin-autostart` on this stack:

```
~/.config/autostart/Bellman.desktop
# or, depending on plugin version / identifier:
~/.config/autostart/io.bellman.desktop.desktop
```

`Exec=` points at the installed GUI binary (`/usr/bin/bellman-app` after deb
install, or the AppImage path when running from AppImage).

AppImage note: autostart records the **absolute path** of the AppImage file at
enable time — moving the AppImage breaks autostart until re-enabled (known
Tauri/plugin limitation; prefer the deb for permanent installs).

## CI workflows

Three workflow files under `.github/workflows/`:

| File | Runner | What it does |
|---|---|---|
| `linux.yml` | `ubuntu-24.04` | stage CLI sidecar + `clippy -D warnings` + full `cargo test --workspace` + UI vitest + `cargo tauri build --bundles deb,appimage` + artefact upload (no `cargo fmt` step — pre-existing sources are not rustfmt-clean; packaging does not reformat C1–C9) |
| `windows.yml` | `windows-latest` | workspace tests + `cargo tauri build --bundles nsis,msi --no-sign` (WebView2 evergreen bootstrapper from conf) |
| `macos.yml` | `macos-latest` | workspace tests + `cargo tauri build --bundles app,dmg --no-sign`; signing/notarization **stubbed** with named secrets |

### Lint the workflows locally

```sh
# Install actionlint once (https://github.com/rhysd/actionlint):
#   go install github.com/rhysd/actionlint/cmd/actionlint@latest
#   # or download release binary into PATH
actionlint .github/workflows/*.yml
```

### macOS signing secrets (TODO — stubbed)

| Secret name | Purpose |
|---|---|
| `APPLE_CERTIFICATE` | base64-encoded Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | password for the `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: … (TEAMID)` |
| `APPLE_ID` | Apple ID email for `notarytool` |
| `APPLE_PASSWORD` | app-specific password |
| `APPLE_TEAM_ID` | 10-char team id |
| `APPLE_API_KEY` / `APPLE_API_KEY_ID` / `APPLE_API_ISSUER` | alternative notarytool API-key auth |

Workflows currently pass `--no-sign` so unsigned artefacts build without these
secrets. When secrets are provisioned, drop `--no-sign` and add staple.

## Idle footprint

Engine numbers (P5) and post-package tray RSS live in **`docs/PERF.md`**. P6
adds the packaged-binary measurement recipe; re-run after every packaging
change that touches release profile flags.

## Acceptance matrix (this card)

| Gate | How verified |
|---|---|
| Local deb + AppImage build green | `cargo tauri build --bundles deb,appimage` |
| Deb install → Bellman in launcher | desktop file under `/usr/share/applications/` + smoke script |
| `bellman` CLI on PATH | sidecar binary; `command -v bellman` after install |
| Three workflow files lint | `actionlint .github/workflows/*.yml` |
| Linux workflow end-to-end on GitHub | push/PR runs `linux.yml` (clippy, tests, package) |
| `docs/QA_P6.md` complete | this file |
| Autostart QA documented | §Fresh-install smoke + manual VM table |
| Idle footprint recorded | `docs/PERF.md` |

## Out of scope (later cards)

- Feature work / refactors of C1–C9 behaviour
- Flatpak / Snap
- Real Apple notarization (secrets not on this machine)
- Windows SmartScreen / code-sign cert
- P7 wake-from-sleep RTC + Settings wake panel
