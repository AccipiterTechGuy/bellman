//! DST and invalid-month-day policies for wall-clock schedules.

use serde::{Deserialize, Serialize};

/// What to do when a wall-clock local time falls in a DST spring-forward gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DstGapPolicy {
    /// Use the first valid local instant after the gap (default).
    #[default]
    FirstValidAfterGap,
}

/// What to do when a wall-clock local time falls in a DST fall-back fold
/// (the same civil time occurs twice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DstFoldPolicy {
    /// Fire once, at the first (earliest) of the two ambiguous instants (default).
    #[default]
    FirstOccurrence,
}

/// What to do when a schedule names a day that does not exist in the target month
/// (e.g. day 31 in April, or Feb 29 in a non-leap year).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InvalidMonthDayPolicy {
    /// Clamp to the last valid day of that month (default).
    #[default]
    Clamp,
}
