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
    cfg.demo_opt_in = choice.demo;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(demo: bool) -> super::super::first_run::WizardChoice {
        super::super::first_run::WizardChoice {
            autostart: true,
            start_minimized: false,
            wake_enabled: false,
            demo,
        }
    }

    #[test]
    fn record_wizard_choice_persists_demo_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        // Unticked (the default) persists as false …
        let cfg = record_wizard_choice(dir.path(), choice(false)).unwrap();
        assert!(!cfg.demo_opt_in);
        let loaded = Config::load(dir.path()).unwrap();
        assert!(!loaded.demo_opt_in);
        // … and ticking it persists as true (Settings reads this key).
        let cfg = record_wizard_choice(dir.path(), choice(true)).unwrap();
        assert!(cfg.demo_opt_in);
        let loaded = Config::load(dir.path()).unwrap();
        assert!(loaded.demo_opt_in);
    }

    /// WIZ1 exit-gate invariant: the wizard never creates the demo's timer —
    /// under either choice the store's timer count is unchanged.
    #[test]
    fn record_wizard_choice_creates_no_timer() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("timers.db");
        let store = bellman_core::open_store(&db).unwrap();
        let before = store.list_timers().unwrap().len();
        drop(store);

        for demo in [false, true] {
            record_wizard_choice(dir.path(), choice(demo)).unwrap();
            let store = bellman_core::open_store(&db).unwrap();
            let after = store.list_timers().unwrap().len();
            assert_eq!(
                before, after,
                "wizard choice (demo={demo}) must not create a timer"
            );
        }
    }
}

