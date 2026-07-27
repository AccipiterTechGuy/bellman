//! Near-horizon heap scheduler with clock-jump misfire handling.
//!
//! Builds on [`crate::store`] (horizon query + claim ledger) and
//! [`crate::occurrence`] (lazy next-fire). See `docs/PLAN.md` and
//! `docs/BUILD_PLAN.md` for product rules.

mod action;
mod clock;
mod config;
mod engine;

pub use action::{FireAction, FireContext, FireKind, NopAction, RecordedFire, RecordingAction};
pub use clock::{Clock, MonoTime, SimulatedClock, SystemClock};
pub use config::{SchedulerConfig, HIGH_FREQ_PERIOD_SECS};
pub use engine::{
    ControlHandle, ControlMsg, DeliveredFire, Scheduler, SchedulerError, SchedulerResult,
    TickResult,
};

#[cfg(test)]
mod tests;
