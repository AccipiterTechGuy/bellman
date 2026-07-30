//! IK3 — the reply channel (`docs/todo/cards/IK3_reply_channel.md`).
//!
//! One writer per file: Bellman writes `status.json`, the app writes
//! `reply-<run_id>.json`. Bellman pre-creates the stub pre-filled at fire,
//! watches it, validates, logs the transition, and folds the result into
//! `status.json` — the mirror that must always show the truth right now.
//!
//! - [`document`] — the `bellman-reply/1` shape, caps and stub content.
//! - [`engine`] — the transport-agnostic ingest: validation, transitions,
//!   event-log lines, `status.json` folding, pickup/watchdog deadlines.
//!   Does not know a file exists.
//! - [`watcher`] — the file transport: safe reads, debounce, quarantine,
//!   the pre-fire barrier, startup scan, reconciler, background thread.
//! - [`gate`] — the R10 interprocess per-timer lock shards.
//! - [`quarantine`] — the copy-only, idempotent `bad/` for rejected replies.
//! - [`notification`] — the `slots/fires/` fire notification with `reply_path`.

mod document;
mod engine;
pub mod gate;
mod notification;
pub mod quarantine;
mod watcher;

pub use document::{
    cap_result, stub_bytes, truncate_text, ReplyDocument, ReplyRejection, MAX_FREE_TEXT_BYTES,
    MAX_REPLY_FILE_BYTES, MAX_RESULT_EVENT_BYTES, MAX_RESULT_STATUS_BYTES, REPLY_SCHEMA_V1,
};
pub use engine::{
    current_claim, new_anchors, IngestOutcome, ReplyEngine, ReplyError, ReplyResult, SharedAnchors,
};
pub use notification::{
    fire_notification_name, fires_dir, write_fire_notification, FireNotification,
    FIRES_DIR_NAME, FIRE_SCHEMA_V1,
};
pub use watcher::{
    barrier_ingest, poll_once, publish_fire_notification, reconcile, spawn_reply_thread,
    startup_scan, InvalidTracker, PollStats, ReplyWatcherStop, DEFAULT_DEBOUNCE,
    DEFAULT_POLL_INTERVAL,
};

#[cfg(test)]
mod tests;
