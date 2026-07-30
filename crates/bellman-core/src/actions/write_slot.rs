//! Write-output-slot wake action: atomic JSON publish of the fire notification.

use crate::slots::atomic_write_json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Wire schema identifier carried by every fire notification (R1).
pub const FIRE_SCHEMA_V1: &str = "bellman-fire/1";

/// Payload written when the wake action is "write output slot".
///
/// Shape rules (docs/todo/json_normalization.md):
/// - R1: every JSON carries `schema` (`bellman-fire/1`).
/// - R2: top-level `kind` is the **event kind** from the R5 vocabulary
///   (`fired` / `fired_late` / `coalesced`); the occurrence type of this
///   firing rides in `occurrence_kind`.
/// - R3: every timestamp ends `_at`, except `scheduled_for` (an intent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteSlotPayload {
    pub schema: String,
    /// Event kind (R5 vocabulary): `fired` | `fired_late` | `coalesced`.
    pub kind: String,
    pub timer_id: Uuid,
    pub timer_name: String,
    pub run_id: Uuid,
    pub scheduled_for: DateTime<Utc>,
    pub fired_at: DateTime<Utc>,
    /// Occurrence type of this firing: `on_time` | `late` | `coalesced` |
    /// `catch_up_<n>`.
    pub occurrence_kind: String,
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
