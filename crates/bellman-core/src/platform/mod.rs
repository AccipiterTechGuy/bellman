//! Platform-specific support (wake-from-sleep RTC, …).
//!
//! See `docs/research/rtc_wake_synthesis.md` for the adopted design.

pub mod wake;

pub use wake::single_next_wake::WakeCandidate;
pub use wake::{
    create_wake, elect_next_wake, status_line, Caveat, DisabledReason, MachineWake, PowerEvent,
    PowerRail, SingleNextWake, WakeCapability, WakeError, WakeMechanism, ARM_SLACK_SECS,
};
