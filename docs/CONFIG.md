# config.json keys (P5+)

Hand-editable JSON at `<data_dir>/config.json` (Linux: `~/.bellman/config.json`).
Writes are atomic (temp + rename). Unknown fields are ignored; missing keys use
the defaults below.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `wizard_completed` | bool | `false` | First-run wizard dismissed |
| `autostart_enabled` | bool | `false` | Launch on login |
| `start_minimized` | bool | `false` | Start tray-only |
| `wake_enabled` | bool | `false` | Master toggle: allow RTC wake-from-sleep (P7) |
| `demo_opt_in` | bool | `false` | Wizard's "Show me the demo" tick (WIZ1); Settings offers the same demo panel from this key. UI preference only — Bellman never creates the demo's timer |
| `horizon_secs` | u64 | `86400` (24 h) | Near-horizon heap window |
| `retention_days` | u64 | `30` | JSONL archive retention (30-day history) |
| `log_rotation_max_bytes` | u64 | `67108864` (64 MiB) | Rotate `events.current.jsonl` before an append crosses this |
| `log_retention_budget_bytes` | u64 | `1073741824` (1 GiB) | Retained-log budget: current + archives; oldest archives pruned first |
| `min_free_slots` | usize | `5` | Empty free-slot floor |
| `max_concurrent_actions` | usize | `16` | Global wake-action concurrency cap (1..=256) |
| `ack_grace_secs` | u64 | `60` | Grace before terminal one-shots are prune-eligible |
| `accuracy_slack_secs` | u64 | `1` | Default high-frequency accuracy slack |
| `prune_interval_secs` | u64 | `604800` (7 d) | Weekly prune cadence / startup catch-up |
| `default_misfire_policy` | string | `"coalesce"` | Default for new calendar timers (`coalesce` / `skip` / `catch_up`) |
| `default_misfire_grace_secs` | u64 | `3600` | Grace window for coalesce / catch_up defaults |
| `pickup_grace_secs` | u64 | `60` | Pickup deadline for integration-owned runs: no valid reply and no `ack_through` within this window ⇒ `no_ack` (a separate job from `ack_grace_secs`, which is pruning) |
| `watchdog_factor` | f64 | `2.0` | Opt-in watchdog multiplier: deadline = `expected_secs × factor` from Bellman's receipt of the latest distinct reply |
| `quarantine_budget_bytes` | u64 | `67108864` (64 MiB) | Aggregate ceiling for the reply quarantine (`timers/bad/`); oldest payload/sidecar pairs pruned first |
| `ipc_enabled` | bool | `true` | Run the local IPC socket server (`$XDG_RUNTIME_DIR/bellman/bellman.sock` on Linux); per-timer `transport.mode` chooses who uses it, everything else stays on files |

## Values are sanitised on load — the floors are real

Bellman clamps a few keys when it reads the file, so a value below a floor is
**silently raised** rather than honoured. A too-small rotation threshold is
the one that surprises people: the log simply never rotates at the size you
asked for.

| Key | Floor / clamp |
|---|---|
| `log_rotation_max_bytes` | **1 MiB** — smaller values are raised to 1 MiB |
| `log_retention_budget_bytes` | **4 MiB** — smaller values are raised to 4 MiB |
| `max_concurrent_actions` | clamped into `1..=256` |
| `horizon_secs`, `retention_days`, `min_free_slots`, `prune_interval_secs`, `ack_grace_secs`, `pickup_grace_secs`, `default_misfire_grace_secs` | `0` means "unset" and falls back to the default in the table above |
| `watchdog_factor` | must be finite and > 0, else the default `2.0` |
| `default_misfire_policy` | anything outside `skip` / `coalesce` / `catch_up` falls back to `coalesce` |

`prune_interval_secs` is the **startup catch-up** threshold ("is a prune
overdue?"), not the cadence of the visible `system.prune` timer — that one is
weekly and is listed by `bellman list` like any other timer.

Sidecar (not JSON): `pause_all` file contains `1`/`0` for vacation mode.

Example minimal ship default:

```json
{
  "horizon_secs": 86400,
  "retention_days": 30,
  "min_free_slots": 5,
  "max_concurrent_actions": 16
}
```
