//! The claim → act → record path.
//!
//! Applying a misfire policy to one due timer, recovering claims a crash left
//! behind, running the action under a store claim, and the two ledger writes
//! (`last_fired` anchor and the full fired mark) that move a timer forward.

use super::misfire::{grace_for, saturating_secs, walk_missed};
use super::types::{DeliveredFire, SchedulerError, SchedulerResult};
use super::Scheduler;
use crate::scheduler::action::{FireAction, FireContext, FireKind};
use crate::scheduler::clock::Clock;
use crate::store::{
    ClaimStatus, MisfirePolicy, RunClaim, StoreError, Timer, TimerId, TimerPatch, TimerUpdate,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};

/// IK2: rewrite `timer.json` from the fresh store row (next_fire advanced by
/// the fire bookkeeping). View-only — errors surface on stderr, never fail
/// the fire path.
fn refresh_timer_json(tree: &crate::tree::TimersTree, store: &crate::store::Store, id: TimerId) {
    let fresh = match store.get_timer(id) {
        Ok(Some(t)) => t,
        _ => return,
    };
    let owner = store.get_timer_owner(id).ok().flatten();
    if let Err(e) = tree.sync_timer_json(&fresh, owner.as_deref()) {
        eprintln!("bellman: timer.json refresh failed for {id}: {e}");
    }
}

impl<C: Clock, A: FireAction> Scheduler<C, A> {
    /// Apply misfire policy for a single overdue / due timer. Returns delivered fires.
    pub(super) fn handle_due_timer(
        &mut self,
        timer_id: TimerId,
        scheduled_for_hint: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> SchedulerResult<Vec<DeliveredFire>> {
        let Some(timer) = self.store.get_timer(timer_id)? else {
            return Ok(vec![]);
        };
        if !timer.enabled {
            return Ok(vec![]);
        }
        // Always trust the store's current next_fire over a possibly stale heap hint.
        let Some(scheduled_for) = timer.next_fire_utc else {
            return Ok(vec![]);
        };
        if scheduled_for > now {
            self.push_if_in_horizon(scheduled_for, timer_id);
            return Ok(vec![]);
        }
        let _ = scheduled_for_hint;

        let lateness = now.signed_duration_since(scheduled_for);
        let grace = grace_for(&timer);

        match &timer.misfire {
            MisfirePolicy::Skip => {
                if lateness <= grace {
                    let kind = if lateness <= ChronoDuration::seconds(1) {
                        FireKind::OnTime
                    } else {
                        FireKind::Late { lateness }
                    };
                    if let Some(d) = self.deliver_one(&timer, scheduled_for, kind)? {
                        self.requeue_timer(timer_id)?;
                        Ok(vec![d])
                    } else {
                        self.requeue_timer(timer_id)?;
                        Ok(vec![])
                    }
                } else {
                    // Beyond grace: skip backlog, advance to first fire after now.
                    self.advance_past_now(timer_id, now)?;
                    Ok(vec![])
                }
            }
            MisfirePolicy::Coalesce { .. } => {
                // Grace is checked per missed occurrence against *now*, not only
                // the oldest. Walk the backlog: drop out-of-grace slots, fire
                // once for the latest in-grace slot (coalesce), then jump ahead.
                let walk = walk_missed(&timer, scheduled_for, now);
                let in_grace: Vec<DateTime<Utc>> = walk
                    .iter()
                    .copied()
                    .filter(|m| now.signed_duration_since(*m) <= grace)
                    .collect();
                // Latest in-grace occurrence (e.g. Monday 09:00 when recovering
                // Monday 10:00 after a weekend, with 1h default grace). Nothing
                // in grace ⇒ skip the whole backlog.
                let Some(&fire_at) = in_grace.last() else {
                    self.advance_past_now(timer_id, now)?;
                    return Ok(vec![]);
                };
                // `walk_missed` caps its output well below u32::MAX, so this
                // never clamps in practice; saturate rather than wrap so a
                // reported missed-count can never come back as a small number.
                let missed = u32::try_from(walk.len()).unwrap_or(u32::MAX);
                let late = now.signed_duration_since(fire_at);
                let kind = if missed > 1 || in_grace.len() > 1 {
                    FireKind::Coalesced {
                        missed_count: missed,
                    }
                } else if late <= ChronoDuration::seconds(1) {
                    FireKind::OnTime
                } else {
                    FireKind::Late { lateness: late }
                };
                // mark_fired(last_fired=fire_at) jumps the ledger past older misses.
                if let Some(d) = self.deliver_one(&timer, fire_at, kind)? {
                    self.advance_past_now(timer_id, now)?;
                    Ok(vec![d])
                } else {
                    self.advance_past_now(timer_id, now)?;
                    Ok(vec![])
                }
            }
            MisfirePolicy::CatchUp {
                grace_secs,
                max_catch_up,
            } => {
                let grace = saturating_secs(*grace_secs);
                let max = *max_catch_up;
                let mut delivered = Vec::new();
                let mut scheduled = scheduled_for;
                let mut index = 0u32;
                // Safety cap on walk steps (includes out-of-grace skips).
                for _ in 0..100_000 {
                    if scheduled > now || index >= max {
                        break;
                    }
                    let late = now.signed_duration_since(scheduled);
                    if late > grace {
                        // Older than grace: drop this slot and continue to newer
                        // misses that may still be inside the window.
                        self.anchor_last_fired(timer_id, scheduled)?;
                        let Some(t) = self.store.get_timer(timer_id)? else {
                            break;
                        };
                        match t.next_fire_utc {
                            Some(nf) if nf > scheduled => {
                                scheduled = nf;
                                continue;
                            }
                            _ => break,
                        }
                    }
                    let Some(timer_now) = self.store.get_timer(timer_id)? else {
                        break;
                    };
                    // A `None` means recovered or already completed — the step
                    // still counts.
                    if let Some(d) =
                        self.deliver_one(&timer_now, scheduled, FireKind::CatchUp { index })?
                    {
                        delivered.push(d);
                    }
                    index += 1;
                    let Some(t) = self.store.get_timer(timer_id)? else {
                        break;
                    };
                    match t.next_fire_utc {
                        Some(nf) if nf > scheduled => {
                            scheduled = nf;
                            if nf > now {
                                self.push_if_in_horizon(nf, timer_id);
                                break;
                            }
                        }
                        Some(_) | None => {
                            if t.next_fire_utc.is_none_or(|nf| nf > now) {
                                if let Some(nf) = t.next_fire_utc {
                                    self.push_if_in_horizon(nf, timer_id);
                                }
                                break;
                            }
                            scheduled = t.next_fire_utc.unwrap_or(now);
                        }
                    }
                }
                let Some(t) = self.store.get_timer(timer_id)? else {
                    return Ok(delivered);
                };
                if t.next_fire_utc.is_some_and(|nf| nf <= now) {
                    self.advance_past_now(timer_id, now)?;
                } else {
                    self.requeue_timer(timer_id)?;
                }
                Ok(delivered)
            }
        }
    }

    /// Recover claims left in `claimed` state after a crash (at-least-once).
    pub(super) fn recover_pending_claims(&mut self) -> SchedulerResult<Vec<DeliveredFire>> {
        let pending = self.store.pending_claims()?;
        let mut out = Vec::new();
        for claim in pending {
            let Some(timer) = self.store.get_timer(claim.timer_id)? else {
                // Timer gone — close the orphan claim so it does not loop. The
                // action was never delivered, so do not record it as success.
                let _ = self.store.fail_run(claim.run_id);
                continue;
            };
            if let Some(d) = self.finish_claimed_run(&timer, &claim, FireKind::Late {
                lateness: self
                    .clock
                    .wall_now()
                    .signed_duration_since(claim.scheduled_for),
            })? {
                out.push(d);
            }
        }
        Ok(out)
    }

    /// Claim → action → complete → advance last_fired / next_fire.
    ///
    /// If a prior crash left a `claimed` row, re-runs the action (at-least-once).
    /// Completed claims are not re-acted (backward-jump / double-fire guard).
    fn deliver_one(
        &mut self,
        timer: &Timer,
        scheduled_for: DateTime<Utc>,
        kind: FireKind,
    ) -> SchedulerResult<Option<DeliveredFire>> {
        let claim = match self.store.claim_run(timer.id, scheduled_for) {
            Ok(c) => c,
            Err(StoreError::AlreadyClaimed {
                timer_id,
                scheduled_for,
            }) => {
                match self.store.get_claim_for(timer_id, scheduled_for)? {
                    Some(existing) if existing.status == ClaimStatus::Claimed => {
                        // Crash between claim and complete — recover the action.
                        return self.finish_claimed_run(timer, &existing, kind);
                    }
                    Some(_) => {
                        // Already completed — never re-fire this slot.
                        self.ensure_advanced_past(timer.id, scheduled_for)?;
                        return Ok(None);
                    }
                    None => {
                        return Err(SchedulerError::Internal(format!(
                            "AlreadyClaimed but no row for {timer_id} @ {scheduled_for}"
                        )));
                    }
                }
            }
            Err(e) => return Err(e.into()),
        };

        self.finish_claimed_run(timer, &claim, kind)
    }

    /// Run the action for an existing claim, complete it, and advance the timer.
    fn finish_claimed_run(
        &mut self,
        timer: &Timer,
        claim: &RunClaim,
        kind: FireKind,
    ) -> SchedulerResult<Option<DeliveredFire>> {
        if claim.status == ClaimStatus::Completed {
            self.ensure_advanced_past(timer.id, claim.scheduled_for)?;
            return Ok(None);
        }

        let ctx = FireContext::from_claim(timer, claim, kind.clone());

        // IK2: project the fire into the per-timer folder tree (view only —
        // failures surface but never break the fire path).
        let tree = self
            .config
            .data_dir
            .as_ref()
            .map(|d| crate::tree::TimersTree::new(d));
        if let (Some(tree), Some(data_dir)) = (tree.as_ref(), self.config.data_dir.as_ref()) {
            match crate::events::EventLog::open_under_configured(data_dir) {
                Ok(mut log) => {
                    if let Err(e) = crate::tree::project_run_started(
                        tree, &self.store, timer, claim, &ctx.kind, &mut log,
                    ) {
                        eprintln!("bellman: timer tree fire projection failed: {e}");
                    }
                }
                Err(e) => eprintln!("bellman: timer tree fire projection (log open) failed: {e}"),
            }
        }

        let action_res = self.action.on_fire(&ctx);

        // Dogfood: when the internal system.prune timer fires, run the prune
        // pass (JSONL rotate/retain + terminal one-shot cleanup).
        if crate::pruner::is_system_prune_timer(timer) {
            if let Some(data_dir) = self.config.data_dir.clone() {
                let prune_cfg = crate::pruner::PruneConfig {
                    retention: self.config.retention,
                    interval: self.config.prune_interval,
                    ack_grace: self.config.ack_grace,
                    max_current_bytes: self.config.log_rotation_max_bytes,
                    budget_bytes: self.config.log_retention_budget_bytes,
                };
                let now = self.clock.wall_now();
                if let Err(e) =
                    crate::pruner::run_prune_under(&mut self.store, &data_dir, &prune_cfg, now, true)
                {
                    eprintln!("bellman: system.prune fire failed: {e}");
                }
            }
        }

        // Close the claim even when the action fails so recovery does not
        // infinite-loop — but record the real outcome: delivered vs
        // wake-failed. Action errors still surface to the caller.
        if claim.status == ClaimStatus::Claimed {
            if action_res.is_ok() {
                self.store.complete_run(claim.run_id)?;
            } else {
                self.store.fail_run(claim.run_id)?;
            }
        }

        // Advance last_fired only when this slot is not yet recorded (crash may
        // have updated next_fire already).
        let fresh = self.store.get_timer(timer.id)?;
        let needs_mark = fresh
            .as_ref()
            .is_some_and(|t| t.last_fired.is_none_or(|lf| lf < claim.scheduled_for));
        if needs_mark {
            self.mark_fired(timer.id, claim.scheduled_for)?;
        } else {
            self.requeue_timer(timer.id)?;
        }

        if let Err(e) = action_res {
            // IK2: status.json stays the firing snapshot — the delivery
            // failure is honest in the claim ledger and the wake_failed event
            // (R5 `failed` is reserved for app reports, IK3). timer.json
            // still picks up the advanced next_fire.
            if let Some(tree) = tree.as_ref() {
                refresh_timer_json(tree, &self.store, timer.id);
            }
            return Err(SchedulerError::Action(e));
        }

        // IK2: timer.json picks up the advanced next_fire from
        // mark_fired/requeue above. status.json intentionally stays at the
        // firing snapshot: the R5 `completed` state is an app report (IK3),
        // and the claim ledger's `Completed` only means wake_delivered.
        if let Some(tree) = tree.as_ref() {
            refresh_timer_json(tree, &self.store, timer.id);
        }

        Ok(Some(DeliveredFire {
            timer_id: timer.id,
            scheduled_for: claim.scheduled_for,
            run_id: claim.run_id,
            kind,
        }))
    }

    /// Set `last_fired` without `record_run` so next_fire advances past `anchor`.
    fn anchor_last_fired(
        &mut self,
        timer_id: TimerId,
        anchor: DateTime<Utc>,
    ) -> SchedulerResult<()> {
        let timer = self
            .store
            .get_timer(timer_id)?
            .ok_or_else(|| SchedulerError::Internal(format!("timer {timer_id} missing")))?;
        self.store.update_timer(TimerUpdate {
            id: timer_id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                last_fired: Some(Some(anchor)),
                ..Default::default()
            },
        })?;
        Ok(())
    }

    fn mark_fired(
        &mut self,
        timer_id: TimerId,
        scheduled_for: DateTime<Utc>,
    ) -> SchedulerResult<()> {
        let timer = self
            .store
            .get_timer(timer_id)?
            .ok_or_else(|| SchedulerError::Internal(format!("timer {timer_id} missing")))?;
        let mut occ = timer.occurrence.clone();
        occ.record_run();
        self.store.update_timer(TimerUpdate {
            id: timer_id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                last_fired: Some(Some(scheduled_for)),
                occurrence: Some(occ),
                ..Default::default()
            },
        })?;
        Ok(())
    }
}
