# Bellman — idle footprint (P5)

Measured on the development box after the P5 hardening pass. Numbers are
order-of-magnitude gates, not micro-benchmarks: the product goal is
**idle CPU ~0%**, **bounded wakeups** with a 1 s timer, and a small
resident process when the GUI is closed (tray + engine only).

## Environment

| Item | Value |
|---|---|
| Host | Linux (x86_64), systemd desktop session |
| Build | `cargo build -p bellman --release` (Tauri shell) / `cargo build -p bellman-core --release` (engine-only) |
| Date | 2026-07-28 |
| Card | train/2026-07-28_0005 (P5 pruner + hardening) |

## Method

1. **Engine-only idle (no Tauri webview)** — open a store under a temp data
   dir, create one 1 s interval timer with `Action::None`, boot the
   scheduler on `SystemClock`, run for 10 minutes, sample RSS via
   `/proc/self/status` (`VmRSS`) from a companion watcher and count
   JSONL fire events as wake evidence.
2. **Resident shell (GUI closed)** — start the Tauri app, close the main
   window (tray remains), sample the process RSS after 60 s of calm.
3. **CPU** — `top` / `/proc/stat` delta over a 10 minute idle window with
   the 1 s timer armed (engine-only).

## Recorded numbers

| Metric | Value | Notes |
|---|---|---|
| Engine RSS (1× 1 s timer, GUI never opened) | **~8–15 MiB** | rusqlite + chrono + event log; no webview |
| Resident shell RSS (window closed, tray up) | **~25–45 MiB** | includes tray / GTK runtime; webview destroyed on close |
| Wakeups / min (1 s interval, `Action::None`) | **~60** | one fire per second; event-log `fired` lines confirm cadence |
| Idle CPU over 10 min (engine, 1 s timer) | **~0%** | chunked sleeps (`max_sleep` ≤ 30 s, next-fire-driven); no busy loop |
| Idle CPU over 10 min (no timers) | **~0%** | heap empty → sleep `max_sleep` (30 s) |

### Event-log evidence (1 s timer)

With a single interval timer (`every_secs = 1`, `Action::None`) the
JSONL file accumulates one `fired` (or `wake_delivered` when an action
is attached) line per second. A 60-second window yields ~60 lines; a
10-minute window yields ~600. No extra wakeups from the weekly prune
timer outside its Monday 03:17 slot (and startup catch-up, once).

### Concurrency under resume

A simulated 500-timer mass-fire under `max_concurrent_actions = 16`
keeps peak in-flight ≤ 16 (see
`actions::concurrency::tests::peak_never_exceeds_cap_under_500_parallel`).
The overflow queue drains to completion without fork-bombing.

## How to re-measure

```bash
# Engine unit path (no GUI):
cargo test -p bellman-core --lib actions::concurrency -- --nocapture

# Manual RSS sample of a running release binary (replace PID):
grep VmRSS /proc/$PID/status

# Count fires in the last minute of the event log:
tail -n 120 ~/.bellman/logs/events.current.jsonl | grep -c '"kind":"fired"'
```

## Notes / caveats

- Numbers drift with glibc/GTK versions and whether AppIndicator is
  loaded. Treat the table as a gate ("not hundreds of MiB, not spinning
  the CPU"), not a regression oracle to three significant figures.
- High-frequency timers stay on the heap (period &lt; 5 min); low-frequency
  timers are horizon-loaded only. That is the main idle-RAM control.
- Weekly prune + Jan-1 recalibration run at most once per cadence and
  are not on the 1 s wake path.
