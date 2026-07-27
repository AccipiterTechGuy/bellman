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
use super::types::{DeliveredFire, SchedulerResult};
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
            if entry.fire_at > now {
                self.heap.push(Reverse(entry));
                break;
            }
            if resolved.contains(&entry.timer_id) {
                continue;
            }
            // Re-read timer; it may have been edited/disabled.
            let Some(timer) = self.store.get_timer(entry.timer_id)? else {
                resolved.insert(entry.timer_id);
                continue;
            };
            if !timer.enabled {
                resolved.insert(entry.timer_id);
                continue;
            }
            let Some(nf) = timer.next_fire_utc else {
                resolved.insert(entry.timer_id);
                continue;
            };
            if nf > now {
                // Stale heap slot — requeue the authoritative future next once.
                self.push_if_in_horizon(nf, timer.id);
                resolved.insert(timer.id);
                continue;
            }
            // Authoritative next is due (ignore heap fire_at; it may be stale).
            let delivered = self.handle_due_timer(timer.id, nf, now)?;
            out.extend(delivered);
            // handle_due must leave the timer not-due (or exhausted). Mark
            // resolved so any further heap dups for this id are discarded.
            resolved.insert(timer.id);
            // If policy only advanced one step and another fire is still due
            // within grace, process it now before leaving the id resolved.
            // (Catch-up / multi-period within grace is handled inside handle_due.)
        }
        Ok(out)
    }
}

/// Grace window for a timer under its misfire policy.
pub(super) fn grace_for(timer: &Timer) -> ChronoDuration {
    match &timer.misfire {
        MisfirePolicy::Skip => match timer.occurrence.kind() {
            OccurrenceKind::Interval { every_secs, .. } => {
                ChronoDuration::seconds(*every_secs as i64)
            }
            _ => ChronoDuration::zero(),
        },
        MisfirePolicy::Coalesce { grace_secs }
        | MisfirePolicy::CatchUp { grace_secs, .. } => {
            ChronoDuration::seconds(*grace_secs as i64)
        }
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
