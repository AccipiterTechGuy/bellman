//! Finding work that is due, and the grace windows that decide what to do with it.
//!
//! The overdue scan run at boot / after a clock jump, the drain of the
//! near-horizon heap, and the two free helpers the misfire policies rely on:
//! the grace window for a policy and the walk over missed instants.

use super::Scheduler;
use crate::scheduler::action::FireAction;
use crate::scheduler::clock::Clock;
use crate::occurrence::OccurrenceKind;
use crate::store::{MisfirePolicy, Timer};
use super::types::{DeliveredFire, SchedulerError, SchedulerResult};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::cmp::Reverse;

impl<C: Clock, A: FireAction> Scheduler<C, A> {
    /// Scan enabled timers whose next fire is in the past and apply policy.
    pub(super) fn misfire_pass(&mut self) -> SchedulerResult<Vec<DeliveredFire>> {
        let now = self.clock.wall_now();
        let timers = self.store.list_timers()?;
        let mut out = Vec::new();
        for t in timers {
            if !t.enabled {
                continue;
            }
            let Some(next) = t.next_fire_utc else {
                continue;
            };
            if next > now {
                continue;
            }
            // Treat every overdue timer through the same due handler.
            let delivered = self.handle_due_timer(t.id, next, now)?;
            out.extend(delivered);
        }
        Ok(out)
    }

    pub(super) fn drain_due(&mut self, now: DateTime<Utc>) -> SchedulerResult<Vec<DeliveredFire>> {
        use crate::scheduler::jitter::apply_execution_jitter;

        let mut out = Vec::new();
        // Timers fully resolved this drain (next_fire > now or exhausted). Stale
        // duplicate heap entries for the same id are dropped on sight.
        let mut resolved = std::collections::HashSet::new();
        // Safety cap against pathological re-push bugs.
        for _ in 0..1_000 {
            // Pop first so the entry we inspect is the entry we own: a
            // not-yet-due head is pushed straight back (it is still the min, so
            // the heap is unchanged) and the drain ends.
            let Some(Reverse(entry)) = self.heap.pop() else {
                break;
            };
            // Heap `fire_at` is the *execution* instant (clean next_fire + jitter).
            // Accuracy slack is a late-coalesce tolerance (see FireKind::Late), not
            // an early-fire window — never deliver before the jittered exec time.
            if entry.fire_at > now {
                self.heap.push(Reverse(entry));
                break;
            }
            let timer_id = match entry.kind {
                super::types::HeapKind::Deadline { run_id, kind } => {
                    self.handle_due_deadline(run_id, kind, now)?;
                    continue;
                }
                super::types::HeapKind::Fire { timer_id } => timer_id,
            };
            if resolved.contains(&timer_id) {
                continue;
            }
            // Re-read timer; it may have been edited/disabled.
            let Some(timer) = self.store.get_timer(timer_id)? else {
                resolved.insert(timer_id);
                continue;
            };
            if !timer.enabled {
                resolved.insert(timer.id);
                continue;
            }
            let Some(nf) = timer.next_fire_utc else {
                resolved.insert(timer.id);
                continue;
            };
            let exec_at = apply_execution_jitter(timer.id, nf, timer.jitter_secs);
            if exec_at > now {
                // Positive jitter still in the future — requeue at exec time.
                self.heap.push(Reverse(super::types::HeapEntry::fire(exec_at, timer.id)));
                resolved.insert(timer.id);
                continue;
            }
            // For negative jitter, exec_at can be before nf. handle_due still
            // requires nf <= now to deliver; if only jitter is past, wait for nf.
            if nf > now {
                self.heap.push(Reverse(super::types::HeapEntry::fire(nf, timer.id)));
                resolved.insert(timer.id);
                continue;
            }
            let delivered = self.handle_due_timer(timer.id, nf, now)?;
            out.extend(delivered);
            // handle_due must leave the timer not-due (or exhausted). Mark
            // resolved so any further heap dups for this id are discarded.
            resolved.insert(timer.id);
        }
        Ok(out)
    }

    /// A lifecycle deadline heap entry woke (IK3). The wall time is only the
    /// wake hint: the MONOTONIC deadline book decides. Not yet due
    /// monotonically (a wall jump moved the hint early) ⇒ re-arm for the
    /// monotonic remainder. Disarmed meanwhile ⇒ no-op.
    fn handle_due_deadline(
        &mut self,
        run_id: uuid::Uuid,
        kind: crate::reply::DeadlineKind,
        now: DateTime<Utc>,
    ) -> SchedulerResult<()> {
        let Some(engine) = self.config.reply_engine() else {
            return Ok(());
        };
        let book_entry = engine
            .deadlines
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .get(&run_id)
            .copied();
        let Some(entry) = book_entry else {
            return Ok(()); // disarmed meanwhile
        };
        let mono_now = std::time::Instant::now();
        if entry.at > mono_now {
            // Wall jumped forward: re-arm for the monotonic remainder.
            let remaining = entry.at.saturating_duration_since(mono_now);
            let wall_at = now
                + chrono::Duration::from_std(remaining)
                    .unwrap_or_else(|_| chrono::Duration::seconds(1));
            self.heap
                .push(Reverse(super::types::HeapEntry::deadline(wall_at, run_id, kind)));
            return Ok(());
        }
        let Some(row) = self.store.get_run_state(run_id)? else {
            return Ok(());
        };
        let Some(timer) = self.store.get_timer(row.timer_id)? else {
            return Ok(());
        };
        // The gate serializes this transition against reply ingest and the
        // watcher's deadline pass.
        let _gate = crate::reply::gate::acquire(&engine.data_dir, timer.id)
            .map_err(|e| SchedulerError::Internal(format!("per-timer gate: {e}")))?;
        let transitioned = match kind {
            crate::reply::DeadlineKind::Pickup => {
                engine.expire_pickup_one(&self.store, &timer, run_id, now)
            }
            crate::reply::DeadlineKind::Watchdog => {
                engine.expire_watchdog_one(&self.store, &timer, run_id, now)
            }
        }
        .map_err(|e| SchedulerError::Internal(format!("deadline transition: {e}")))?;
        if transitioned {
            engine
                .project_status(&self.store, &timer, &run_id)
                .map_err(|e| SchedulerError::Internal(format!("deadline projection: {e}")))?;
        }
        Ok(())
    }
}

/// A user-supplied second count as a [`ChronoDuration`], saturating instead of
/// wrapping.
///
/// Interval periods and grace windows arrive as `u64` from the store. A bare
/// `as i64` turns a huge value **negative**, i.e. a window in the past, so every
/// fire would read as out-of-grace (and a period-derived grace would go the
/// wrong way entirely). `ChronoDuration` has its own ceiling below `i64::MAX`
/// seconds, so both limits clamp to [`ChronoDuration::MAX`]: an absurdly large
/// window means "always in grace", which is what such a value asks for.
pub(super) fn saturating_secs(secs: u64) -> ChronoDuration {
    i64::try_from(secs)
        .ok()
        .and_then(ChronoDuration::try_seconds)
        .unwrap_or(ChronoDuration::MAX)
}

/// Grace window for a timer under its misfire policy.
pub(super) fn grace_for(timer: &Timer) -> ChronoDuration {
    match &timer.misfire {
        MisfirePolicy::Skip => match timer.occurrence.kind() {
            OccurrenceKind::Interval { every_secs, .. } => saturating_secs(*every_secs),
            // Calendar kinds listed explicitly: a new occurrence kind must be a
            // compile error here, not a silent zero-grace fall-through.
            OccurrenceKind::Once { .. }
            | OccurrenceKind::Daily { .. }
            | OccurrenceKind::Weekly { .. }
            | OccurrenceKind::Monthly { .. }
            | OccurrenceKind::Yearly { .. }
            | OccurrenceKind::Cron { .. } => ChronoDuration::zero(),
        },
        MisfirePolicy::Coalesce { grace_secs }
        | MisfirePolicy::CatchUp { grace_secs, .. } => saturating_secs(*grace_secs),
    }
}

/// Missed scheduled instants from `first_due` through `now` (inclusive), in order.
pub(super) fn walk_missed(
    timer: &Timer,
    first_due: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let mut out = Vec::new();
    if first_due > now {
        return out;
    }
    out.push(first_due);
    let mut occ = timer.occurrence.clone();
    let tz = occ.timezone();
    let mut cursor = first_due.with_timezone(&tz);
    for _ in 0..10_000 {
        match occ.next_fire(cursor) {
            Some(c) => {
                let c_utc = c.with_timezone(&Utc);
                if c_utc > now {
                    break;
                }
                out.push(c_utc);
                cursor = c;
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! Saturation regression tests for the user-supplied second counts that
    //! feed the grace window. Before the cast audit these were bare `as i64`
    //! casts: a value near `u64::MAX` wrapped **negative**, turning the grace
    //! window into a duration in the past so every overdue fire read as
    //! out-of-grace and was silently dropped.

    use super::{grace_for, saturating_secs, Scheduler};
    use crate::occurrence::{Occurrence, OccurrenceKind};
    use crate::scheduler::action::RecordingAction;
    use crate::scheduler::clock::SimulatedClock;
    use crate::scheduler::config::SchedulerConfig;
    use crate::store::{
        Action, MisfirePolicy, NewTimer, OverlapPolicy, RetryPolicy, Store, Timer,
    };
    use chrono::{Duration as ChronoDuration, NaiveTime, TimeZone, Utc};

    fn timer_with(kind: OccurrenceKind, misfire: MisfirePolicy) -> Timer {
        let occurrence = Occurrence::new(kind, "UTC").expect("occurrence");
        Timer {
            id: uuid::Uuid::nil(),
            name: "saturation".into(),
            enabled: true,
            tz: occurrence.tz_name().to_string(),
            occurrence,
            next_fire_utc: None,
            last_fired: None,
            misfire,
            overlap: OverlapPolicy::default(),
            retry: RetryPolicy::default(),
            valid_from: None,
            valid_until: None,
            max_runs: None,
            tags: Vec::new(),
            action: Action::default(),
            revision: 1,
            jitter_secs: 0,
            accuracy_slack_secs: None,
            wake_machine: false,
        }
    }

    #[test]
    fn saturating_secs_is_monotonic_and_never_negative() {
        let huge = u64::try_from(i64::MAX).expect("i64::MAX fits u64");
        let mut prev = ChronoDuration::zero();
        for secs in [0u64, 1, 3600, 86_400, huge - 1, huge, u64::MAX] {
            let d = saturating_secs(secs);
            assert!(d >= ChronoDuration::zero(), "{secs}s produced {d} (negative)");
            assert!(d >= prev, "{secs}s produced {d}, below the previous step {prev}");
            prev = d;
        }
        assert_eq!(saturating_secs(u64::MAX), ChronoDuration::MAX);
    }

    #[test]
    fn near_max_interval_period_gives_a_future_skip_grace() {
        for every_secs in [u64::MAX, u64::MAX - 1, 1 << 63] {
            let t = timer_with(
                OccurrenceKind::Interval {
                    every_secs,
                    anchor: Utc.with_ymd_and_hms(2030, 6, 1, 12, 0, 0).unwrap(),
                },
                MisfirePolicy::Skip,
            );
            let grace = grace_for(&t);
            assert!(
                grace > ChronoDuration::zero(),
                "every_secs={every_secs} wrapped to a past grace window: {grace}"
            );
        }
    }

    #[test]
    fn near_max_grace_secs_gives_a_future_window_for_both_policies() {
        for grace_secs in [u64::MAX, u64::MAX - 1, 1 << 63] {
            for misfire in [
                MisfirePolicy::Coalesce { grace_secs },
                MisfirePolicy::CatchUp {
                    grace_secs,
                    max_catch_up: 5,
                },
            ] {
                let t = timer_with(
                    OccurrenceKind::Daily {
                        at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                    },
                    misfire.clone(),
                );
                let grace = grace_for(&t);
                assert!(
                    grace > ChronoDuration::zero(),
                    "grace_secs={grace_secs} under {misfire:?} wrapped to a past window: {grace}"
                );
            }
        }
    }

    /// End-to-end proof: a `u64::MAX` coalesce grace must still deliver the
    /// overdue slot. With the old wrapping cast the window was −1 s, nothing was
    /// ever in grace, and the backlog was skipped without firing.
    #[test]
    fn near_max_coalesce_grace_still_delivers_the_overdue_fire() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(dir.path().join("timers.db")).expect("open");

        let at = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let thursday_morning = Utc.with_ymd_and_hms(2030, 6, 6, 8, 0, 0).unwrap();
        let clock = SimulatedClock::new(thursday_morning);

        // last_fired = Wednesday 09:00 ⇒ next = Thursday 09:00.
        let occ = Occurrence::new(OccurrenceKind::Daily { at }, "UTC").unwrap();
        let mut new = NewTimer::new("huge-grace", occ);
        new.last_fired = Some(Utc.with_ymd_and_hms(2030, 6, 5, 9, 0, 0).unwrap());
        new.misfire = MisfirePolicy::Coalesce {
            grace_secs: u64::MAX,
        };
        let timer = store.create_timer(new).expect("create");

        let mut sched = Scheduler::new(
            store,
            clock.clone(),
            RecordingAction::new(),
            SchedulerConfig::default(),
        );
        sched.boot().unwrap();

        // Suspended over the weekend; wake at Monday 10:00.
        let monday_nine = Utc.with_ymd_and_hms(2030, 6, 10, 9, 0, 0).unwrap();
        clock.set_wall(Utc.with_ymd_and_hms(2030, 6, 10, 10, 0, 0).unwrap());
        let r = sched.tick().unwrap();

        assert_eq!(
            r.fires.len(),
            1,
            "a u64::MAX grace must coalesce the backlog into one fire, got {}",
            r.fires.len()
        );
        assert_eq!(r.fires[0].timer_id, timer.id);
        assert_eq!(
            r.fires[0].scheduled_for, monday_nine,
            "must fire the latest in-grace slot, never a past/negative instant"
        );
    }
}
