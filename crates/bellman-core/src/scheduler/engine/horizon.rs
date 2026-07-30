//! Heap and horizon maintenance.
//!
//! Rebuilding the near-horizon heap from the store, walking a timer's backlog
//! past wall-now without firing, and the guarded pushes that keep the heap to
//! the configured horizon (plus always-resident high-frequency intervals).

use super::types::{HeapEntry, SchedulerError, SchedulerResult};
use super::Scheduler;
use crate::scheduler::action::FireAction;
use crate::scheduler::clock::Clock;
use crate::scheduler::jitter::{apply_execution_jitter, is_high_frequency};
use crate::store::{TimerId, TimerPatch, TimerUpdate};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::cmp::Reverse;

impl<C: Clock, A: FireAction> Scheduler<C, A> {
    /// Explicit horizon rebuild (also triggered by control Refill).
    pub fn rebuild_horizon(&mut self) -> SchedulerResult<()> {
        self.heap.clear();
        let now = self.clock.wall_now();
        let horizon = now
            + ChronoDuration::from_std(self.config.horizon).map_err(|e| {
                SchedulerError::Internal(format!("horizon duration: {e}"))
            })?;

        let due = self.store.timers_due_by(horizon)?;
        let mut seen = std::collections::HashSet::new();
        for t in due {
            if let Some(nf) = t.next_fire_utc {
                // Heap uses execution time (with jitter); display stays clean.
                let exec_at = apply_execution_jitter(t.id, nf, t.jitter_secs);
                self.heap.push(Reverse(HeapEntry::fire(exec_at, t.id)));
                seen.insert(t.id);
            }
        }

        // Always-resident high-frequency interval timers.
        for t in self.store.list_timers()? {
            if !t.enabled || seen.contains(&t.id) {
                continue;
            }
            if !is_high_frequency(&t) {
                continue;
            }
            if let Some(nf) = t.next_fire_utc {
                let exec_at = apply_execution_jitter(t.id, nf, t.jitter_secs);
                self.heap.push(Reverse(HeapEntry::fire(exec_at, t.id)));
            }
        }

        // Re-arm lifecycle deadline entries (IK3): the rebuild clears the
        // heap, so pickup/watchdog deadlines would otherwise be lost. The
        // wall estimate converts the monotonic remainder; on wake the
        // monotonic book is re-checked anyway.
        if let Some(engine) = self.config.reply_engine() {
            let book: Vec<(uuid::Uuid, _)> = engine
                .deadlines
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entries
                .iter()
                .map(|(id, d)| (*id, *d))
                .collect();
            let mono_now = std::time::Instant::now();
            for (run_id, deadline) in book {
                let remaining = deadline.at.saturating_duration_since(mono_now);
                let wall_at = now
                    + ChronoDuration::from_std(remaining)
                        .unwrap_or_else(|_| ChronoDuration::zero());
                self.heap
                    .push(Reverse(HeapEntry::deadline(wall_at, run_id, deadline.kind)));
            }
        }
        Ok(())
    }

    /// Advance the timer so `next_fire_utc` is strictly after `now` without
    /// recording a delivered run (skip path).
    pub(super) fn advance_past_now(
        &mut self,
        timer_id: TimerId,
        now: DateTime<Utc>,
    ) -> SchedulerResult<()> {
        let timer = self
            .store
            .get_timer(timer_id)?
            .ok_or_else(|| SchedulerError::Internal(format!("timer {timer_id} missing")))?;

        // Walk the occurrence from the current next (or last_fired) until the
        // candidate is strictly after `now`. Persist by setting last_fired to
        // the last skipped instant so store recompute lands on the future slot.
        let mut occ = timer.occurrence.clone();
        let tz = occ.timezone();
        let mut cursor = timer
            .last_fired
            .unwrap_or_else(|| {
                timer
                    .next_fire_utc
                    .map_or(now, |nf| nf - ChronoDuration::nanoseconds(1))
            })
            .with_timezone(&tz);

        let mut last_skipped: Option<DateTime<Utc>> = timer.last_fired;
        // Safety cap for pathological schedules.
        for _ in 0..100_000 {
            match occ.next_fire(cursor) {
                Some(candidate) => {
                    let c_utc = candidate.with_timezone(&Utc);
                    if c_utc > now {
                        break;
                    }
                    last_skipped = Some(c_utc);
                    cursor = candidate;
                }
                None => break,
            }
        }

        // Anchor last_fired at the last skipped (or `now` if nothing walked) so
        // the store's next_fire lands in the future. No record_run — skip is not
        // a delivery.
        let anchor = last_skipped.unwrap_or(now);
        self.store.update_timer(TimerUpdate {
            id: timer_id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                last_fired: Some(Some(anchor)),
                // Keep occurrence (may have consumed pending_skips during walk).
                occurrence: Some(occ),
                ..Default::default()
            },
        })?;
        self.requeue_timer(timer_id)?;
        Ok(())
    }

    pub(super) fn ensure_advanced_past(
        &mut self,
        timer_id: TimerId,
        scheduled_for: DateTime<Utc>,
    ) -> SchedulerResult<()> {
        let Some(timer) = self.store.get_timer(timer_id)? else {
            return Ok(());
        };
        match timer.next_fire_utc {
            Some(nf) if nf > scheduled_for => {
                // May still be ≤ now (backlog). Let the caller drain again, but
                // never re-push the already-claimed instant.
                self.push_if_in_horizon(nf, timer_id);
                Ok(())
            }
            Some(_) | None => {
                // Still pointing at the claimed slot (or exhausted). Bump last_fired
                // and, if still overdue, jump the backlog past wall-now.
                let occ = timer.occurrence.clone();
                // Do not record_run — the original claim owns the delivery.
                self.store.update_timer(TimerUpdate {
                    id: timer_id,
                    expected_revision: timer.revision,
                    patch: TimerPatch {
                        last_fired: Some(Some(scheduled_for)),
                        occurrence: Some(occ),
                        ..Default::default()
                    },
                })?;
                let now = self.clock.wall_now();
                let Some(t2) = self.store.get_timer(timer_id)? else {
                    return Ok(());
                };
                if t2.next_fire_utc.is_some_and(|nf| nf <= now) {
                    self.advance_past_now(timer_id, now)?;
                } else {
                    self.requeue_timer(timer_id)?;
                }
                Ok(())
            }
        }
    }

    pub(super) fn requeue_timer(&mut self, timer_id: TimerId) -> SchedulerResult<()> {
        let Some(timer) = self.store.get_timer(timer_id)? else {
            return Ok(());
        };
        if timer.enabled {
            if let Some(nf) = timer.next_fire_utc {
                self.push_if_in_horizon(nf, timer_id);
            }
        }
        Ok(())
    }

    pub(super) fn push_if_in_horizon(&mut self, fire_at: DateTime<Utc>, timer_id: TimerId) {
        let now = self.clock.wall_now();
        let timer = self.store.get_timer(timer_id).ok().flatten();
        let jitter = timer.as_ref().map_or(0, |t| t.jitter_secs);
        let exec_at = apply_execution_jitter(timer_id, fire_at, jitter);
        let horizon_ok = ChronoDuration::from_std(self.config.horizon)
            .ok()
            .is_none_or(|h| exec_at <= now + h);
        let force = timer.as_ref().is_some_and(is_high_frequency);
        if horizon_ok || force {
            self.heap.push(Reverse(HeapEntry::fire(exec_at, timer_id)));
        }
    }

    /// Arm a lifecycle deadline heap entry (IK3). Lifecycle deadlines are
    /// not horizon-limited: the scheduler must wake for them regardless.
    pub(super) fn push_deadline(
        &mut self,
        run_id: uuid::Uuid,
        kind: crate::reply::DeadlineKind,
        wall_at: DateTime<Utc>,
    ) {
        self.heap
            .push(Reverse(HeapEntry::deadline(wall_at, run_id, kind)));
    }
}
