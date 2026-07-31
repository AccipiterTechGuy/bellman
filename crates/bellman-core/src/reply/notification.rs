//! Fire notifications — one JSON per run under `<slots>/fires/`.
//!
//! A fire notification is written **after** both required run-file projections
//! (`status.json` and the reply stub) exist on disk. It never lives in
//! `slots/done/` (that path is owned by `SlotService` alone) and the file name
//! is per-run, so rewriting the same run's notification is idempotent.
//!
//! Carried schema `bellman-slot/1`; the top-level `kind` is the event kind
//! (`fired` / `fired_late` / `coalesced`), the occurrence type rides in
//! `occurrence_kind`. Both path fields are absolute native paths.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Schema tag carried by every fire notification.
pub const FIRE_SCHEMA_V1: &str = "bellman-slot/1";

/// Directory name for fire notifications inside the slots root.
pub const FIRES_DIR_NAME: &str = "fires";

/// `<slots>/fires` — the only directory fire notifications are written to.
pub fn fires_dir(slots_root: &Path) -> PathBuf {
    slots_root.join(FIRES_DIR_NAME)
}

/// Deterministic per-run file name: `fire-<full run_id>.json`.
pub fn fire_notification_name(run_id: Uuid) -> String {
    format!("fire-{run_id}.json")
}

/// Where an IPC client connects (IK6) — data, not code. Present on
/// `timer.json` and on fire notifications whenever the IPC server is up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcEndpoint {
    /// Absolute native path of the one Bellman socket.
    pub socket: PathBuf,
}

/// A fire notification published for one run of one timer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FireNotification {
    /// Carried schema tag; always [`FIRE_SCHEMA_V1`] for new writes.
    pub schema: String,
    /// Event kind: `fired` | `fired_late` | `coalesced`.
    pub kind: String,
    /// Occurrence type of the timer that produced this run.
    pub occurrence_kind: String,
    /// Timer that owns the run.
    pub timer_id: Uuid,
    /// Human-readable timer name at fire time.
    pub timer_name: String,
    /// The one configured consumer of this notification.
    pub app_name: String,
    /// The run this notification is for.
    pub run_id: Uuid,
    /// When the run was scheduled to fire (UTC).
    pub scheduled_for: DateTime<Utc>,
    /// When the run actually fired (UTC).
    pub fired_at: DateTime<Utc>,
    /// Absolute native path to the run's `status.json` projection.
    pub status_path: PathBuf,
    /// Absolute native path to the run's reply stub. **File transport only**
    /// (IK6): an IPC-only firing deliberately has no stub and omits the
    /// field — it never advertises a file Bellman did not create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_path: Option<PathBuf>,
    /// The one Bellman socket (IK6), present when the IPC server is up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipc: Option<IpcEndpoint>,
}

impl FireNotification {
    /// Build a notification carrying [`FIRE_SCHEMA_V1`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: &str,
        occurrence_kind: &str,
        timer_id: Uuid,
        timer_name: &str,
        app_name: &str,
        run_id: Uuid,
        scheduled_for: DateTime<Utc>,
        fired_at: DateTime<Utc>,
        status_path: PathBuf,
        reply_path: Option<PathBuf>,
        ipc: Option<IpcEndpoint>,
    ) -> Self {
        Self {
            schema: FIRE_SCHEMA_V1.to_string(),
            kind: kind.to_string(),
            occurrence_kind: occurrence_kind.to_string(),
            timer_id,
            timer_name: timer_name.to_string(),
            app_name: app_name.to_string(),
            run_id,
            scheduled_for,
            fired_at,
            status_path,
            reply_path,
            ipc,
        }
    }
}

/// Write `n` atomically (temp + rename) into `<slots>/fires/`, creating the dir.
///
/// A notification for the same `run_id` is replaced (idempotent rewrite is
/// fine — the name is per-run), but the notification is **never** written while
/// either required projection is missing: returns `InvalidInput` when
/// `status_path` or `reply_path` does not exist. This is the FILE adapter's
/// writer — a notification without a `reply_path` (IPC encoding, IK6) is not
/// eligible here by construction.
pub fn write_fire_notification(slots_root: &Path, n: &FireNotification) -> io::Result<PathBuf> {
    let reply_ok = n.reply_path.as_ref().map(|p| p.exists()).unwrap_or(false);
    if !n.status_path.exists() || !reply_ok {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "fire notification not eligible: status_path {} exists={} reply_path {:?} exists={}",
                n.status_path.display(),
                n.status_path.exists(),
                n.reply_path,
                reply_ok,
            ),
        ));
    }
    let dir = fires_dir(slots_root);
    let name = fire_notification_name(n.run_id);
    crate::slots::atomic_write_json(&dir, &name, n)
        .map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(slots_root: &Path) -> FireNotification {
        let status_path = slots_root.join("timers/t-bulb/status.json");
        let reply_path = slots_root.join(
            "timers/t-bulb/reply-9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08.json",
        );
        std::fs::create_dir_all(status_path.parent().unwrap()).unwrap();
        std::fs::write(&status_path, b"{}").unwrap();
        std::fs::write(&reply_path, b"{}").unwrap();
        FireNotification::new(
            "fired",
            "daily",
            Uuid::from_u128(0x3f1a),
            "bulb-test",
            "lightbulb",
            Uuid::from_u128(0x9f2c),
            DateTime::parse_from_rfc3339("2026-07-30T05:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2026-07-30T05:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
            status_path,
            Some(reply_path),
            None,
        )
    }

    #[test]
    fn name_is_deterministic_and_per_run() {
        let id = Uuid::from_u128(0x9f2c1d77_4e8a_4b02_9f61_77aa3e5c1d08);
        let a = fire_notification_name(id);
        let b = fire_notification_name(id);
        assert_eq!(a, b);
        assert_eq!(a, "fire-9f2c1d77-4e8a-4b02-9f61-77aa3e5c1d08.json");
    }

    #[test]
    fn fires_dir_is_under_slots_root() {
        let root = Path::new("/tmp/bellman-test-slots");
        assert_eq!(fires_dir(root), root.join("fires"));
    }

    #[test]
    fn write_roundtrip_parses_back() {
        let tmp = tempfile::tempdir().unwrap();
        let n = sample(tmp.path());
        let written = write_fire_notification(tmp.path(), &n).unwrap();
        assert_eq!(written, fires_dir(tmp.path()).join(fire_notification_name(n.run_id)));

        let bytes = std::fs::read(&written).unwrap();
        let parsed: FireNotification = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.schema, FIRE_SCHEMA_V1);
        assert_eq!(parsed.kind, "fired");
        assert_eq!(parsed.occurrence_kind, "daily");
        assert_eq!(parsed.timer_id, n.timer_id);
        assert_eq!(parsed.run_id, n.run_id);
        assert_eq!(parsed.scheduled_for, n.scheduled_for);
        assert_eq!(parsed.fired_at, n.fired_at);
        assert_eq!(parsed.status_path, n.status_path);
        assert_eq!(parsed.reply_path, n.reply_path);

        // Idempotent rewrite of the same run replaces the file.
        let rewritten = write_fire_notification(tmp.path(), &n).unwrap();
        assert_eq!(rewritten, written);
    }

    #[test]
    fn refuses_to_write_when_reply_path_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut n = sample(tmp.path());
        std::fs::remove_file(n.reply_path.as_ref().unwrap()).unwrap();
        let err = write_fire_notification(tmp.path(), &n).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!fires_dir(tmp.path()).join(fire_notification_name(n.run_id)).exists());

        // Same for a missing status_path.
        n.reply_path = Some(tmp.path().join("timers/t-bulb/reply-back.json"));
        std::fs::write(n.reply_path.as_ref().unwrap(), b"{}").unwrap();
        std::fs::remove_file(&n.status_path).unwrap();
        let err = write_fire_notification(tmp.path(), &n).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // And for an IPC-encoding notification (no reply_path at all, IK6).
        let mut n = sample(tmp.path());
        n.reply_path = None;
        let err = write_fire_notification(tmp.path(), &n).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
