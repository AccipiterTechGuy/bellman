//! The loop that drives the engine.
//!
//! Boot ordering, one-tick semantics, sleep sizing, the two run helpers, the
//! interruptible wait, control-message application, and the wall-vs-monotonic
//! clock-jump detector. Everything here is about *when* work happens; the work
//! itself lives in the sibling modules.

use super::types::{ControlMsg, DeliveredFire, SchedulerError, SchedulerResult, TickResult};
use super::Scheduler;
use crate::scheduler::action::FireAction;
use crate::scheduler::clock::{Clock, MonoTime};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

impl<C: Clock, A: FireAction> Scheduler<C, A> {
    /// Startup: recover pending claims, misfire pass, rebuild horizon.
    ///
    /// Recovery order matches BUILD_PLAN: pending run-claim recovery → misfire
    /// scan → horizon rebuild. Also runs P5 maintenance (system.prune ensure +
    /// catch-up, Jan-1 year recalibration) when `config.data_dir` is set.
    /// Returns all fires delivered during boot.
    pub fn boot(&mut self) -> SchedulerResult<Vec<DeliveredFire>> {
        self.last_wall = self.clock.wall_now();
        self.last_mono = self.clock.mono_now();

        // P5: ensure system.prune, catch-up prune, Jan-1 recalibrate.
        self.run_startup_maintenance()?;

        // IK3 (R10): read replies BEFORE anything can fire. An app can
        // answer while Bellman is stopped; superseding that run before its
        // reply was read would silently record the outcome unknown.
        self.run_reply_startup_scan();

        let mut fires = self.recover_pending_claims()?;
        fires.extend(self.misfire_pass()?);
        self.rebuild_horizon()?;
        self.booted = true;
        Ok(fires)
    }

    /// Scan every `reply-*.json`, fold in valid replies for still-current
    /// runs, then rebuild stale `status.json` / create-only stubs (R10
    /// startup order). No-op when `config.data_dir` is unset (unit tests).
    fn run_reply_startup_scan(&mut self) {
        let Some(engine) = self.config.reply_engine() else {
            return;
        };
        let now = self.clock.wall_now();
        crate::reply::startup_scan(&engine, &self.store, now);
    }

    /// Ensure `system.prune`, catch-up prune if due, Jan-1 pass if needed.
    ///
    /// No-ops when `config.data_dir` is unset (unit tests keep a pure store).
    fn run_startup_maintenance(&mut self) -> SchedulerResult<()> {
        let Some(data_dir) = self.config.data_dir.clone() else {
            return Ok(());
        };
        let prune_cfg = crate::pruner::PruneConfig {
            retention: self.config.retention,
            interval: self.config.prune_interval,
            ack_grace: self.config.ack_grace,
            max_current_bytes: self.config.log_rotation_max_bytes,
            budget_bytes: self.config.log_retention_budget_bytes,
            quarantine_budget_bytes: self.config.quarantine_budget_bytes,
        };
        let now = self.clock.wall_now();
        match crate::pruner::startup_maintenance(&mut self.store, &data_dir, &prune_cfg, now) {
            Ok(notes) => {
                for n in notes {
                    // No tracing dep required — best-effort stderr for ops.
                    eprintln!("bellman: {n}");
                }
                Ok(())
            }
            Err(e) => {
                // Maintenance failure must not brick the scheduler; log and continue.
                eprintln!("bellman: startup maintenance error: {e}");
                Ok(())
            }
        }
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
                Ok(ControlMsg::SetPauseAll(paused)) => {
                    self.pause_all = paused;
                }
                Ok(ControlMsg::ArmDeadline {
                    run_id,
                    kind,
                    wall_at,
                }) => {
                    self.push_deadline(run_id, kind, wall_at);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }

        // Global pause-all: the heap stays warm (so we wake up at the right
        // moments to clear it on resume), but nothing fires while paused.
        if self.pause_all {
            // Treat the tick as "did nothing" — the loop's sleep helper will use
            // the normal horizon to keep the wake cadence correct.
            return Ok(result);
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
            if let Some(msg) = self.wait_interruptible(sleep) {
                self.apply_control_msg(msg, &mut acc)?;
                if acc.shutdown {
                    break;
                }
            }
        }
        Ok(acc)
    }

    /// Run until a shutdown control message is received.
    ///
    /// Sleeps are interruptible: a [`ControlMsg::Refill`] or
    /// [`ControlMsg::Shutdown`] on the control channel wakes the loop immediately
    /// so insert/edit does not wait for `max_sleep`.
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
            if let Some(msg) = self.wait_interruptible(sleep) {
                self.apply_control_msg(msg, &mut acc)?;
                if acc.shutdown {
                    break;
                }
            }
        }
        Ok(acc)
    }

    /// Block until `d` elapses or a control message arrives.
    ///
    /// On OS clocks, uses `recv_timeout` so senders wake the loop. On simulated
    /// clocks, drains pending messages then advances virtual time by `d`.
    fn wait_interruptible(&self, d: Duration) -> Option<ControlMsg> {
        if let Ok(msg) = self.control_rx.try_recv() {
            return Some(msg);
        }
        if d.is_zero() {
            return None;
        }
        if self.clock.uses_os_time() {
            // Timeout and Disconnected both mean "no message"; the loop then
            // re-ticks and re-evaluates the clock.
            self.control_rx.recv_timeout(d).ok()
        } else {
            self.clock.sleep(d);
            self.control_rx.try_recv().ok()
        }
    }

    fn apply_control_msg(
        &mut self,
        msg: ControlMsg,
        acc: &mut TickResult,
    ) -> SchedulerResult<()> {
        match msg {
            ControlMsg::Refill => {
                self.rebuild_horizon()?;
                acc.refilled = true;
            }
            ControlMsg::Shutdown => {
                acc.shutdown = true;
            }
            ControlMsg::SetPauseAll(paused) => {
                self.pause_all = paused;
            }
            ControlMsg::ArmDeadline {
                run_id,
                kind,
                wall_at,
            } => {
                self.push_deadline(run_id, kind, wall_at);
            }
        }
        // Drain any further queued control messages without sleeping.
        loop {
            match self.control_rx.try_recv() {
                Ok(ControlMsg::Refill) => {
                    self.rebuild_horizon()?;
                    acc.refilled = true;
                }
                Ok(ControlMsg::Shutdown) => {
                    acc.shutdown = true;
                }
                Ok(ControlMsg::SetPauseAll(paused)) => {
                    self.pause_all = paused;
                }
                Ok(ControlMsg::ArmDeadline {
                    run_id,
                    kind,
                    wall_at,
                }) => {
                    self.push_deadline(run_id, kind, wall_at);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        Ok(())
    }

    fn is_clock_jump(&self, wall: DateTime<Utc>, mono: MonoTime) -> bool {
        let wall_delta = wall.signed_duration_since(self.last_wall);
        let wall_ms = wall_delta.num_milliseconds();
        let mono_delta = mono.saturating_sub(self.last_mono);
        // Saturate, never truncate: i64::MAX ms of monotonic time is ~292
        // million years, so this cannot clamp in practice — and clamping *high*
        // keeps the divergence large, so a real jump still gets reported. A
        // wrapping `as` could shrink the delta and hide one.
        let mono_ms = i64::try_from(mono_delta.as_millis()).unwrap_or(i64::MAX);
        let divergence = wall_ms.saturating_sub(mono_ms).unsigned_abs();
        // Same for the configured threshold: clamping high degrades to "never
        // report a jump", which is what an absurd config asks for. Wrapping
        // would instead produce a tiny threshold and spurious jumps.
        let threshold_ms =
            u64::try_from(self.config.jump_threshold.as_millis()).unwrap_or(u64::MAX);
        divergence > threshold_ms
    }
}
