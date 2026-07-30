//! Weekly `system.prune` + Jan-1 year consistency pass.
//!
//! ## Weekly prune
//! A persisted internal timer named [`SYSTEM_PRUNE_NAME`] (visible in the GUI)
//! schedules the job; startup also catch-up runs when
//! `now > last_prune + prune_interval`. Each run:
//! 1. Rotates/retains the JSONL event log (archive rename + delete past retention).
//! 2. Deletes elapsed **terminal** one-shots only (fired/skipped, past validity
//!    and ack grace). Failed/pending one-shots are preserved.
//! 3. Writes a `pruned` tombstone event per deleted timer.
//!
//! ## Jan-1 consistency
//! If `last_recalibration < year_start(now)`, recompute + verify every timer's
//! `next_fire_utc`, refresh denormalized columns, emit `year_recalibrate`, and
//! stamp `last_recalibration`. Idempotent within the same year.

mod year;

pub use year::{
    needs_year_recalibration, run_year_recalibration, year_start, YearRecalibrateReport,
};

use crate::events::{EventPublisher, RunState, EventRecord};
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::store::{
    Action, MisfirePolicy, NewTimer, OverlapPolicy, RetryPolicy, Store, StoreError, StoreResult,
    Timer, TimerId,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

/// Display name of the dogfood weekly prune timer (visible in the GUI).
pub const SYSTEM_PRUNE_NAME: &str = "system.prune";
/// Tag applied to the system prune timer.
pub const SYSTEM_PRUNE_TAG: &str = "system";
/// Stable id so restarts re-find the same row (not a second copy).
pub const SYSTEM_PRUNE_ID: &str = "00000000-0000-4000-8000-000000000001";

/// Default prune cadence (7 days).
pub const DEFAULT_PRUNE_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Default ack grace (product: 60 s).
pub const DEFAULT_ACK_GRACE: Duration = Duration::from_secs(60);

/// Tunables for a prune pass.
#[derive(Debug, Clone)]
pub struct PruneConfig {
    /// JSONL archive retention window.
    pub retention: Duration,
    /// Cadence used for startup catch-up (`now > last_prune + interval`).
    pub interval: Duration,
    /// Grace after a terminal fire/skip before the one-shot may be deleted.
    pub ack_grace: Duration,
    /// Rotate the live log before an append would cross this size.
    pub max_current_bytes: u64,
    /// Retained-log budget: current + final archives stay within this.
    pub budget_bytes: u64,
    /// Aggregate ceiling for the reply quarantine (`timers/bad/`).
    pub quarantine_budget_bytes: u64,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            retention: Duration::from_secs(30 * 24 * 60 * 60),
            interval: DEFAULT_PRUNE_INTERVAL,
            ack_grace: DEFAULT_ACK_GRACE,
            max_current_bytes: crate::events::DEFAULT_MAX_CURRENT_BYTES,
            budget_bytes: crate::events::DEFAULT_BUDGET_BYTES,
            quarantine_budget_bytes: crate::app_config::DEFAULT_QUARANTINE_BUDGET_BYTES,
        }
    }
}

/// Outcome of one prune pass.
#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    /// Archive path created by rotation (if current was non-empty).
    pub archived: Option<std::path::PathBuf>,
    /// Number of archive files removed by retention (age + budget).
    pub archives_removed: usize,
    /// One-shot timers deleted.
    pub timers_pruned: usize,
    /// Ids of deleted timers (order of deletion).
    pub pruned_timer_ids: Vec<TimerId>,
    /// Orphan timer folders removed by the tree sweep.
    pub orphan_folders_removed: usize,
    /// True when this pass was a no-op because prune was not yet due.
    pub skipped_not_due: bool,
    /// True when the publisher lease was held elsewhere, so rotation and
    /// retention did NOT happen and `last_prune` was deliberately NOT
    /// stamped — the next pass must retry.
    pub skipped_not_leader: bool,
}

/// Errors from prune / ensure-timer helpers.
#[derive(Debug)]
pub enum PruneError {
    Store(StoreError),
    EventLog(String),
    Internal(String),
}

impl std::fmt::Display for PruneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "prune store: {e}"),
            Self::EventLog(s) => write!(f, "prune event log: {s}"),
            Self::Internal(s) => write!(f, "prune: {s}"),
        }
    }
}

impl std::error::Error for PruneError {}

impl From<StoreError> for PruneError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

pub type PruneResult<T> = Result<T, PruneError>;

/// Parse the stable system.prune timer id.
pub fn system_prune_id() -> TimerId {
    Uuid::parse_str(SYSTEM_PRUNE_ID).expect("static system.prune uuid")
}

/// True when this timer is the internal weekly prune job.
pub fn is_system_prune_timer(timer: &Timer) -> bool {
    timer.id == system_prune_id()
        || (timer.name == SYSTEM_PRUNE_NAME && timer.tags.iter().any(|t| t == SYSTEM_PRUNE_TAG))
}

/// Ensure the `system.prune` weekly timer exists (create if missing).
///
/// Visible in the GUI like any other timer. Uses a weekly wall-clock schedule
/// (Monday 03:17 local) so it dogfoods the calendar path; startup catch-up
/// still keys off `meta.last_prune`.
pub fn ensure_system_prune_timer(store: &mut Store) -> PruneResult<Timer> {
    let id = system_prune_id();
    if let Some(existing) = store.get_timer(id)? {
        return Ok(existing);
    }
    // Also re-find by name if an older install created one without the fixed id.
    for t in store.list_timers()? {
        if t.name == SYSTEM_PRUNE_NAME && t.tags.iter().any(|tag| tag == SYSTEM_PRUNE_TAG) {
            return Ok(t);
        }
    }

    let tz = iana_system_tz();
    // Weekly Monday 03:17 — low-traffic hour, calendar dogfood.
    let occ = Occurrence::new(
        OccurrenceKind::Weekly {
            days: crate::occurrence::Weekdays::from_slice(&[chrono::Weekday::Mon]),
            at: chrono::NaiveTime::from_hms_opt(3, 17, 0).expect("valid time"),
        },
        tz,
    )
    .map_err(PruneError::Internal)?;

    let timer = store.create_timer(NewTimer {
        id: Some(id),
        name: SYSTEM_PRUNE_NAME.into(),
        enabled: true,
        occurrence: occ,
        misfire: MisfirePolicy::Coalesce { grace_secs: 3600 },
        overlap: OverlapPolicy::Skip,
        retry: RetryPolicy {
            max_retries: 0,
            delay_secs: 0,
        },
        tags: vec![SYSTEM_PRUNE_TAG.into(), "internal".into()],
        action: Action::None,
        last_fired: None,
        jitter_secs: 0,
        accuracy_slack_secs: None,
        wake_machine: false,
    })?;
    Ok(timer)
}

fn iana_system_tz() -> String {
    // Prefer the local offset's common IANA guess; fall back to UTC.
    // chrono's Local doesn't always expose the IANA name; use Etc/GMT when unknown.
    std::env::var("TZ").unwrap_or_else(|_| {
        // Best-effort: read /etc/localtime symlink target on Linux.
        if let Ok(link) = std::fs::read_link("/etc/localtime") {
            let s = link.to_string_lossy();
            if let Some(idx) = s.find("zoneinfo/") {
                let name = &s[idx + "zoneinfo/".len()..];
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
        "UTC".into()
    })
}

/// True when a prune pass should run now (startup catch-up or overdue).
pub fn prune_is_due(last_prune: Option<DateTime<Utc>>, now: DateTime<Utc>, interval: Duration) -> bool {
    match last_prune {
        None => true,
        Some(lp) => {
            let iv = ChronoDuration::from_std(interval).unwrap_or_else(|_| ChronoDuration::days(7));
            now > lp + iv
        }
    }
}

/// Run the full prune pass: JSONL rotate+retain, terminal one-shot deletion,
/// tombstones, timer-folder orphan sweep + reconciliation, stamp `last_prune`.
///
/// `tree` (when given) is the per-timer folder view root: pruned one-shots
/// lose their folder, orphans (folders with no database timer — a crash
/// between the database delete and the folder delete) are swept, and every
/// live timer's folder/`timer.json` is reconciled. Every removal is logged
/// as a `pruned` event — never silent.
///
/// When `force` is false, returns early with `skipped_not_due` if not due.
pub fn run_prune(
    store: &mut Store,
    publisher: &mut EventPublisher,
    config: &PruneConfig,
    now: DateTime<Utc>,
    force: bool,
    tree: Option<&crate::tree::TimersTree>,
) -> PruneResult<PruneReport> {
    let meta = store.meta()?;
    if !force && !prune_is_due(meta.last_prune, now, config.interval) {
        return Ok(PruneReport {
            skipped_not_due: true,
            ..PruneReport::default()
        });
    }

    // Rotation goes through the elected publisher (R11) — never a side
    // path. When the lease is held elsewhere (the live watcher leads this
    // second), NOTHING was rotated: report it honestly and do NOT stamp
    // last_prune — the next pass must retry.
    let (leader, archived, retain) = publisher
        .rotate_and_retain(store)
        .map_err(|e| PruneError::EventLog(e.to_string()))?;
    if !leader {
        return Ok(PruneReport {
            skipped_not_leader: true,
            ..PruneReport::default()
        });
    }
    // Log every archive prune — never silent.
    if retain.removed_count() > 0 {
        let note = EventRecord::new(RunState::Pruned)
            .with_logged_at(now)
            .with_message("log_retention")
            .with_detail(serde_json::json!({
                "reason": "log_retention",
                "aged_out": retain.aged.len(),
                "budget_pruned": retain.budget.len(),
                "bytes_removed": retain.bytes_removed,
            }));
        store
            .enqueue_event(&note)
            .map_err(|e| PruneError::EventLog(e.to_string()))?;
    }

    let mut report = PruneReport {
        archived,
        archives_removed: retain.removed_count(),
        timers_pruned: 0,
        pruned_timer_ids: Vec::new(),
        orphan_folders_removed: 0,
        skipped_not_due: false,
        skipped_not_leader: false,
    };

    let candidates = store.list_timers()?;
    for timer in candidates {
        if is_system_prune_timer(&timer) {
            continue;
        }
        if !is_terminal_oneshot(&timer, store, now, config.ack_grace)? {
            continue;
        }
        let id = timer.id;
        let name = timer.name.clone();
        if store.delete_timer(id)? {
            let _ = store.clear_timer_owner(id);
            report.timers_pruned = report.timers_pruned.saturating_add(1);
            report.pruned_timer_ids.push(id);
            // Terminal one-shots have no open claim, so no `cancelled` event
            // is owed here; just drop the folder view.
            if let Some(tree) = tree {
                if let Err(e) = tree.remove_for(id) {
                    eprintln!("bellman: timer folder remove failed for {id}: {e}");
                }
            }
            let tomb = EventRecord::new(RunState::Pruned)
                .with_logged_at(now)
                .with_timer(id, name)
                .with_message("elapsed_oneshot")
                .with_detail(serde_json::json!({
                    "reason": "terminal_oneshot",
                    "ack_grace_secs": config.ack_grace.as_secs(),
                }));
            store
                .enqueue_event(&tomb)
                .map_err(|e| PruneError::EventLog(e.to_string()))?;
        }
    }

    // Folder tree maintenance: sweep orphans, then reconcile the view.
    if let Some(tree) = tree {
        let live_ids: std::collections::HashSet<TimerId> = store
            .list_timers()?
            .iter()
            .map(|t| t.id)
            .collect();
        match tree.sweep_orphans(&live_ids) {
            Ok(removed) => {
                report.orphan_folders_removed = removed.len();
                for folder in removed {
                    let note = EventRecord::new(RunState::Pruned)
                        .with_logged_at(now)
                        .with_message("orphan_timer_folder")
                        .with_detail(serde_json::json!({
                            "reason": "orphan_folder",
                            "folder": folder.display().to_string(),
                        }));
                    store
                        .enqueue_event(&note)
                        .map_err(|e| PruneError::EventLog(e.to_string()))?;
                }
            }
            Err(e) => eprintln!("bellman: timer folder orphan sweep failed: {e}"),
        }
        if let Err(e) = crate::tree::reconcile_folders(tree, store) {
            eprintln!("bellman: timer folder reconcile failed: {e}");
        }
        // Reply quarantine retention: 30-day window + aggregate ceiling,
        // oldest payload/sidecar pairs first. Shares the quarantine lock
        // with artifact creation (the pruner holds no timer shard here).
        let bad = crate::reply::quarantine::quarantine_dir(tree.root());
        if let Ok(_lock) = crate::reply::gate::acquire_quarantine(
            tree.root().parent().unwrap_or(tree.root()),
        ) {
            match crate::reply::quarantine::prune(
                &bad,
                config.retention,
                config.quarantine_budget_bytes,
                now,
            ) {
                Ok(0) => {}
                Ok(n) => {
                    let note = EventRecord::new(RunState::Pruned)
                        .with_logged_at(now)
                        .with_message("reply_quarantine_retention")
                        .with_detail(serde_json::json!({
                            "reason": "quarantine_retention",
                            "artifacts_removed": n,
                        }));
                    if let Err(e) = store.enqueue_event(&note) {
                        eprintln!("bellman: quarantine prune log failed: {e}");
                    }
                }
                Err(e) => eprintln!("bellman: reply quarantine prune failed: {e}"),
            }
        }
    }

    store.set_last_prune(now)?;
    Ok(report)
}

/// Publisher config for prune/startup: ALL retention and rotation knobs
/// come from the [`PruneConfig`] (itself built from `config.json`), so no
/// production writer silently falls back to the 64 MiB / 1 GiB defaults.
fn publisher_config(data_dir: &Path, config: &PruneConfig) -> crate::events::EventLogConfig {
    crate::events::EventLogConfig::new(data_dir.join("logs"))
        .with_retention(config.retention)
        .with_max_current_bytes(config.max_current_bytes)
        .with_budget_bytes(config.budget_bytes)
}

/// Open the publisher under `data_dir` with the given config and run prune.
pub fn run_prune_under(
    store: &mut Store,
    data_dir: &Path,
    config: &PruneConfig,
    now: DateTime<Utc>,
    force: bool,
) -> PruneResult<PruneReport> {
    let mut publisher = EventPublisher::with_config(publisher_config(data_dir, config))
        .map_err(|e| PruneError::EventLog(e.to_string()))?;
    let tree = crate::tree::TimersTree::new(data_dir);
    run_prune(store, &mut publisher, config, now, force, Some(&tree))
}

/// Terminal-state one-shot eligible for deletion?
///
/// Rules (product):
/// - Kind is `Once`
/// - Terminal: either exhausted (`next_fire_utc` is None and `last_fired` is
///   Some — fired), or disabled with no future fire after a skip, or past
///   `valid_until`
/// - Past validity end (if set) **and** past ack grace from the terminal event
/// - NOT pending (still has next_fire in the future)
/// - NOT failed mid-flight: a pending `claimed` run blocks prune
pub fn is_terminal_oneshot(
    timer: &Timer,
    store: &Store,
    now: DateTime<Utc>,
    ack_grace: Duration,
) -> StoreResult<bool> {
    if !matches!(timer.occurrence.kind(), OccurrenceKind::Once { .. }) {
        return Ok(false);
    }

    // In-flight claim → not terminal.
    let runs = store.runs_for_timer(timer.id)?;
    if runs.iter().any(|r| r.status == crate::store::ClaimStatus::Claimed) {
        return Ok(false);
    }

    // Still scheduled in the future → preserve.
    if timer.next_fire_utc.is_some_and(|nf| nf > now) {
        return Ok(false);
    }

    // Terminal shapes:
    // 1) Fired: last_fired is set and no next fire (exhausted once).
    // 2) Skipped / validity ended: next_fire is None (or past), and either
    //    disabled, past valid_until, or last_fired set with max_runs done.
    let fired = timer.last_fired.is_some() && timer.next_fire_utc.is_none();
    let past_validity = timer
        .valid_until
        .is_some_and(|until| now >= until)
        || timer
            .occurrence
            .valid_until()
            .is_some_and(|until| now >= until);
    let skipped_exhausted = timer.next_fire_utc.is_none() && (past_validity || !timer.enabled);

    if !fired && !skipped_exhausted && !past_validity {
        // Overdue once still sitting with next_fire in the past and never
        // claimed — treat as pending (not terminal). Preserve.
        if timer.next_fire_utc.is_some() && timer.last_fired.is_none() {
            return Ok(false);
        }
        if !fired {
            return Ok(false);
        }
    }

    // Ack grace: wait after the terminal moment.
    let terminal_at = timer
        .last_fired
        .or(timer.valid_until)
        .or(timer.occurrence.valid_until())
        .unwrap_or(now);
    let grace = ChronoDuration::from_std(ack_grace).unwrap_or_else(|_| ChronoDuration::seconds(60));
    if now < terminal_at + grace {
        return Ok(false);
    }

    Ok(true)
}

/// Startup maintenance: ensure system.prune timer, catch-up prune if due,
/// Jan-1 recalibration if needed. Returns human-readable summary lines.
pub fn startup_maintenance(
    store: &mut Store,
    data_dir: &Path,
    prune_config: &PruneConfig,
    now: DateTime<Utc>,
) -> PruneResult<Vec<String>> {
    let mut notes = Vec::new();
    let t = ensure_system_prune_timer(store)?;
    notes.push(format!(
        "system.prune ready id={} next={:?}",
        t.id, t.next_fire_utc
    ));

    let meta = store.meta()?;
    if prune_is_due(meta.last_prune, now, prune_config.interval) {
        let report = run_prune_under(store, data_dir, prune_config, now, true)?;
        notes.push(format!(
            "prune catch-up: timers={} archives_removed={} archived={:?}",
            report.timers_pruned, report.archives_removed, report.archived
        ));
    }

    if needs_year_recalibration(store, now)? {
        let yr = run_year_recalibration(store, true, now)?;
        notes.push(format!(
            "year_recalibrate: checked={} updated={}",
            yr.timers_checked, yr.timers_updated
        ));
    }

    Ok(notes)
}

#[cfg(test)]
mod tests;
