//! Bellman core — scheduling engine library.
//!
//! Phase P0: occurrence engine + persistent timer store + near-horizon scheduler.
//! Later phases add slots, events, actions, and platform wake support under this crate.

pub mod occurrence;
pub mod scheduler;
pub mod store;

pub use occurrence::{
    DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy, Occurrence, OccurrenceKind, Weekdays,
};
pub use scheduler::{
    Clock, ControlHandle, ControlMsg, DeliveredFire, FireAction, FireContext, FireKind,
    NopAction, RecordedFire, RecordingAction, Scheduler, SchedulerConfig, SchedulerError,
    SchedulerResult, SimulatedClock, SystemClock, TickResult, HIGH_FREQ_PERIOD_SECS,
};
pub use store::{
    Action, ClaimStatus, MisfirePolicy, NewTimer, OpenOptions, OverlapPolicy, RetryPolicy,
    RunClaim, Store, StoreError, StoreResult, Timer, TimerId, TimerPatch, TimerUpdate,
};
