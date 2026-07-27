//! Heap-loop scheduler engine.
//!
//! - `BinaryHeap<Reverse<(fire_at_utc, TimerId)>>` over the near-horizon window
//! - Chunked sleeps `min(next_fire − now, max_sleep)` re-reading the wall clock
//! - Wall-vs-monotonic clock-jump detector → misfire pass + horizon rebuild
//! - Per-timer misfire policy (skip / coalesce / catch_up)
//! - Claim-before-work via the store run ledger
//! - Channel-driven refill on insert/edit

use super::action::{FireAction, FireContext, FireKind};
use super::clock::{Clock, MonoTime};
use super::config::{SchedulerConfig, HIGH_FREQ_PERIOD_SECS};
use crate::occurrence::OccurrenceKind;
use crate::store::{
    MisfirePolicy, Store, StoreError, Timer, TimerId, TimerPatch, TimerUpdate,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

/// Control messages for the running loop (refill after insert/edit, shutdown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMsg {
    /// Rebuild the horizon heap from the store (insert/edit/delete).
    Refill,
    /// Request a clean loop exit.
    Shutdown,
}

/// Cloneable handle for waking the engine from another thread / caller.
#[derive(Debug, Clone)]
pub struct ControlHandle {
    tx: Sender<ControlMsg>,
}

impl ControlHandle {
    pub fn refill(&self) {
        let _ = self.tx.send(ControlMsg::Refill);
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(ControlMsg::Shutdown);
    }

    pub fn sender(&self) -> Sender<ControlMsg> {
        self.tx.clone()
    }
}

/// Outcome of a single [`Scheduler::tick`].
#[derive(Debug, Default)]
pub struct TickResult {
    /// Fires delivered during this tick.
    pub fires: Vec<DeliveredFire>,
    /// Wall/mono divergence triggered misfire + rebuild.
    pub clock_jump: bool,
    /// Horizon was rebuilt (jump, refill message, or post-fire).
    pub refilled: bool,
    /// Shutdown was requested.
    pub shutdown: bool,
}

/// One successfully claimed + actioned fire.
#[derive(Debug, Clone)]
pub struct DeliveredFire {
    pub timer_id: TimerId,
    pub scheduled_for: DateTime<Utc>,
    pub run_id: uuid::Uuid,
    pub kind: FireKind,
}

/// Scheduler errors (wrap store + action failures).
#[derive(Debug)]
pub enum SchedulerError {
    Store(StoreError),
    Action(String),
    Internal(String),
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "store: {e}"),
            Self::Action(e) => write!(f, "action: {e}"),
            Self::Internal(e) => write!(f, "internal: {e}"),
        }
    }
}

impl std::error::Error for SchedulerError {}

impl From<StoreError> for SchedulerError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

pub type SchedulerResult<T> = Result<T, SchedulerError>;

#[derive(Debug, Clone, Eq, PartialEq)]
struct HeapEntry {
    fire_at: DateTime<Utc>,
    timer_id: TimerId,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.fire_at
            .cmp(&other.fire_at)
            .then_with(|| self.timer_id.cmp(&other.timer_id))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Near-horizon timer engine.
pub struct Scheduler<C: Clock, A: FireAction> {
    store: Store,
    clock: C,
    action: A,
    config: SchedulerConfig,
    heap: BinaryHeap<Reverse<HeapEntry>>,
    last_wall: DateTime<Utc>,
    last_mono: MonoTime,
    control_tx: Sender<ControlMsg>,
    control_rx: Receiver<ControlMsg>,
    booted: bool,
}

impl<C: Clock, A: FireAction> Scheduler<C, A> {
    /// Build a scheduler. Call [`Self::boot`] before ticking.
    pub fn new(store: Store, clock: C, action: A, config: SchedulerConfig) -> Self {
        let (control_tx, control_rx) = mpsc::channel();
        let wall = clock.wall_now();
        let mono = clock.mono_now();
        Self {
            store,
            clock,
            action,
            config,
            heap: BinaryHeap::new(),
            last_wall: wall,
            last_mono: mono,
            control_tx,
            control_rx,
            booted: false,
        }
    }

    /// Handle used to signal refill / shutdown from outside the loop.
    pub fn control_handle(&self) -> ControlHandle {
        ControlHandle {
            tx: self.control_tx.clone(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    pub fn action(&self) -> &A {
        &self.action
    }

    pub fn action_mut(&mut self) -> &mut A {
        &mut self.action
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    pub fn heap_len(&self) -> usize {
        self.heap.len()
    }

    /// Peek the next fire on the heap (if any).
    pub fn peek_next(&self) -> Option<(DateTime<Utc>, TimerId)> {
        self.heap
            .peek()
            .map(|Reverse(e)| (e.fire_at, e.timer_id))
    }

    /// Startup: record clock baselines, run misfire pass, rebuild horizon.
    ///
    /// Returns fires delivered by the startup misfire pass (if any).
    pub fn boot(&mut self) -> SchedulerResult<Vec<DeliveredFire>> {
        self.last_wall = self.clock.wall_now();
        self.last_mono = self.clock.mono_now();
        let fires = self.misfire_pass()?;
        self.rebuild_horizon()?;
        self.booted = true;
        Ok(fires)
    }

    /// One loop iteration: control msgs, clock-jump check, drain due timers.
    ///
    /// Does **not** sleep — the caller (or [`Self::run_for`]) handles sleeping
    /// via [`Self::next_sleep`].
    pub fn tick(&mut self) -> SchedulerResult<TickResult> {
        if !self.booted {
            self.boot()?;
        }

        let mut result = TickResult::default();

        // Drain control channel first so edits apply before we fire.
        loop {
            match self.control_rx.try_recv() {
                Ok(ControlMsg::Refill) => {
                    self.rebuild_horizon()?;
                    result.refilled = true;
                }
                Ok(ControlMsg::Shutdown) => {
                    result.shutdown = true;
                    return Ok(result);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        let wall = self.clock.wall_now();
        let mono = self.clock.mono_now();

        if self.is_clock_jump(wall, mono) {
            result.clock_jump = true;
            let jumped = self.misfire_pass()?;
            result.fires.extend(jumped);
            self.rebuild_horizon()?;
            result.refilled = true;
            self.last_wall = wall;
            self.last_mono = mono;
            // After jump, still drain anything due at the new wall time.
        } else {
            self.last_wall = wall;
            self.last_mono = mono;
        }

        let now = self.clock.wall_now();
        let fires = self.drain_due(now)?;
        result.fires.extend(fires);
        Ok(result)
    }

    /// Sleep duration until the next useful wake: `min(next_fire − now, max_sleep)`.
    ///
    /// Zero when work is due now. `max_sleep` when the heap is empty (periodic
    /// refill / jump check backstop).
    pub fn next_sleep(&self) -> Duration {
        let now = self.clock.wall_now();
        match self.peek_next() {
            Some((fire_at, _)) if fire_at <= now => Duration::ZERO,
            Some((fire_at, _)) => {
                let delta = fire_at.signed_duration_since(now);
                let as_std = delta.to_std().unwrap_or(Duration::ZERO);
                if as_std.is_zero() {
                    Duration::ZERO
                } else {
                    as_std.min(self.config.max_sleep)
                }
            }
            None => self.config.max_sleep,
        }
    }

    /// Run the loop for approximately `duration` of wall time (uses `clock.sleep`).
    pub fn run_for(&mut self, duration: Duration) -> SchedulerResult<TickResult> {
        if !self.booted {
            self.boot()?;
        }
        let start = self.clock.wall_now();
        let end = start
            + ChronoDuration::from_std(duration).map_err(|e| {
                SchedulerError::Internal(format!("run_for duration: {e}"))
            })?;
        let mut acc = TickResult::default();
        loop {
            let t = self.tick()?;
            acc.fires.extend(t.fires);
            acc.clock_jump |= t.clock_jump;
            acc.refilled |= t.refilled;
            if t.shutdown {
                acc.shutdown = true;
                break;
            }
            let now = self.clock.wall_now();
            if now >= end {
                break;
            }
            let mut sleep = self.next_sleep();
            let remain = end
                .signed_duration_since(now)
                .to_std()
                .unwrap_or(Duration::ZERO);
            if remain.is_zero() {
                break;
            }
            sleep = sleep.min(remain);
            if sleep.is_zero() {
                // Head still reports due after a drain with no progress — yield a
                // short slice so SystemClock wall time can advance (and avoid a
                // tight CPU spin). SimulatedClock advances both axes.
                sleep = Duration::from_millis(1).min(remain);
            }
            self.clock.sleep(sleep);
        }
        Ok(acc)
    }

    /// Run until a shutdown control message is received.
    pub fn run_until_shutdown(&mut self) -> SchedulerResult<TickResult> {
        if !self.booted {
            self.boot()?;
        }
        let mut acc = TickResult::default();
        loop {
            let t = self.tick()?;
            acc.fires.extend(t.fires);
            acc.clock_jump |= t.clock_jump;
            acc.refilled |= t.refilled;
            if t.shutdown {
                acc.shutdown = true;
                break;
            }
            let sleep = match self.next_sleep() {
                d if d.is_zero() => Duration::from_millis(1),
                d => d,
            };
            self.clock.sleep(sleep);
        }
        Ok(acc)
    }

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
                self.heap.push(Reverse(HeapEntry {
                    fire_at: nf,
                    timer_id: t.id,
                }));
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
                self.heap.push(Reverse(HeapEntry {
                    fire_at: nf,
                    timer_id: t.id,
                }));
            }
        }
        Ok(())
    }

    /// Consume the scheduler, returning the action sink (tests read recorded fires).
    pub fn into_action(self) -> A {
        self.action
    }

    // ── internals ───────────────────────────────────────────────────────

    fn is_clock_jump(&self, wall: DateTime<Utc>, mono: MonoTime) -> bool {
        let wall_delta = wall.signed_duration_since(self.last_wall);
        let wall_ms = wall_delta.num_milliseconds();
        let mono_delta = mono.saturating_sub(self.last_mono);
        let mono_ms = mono_delta.as_millis() as i64;
        let divergence = (wall_ms - mono_ms).unsigned_abs();
        let threshold_ms = self.config.jump_threshold.as_millis() as u64;
        divergence > threshold_ms
    }

    /// Scan enabled timers whose next fire is in the past and apply policy.
    fn misfire_pass(&mut self) -> SchedulerResult<Vec<DeliveredFire>> {
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

    fn drain_due(&mut self, now: DateTime<Utc>) -> SchedulerResult<Vec<DeliveredFire>> {
        let mut out = Vec::new();
        // Timers fully resolved this drain (next_fire > now or exhausted). Stale
        // duplicate heap entries for the same id are dropped on sight.
        let mut resolved = std::collections::HashSet::new();
        // Safety cap against pathological re-push bugs.
        for _ in 0..1_000 {
            let Some(Reverse(head)) = self.heap.peek() else {
                break;
            };
            if head.fire_at > now {
                break;
            }
            let Reverse(entry) = self.heap.pop().expect("peeked");
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

    /// Apply misfire policy for a single overdue / due timer. Returns delivered fires.
    fn handle_due_timer(
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
                    match self.deliver_one(&timer, scheduled_for, kind)? {
                        Some(d) => {
                            self.requeue_timer(timer_id)?;
                            Ok(vec![d])
                        }
                        None => {
                            self.requeue_timer(timer_id)?;
                            Ok(vec![])
                        }
                    }
                } else {
                    // Beyond grace: skip backlog, advance to first fire after now.
                    self.advance_past_now(timer_id, now)?;
                    Ok(vec![])
                }
            }
            MisfirePolicy::Coalesce { .. } => {
                if lateness <= grace {
                    // Count missed slots for the event kind (best-effort).
                    let missed = count_missed(&timer, scheduled_for, now);
                    let kind = if missed > 1 {
                        FireKind::Coalesced {
                            missed_count: missed,
                        }
                    } else if lateness <= ChronoDuration::seconds(1) {
                        FireKind::OnTime
                    } else {
                        FireKind::Late { lateness }
                    };
                    // Fire once for the original scheduled instant, then jump to future.
                    match self.deliver_one(&timer, scheduled_for, kind)? {
                        Some(d) => {
                            // Coalesce: after one recovery fire, skip any further
                            // backlog still ≤ now.
                            self.advance_past_now(timer_id, now)?;
                            Ok(vec![d])
                        }
                        None => {
                            self.advance_past_now(timer_id, now)?;
                            Ok(vec![])
                        }
                    }
                } else {
                    self.advance_past_now(timer_id, now)?;
                    Ok(vec![])
                }
            }
            MisfirePolicy::CatchUp {
                grace_secs,
                max_catch_up,
            } => {
                let grace = ChronoDuration::seconds(*grace_secs as i64);
                let max = *max_catch_up;
                let mut delivered = Vec::new();
                let mut scheduled = scheduled_for;
                let mut index = 0u32;
                while index < max {
                    if scheduled > now {
                        break;
                    }
                    let late = now.signed_duration_since(scheduled);
                    if late > grace {
                        // This (and older) misses are outside grace — drop them.
                        break;
                    }
                    let Some(timer_now) = self.store.get_timer(timer_id)? else {
                        break;
                    };
                    match self.deliver_one(
                        &timer_now,
                        scheduled,
                        FireKind::CatchUp { index },
                    )? {
                        Some(d) => delivered.push(d),
                        None => {
                            // Already claimed — still walk forward.
                        }
                    }
                    index += 1;
                    // Next candidate from occurrence after this scheduled time.
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
                            // deliver_one already advanced; stop if nothing due.
                            if t.next_fire_utc.map(|nf| nf > now).unwrap_or(true) {
                                if let Some(nf) = t.next_fire_utc {
                                    self.push_if_in_horizon(nf, timer_id);
                                }
                                break;
                            }
                            scheduled = t.next_fire_utc.unwrap_or(now);
                        }
                    }
                }
                // Drop any remaining backlog beyond catch-up cap / grace.
                let Some(t) = self.store.get_timer(timer_id)? else {
                    return Ok(delivered);
                };
                if t.next_fire_utc.map(|nf| nf <= now).unwrap_or(false) {
                    self.advance_past_now(timer_id, now)?;
                } else {
                    self.requeue_timer(timer_id)?;
                }
                Ok(delivered)
            }
        }
    }

    /// Claim → action → complete → advance last_fired / next_fire.
    fn deliver_one(
        &mut self,
        timer: &Timer,
        scheduled_for: DateTime<Utc>,
        kind: FireKind,
    ) -> SchedulerResult<Option<DeliveredFire>> {
        let claim = match self.store.claim_run(timer.id, scheduled_for) {
            Ok(c) => c,
            Err(StoreError::AlreadyClaimed { .. }) => {
                // Prior claim (crash recovery or re-entry). Do not re-fire.
                // Still ensure the timer has moved past this scheduled instant.
                self.ensure_advanced_past(timer.id, scheduled_for)?;
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };

        let ctx = FireContext::from_claim(timer, &claim, kind.clone());
        let action_res = self.action.on_fire(&ctx);

        // Complete the claim regardless — action failures are surfaced after
        // ledger bookkeeping so we never double-fire on retry of the same slot.
        self.store.complete_run(claim.run_id)?;
        self.mark_fired(timer.id, scheduled_for)?;

        if let Err(e) = action_res {
            return Err(SchedulerError::Action(e));
        }

        Ok(Some(DeliveredFire {
            timer_id: timer.id,
            scheduled_for,
            run_id: claim.run_id,
            kind,
        }))
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

    /// Advance the timer so `next_fire_utc` is strictly after `now` without
    /// recording a delivered run (skip path).
    fn advance_past_now(&mut self, timer_id: TimerId, now: DateTime<Utc>) -> SchedulerResult<()> {
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
                    .map(|nf| nf - ChronoDuration::nanoseconds(1))
                    .unwrap_or(now)
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

    fn ensure_advanced_past(
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
                if t2.next_fire_utc.map(|nf| nf <= now).unwrap_or(false) {
                    self.advance_past_now(timer_id, now)?;
                } else {
                    self.requeue_timer(timer_id)?;
                }
                Ok(())
            }
        }
    }

    fn requeue_timer(&mut self, timer_id: TimerId) -> SchedulerResult<()> {
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

    fn push_if_in_horizon(&mut self, fire_at: DateTime<Utc>, timer_id: TimerId) {
        let now = self.clock.wall_now();
        let horizon_ok = ChronoDuration::from_std(self.config.horizon)
            .ok()
            .map(|h| fire_at <= now + h)
            .unwrap_or(true);
        let force = self
            .store
            .get_timer(timer_id)
            .ok()
            .flatten()
            .map(|t| is_high_frequency(&t))
            .unwrap_or(false);
        if horizon_ok || force {
            self.heap.push(Reverse(HeapEntry {
                fire_at,
                timer_id,
            }));
        }
    }
}

fn is_high_frequency(timer: &Timer) -> bool {
    match timer.occurrence.kind() {
        OccurrenceKind::Interval { every_secs, .. } => *every_secs < HIGH_FREQ_PERIOD_SECS,
        _ => false,
    }
}

/// Grace window for a timer under its misfire policy.
fn grace_for(timer: &Timer) -> ChronoDuration {
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

/// Best-effort count of missed scheduled instants in (scheduled_for, now].
fn count_missed(timer: &Timer, scheduled_for: DateTime<Utc>, now: DateTime<Utc>) -> u32 {
    let mut occ = timer.occurrence.clone();
    let tz = occ.timezone();
    let mut cursor = scheduled_for.with_timezone(&tz);
    let mut count = 1u32; // the original scheduled_for itself
    for _ in 0..10_000 {
        match occ.next_fire(cursor) {
            Some(c) => {
                let c_utc = c.with_timezone(&Utc);
                if c_utc > now {
                    break;
                }
                count = count.saturating_add(1);
                cursor = c;
            }
            None => break,
        }
    }
    count
}
