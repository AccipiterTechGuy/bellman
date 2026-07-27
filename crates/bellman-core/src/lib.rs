//! Bellman core — scheduling engine library.
//!
//! Phase P0–P2: occurrence engine + persistent timer store + near-horizon
//! scheduler + JSON slot IPC. Later phases add events, actions, and platform
//! wake support under this crate.

pub mod occurrence;
pub mod scheduler;
pub mod slots;
pub mod store;

pub use occurrence::{
    DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy, Occurrence, OccurrenceKind, Weekdays,
};
pub use scheduler::{
    Clock, ControlHandle, ControlMsg, DeliveredFire, FireAction, FireContext, FireKind, NopAction,
    RecordedFire, RecordingAction, Scheduler, SchedulerConfig, SchedulerError, SchedulerResult,
    SimulatedClock, SystemClock, TickResult, HIGH_FREQ_PERIOD_SECS,
};
pub use slots::{
    atomic_write_json, make_add_request, poll_once, SlotConfig, SlotError, SlotLayout,
    SlotOperation, SlotRequest, SlotResponse, SlotResult, SlotService, SlotStatus, MIN_FREE_SLOTS,
    SCHEMA_V1,
};
pub use store::{
    Action, ClaimStatus, MisfirePolicy, NewTimer, OpenOptions, OverlapPolicy, RetryPolicy,
    RunClaim, SlotRequestRecord, Store, StoreError, StoreResult, Timer, TimerId, TimerPatch,
    TimerUpdate,
};
