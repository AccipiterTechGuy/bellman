//! Append-only JSONL event log (`logs/events.current.jsonl`).
//!
//! One self-contained JSON line per lifecycle event. Weekly rotation is an
//! atomic rename into `logs/archive/events-<ISO-week>.jsonl` plus a fresh
//! current file. Readers skip unparseable lines (torn tails after a crash)
//! and report the skip count. No per-line fsync — flush on write, sync on
//! rotate. Retention deletes archive files older than the configured window
//! (product default: 30 days).

mod log;
mod reader;
mod record;

pub use log::{EventLog, EventLogConfig, EventLogError, EventLogResult, CURRENT_FILE_NAME};
pub use reader::{read_events, read_events_from, ReadStats};
pub use record::{EventKind, EventRecord, EVENT_SCHEMA_V1};

#[cfg(test)]
mod tests;
