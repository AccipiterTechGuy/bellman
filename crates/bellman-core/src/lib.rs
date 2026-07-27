//! Bellman core — scheduling engine library.
//!
//! Phase P0 exposes the occurrence engine. Later phases add store, scheduler,
//! slots, events, actions, and platform wake support under this crate.

pub mod occurrence;

pub use occurrence::{
    DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy, Occurrence, OccurrenceKind, Weekdays,
};
