//! Bellman core — scheduling engine library.
//!
//! Phase P0: occurrence engine + persistent timer store. Later phases add
//! scheduler, slots, events, actions, and platform wake support under this crate.

pub mod occurrence;
pub mod store;

pub use occurrence::{
    DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy, Occurrence, OccurrenceKind, Weekdays,
};
pub use store::{
    Action, ClaimStatus, MisfirePolicy, NewTimer, OpenOptions, OverlapPolicy, RetryPolicy,
    RunClaim, Store, StoreError, StoreResult, Timer, TimerId, TimerPatch, TimerUpdate,
};
