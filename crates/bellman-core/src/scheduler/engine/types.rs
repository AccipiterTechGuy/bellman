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
    /// Flip the global pause-all flag (true = scheduler parks the heap).
    SetPauseAll(bool),
    /// Arm a lifecycle deadline entry on the heap (IK3 pickup/watchdog).
    /// Sent by the watcher when a reply arms/rearms a deadline outside the
    /// scheduler thread. Disarming needs no message: the expiry check is
    /// lazy (a disarmed entry is a no-op on wake).
    ArmDeadline {
        /// The run whose deadline this is.
        run_id: uuid::Uuid,
        /// Pickup grace, or the opt-in watchdog.
        kind: crate::reply::DeadlineKind,
        /// When it expires, so the loop can wake exactly then instead of
        /// discovering it on the next poll.
        wall_at: DateTime<Utc>,
    },
}

/// Cloneable handle for waking the engine from another thread / caller.
#[derive(Debug, Clone)]
pub struct ControlHandle {
    pub(super) tx: Sender<ControlMsg>,
}

impl ControlHandle {
    /// Ask the loop to rebuild its horizon heap now — what an external
    /// writer sends so a newly created timer does not wait for a tick.
    pub fn refill(&self) {
        let _ = self.tx.send(ControlMsg::Refill);
    }

    /// Ask the loop to stop after its current iteration.
    pub fn shutdown(&self) {
        let _ = self.tx.send(ControlMsg::Shutdown);
    }

    /// Toggle the global pause-all flag at runtime. The next tick observes it.
    pub fn set_pause_all(&self, paused: bool) {
        let _ = self.tx.send(ControlMsg::SetPauseAll(paused));
    }

    /// Arm a lifecycle deadline heap entry (IK3). Sent by the watcher when
    /// a reply arms/rearms a pickup/watchdog deadline outside the scheduler
    /// thread. Disarming needs no message — a disarmed entry is a lazy
    /// no-op on wake.
    pub fn arm_deadline(
        &self,
        run_id: uuid::Uuid,
        kind: crate::reply::DeadlineKind,
        wall_at: DateTime<Utc>,
    ) {
        let _ = self.tx.send(ControlMsg::ArmDeadline {
            run_id,
            kind,
            wall_at,
        });
    }

    /// A clonable sender for the control channel.
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
    /// The timer that fired.
    pub timer_id: TimerId,
    /// The instant it was meant to fire.
    pub scheduled_for: DateTime<Utc>,
    /// Identity of the firing.
    pub run_id: uuid::Uuid,
    /// On time, late, coalesced or catch-up.
    pub kind: FireKind,
}

/// Scheduler errors (wrap store + action failures).
#[derive(Debug)]
pub enum SchedulerError {
    /// A database operation failed.
    Store(StoreError),
    /// A fire action returned an error.
    Action(String),
    /// A scheduler invariant failed.
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

/// Result alias for scheduler operations.
pub type SchedulerResult<T> = Result<T, SchedulerError>;

/// What a heap slot wakes for.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum HeapKind {
    /// A timer's scheduled fire.
    Fire { timer_id: TimerId },
    /// An IK3 lifecycle deadline (pickup / opt-in watchdog). The wall time
    /// is a WAKE HINT: the monotonic deadline book decides whether the
    /// deadline has actually lapsed (wall jumps can never fire it early —
    /// on such a wake the entry is simply re-armed for the remainder).
    Deadline {
        run_id: uuid::Uuid,
        kind: crate::reply::DeadlineKind,
    },
}

impl HeapKind {
    fn sort_key(&self) -> (u8, String) {
        match self {
            Self::Fire { timer_id } => (0, timer_id.to_string()),
            Self::Deadline { run_id, .. } => (1, run_id.to_string()),
        }
    }
}

/// One slot in the near-horizon heap: earliest `fire_at` wins, the kind key
/// breaks ties so ordering is total and deterministic.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct HeapEntry {
    pub(super) fire_at: DateTime<Utc>,
    pub(super) kind: HeapKind,
}

impl HeapEntry {
    pub(super) fn fire(fire_at: DateTime<Utc>, timer_id: TimerId) -> Self {
        Self {
            fire_at,
            kind: HeapKind::Fire { timer_id },
        }
    }

    pub(super) fn deadline(
        wall_at: DateTime<Utc>,
        run_id: uuid::Uuid,
        kind: crate::reply::DeadlineKind,
    ) -> Self {
        Self {
            fire_at: wall_at,
            kind: HeapKind::Deadline { run_id, kind },
        }
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.fire_at
            .cmp(&other.fire_at)
            .then_with(|| self.kind.sort_key().cmp(&other.kind.sort_key()))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
