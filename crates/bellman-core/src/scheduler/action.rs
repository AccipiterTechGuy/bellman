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
pub trait FireAction {
    fn on_fire(&mut self, ctx: &FireContext<'_>) -> Result<(), String>;
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
