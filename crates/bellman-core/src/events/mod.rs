//! Append-only JSONL event log (`logs/events.current.jsonl`).
//!
//! One self-contained JSON line per lifecycle event. Rotation is a rename
//! into `logs/archive/` followed by gzip compression
//! (`events-<ISO-week>.jsonl.gz`); it triggers at the ISO-week boundary or
//! before an append would take current past the configured size cap (default
//! 64 MiB). Readers skip unparseable lines (torn tails after a crash) and
//! transparently read both plain and compressed archives. No per-line fsync —
//! flush on write, sync on rotate. Retention deletes archives older than the
//! configured window (default 30 days), then oldest archives until current +
//! archives fit the retained-log budget (default 1 GiB); the live current
//! file is never deleted.

mod log;
mod reader;
mod record;

pub use log::{
    EventLog, EventLogConfig, EventLogError, EventLogResult, RetainReport, CURRENT_FILE_NAME,
    DEFAULT_BUDGET_BYTES, DEFAULT_MAX_CURRENT_BYTES,
};
pub use reader::{read_events, read_events_from, ReadStats};
pub use record::{EventRecord, RunState, EVENT_SCHEMA_V1};

#[cfg(test)]
mod tests;
