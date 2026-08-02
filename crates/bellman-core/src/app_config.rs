//! Hand-editable `config.json` for engine tunables.
//!
//! Lives at `<data_dir>/config.json`. Atomic-rename writes (temp + rename in
//! the same directory). Unknown fields are ignored on load (`deny_unknown_fields`
//! is forbidden — BUILD_PLAN rule 7). Missing keys fall back to product defaults
//! so packaging can ship a partial file.

use crate::occurrence::Occurrence;
use crate::store::MisfirePolicy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default near-horizon window (24 h).
pub const DEFAULT_HORIZON_SECS: u64 = 24 * 60 * 60;
/// Default JSONL archive retention (30 days).
pub const DEFAULT_RETENTION_DAYS: u64 = 30;
/// Default empty free-slot floor.
pub const DEFAULT_MIN_FREE_SLOTS: usize = 5;
/// Default global concurrent wake-action cap (resume mass-fire backpressure).
pub const DEFAULT_MAX_CONCURRENT_ACTIONS: usize = 16;
/// Default ack grace before a fired one-shot is eligible for prune (seconds).
pub const DEFAULT_ACK_GRACE_SECS: u64 = 60;
/// Default accuracy slack for high-frequency timers (seconds).
pub const DEFAULT_ACCURACY_SLACK_SECS: u64 = 1;
/// Weekly prune cadence (7 days).
pub const DEFAULT_PRUNE_INTERVAL_SECS: u64 = 7 * 24 * 60 * 60;
/// Default calendar misfire grace (1 hour) — product default Coalesce window.
pub const DEFAULT_MISFIRE_GRACE_SECS: u64 = 3600;
/// Default calendar misfire policy name (`coalesce` | `skip` | `catch_up`).
pub const DEFAULT_MISFIRE_POLICY: &str = "coalesce";
/// Rotate `events.current.jsonl` before an append crosses this (64 MiB).
pub const DEFAULT_LOG_ROTATION_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Retained-log budget for current + final archives (1 GiB).
pub const DEFAULT_LOG_RETENTION_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
/// Default pickup deadline: a fired run with no pickup signal (valid reply or
/// slot-feed `ack_through`) becomes `no_ack` after this many seconds. Its own
/// knob — `ack_grace_secs` seeds the value but is a different job (pruning).
pub const DEFAULT_PICKUP_GRACE_SECS: u64 = DEFAULT_ACK_GRACE_SECS;
/// Default watchdog factor: deadline = `expected_secs × factor`. Forgiving by
/// default so apps do not pad their estimates into fiction.
pub const DEFAULT_WATCHDOG_FACTOR: f64 = 2.0;
/// Aggregate ceiling for the reply quarantine (`bad/`), oldest pairs first.
pub const DEFAULT_QUARANTINE_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// Path of the user/engine config file under the data dir.
pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("config.json")
}

/// Engine + shell preferences persisted as hand-editable JSON.
///
/// Wizard fields share this file so there is a single `config.json`. New keys
/// always have `#[serde(default)]` so older files keep loading.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    /// True once the user has dismissed the first-run wizard (any choice).
    #[serde(default)]
    pub wizard_completed: bool,
    /// True when the user opted in to launch on login.
    #[serde(default)]
    pub autostart_enabled: bool,
    /// True when the user opted to start the app hidden (tray-only).
    #[serde(default)]
    pub start_minimized: bool,
    /// True when the user opted to set up OS wake-from-sleep.
    #[serde(default)]
    pub wake_enabled: bool,
    /// True when the user ticked "Show me the demo" in the first-run wizard.
    /// Remembered so Settings can offer the same demo panel later (WIZ1).
    /// Purely a UI preference: Bellman never creates the demo's timer.
    #[serde(default)]
    pub demo_opt_in: bool,

    /// Near-horizon window in seconds (heap residency). Product default: 86400.
    #[serde(default = "default_horizon_secs")]
    pub horizon_secs: u64,
    /// JSONL archive retention in days. Product default: 30.
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,
    /// Minimum free slot pairs. Product default: 5.
    #[serde(default = "default_min_free_slots")]
    pub min_free_slots: usize,
    /// Global concurrent wake-action cap. Product default: 16.
    #[serde(default = "default_max_concurrent_actions")]
    pub max_concurrent_actions: usize,
    /// Ack grace (seconds) before a terminal one-shot may be pruned.
    #[serde(default = "default_ack_grace_secs")]
    pub ack_grace_secs: u64,
    /// Default accuracy slack (seconds) for high-frequency timers.
    #[serde(default = "default_accuracy_slack_secs")]
    pub accuracy_slack_secs: u64,
    /// Prune cadence in seconds (default 7 days). Startup catch-up when
    /// `now > last_prune + prune_interval_secs`.
    #[serde(default = "default_prune_interval_secs")]
    pub prune_interval_secs: u64,

    /// Default misfire policy for **new calendar** timers: `coalesce`, `skip`,
    /// or `catch_up`. Per-timer overrides still win; this is the Settings
    /// default applied when the dialog does not set a policy.
    #[serde(default = "default_misfire_policy")]
    pub default_misfire_policy: String,
    /// Grace window (seconds) for coalesce / catch_up calendar defaults.
    #[serde(default = "default_misfire_grace_secs")]
    pub default_misfire_grace_secs: u64,

    /// Rotate the live event log before an append would take it past this
    /// many bytes. Product default: 64 MiB.
    #[serde(default = "default_log_rotation_max_bytes")]
    pub log_rotation_max_bytes: u64,
    /// Retained-log budget: current + compressed archives stay within this
    /// many bytes (oldest archives pruned first; the live file is never
    /// deleted). Product default: 1 GiB.
    #[serde(default = "default_log_retention_budget_bytes")]
    pub log_retention_budget_bytes: u64,

    /// Pickup deadline (seconds) for integration-owned runs: no valid reply
    /// and no `ack_through` within this window ⇒ `no_ack`. Product default:
    /// 60 (seeded from the ack grace, but a separately named job).
    #[serde(default = "default_pickup_grace_secs")]
    pub pickup_grace_secs: u64,
    /// Opt-in watchdog factor: deadline = `expected_secs × factor` from
    /// Bellman's receipt of the latest distinct reply. Product default: 2.0.
    #[serde(default = "default_watchdog_factor")]
    pub watchdog_factor: f64,
    /// Aggregate byte ceiling for the reply quarantine (`bad/`). Product
    /// default: 64 MiB; oldest payload/sidecar pairs are removed first.
    #[serde(default = "default_quarantine_budget_bytes")]
    pub quarantine_budget_bytes: u64,

    /// IK6: run the local IPC socket server (one socket for all of Bellman,
    /// `$XDG_RUNTIME_DIR/bellman/bellman.sock` on Linux). Per-timer
    /// `transport.mode` chooses who uses it; with the server off, every
    /// firing resolves to the file transport. Product default: true.
    #[serde(default = "default_ipc_enabled")]
    pub ipc_enabled: bool,
}

fn default_horizon_secs() -> u64 {
    DEFAULT_HORIZON_SECS
}
fn default_retention_days() -> u64 {
    DEFAULT_RETENTION_DAYS
}
fn default_min_free_slots() -> usize {
    DEFAULT_MIN_FREE_SLOTS
}
fn default_max_concurrent_actions() -> usize {
    DEFAULT_MAX_CONCURRENT_ACTIONS
}
fn default_ack_grace_secs() -> u64 {
    DEFAULT_ACK_GRACE_SECS
}
fn default_accuracy_slack_secs() -> u64 {
    DEFAULT_ACCURACY_SLACK_SECS
}
fn default_prune_interval_secs() -> u64 {
    DEFAULT_PRUNE_INTERVAL_SECS
}
fn default_misfire_policy() -> String {
    DEFAULT_MISFIRE_POLICY.to_string()
}
fn default_misfire_grace_secs() -> u64 {
    DEFAULT_MISFIRE_GRACE_SECS
}
fn default_log_rotation_max_bytes() -> u64 {
    DEFAULT_LOG_ROTATION_MAX_BYTES
}
fn default_log_retention_budget_bytes() -> u64 {
    DEFAULT_LOG_RETENTION_BUDGET_BYTES
}
fn default_pickup_grace_secs() -> u64 {
    DEFAULT_PICKUP_GRACE_SECS
}
fn default_watchdog_factor() -> f64 {
    DEFAULT_WATCHDOG_FACTOR
}
fn default_quarantine_budget_bytes() -> u64 {
    DEFAULT_QUARANTINE_BUDGET_BYTES
}
fn default_ipc_enabled() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            wizard_completed: false,
            autostart_enabled: false,
            start_minimized: false,
            wake_enabled: false,
            demo_opt_in: false,
            horizon_secs: DEFAULT_HORIZON_SECS,
            retention_days: DEFAULT_RETENTION_DAYS,
            min_free_slots: DEFAULT_MIN_FREE_SLOTS,
            max_concurrent_actions: DEFAULT_MAX_CONCURRENT_ACTIONS,
            ack_grace_secs: DEFAULT_ACK_GRACE_SECS,
            accuracy_slack_secs: DEFAULT_ACCURACY_SLACK_SECS,
            prune_interval_secs: DEFAULT_PRUNE_INTERVAL_SECS,
            default_misfire_policy: DEFAULT_MISFIRE_POLICY.to_string(),
            default_misfire_grace_secs: DEFAULT_MISFIRE_GRACE_SECS,
            log_rotation_max_bytes: DEFAULT_LOG_ROTATION_MAX_BYTES,
            log_retention_budget_bytes: DEFAULT_LOG_RETENTION_BUDGET_BYTES,
            pickup_grace_secs: DEFAULT_PICKUP_GRACE_SECS,
            watchdog_factor: DEFAULT_WATCHDOG_FACTOR,
            quarantine_budget_bytes: DEFAULT_QUARANTINE_BUDGET_BYTES,
            ipc_enabled: true,
        }
    }
}

impl AppConfig {
    /// Load from disk, or return defaults if the file is missing.
    ///
    /// Parse failures fall back to defaults (with a best-effort warning via
    /// the returned `Ok` path — callers may log). Corrupt files are not
    /// overwritten until the next explicit [`Self::save`].
    pub fn load(data_dir: &Path) -> std::io::Result<Self> {
        let path = config_path(data_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(&path)?;
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(cfg) => Ok(cfg.sanitized()),
            Err(_) => Ok(Self::default()),
        }
    }

    /// Atomically write to disk (temp + rename in the same directory).
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = config_path(data_dir);
        let tmp = data_dir.join("config.json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Clamp nonsensical values to safe product floors/ceilings.
    pub fn sanitized(mut self) -> Self {
        if self.horizon_secs == 0 {
            self.horizon_secs = DEFAULT_HORIZON_SECS;
        }
        if self.retention_days == 0 {
            self.retention_days = DEFAULT_RETENTION_DAYS;
        }
        if self.min_free_slots == 0 {
            self.min_free_slots = DEFAULT_MIN_FREE_SLOTS;
        }
        if self.max_concurrent_actions == 0 {
            self.max_concurrent_actions = 1;
        }
        // Hard ceiling so a typo cannot open thousands of concurrent launches.
        if self.max_concurrent_actions > 256 {
            self.max_concurrent_actions = 256;
        }
        if self.prune_interval_secs == 0 {
            self.prune_interval_secs = DEFAULT_PRUNE_INTERVAL_SECS;
        }
        let p = self.default_misfire_policy.to_ascii_lowercase();
        self.default_misfire_policy = match p.as_str() {
            "skip" | "coalesce" | "catch_up" => p,
            _ => DEFAULT_MISFIRE_POLICY.to_string(),
        };
        if self.default_misfire_grace_secs == 0 {
            self.default_misfire_grace_secs = DEFAULT_MISFIRE_GRACE_SECS;
        }
        // Sane floors: a rotation cap must fit at least a few events, and the
        // budget must fit at least one rotated extent.
        if self.log_rotation_max_bytes < 1024 * 1024 {
            self.log_rotation_max_bytes = 1024 * 1024;
        }
        if self.log_retention_budget_bytes < 4 * 1024 * 1024 {
            self.log_retention_budget_bytes = 4 * 1024 * 1024;
        }
        if self.pickup_grace_secs == 0 {
            self.pickup_grace_secs = DEFAULT_PICKUP_GRACE_SECS;
        }
        if !(self.watchdog_factor.is_finite() && self.watchdog_factor > 0.0) {
            self.watchdog_factor = DEFAULT_WATCHDOG_FACTOR;
        }
        if self.quarantine_budget_bytes < 1024 * 1024 {
            self.quarantine_budget_bytes = 1024 * 1024;
        }
        self
    }

    /// Pickup deadline for integration-owned runs (R7).
    pub fn pickup_grace(&self) -> Duration {
        Duration::from_secs(self.pickup_grace_secs)
    }

    /// `horizon_secs` as a `Duration`.
    pub fn horizon(&self) -> Duration {
        Duration::from_secs(self.horizon_secs)
    }

    /// `retention_days` as a `Duration`.
    pub fn retention(&self) -> Duration {
        Duration::from_secs(self.retention_days.saturating_mul(24 * 60 * 60))
    }

    /// `ack_grace_secs` as a `Duration` — the prune eligibility window, not
    /// the reply pickup deadline (that is `pickup_grace`).
    pub fn ack_grace(&self) -> Duration {
        Duration::from_secs(self.ack_grace_secs)
    }

    /// `prune_interval_secs` as a `Duration`.
    pub fn prune_interval(&self) -> Duration {
        Duration::from_secs(self.prune_interval_secs)
    }

    /// `accuracy_slack_secs` as a `Duration`.
    pub fn accuracy_slack(&self) -> Duration {
        Duration::from_secs(self.accuracy_slack_secs)
    }

    /// Resolve the misfire policy for a **new** timer from Settings defaults.
    ///
    /// - Interval (elapsed-time) timers always keep product default [`MisfirePolicy::Skip`]
    ///   — Settings defaults only apply to calendar kinds.
    /// - Calendar kinds use `default_misfire_policy` + `default_misfire_grace_secs`.
    pub fn misfire_for_occurrence(&self, occ: &Occurrence) -> MisfirePolicy {
        if occ.kind().is_elapsed_time() {
            return MisfirePolicy::default_interval();
        }
        let grace = self.default_misfire_grace_secs;
        match self.default_misfire_policy.as_str() {
            "skip" => MisfirePolicy::Skip,
            "catch_up" => MisfirePolicy::CatchUp {
                grace_secs: grace,
                max_catch_up: 10,
            },
            // "coalesce" and any other sanitized value
            _ => MisfirePolicy::Coalesce { grace_secs: grace },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::occurrence::{Occurrence, OccurrenceKind};
    use chrono::{NaiveTime, TimeZone, Utc};

    #[test]
    fn defaults_match_product() {
        let c = AppConfig::default();
        assert_eq!(c.horizon_secs, DEFAULT_HORIZON_SECS);
        assert_eq!(c.retention_days, DEFAULT_RETENTION_DAYS);
        assert_eq!(c.min_free_slots, DEFAULT_MIN_FREE_SLOTS);
        assert_eq!(c.max_concurrent_actions, DEFAULT_MAX_CONCURRENT_ACTIONS);
    }

    #[test]
    fn misfire_for_occurrence_applies_calendar_defaults() {
        let mut c = AppConfig {
            default_misfire_policy: "skip".into(),
            default_misfire_grace_secs: 120,
            ..Default::default()
        };
        let daily = Occurrence::new(
            OccurrenceKind::Daily {
                at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            },
            "UTC",
        )
        .unwrap();
        assert_eq!(c.misfire_for_occurrence(&daily), MisfirePolicy::Skip);

        c.default_misfire_policy = "catch_up".into();
        c.default_misfire_grace_secs = 90;
        assert_eq!(
            c.misfire_for_occurrence(&daily),
            MisfirePolicy::CatchUp {
                grace_secs: 90,
                max_catch_up: 10,
            }
        );

        c.default_misfire_policy = "coalesce".into();
        c.default_misfire_grace_secs = 42;
        assert_eq!(
            c.misfire_for_occurrence(&daily),
            MisfirePolicy::Coalesce { grace_secs: 42 }
        );
    }

    #[test]
    fn misfire_for_occurrence_interval_always_skip() {
        let c = AppConfig {
            default_misfire_policy: "coalesce".into(),
            default_misfire_grace_secs: 99,
            ..Default::default()
        };
        let interval = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 60,
                anchor: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            },
            "UTC",
        )
        .unwrap();
        assert_eq!(
            c.misfire_for_occurrence(&interval),
            MisfirePolicy::Skip,
            "interval timers must not pick up calendar settings defaults"
        );
    }

    #[test]
    fn load_missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = AppConfig::load(dir.path()).unwrap();
        assert_eq!(c, AppConfig::default());
    }

    #[test]
    fn save_load_roundtrip_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let c = AppConfig {
            horizon_secs: 3600,
            retention_days: 14,
            min_free_slots: 7,
            max_concurrent_actions: 8,
            ..AppConfig::default()
        };
        c.save(dir.path()).unwrap();
        assert!(!dir.path().join("config.json.tmp").exists());
        let loaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(loaded.horizon_secs, 3600);
        assert_eq!(loaded.retention_days, 14);
        assert_eq!(loaded.min_free_slots, 7);
        assert_eq!(loaded.max_concurrent_actions, 8);
    }

    #[test]
    fn unknown_fields_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            br#"{"horizon_secs":120,"future_key":true}"#,
        )
        .unwrap();
        let c = AppConfig::load(dir.path()).unwrap();
        assert_eq!(c.horizon_secs, 120);
        assert_eq!(c.retention_days, DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn sanitized_clamps_zero_and_huge_concurrency() {
        let c = AppConfig {
            max_concurrent_actions: 0,
            horizon_secs: 0,
            ..AppConfig::default()
        }
        .sanitized();
        assert_eq!(c.max_concurrent_actions, 1);
        assert_eq!(c.horizon_secs, DEFAULT_HORIZON_SECS);

        let c2 = AppConfig {
            max_concurrent_actions: 10_000,
            ..AppConfig::default()
        }
        .sanitized();
        assert_eq!(c2.max_concurrent_actions, 256);
    }
}
