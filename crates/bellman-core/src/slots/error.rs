//! Slot-layer errors.

use std::path::PathBuf;

/// Result alias for slot operations.
pub type SlotResult<T> = Result<T, SlotError>;

/// Errors produced by the slot IPC layer.
#[derive(Debug)]
pub enum SlotError {
    /// Filesystem I/O.
    Io(String),
    /// Path is a symlink (rejected for safety).
    Symlink(PathBuf),
    /// Input exceeded the size cap.
    Oversized {
        /// The offending input.
        path: PathBuf,
        /// Its size in bytes.
        size: u64,
        /// The cap it exceeded. Rejected unread — an oversize file is never
        /// parsed, only measured.
        max: u64,
    },
    /// JSON parse / schema validation failure.
    Invalid(String),
    /// No free slot available for publish (should not happen if replenish holds).
    NoFreeSlot,
    /// Store-layer failure while applying a request.
    Store(String),
    /// Internal invariant break.
    Internal(String),
}

impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(m) => write!(f, "io: {m}"),
            Self::Symlink(p) => write!(f, "symlink rejected: {}", p.display()),
            Self::Oversized { path, size, max } => write!(
                f,
                "oversized input {}: {size} bytes (max {max})",
                path.display()
            ),
            Self::Invalid(m) => write!(f, "invalid slot input: {m}"),
            Self::NoFreeSlot => write!(f, "no free slot available"),
            Self::Store(m) => write!(f, "store: {m}"),
            Self::Internal(m) => write!(f, "internal: {m}"),
        }
    }
}

impl std::error::Error for SlotError {}

impl From<std::io::Error> for SlotError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<serde_json::Error> for SlotError {
    fn from(e: serde_json::Error) -> Self {
        Self::Invalid(e.to_string())
    }
}

impl From<crate::store::StoreError> for SlotError {
    fn from(e: crate::store::StoreError) -> Self {
        Self::Store(e.to_string())
    }
}
