//! Bellman core — scheduling engine library.
//!
//! Phase P0–P2: occurrence engine + persistent timer store + near-horizon
//! scheduler + JSON slot IPC + event log + wake actions. Later phases add
//! platform wake support under this crate.

pub mod actions;
pub mod events;
pub mod occurrence;
pub mod scheduler;
pub mod service;
pub mod slots;
pub mod store;

pub use actions::{
    notify_stub, run_launch, write_output_slot, ActionRunner, ActionRunnerConfig, LaunchConfig,
    LaunchOutcome, NotifyOutcome, NotifySink, StubNotifySink, WriteSlotPayload,
    DEFAULT_OUTPUT_CAP_BYTES,
};
pub use events::{
    read_events, EventKind, EventLog, EventLogConfig, EventLogError, EventRecord, ReadStats,
    CURRENT_FILE_NAME,
};
pub use occurrence::{
    DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy, Occurrence, OccurrenceKind, Weekdays,
};
pub use scheduler::{
    Clock, ControlHandle, ControlMsg, DeliveredFire, FireAction, FireContext, FireKind, NopAction,
    RecordedFire, RecordingAction, Scheduler, SchedulerConfig, SchedulerError, SchedulerResult,
    SimulatedClock, SystemClock, TickResult, HIGH_FREQ_PERIOD_SECS,
};
pub use service::log_query::{current_log_path, read_log_tail, LogPath};
pub use service::run_now::{
    open_store, publish_fire_slot_response, resolve_logs_dir, resolve_slots_root_optional,
    run_now, slot_record_for_timer, RunNowError, RunNowOptions, RunNowOutcome,
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
