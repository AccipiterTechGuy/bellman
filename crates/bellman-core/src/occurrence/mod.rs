//! Occurrence engine: lazy `next_fire` over calendar and elapsed-time schedules.
//!
//! Policies (defaults match the build plan):
//! - DST spring-forward gap → first valid instant after the gap
//! - DST fall-back fold → fire once at the first (earliest) occurrence
//! - Invalid month-day (31st / Feb 29) → clamp to last valid day of that month
//! - Interval timers anchor to UTC elapsed time, never wall-clock

mod civil;
mod kind;
mod policy;
mod schedule;

pub use kind::{OccurrenceKind, Weekdays};
pub use policy::{DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy};
pub use schedule::{parse_weekdays, Occurrence};

#[cfg(test)]
mod golden_tests;
