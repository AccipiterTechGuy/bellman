//! Persisted user configuration.
//!
//! Single JSON file at `<data_dir>/config.json` (created on first launch).
//! Holds the first-run wizard answer and the global pause-all flag.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Path of the user config file (under the data dir).
pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("config.json")
}

/// Persisted user preferences.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    /// True once the user has dismissed the first-run wizard (any choice).
    pub wizard_completed: bool,
    /// True when the user opted in to launch on login.
    pub autostart_enabled: bool,
    /// True when the user opted to start the app hidden (tray-only).
    pub start_minimized: bool,
    /// True when the user opted to set up the OS wake-from-sleep feature.
    /// (Wired up in C7; read in C7, used in C11 once wake is implemented.)
    #[serde(default)]
    pub wake_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wizard_completed: false,
            autostart_enabled: false,
            start_minimized: false,
            wake_enabled: false,
        }
    }
}

impl Config {
    /// Load from disk, or return the default if the file is missing / fresh.
    pub fn load(data_dir: &Path) -> std::io::Result<Self> {
        let path = config_path(data_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(cfg) => Ok(cfg),
                Err(e) => {
                    log::warn!(
                        "bellman: config parse failed ({e}); falling back to defaults"
                    );
                    Ok(Self::default())
                }
            },
            Err(e) => Err(e),
        }
    }

    /// Atomically write to disk (temp + rename in the same directory).
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let path = config_path(data_dir);
        let tmp = data_dir.join("config.json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Apply the wizard choice and persist the resulting config.
    pub fn record_wizard_choice(data_dir: &Path, choice: super::first_run::WizardChoice) -> std::io::Result<Self> {
        let mut cfg = Self::load(data_dir)?;
        cfg.wizard_completed = true;
        cfg.autostart_enabled = choice.autostart;
        cfg.start_minimized = choice.start_minimized;
        cfg.wake_enabled = choice.wake_enabled;
        cfg.save(data_dir)?;
        Ok(cfg)
    }
}

/// Read the persisted global pause-all flag (default false). The flag lives
/// in a tiny sidecar file (`pause_all`) so it survives across `--run-now` CLI
/// invocations without forcing a schema migration on `Config`.
pub fn read_pause_all_flag(data_dir: &Path) -> bool {
    let p = data_dir.join("pause_all");
    match std::fs::read(&p) {
        Ok(bytes) => {
            let s = std::str::from_utf8(&bytes).unwrap_or("").trim();
            matches!(s, "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

/// Persist the global pause-all flag.
pub fn write_pause_all_flag(data_dir: &Path, paused: bool) -> std::io::Result<()> {
    let p = data_dir.join("pause_all");
    std::fs::write(&p, if paused { b"1" } else { b"0" })
}
