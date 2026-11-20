# Developing Bellman

Back to the [README](../README.md). See also [CONTRIBUTING.md](../CONTRIBUTING.md).

Building and installing the packages is covered in the install guides for
[Linux](INSTALL_LINUX.md) and [macOS](INSTALL_MACOS.md). This section is
for working *on* Bellman from a checkout.

## Dev launch (this tree)

```sh
./launch.sh                    # freshness-aware: fresh GUI binary, else rebuild / tauri dev
./restart_bellman.sh           # safely restart only this checkout's Bellman GUI
scripts/install_desktop.sh     # repo-controlled ~/.local/share/applications/Bellman.desktop
```

`launch.sh` never silently reuses a stale `target/release` or `target/debug`
`bellman-app`. A binary is **fresh** only when its mtime is ≥ every GUI-affecting
input (`crates/`, `src-tauri/{src,capabilities,icons,linux}`, manifests/lock,
`ui/src`, `ui/index.html`, vite/svelte configs, package files). Stale reuse
requires an explicit opt-in: `BELLMAN_ALLOW_STALE=1` (alias
`BELLMAN_APP_ALLOW_STALE=1`). Otherwise the launcher rebuilds
(`cargo tauri build --no-bundle`) or enters `cargo tauri dev` — and still will
**not** exec a still-stale binary after a no-op rebuild unless that opt-in is set.

`scripts/install_desktop.sh` installs a developer desktop entry that **Exec**s
this tree’s `launch.sh`, uses the Bellman icon from `src-tauri/icons` (not a
stock theme icon), and keeps a single main `Categories=Utility;` so
`desktop-file-validate` stays clean. Packaged deb/AppImage entries still use
`src-tauri/linux/bellman.desktop` via Tauri’s `desktopTemplate`.

Headless selection tests: `./tests/launch_freshness.sh`. Safe worktree metadata
prune: `scripts/repo_hygiene.sh` (absent worktree records only).

## Reference

See [PLAN.md](PLAN.md) for the full specification and decided logic,
[BUILD_PLAN.md](BUILD_PLAN.md) for the phased build,
[research/synthesis.md](research/synthesis.md) for the four-way independent
research synthesis behind the stack choice (Tauri v2 + Rust core), and
[research/rtc_wake_synthesis.md](research/rtc_wake_synthesis.md) for the
per-OS wake-from-sleep design.
