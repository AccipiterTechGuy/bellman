//! Append-only JSONL event log (`logs/events.current.jsonl`).
//!
//! One self-contained JSON line per lifecycle event. R11: producers never
//! append directly — they enqueue into the SQLite outbox
//! (`Store::enqueue_event`), and one publisher elected by an OS file lock
//! ([`EventPublisher`]) appends + fdatasyncs + rotates through a durable
//! journal. Rotation triggers at the ISO-week boundary or before an append
//! would take current past the configured size cap (default 64 MiB). Readers
//! skip unparseable lines (torn tails after a crash), transparently read
//! both plain and compressed archives, and dedupe by `event_id` (delivery is
//! at-least-once). Retention deletes archives older than the configured
//! window (default 30 days), then oldest archives until current + archives
//! fit the retained-log budget (default 1 GiB); the live current file is
//! never deleted.

mod log;
mod publisher;
mod reader;
mod record;

pub use log::{
    EventLog, EventLogConfig, EventLogError, EventLogResult, RetainReport, CURRENT_FILE_NAME,
    DEFAULT_BUDGET_BYTES, DEFAULT_MAX_CURRENT_BYTES, DEFAULT_RETENTION_DAYS,
};
pub use publisher::{
    EventPublisher, PublishReport, PublisherHealth, HEALTH_FILE_NAME, HEALTH_SCHEMA_V1,
    PUBLISHER_LEASE_NAME, ROTATING_FILE_NAME,
};
pub use reader::{read_events, read_events_from, ReadStats};
pub use record::{EventRecord, RunState, EVENT_SCHEMA_V1};

#[cfg(test)]
mod tests;
