//! Store error types.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Result alias for store operations.
pub type StoreResult<T> = Result<T, StoreError>;

/// Errors produced by the timer store.
#[derive(Debug)]
pub enum StoreError {
    /// Underlying rusqlite / SQLite failure.
    Sqlite(String),
    /// Filesystem I/O outside SQLite.
    Io(String),
    /// Database path appears to live on a network filesystem.
    NetworkFilesystem(String),
    /// Timer id not found.
    NotFound(Uuid),
    /// Optimistic concurrency failure.
    StaleRevision {
        id: Uuid,
        expected: i64,
        actual: i64,
    },
    /// Claim ledger already has `(timer_id, scheduled_for)`.
    AlreadyClaimed {
        timer_id: Uuid,
        scheduled_for: DateTime<Utc>,
    },
    /// Run claim id not found (or not in claimed state for complete).
    RunNotFound(Uuid),
    /// Occurrence kind failed validation.
    InvalidOccurrence(String),
    /// JSON (de)serialization.
    Serde(String),
    /// Unexpected internal invariant break.
    Internal(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(m) => write!(f, "sqlite: {m}"),
            Self::Io(m) => write!(f, "io: {m}"),
            Self::NetworkFilesystem(m) => write!(f, "network filesystem refused: {m}"),
            Self::NotFound(id) => write!(f, "timer not found: {id}"),
            Self::StaleRevision {
                id,
                expected,
                actual,
            } => write!(
                f,
                "stale revision for {id}: expected {expected}, actual {actual}"
            ),
            Self::AlreadyClaimed {
                timer_id,
                scheduled_for,
            } => write!(
                f,
                "run already claimed for timer {timer_id} at {scheduled_for}"
            ),
            Self::RunNotFound(id) => write!(f, "run not found: {id}"),
            Self::InvalidOccurrence(m) => write!(f, "invalid occurrence: {m}"),
            Self::Serde(m) => write!(f, "serde: {m}"),
            Self::Internal(m) => write!(f, "internal: {m}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e.to_string())
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e.to_string())
    }
}
