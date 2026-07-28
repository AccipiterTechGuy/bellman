# Bellman — idle footprint (P5)

**Measured** (not estimated) on the development box after the P5 hardening pass.
Single observed values from a real run; raw evidence is committed under
`docs/qa4-evidence/perf-idle/`.

## Environment

| Item | Value |
|---|---|
| Host | Linux 7.0.0-28-generic x86_64 (see report `host`) |
| Binary measured | `target/release/examples/perf_idle` (engine-only) |
| Build | `cargo build -p bellman-core --example perf_idle --release` |
| Measured at | 2026-07-28T13:00:29Z → 13:10:29Z (UTC) |
| Card | train/2026-07-28_0005 (P5 pruner + hardening) |
| PID | 887356 |

## Method (reproducible)

Harness: `scripts/perf_idle.sh` → `examples/perf_idle.rs`.

1. Open a fresh store under `docs/qa4-evidence/perf-idle/run-data/`.
2. Create **one** 1 s interval timer (`Action::None`).
3. Boot `Scheduler` + `ActionRunner` with a real `EventLog` under
   `run-data/logs/events.current.jsonl`.
4. Drive `run_for` for **600 s** wall time (`SystemClock`).
5. Sample `/proc/<pid>/status` **VmRSS** every 30 s.
6. Sample `/proc/<pid>/stat` utime+stime (jiffies) at start and end; convert
   with `CLK_TCK` from `getconf`.
7. Count JSONL lines with `"kind":"fired"` as fire-rate evidence.

```bash
# Full 10-minute acceptance window:
scripts/perf_idle.sh

# 60 s smoke:
PERF_SECS=60 scripts/perf_idle.sh
```

## Measured numbers (this run)

| Metric | Value | Source |
|---|---|---|
| Engine RSS (median of 21 samples) | **7092 KiB (6.93 MiB)** | `VmRSS` in `/proc/887356/status` |
| Engine RSS min → max | 6956 → 7208 KiB | same, samples every 30 s |
| Wall window | **600.000 s** | harness wall clock |
| CPU over window | **0.093 %** | 56 jiffies / (600 s × 100 Hz) |
| `fired` events in JSONL | **600** | `grep -c '"kind":"fired"' events.current.jsonl` |
| Fires / min (1 s timer) | **60.0** | 600 fires / 10.0 min |
| `wake_delivered` lines | 600 | one per fire (`Action::None`) |
| Total JSONL lines | 1201 | +1 `year_recalibrate` at boot |

### Event-log evidence

Committed artifacts:

- `docs/qa4-evidence/perf-idle/perf_idle_report.json` — full machine-readable report
- `docs/qa4-evidence/perf-idle/events.current.jsonl` — 600× `fired` + 600× `wake_delivered`
- `docs/qa4-evidence/perf-idle/harness.log` — stdout of the 10 min run (RSS sample stream)
- `docs/qa4-evidence/perf-idle/perf_idle.stdout` — final report dump

Spot-check:

```bash
grep -c '"kind":"fired"' docs/qa4-evidence/perf-idle/events.current.jsonl   # → 600
head -n 3 docs/qa4-evidence/perf-idle/events.current.jsonl
tail -n 3 docs/qa4-evidence/perf-idle/events.current.jsonl
python3 -c 'import json;print(json.load(open("docs/qa4-evidence/perf-idle/perf_idle_report.json"))["cpu_pct_over_window"])'
```

First fire ~`2026-07-28T13:00:30Z`, last fire ~`2026-07-28T13:10:29Z` — a full
10-minute cadence with no gaps large enough to drop the 60/min average.

## Resident shell (GUI closed) — packaging measurement (P6)

P6 ships the tray binary as **`bellman-app`** (CLI sidecar is `bellman`).
Measure after a release package build:

```bash
# Build (or install the deb):
cargo tauri build --bundles deb --ci --no-sign
# Run under an isolated data dir so we don't touch the user store:
env XDG_DATA_HOME=$PWD/target/perf-tray/data \
    XDG_CONFIG_HOME=$PWD/target/perf-tray/config \
    HOME=$PWD/target/perf-tray/home \
    target/release/bellman-app &
sleep 8
# Close the main window from the UI (or send a hide via the tray) so only
# the tray + engine remain; then:
pid=$(pgrep -n -f 'bellman-app' || true)
if [[ -n "$pid" ]]; then
  grep VmRSS /proc/$pid/status
  # Optional: sample for 60 s
  for i in $(seq 1 6); do sleep 10; awk '/VmRSS/{print}' /proc/$pid/status; done
fi
kill "$pid" 2>/dev/null || true
```

| Metric | Value | Notes |
|---|---|---|
| Engine RSS (P5 gate, above) | **7092 KiB median** | engine-only harness |
| Tray shell RSS (window open, P6) | **~192 MiB (192100–192536 KiB)** | `target/release/bellman-app`, DISPLAY=:0, 2026-07-28 |
| Tray shell RSS (GUI closed) | **not isolated this run** | window was left open for the sample; re-measure after hide in C11 |

### P6 headed sample (2026-07-28)

```
pid=983737  binary=target/release/bellman-app
VmRSS samples (≈2 s apart, window open): 192536 → 192536 → 192100 KiB
stderr showed scheduler boot: system.prune ready, year_recalibrate checked=1
```

WebKitGTK + tray dominates RSS versus the ~7 MiB engine-only harness — expected.
The **engine RSS row remains the numeric idle gate** for the core scheduler;
tray RSS is informational packaging evidence for C11 to refine (hide window,
sample over 10 min).

## Notes

- High-frequency timers stay on the heap (period &lt; 5 min); low-frequency
  timers are horizon-loaded only.
- Weekly prune + Jan-1 recalibration run at most once per cadence and are not
  on the 1 s wake path (this run emitted one `year_recalibrate` at boot only).
- Concurrency cap under mass-fire is covered by
  `actions::concurrency::tests::peak_never_exceeds_cap_under_500_parallel`
  (unit), separate from idle footprint.
