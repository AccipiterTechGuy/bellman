# Bellman

Cross-platform (Windows / macOS / Linux) **task scheduler** desktop app — the
desktop cousin of cron. Named after the bellmen (knocker-uppers) who woke
people before alarm clocks existed: Bellman's job is waking *applications*.

- Timers with name, time, and occurrence (once / interval / daily / weekly /
  monthly / yearly), second-level resolution, year-round calendar with automatic
  new-year recalibration.
- Two drive modes: a `bellman` CLI (AI-skill friendly) and a GUI with three
  pages — events list with next-fire times, weekly repeats, monthly calendar.
- **JSON slot-pair integration layer**: external apps register, modify, or
  delete their own wake-up timers by writing one JSON file; ≥5 empty slot pairs
  are always kept ready and auto-replenished.
- JSONL event log with weekly pruning; memory-smart core — only near-horizon
  timers stay resident (min-heap window).

## Status

Feature-complete core + GUI through P5; **P6 packaging** ships Linux
`.deb` / `.AppImage`, Windows NSIS/MSI and macOS dmg CI (unsigned), with the
`bellman` CLI and `bellman-app` tray shell co-installed. See
[docs/QA_P6.md](docs/QA_P6.md) for install smoke, [docs/PLAN.md](docs/PLAN.md)
for the product spec, and [docs/BUILD_PLAN.md](docs/BUILD_PLAN.md) for the
phase plan.

### Quick package (Linux)

```sh
cd ui && npm ci && npm run build && cd ..
bash scripts/stage_cli_sidecar.sh
cargo tauri build --bundles deb,appimage --ci --no-sign
# → target/release/bundle/deb/Bellman_*.deb
# → target/release/bundle/appimage/Bellman_*.AppImage
scripts/smoke_install_deb.sh                  # host (sudo) or:
SMOKE_MODE=docker scripts/smoke_install_deb.sh
```

After deb install: launcher entry **Bellman**, CLI `bellman` on PATH, GUI
binary `bellman-app`.
