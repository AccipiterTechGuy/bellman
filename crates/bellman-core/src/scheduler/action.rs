//! Pluggable fire callback injected into the scheduler.
//!
//! C6 will wire real launch / notify / slot actions behind this trait. The
//! engine only requires claim-before-work + a single callback per fire.

use crate::store::{RunClaim, Timer};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use uuid::Uuid;

/// How the fire was classified by the misfire / due logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FireKind {
    /// Due and within grace (normal on-time or slightly late).
    OnTime,
    /// Delivered late but still within grace (single fire).
    Late { lateness: ChronoDuration },
    /// Coalesced backlog into one recovery fire.
    Coalesced { missed_count: u32 },
    /// One step of a catch-up burst.
    CatchUp { index: u32 },
}

/// Context passed to [`FireAction::on_fire`].
#[derive(Debug)]
pub struct FireContext<'a> {
    pub timer: &'a Timer,
    pub scheduled_for: DateTime<Utc>,
    pub run_id: Uuid,
    pub kind: FireKind,
    pub claimed_at: DateTime<Utc>,
}

impl<'a> FireContext<'a> {
    pub fn from_claim(timer: &'a Timer, claim: &RunClaim, kind: FireKind) -> Self {
        Self {
            timer,
            scheduled_for: claim.scheduled_for,
            run_id: claim.run_id,
            kind,
            claimed_at: claim.claimed_at,
        }
    }
}

/// Injected action sink. Implementations must be side-effect free w.r.t. the
/// claim ledger — the engine claims before calling and completes after.
///
/// SCH1: `on_fire` is the fire-PRODUCER hook. It runs on the scheduler
/// thread after the fire transaction committed and must only do short work —
/// the configured action (`Launch` / `Notify` / `None`) executes on a
/// dispatcher worker, never here.
pub trait FireAction {
    fn on_fire(&mut self, ctx: &FireContext<'_>) -> Result<(), String>;

    /// True when `on_fire` executed the action to completion on the caller's
    /// thread (legacy/test actions): the scheduler then closes the claim
    /// itself. False for the worker-pool dispatcher — its workers commit the
    /// durable result, so the loop never waits for an action.
    fn executes_inline(&self) -> bool {
        true
    }

    /// Startup recovery may begin: the R10 reply scan/ingest, outbox
    /// recovery and folder reconciliation completed, so the dispatcher pump
    /// and transport-publication pump may start. Default no-op.
    fn boot_complete(&mut self) {}

    /// Stop accepting new jobs and let in-flight lanes finish (shutdown
    /// drains rather than truncating). Default no-op.
    fn shutdown(&mut self) {}
}

/// No-op action (useful when only the ledger / next-fire advance matters).
#[derive(Debug, Default, Clone)]
pub struct NopAction;

impl FireAction for NopAction {
    fn on_fire(&mut self, _ctx: &FireContext<'_>) -> Result<(), String> {
        Ok(())
    }
}

/// Records every fire for assertions in tests / demos.
#[derive(Debug, Default, Clone)]
pub struct RecordingAction {
    pub events: Vec<RecordedFire>,
}

/// One recorded fire event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedFire {
    pub timer_id: uuid::Uuid,
    pub timer_name: String,
    pub scheduled_for: DateTime<Utc>,
    pub run_id: Uuid,
    pub kind: FireKind,
}

impl FireAction for RecordingAction {
    fn on_fire(&mut self, ctx: &FireContext<'_>) -> Result<(), String> {
        self.events.push(RecordedFire {
            timer_id: ctx.timer.id,
            timer_name: ctx.timer.name.clone(),
            scheduled_for: ctx.scheduled_for,
            run_id: ctx.run_id,
            kind: ctx.kind.clone(),
        });
        Ok(())
    }
}

impl RecordingAction {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn fires_for(&self, timer_id: uuid::Uuid) -> impl Iterator<Item = &RecordedFire> {
        self.events.iter().filter(move |e| e.timer_id == timer_id)
    }
}
