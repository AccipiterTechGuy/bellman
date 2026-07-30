//! JSON slot IPC: `slots/{free,work,done,bad}/`.
//!
//! Integrators publish complete requests via temp-file + atomic same-dir rename
//! into `free/` (never edit a free stub in place). Bellman claims by renaming
//! into `work/`, answers into `done/`, and quarantines garbage into `bad/` with
//! a `.err.json` sidecar. `request_id` is the durable idempotency key.
//!
//! Watcher events are latency hints; the periodic rescan is the source of truth.
//! Free-stub count is an invariant: after every claim and on every sweep,
//! `count(empty free stubs) >= MIN_FREE_SLOTS` (default 5).

mod atomic;
mod envelope;
mod error;
mod layout;
mod payload;
mod service;
mod watcher;

pub use atomic::{
    atomic_write_bytes, atomic_write_json, read_capped, safe_child_path, DEFAULT_MAX_READ_BYTES,
};
pub use envelope::{
    SlotErrSidecar, SlotOperation, SlotPayload, SlotRequest, SlotResponse, SlotRunEvent,
    SlotStatus, SCHEMA_V1,
};
pub use error::{SlotError, SlotResult};
pub use layout::{
    parse_slot_id_from_name, SlotLayout, DEFAULT_DONE_RETENTION, DEFAULT_ORPHAN_AGE, MIN_FREE_SLOTS,
};
pub use service::{
    make_add_request, reserved_slot_id_from_path, response_is_ok, SlotConfig, SlotService,
};
pub use watcher::{
    poll_once, run_slot_loop, run_slot_loop_with_debounce, spawn_slot_thread, spawn_watch_thread,
    watch_free_dir, watch_free_dir_with_debounce, SlotWake, SlotWatcherStop, WatchConfig,
    WatchStop, DEFAULT_DEBOUNCE,
};

#[cfg(test)]
mod tests;
