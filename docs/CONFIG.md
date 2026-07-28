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
| `horizon_secs` | u64 | `86400` (24 h) | Near-horizon heap window |
| `retention_days` | u64 | `30` | JSONL archive retention |
| `min_free_slots` | usize | `5` | Empty free-slot floor |
| `max_concurrent_actions` | usize | `16` | Global wake-action concurrency cap (1..=256) |
| `ack_grace_secs` | u64 | `60` | Grace before terminal one-shots are prune-eligible |
| `accuracy_slack_secs` | u64 | `1` | Default high-frequency accuracy slack |
| `prune_interval_secs` | u64 | `604800` (7 d) | Weekly prune cadence / startup catch-up |
| `default_misfire_policy` | string | `"coalesce"` | Default for new calendar timers (`coalesce` / `skip` / `catch_up`) |
| `default_misfire_grace_secs` | u64 | `3600` | Grace window for coalesce / catch_up defaults |

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
