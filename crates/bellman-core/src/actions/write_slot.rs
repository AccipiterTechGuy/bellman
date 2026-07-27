//! Write-output-slot wake action: atomic JSON publish of the fire notification.

use crate::slots::atomic_write_json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Payload written when the wake action is "write output slot".
#[derive(Debug, Clone, Serialize)]
pub struct WriteSlotPayload {
    pub schema: &'static str,
    pub timer_id: Uuid,
    pub timer_name: String,
    pub run_id: Uuid,
    pub scheduled_for: DateTime<Utc>,
    pub fired_at: DateTime<Utc>,
    pub kind: String,
}

/// Write a fire-notification JSON into `dir/file_name` via temp+rename.
///
/// `dir` is created if missing. Returns the final path.
pub fn write_output_slot(
    dir: impl AsRef<Path>,
    file_name: &str,
    payload: &WriteSlotPayload,
) -> Result<PathBuf, String> {
    let dir = dir.as_ref();
    atomic_write_json(dir, file_name, payload).map_err(|e| e.to_string())
}
