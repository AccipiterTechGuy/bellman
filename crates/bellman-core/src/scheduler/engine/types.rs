//! Value types exchanged with the engine.
//!
//! Control messages and their handle, the per-tick result, the delivered-fire
//! record, the scheduler error type, and the horizon heap entry. No scheduling
//! logic lives here — only the shapes the rest of `engine` passes around.

use crate::scheduler::action::FireKind;
use crate::store::{StoreError, TimerId};
use chrono::{DateTime, Utc};
use std::sync::mpsc::Sender;

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
    pub(super) tx: Sender<ControlMsg>,
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

/// Outcome of a single [`super::Scheduler::tick`].
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

/// One slot in the near-horizon heap: earliest `fire_at` wins, `timer_id`
/// breaks ties so ordering is total and deterministic.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct HeapEntry {
    pub(super) fire_at: DateTime<Utc>,
    pub(super) timer_id: TimerId,
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
