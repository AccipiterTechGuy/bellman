//! Jan-1 year consistency pass.
//!
//! Lazy `next_fire()` means there is no materialized year grid to rebuild.
//! This pass re-verifies every timer's denormalized `next_fire_utc`, rewrites
//! rows that drifted, emits a single `year_recalibrate` event, and stamps
//! `meta.last_recalibration`. Idempotent: a second call in the same year is
//! a no-op when `last_recalibration >= year_start(now)`.

use super::{PruneError, PruneResult};
use crate::events::{EventRecord, RunState};
use crate::store::{Store, TimerPatch, TimerUpdate};
use chrono::{DateTime, Datelike, TimeZone, Utc};

/// Report from one recalibration pass.
#[derive(Debug, Clone, Default)]
pub struct YearRecalibrateReport {
    /// How many timers the pass looked at.
    pub timers_checked: usize,
    /// How many needed correcting. Next-fire is computed lazily, so this is
    /// a consistency check rather than a yearly grid rebuild — normally 0.
    pub timers_updated: usize,
    /// True when the pass was skipped because already done this year.
    pub skipped_idempotent: bool,
    /// The year the pass ran for.
    pub year: i32,
}

/// UTC midnight of January 1 of `now`'s calendar year.
pub fn year_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), 1, 1, 0, 0, 0)
        .single()
        .expect("Jan 1 00:00:00 UTC is a valid civil time")
}

/// True when `last_recalibration` is missing or before this year's start.
pub fn needs_year_recalibration(store: &Store, now: DateTime<Utc>) -> PruneResult<bool> {
    let meta = store.meta()?;
    let ys = year_start(now);
    Ok(match meta.last_recalibration {
        None => true,
        Some(lr) => lr < ys,
    })
}

/// Recompute + verify all timers; enqueue `year_recalibrate`; stamp meta.
///
/// When not needed (already recalibrated this year), returns
/// `skipped_idempotent = true` without writing.
pub fn run_year_recalibration(
    store: &mut Store,
    enqueue: bool,
    now: DateTime<Utc>,
) -> PruneResult<YearRecalibrateReport> {
    let ys = year_start(now);
    let year = now.year();
    if !needs_year_recalibration(store, now)? {
        return Ok(YearRecalibrateReport {
            skipped_idempotent: true,
            year,
            ..YearRecalibrateReport::default()
        });
    }

    let timers = store.list_timers()?;
    let mut updated = 0usize;
    let checked = timers.len();

    for timer in timers {
        // Touch via empty patch so the store recomputes next_fire_utc from
        // last_fired + occurrence in the same transaction (existing path).
        let before = timer.next_fire_utc;
        let after = store.update_timer(TimerUpdate {
            id: timer.id,
            expected_revision: timer.revision,
            patch: TimerPatch::default(),
        })?;
        if after.next_fire_utc != before {
            updated = updated.saturating_add(1);
        }
    }

    store.set_last_recalibration(now)?;

    if enqueue {
        let rec = EventRecord::new(RunState::YearRecalibrate)
            .with_logged_at(now)
            .with_message(format!("year={year}"))
            .with_count(u32::try_from(checked).unwrap_or(u32::MAX))
            .with_detail(serde_json::json!({
                "year": year,
                "year_start": ys.to_rfc3339(),
                "timers_checked": checked,
                "timers_updated": updated,
            }));
        store
            .enqueue_event(&rec)
            .map_err(|e| PruneError::EventLog(e.to_string()))?;
    }

    Ok(YearRecalibrateReport {
        timers_checked: checked,
        timers_updated: updated,
        skipped_idempotent: false,
        year,
    })
}
