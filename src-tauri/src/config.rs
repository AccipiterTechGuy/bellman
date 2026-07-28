//! Persisted user configuration — thin shell over `bellman_core::AppConfig`.
//!
//! Single JSON file at `<data_dir>/config.json` (created on first launch /
//! first save). Hand-editable; atomic-rename writes. Engine keys (horizon,
//! retention, slot floor, concurrency cap) live alongside wizard fields so
//! packaging can ship one sane default file.

pub use bellman_core::{config_path, AppConfig as Config};

use std::path::Path;

/// Apply the wizard choice and persist the resulting config.
pub fn record_wizard_choice(
    data_dir: &Path,
    choice: super::first_run::WizardChoice,
) -> std::io::Result<Config> {
    let mut cfg = Config::load(data_dir)?;
    cfg.wizard_completed = true;
    cfg.autostart_enabled = choice.autostart;
    cfg.start_minimized = choice.start_minimized;
    cfg.wake_enabled = choice.wake_enabled;
    cfg.save(data_dir)?;
    Ok(cfg)
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
