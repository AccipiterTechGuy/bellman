//! Bellman core — scheduling engine library.
//!
//! Phase P0–P5: occurrence engine + persistent timer store + near-horizon
//! scheduler + JSON slot IPC + event log + wake actions + weekly prune +
//! year recalibration + concurrency backpressure + config.json. Later phases
//! add platform wake support under this crate.

pub mod actions;
pub mod app_config;
pub mod calendar;
pub mod events;
pub mod ipc;
pub mod occurrence;
pub mod platform;
pub mod pruner;
pub mod scheduler;
pub mod service;
pub mod slots;
pub mod store;
pub mod tree;
pub mod visible;

pub use actions::{
    notify_stub, run_launch, run_parallel_under_cap, ActionExecutor, ActionLimiter,
    CancellationToken, Dispatcher, DispatcherConfig, ExecOutcome, ExecutorConfig, LaunchConfig,
    LaunchOutcome, LimiterStats, NotifyOutcome, NotifySink, StubNotifySink,
    DEFAULT_OUTPUT_CAP_BYTES,
};
pub use app_config::{
    config_path, AppConfig, DEFAULT_ACK_GRACE_SECS, DEFAULT_HORIZON_SECS,
    DEFAULT_MAX_CONCURRENT_ACTIONS, DEFAULT_MIN_FREE_SLOTS, DEFAULT_RETENTION_DAYS,
};
pub use events::{
    read_events, EventLog, EventLogConfig, EventLogError, EventRecord, ReadStats, RunState,
    CURRENT_FILE_NAME,
};
pub use occurrence::{
    DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy, Occurrence, OccurrenceKind, Weekdays,
};
pub use platform::{
    create_wake, elect_next_wake, status_line, Caveat, DisabledReason, MachineWake, PowerEvent,
    PowerRail, SingleNextWake, WakeCandidate, WakeCapability, WakeError, WakeMechanism,
    ARM_SLACK_SECS,
};
pub use pruner::{
    ensure_system_prune_timer, is_system_prune_timer, needs_year_recalibration, prune_is_due,
    run_prune, run_prune_under, run_year_recalibration, startup_maintenance, system_prune_id,
    year_start, PruneConfig, PruneError, PruneReport, YearRecalibrateReport, SYSTEM_PRUNE_NAME,
};
pub use scheduler::{
    apply_execution_jitter, jitter_offset_secs, Clock, ControlHandle, ControlMsg, DeliveredFire,
    FireAction, FireContext, FireKind, NopAction, RecordedFire, RecordingAction, Scheduler,
    SchedulerConfig, SchedulerError, SchedulerResult, SimulatedClock, SystemClock, TickResult,
    HIGH_FREQ_PERIOD_SECS,
};
pub use service::log_query::{
    current_log_path, logs_dir_from_data, read_log_history, read_log_tail, LogPath,
};
pub use service::run_now::{
    open_store, resolve_logs_dir, resolve_slots_root_optional, run_now, slot_record_for_timer,
    RunNowError, RunNowOptions, RunNowOutcome,
};
pub use slots::{
    atomic_write_json, make_add_request, poll_once, SlotConfig, SlotError, SlotLayout,
    SlotOperation, SlotRequest, SlotResponse, SlotResult, SlotService, SlotStatus, MIN_FREE_SLOTS,
    SCHEMA_V1,
};
pub use store::{
    Action, ClaimStatus, FailureKind, Meta, MisfirePolicy, NewTimer, OpenOptions, OverlapPolicy,
    RetryPolicy, RunClaim, RunStateRow, SlotRequestRecord, Store, StoreError, StoreResult, Timer,
    TimerId, TimerPatch, TimerUpdate, TransportMode,
};
pub mod reply;
pub use calendar::{
    build_snapshot, build_truth_window, local_date, month_bounds, parse_date, parse_tz, render_svg,
    resolve_day_phrase, resolve_month, snapshot_month_from_store, svg_to_png, system_tz_name,
    tasks_from_store, CalendarBuildOptions, CalendarCaps, CalendarDay, CalendarEntry,
    CalendarFormat, CalendarSnapshot, CalendarStatus, ExpandableTask, OutcomeLabel,
    TruthBuildOptions, TruthEntry, TruthSource, TruthWindow, WeekStart, MONTH_NAMES,
};
pub use reply::{
    new_anchors, IngestOutcome, ReplyDocument, ReplyEngine, ReplyError, ReplyRejection,
    ReplyResult, SharedAnchors, REPLY_SCHEMA_V1,
};
pub use tree::{
    folder_name, log_cancelled_for_open_runs, project_fire, reconcile_folders, reply_file_name,
    short_id, slugify, RunStatus, TimersTree, TreeError, TreeResult, README_FILE_NAME,
    RUN_SCHEMA_V1, STATUS_FILE_NAME, TIMER_FILE_NAME, TIMER_SCHEMA_V1, TREE_DIR_NAME,
};
pub use visible::{
    default_backup_dir, default_snapshot_path, diff_scans, disable_task, edit_task, enable_task,
    find_task, load_snapshot, new_cron_task, outcome_to_last_result, platform_name,
    refuse_system_write, run_task, save_snapshot, scan, timer_logs, DiscoveredTask, LastResult,
    RunOutcome, ScanDiff, ScanResult, SourceFilter, SourceKind, TaskChange, TaskId, WritePlan,
};
