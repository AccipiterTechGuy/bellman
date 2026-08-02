//! Visible Scheduler — one honest list of every schedule on the machine.
//!
//! Linux v1 discovers user/system crontabs, cron.d, run-parts dirs, anacron,
//! systemd timers (system + user), `at` jobs, and Bellman store timers.
//! Windows/macOS return an explicit "not implemented" note rather than an
//! empty list that would read as "nothing scheduled".
//!
//! **Safety:** reading is free; writing is restricted to the invoking user's
//! own crontab, requires `--apply`, backs up first, and preserves non-managed
//! lines byte-for-byte. System files are display-only (no sudo).

pub mod cron;
pub mod explain;
pub mod id;
pub mod next_run;
pub mod providers;
pub mod run_now;
pub mod scan;
pub mod snapshot;
pub mod types;

#[cfg(test)]
mod schema_tests;

pub use cron::write::{
    default_backup_dir, disable_task, edit_task, enable_task, new_cron_task, refuse_system_write,
};
pub use providers::systemd::timer_logs;
pub use run_now::{outcome_to_last_result, run_task};
pub use scan::{find_task, platform_name, scan};
pub use snapshot::{default_snapshot_path, diff_scans, load_snapshot, save_snapshot};
pub use types::{
    DiscoveredTask, LastResult, RunOutcome, ScanDiff, ScanResult, SourceFilter, SourceKind,
    TaskChange, TaskId, WritePlan,
};
