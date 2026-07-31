//! Slot service: claim free requests, apply against the store, write done/.

use super::atomic::{atomic_write_json, read_capped, refuse_symlink, DEFAULT_MAX_READ_BYTES};
use super::envelope::{
    SlotOperation, SlotPayload, SlotRequest, SlotResponse, SlotRunEvent, SlotStatus,
};
use super::error::{SlotError, SlotResult};
use super::layout::{SlotLayout, DEFAULT_DONE_RETENTION, DEFAULT_ORPHAN_AGE, MIN_FREE_SLOTS};
use super::payload::{new_timer_from_payload, patch_from_payload};
use crate::reply::RunDb;
use crate::store::{SlotRequestRecord, Store, TimerId, TimerUpdate};
use chrono::Utc;
use rusqlite::Transaction;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// Configuration for the slot service.
#[derive(Debug, Clone)]
pub struct SlotConfig {
    /// Floor of empty free stubs (product default: 5).
    pub min_free: usize,
    /// Max bytes accepted when reading a free/work slot file.
    pub max_read_bytes: u64,
    /// Age after which `work/` claims are re-processed (orphan sweep).
    pub orphan_age: Duration,
    /// Retention for `done/` files before GC.
    pub done_retention: Duration,
    /// Max un-acked run events to embed in an output response.
    pub max_events: usize,
}

impl Default for SlotConfig {
    fn default() -> Self {
        Self {
            min_free: MIN_FREE_SLOTS,
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
            orphan_age: DEFAULT_ORPHAN_AGE,
            done_retention: DEFAULT_DONE_RETENTION,
            max_events: 64,
        }
    }
}

/// Processes slot requests against a [`Store`].
///
/// Lifecycle: producer publishes into `free/` → service claims into `work/` →
/// answers into `done/` (or quarantines into `bad/`). Replenish after every
/// claim; periodic rescan is the source of truth (watcher is a latency hint).
pub struct SlotService {
    layout: SlotLayout,
    config: SlotConfig,
    /// IK2: per-timer folder tree root (view). When set, add/modify/delete
    /// requests project their folder/`timer.json` after the store transaction
    /// commits — post-commit, never inside it.
    timers_tree: Option<crate::tree::TimersTree>,
}

impl SlotService {
    /// Open (or create) the slots root and replenish free stubs to the floor.
    pub fn open(root: impl AsRef<Path>, config: SlotConfig) -> SlotResult<Self> {
        let layout = SlotLayout::open_with(root, config.min_free)?;
        Ok(Self {
            layout,
            config,
            timers_tree: None,
        })
    }

    /// Attach the per-timer folder tree so slot add/modify/delete also
    /// project `<data_dir>/timers/<slug>-<id>/` (IK2).
    pub fn with_timers_tree(mut self, tree: crate::tree::TimersTree) -> Self {
        self.timers_tree = Some(tree);
        self
    }

    pub fn layout(&self) -> &SlotLayout {
        &self.layout
    }

    pub fn config(&self) -> &SlotConfig {
        &self.config
    }

    pub fn free_count(&self) -> SlotResult<usize> {
        self.layout.free_count()
    }

    /// Publish a filled request into a free slot (producer helper).
    ///
    /// Picks an empty free stub via exclusive rename (so concurrent producers
    /// never share a slot), writes the complete request via temp+rename back
    /// into `free/`, and returns the final path.
    pub fn publish(&self, mut request: SlotRequest) -> SlotResult<PathBuf> {
        if request
            .request_id
            .as_ref()
            .is_none_or(std::string::String::is_empty)
        {
            request.request_id = Some(Uuid::new_v4().to_string());
        }
        if request.operation.is_none() {
            return Err(SlotError::Invalid(
                "publish requires operation (add|modify|delete)".into(),
            ));
        }
        if request.logged_at.is_none() {
            request.logged_at = Some(Utc::now());
        }
        if request.schema.is_empty() {
            request.schema = super::envelope::SCHEMA_V1.to_string();
        }

        // Exclusive claim: rename free stub → free/.claim-<uuid>-slot-N.json so
        // two producers cannot overwrite the same reserved id.
        let free_files = self.layout.list_free_files()?;
        let mut claimed: Option<(PathBuf, String, String)> = None; // claim_path, slot_id, final_name
        for path in free_files {
            if refuse_symlink(&path).is_err() {
                continue;
            }
            let bytes = match read_capped(&path, self.config.max_read_bytes) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let existing: SlotRequest = match serde_json::from_slice(&bytes) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if !existing.is_free_stub() {
                continue;
            }
            let final_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("slot-unknown.json")
                .to_string();
            let claim_name = format!(".claim-{}-{}", Uuid::new_v4(), final_name);
            let claim_path = self.layout.free_dir().join(&claim_name);
            match std::fs::rename(&path, &claim_path) {
                Ok(()) => {
                    claimed = Some((claim_path, existing.slot_id, final_name));
                    break;
                }
                Err(_) => continue, // lost race — try next stub
            }
        }
        let (claim_path, slot_id, final_name) = claimed.ok_or(SlotError::NoFreeSlot)?;
        request.slot_id = slot_id;

        // Write the complete request onto the reserved final name.
        let write_result = atomic_write_json(&self.layout.free_dir(), &final_name, &request);
        // Drop the claim temp regardless.
        let _ = std::fs::remove_file(&claim_path);
        write_result?;

        // Replenish so free stubs stay at floor even while this filled request sits in free/.
        let _ = self.layout.replenish()?;
        Ok(self.layout.free_dir().join(final_name))
    }

    /// One full scan pass: claim filled free slots, process work, sweep, GC, replenish.
    ///
    /// Returns the number of requests processed (ok or error responses written).
    pub fn poll(&self, store: &mut Store) -> SlotResult<usize> {
        let mut processed = 0usize;

        // 1. Claim filled free → work.
        for free_path in self.layout.list_free_files()? {
            match self.try_claim_and_queue(&free_path) {
                Ok(true) => {}
                Ok(false) => {}
                Err(e) => {
                    // Quarantine unreadable / symlink / oversized free inputs.
                    let reason = e.to_string();
                    let _ = self.layout.quarantine(&free_path, &reason, None);
                    let _ = self.layout.replenish()?;
                }
            }
        }

        // 2. Process everything currently in work/ (including orphans).
        for work_path in self.layout.list_work_files()? {
            match self.process_work_file(&work_path, store) {
                Ok(()) => processed += 1,
                Err(e) => {
                    let reason = e.to_string();
                    let _ = self.layout.quarantine(&work_path, &reason, None);
                    let _ = self.layout.replenish()?;
                }
            }
        }

        // 3. Orphan sweep is covered by step 2 (work/ is reprocessed). Files
        // stuck past orphan_age that fail again go to bad/ above.

        // 4. GC done/ after retention.
        let _ = self.layout.gc_done(self.config.done_retention)?;

        // 5. Replenish free stubs (invariant).
        let _ = self.layout.replenish()?;

        Ok(processed)
    }

    /// Claim a free file into work if it is a filled request.
    /// Returns Ok(true) if claimed, Ok(false) if stub / lost race / not a request.
    fn try_claim_and_queue(&self, free_path: &Path) -> SlotResult<bool> {
        refuse_symlink(free_path)?;
        let bytes = read_capped(free_path, self.config.max_read_bytes)?;
        let req: SlotRequest = serde_json::from_slice(&bytes)
            .map_err(|e| SlotError::Invalid(format!("parse {}: {e}", free_path.display())))?;
        if req.is_free_stub() {
            return Ok(false);
        }
        if !req.schema_supported() {
            return Err(SlotError::Invalid(format!(
                "unsupported schema '{}'",
                req.schema
            )));
        }
        match self.layout.claim_file(free_path)? {
            Some(_) => {
                let _ = self.layout.replenish()?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// IK3: after a slot request advanced the `ack_through` cursor, satisfy
    /// pickup for the current run (or revise its provisional `no_ack`).
    /// Best-effort: a failure here is retried by the reply watcher's
    /// pickup-expiry pass, which also consults the cursor.
    fn reply_ack_hook(store: &Store, data_dir: &Path, timer_id: Uuid, through: u64) {
        let Ok(Some(timer)) = store.get_timer(timer_id) else {
            return;
        };
        let app_cfg = crate::app_config::AppConfig::load(data_dir).unwrap_or_default();
        let engine = crate::reply::ReplyEngine {
            tree: crate::tree::TimersTree::new(data_dir),
            data_dir: data_dir.to_path_buf(),
            pickup_grace: app_cfg.pickup_grace(),
            watchdog_factor: app_cfg.watchdog_factor,
            anchors: crate::reply::new_anchors(),
            deadlines: crate::reply::new_deadlines(),
            fire_slot_file: None,
        };
        let Ok(_gate) = crate::reply::gate::acquire(data_dir, timer_id) else {
            return;
        };
        match engine.on_ack_through(store, &timer, through, Utc::now()) {
            Ok(true) => {
                if let Some(row) = store.current_run_state(timer_id).ok().flatten() {
                    if let Err(e) = engine.project_status(store, &timer, &row.run_id) {
                        eprintln!("bellman: ack_through status projection failed: {e}");
                    }
                }
            }
            Ok(false) => {}
            Err(e) => eprintln!("bellman: ack_through reply hook failed: {e}"),
        }
    }

    fn process_work_file(&self, work_path: &Path, store: &mut Store) -> SlotResult<()> {
        refuse_symlink(work_path)?;
        let bytes = read_capped(work_path, self.config.max_read_bytes)?;
        let mut req: SlotRequest = serde_json::from_slice(&bytes)
            .map_err(|e| SlotError::Invalid(format!("parse work {}: {e}", work_path.display())))?;

        if req.is_free_stub() {
            // Should not be in work/; move to bad.
            return Err(SlotError::Invalid(
                "empty stub found in work/ (not a request)".into(),
            ));
        }

        // Trusted identity comes from the claimed filename only — never from the
        // envelope. Path traversal / cross-slot forge via slot_id is rejected.
        let reserved_id = reserved_slot_id_from_path(work_path)?;
        if req.slot_id != reserved_id {
            return Err(SlotError::Invalid(format!(
                "envelope slot_id {:?} does not match reserved id {:?} from filename",
                req.slot_id, reserved_id
            )));
        }
        // Force trusted id for all downstream writes (defense in depth).
        req.slot_id = reserved_id.clone();

        let request_id = req
            .request_id
            .clone()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SlotError::Invalid("missing request_id".into()))?;
        let operation = req
            .operation
            .ok_or_else(|| SlotError::Invalid("missing operation".into()))?;
        let max_events = self.config.max_events;

        // R10: for delete, snapshot the timer row pre-commit AND acquire the
        // per-timer gate BEFORE the delete transaction — required, not
        // optional; a lock failure aborts the request.
        let pre_delete_timer = if self.timers_tree.is_some() && operation == SlotOperation::Delete {
            req.payload
                .as_ref()
                .and_then(|p| serde_json::from_value::<SlotPayload>(p.clone()).ok())
                .and_then(|p| p.resolved_timer_id())
                .and_then(|tid| store.get_timer(tid).ok().flatten())
        } else {
            None
        };
        let _delete_gate = if let (SlotOperation::Delete, Some(timer)) =
            (operation, pre_delete_timer.as_ref())
        {
            let data_dir = store
                .path()
                .parent()
                .map(|p| p.to_path_buf())
                .ok_or_else(|| SlotError::Internal("store has no data dir".into()))?;
            Some(
                crate::reply::gate::acquire(&data_dir, timer.id)
                    .map_err(|e| SlotError::Internal(format!("per-timer gate: {e}")))?,
            )
        } else {
            None
        };

        // Single Immediate transaction: check ledger / apply mutations / write
        // response. Concurrent consumers serialize; crash mid-apply rolls back
        // so a resubmit never double-mutates.
        let rec = store.slot_execute_once(&request_id, |tx| {
            let response =
                match apply_request_tx(tx, &req, operation, &request_id, max_events) {
                    Ok(resp) => resp,
                    Err(SlotError::Invalid(msg)) => {
                        // Logical errors are durable results (idempotent too).
                        SlotResponse::err(&reserved_id, &request_id, msg)
                    }
                    Err(e) => {
                        return Err(crate::store::StoreError::Internal(e.to_string()));
                    }
                };
            let app_name = req
                .payload
                .as_ref()
                .and_then(|p| p.get("app_name"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Ok(SlotRequestRecord {
                request_id: request_id.clone(),
                slot_id: reserved_id.clone(),
                operation: operation.as_str().to_string(),
                app_name,
                timer_id: response.timer_id,
                status: response.status.as_str().to_string(),
                response_json: serde_json::to_string(&response).map_err(|e| {
                    crate::store::StoreError::Serde(format!("serialize response: {e}"))
                })?,
                created_at: Utc::now(),
            })
        })?;

        let response: SlotResponse = serde_json::from_str(&rec.response_json)
            .map_err(|e| SlotError::Internal(format!("stored response corrupt: {e}")))?;

        // IK3: the slot feed is a durable acknowledgement path that predates
        // the reply channel — a cursor advance past the current run satisfies
        // pickup (and revises a provisional `no_ack`) while that run is
        // current. Post-commit, best-effort like the other view projections.
        if response.status == SlotStatus::Ok {
            if let (Some(tree), Some(payload_v)) = (&self.timers_tree, req.payload.as_ref()) {
                if let Ok(payload) = serde_json::from_value::<SlotPayload>(payload_v.clone()) {
                    if let (Some(ack), Some(tid)) =
                        (payload.ack_through, payload.resolved_timer_id())
                    {
                        if let Some(data_dir) = tree.root().parent().map(|p| p.to_path_buf()) {
                            Self::reply_ack_hook(store, &data_dir, tid, ack);
                        }
                    }
                }
            }
        }

        // IK2: project the folder tree post-commit (view only — never fails
        // the request).
        if let Some(tree) = &self.timers_tree {
            if response.status == SlotStatus::Ok {
                match operation {
                    SlotOperation::Add | SlotOperation::Modify => {
                        if let Some(tid) = response.timer_id {
                            match store.get_timer(tid) {
                                Ok(Some(timer)) => {
                                    let owner = store.get_timer_owner(tid).ok().flatten();
                                    if let Err(e) =
                                        tree.sync_timer_json(&timer, owner.as_deref())
                                    {
                                        eprintln!("bellman: timer folder sync failed: {e}");
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => eprintln!("bellman: timer folder sync lookup: {e}"),
                            }
                        }
                    }
                    SlotOperation::Delete => {
                        // The `cancelled` transition committed inside the
                        // delete transaction (see apply_request_tx); only the
                        // folder removal remains, after the commit.
                        if let Some(timer) = pre_delete_timer.as_ref() {
                            if let Err(e) = tree.remove_for(timer.id) {
                                eprintln!("bellman: timer folder removal failed: {e}");
                            }
                        }
                    }
                }
            }
        }

        self.write_done(&reserved_id, &response)?;
        let _ = std::fs::remove_file(work_path);
        let _ = self.layout.replenish()?;
        Ok(())
    }

    fn write_done(&self, reserved_slot_id: &str, response: &SlotResponse) -> SlotResult<PathBuf> {
        // reserved_slot_id is already validated grammar (digits/alnum only).
        let name = format!("slot-{reserved_slot_id}.json");
        atomic_write_json(&self.layout.done_dir(), &name, response)
    }

    /// Read a done/ response by slot_id (tests / integrators).
    pub fn read_done(&self, slot_id: &str) -> SlotResult<Option<SlotResponse>> {
        let reserved = normalize_slot_id_arg(slot_id)?;
        let name = format!("slot-{reserved}.json");
        let path = super::atomic::safe_child_path(&self.layout.done_dir(), &name)?;
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_capped(&path, self.config.max_read_bytes)?;
        let resp: SlotResponse = serde_json::from_slice(&bytes)?;
        Ok(Some(resp))
    }

    /// Force orphan listing (exposed for tests).
    pub fn orphan_work_paths(&self) -> SlotResult<Vec<PathBuf>> {
        self.layout.list_orphan_work(self.config.orphan_age)
    }
}

/// Reserved slot id from a free/work/done filename (`slot-0007.json` → `0007`).
///
/// Restricted grammar: ASCII alphanumerics, `_`, `-` only — no path separators.
pub fn reserved_slot_id_from_path(path: &Path) -> SlotResult<String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| SlotError::Invalid("slot path has no file name".into()))?;
    reserved_slot_id_from_filename(name)
}

fn reserved_slot_id_from_filename(file_name: &str) -> SlotResult<String> {
    let stem = file_name
        .strip_suffix(".json")
        .ok_or_else(|| SlotError::Invalid(format!("not a .json slot file: {file_name}")))?;
    let id = stem.strip_prefix("slot-").ok_or_else(|| {
        SlotError::Invalid(format!("slot file must be named slot-<id>.json: {file_name}"))
    })?;
    validate_slot_id_grammar(id)?;
    Ok(id.to_string())
}

/// Accept `0007` or `slot-0007` as a reader argument; reject traversal.
fn normalize_slot_id_arg(slot_id: &str) -> SlotResult<String> {
    let id = slot_id.strip_prefix("slot-").unwrap_or(slot_id);
    validate_slot_id_grammar(id)?;
    Ok(id.to_string())
}

fn validate_slot_id_grammar(id: &str) -> SlotResult<()> {
    if id.is_empty() {
        return Err(SlotError::Invalid("empty slot_id".into()));
    }
    if id.contains('/') || id.contains('\\') || id.contains('\0') || id.contains("..") {
        return Err(SlotError::Invalid(format!(
            "slot_id contains forbidden path characters: {id:?}"
        )));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(SlotError::Invalid(format!(
            "slot_id has invalid characters (allowed: A-Za-z0-9_-): {id:?}"
        )));
    }
    Ok(())
}

/// Apply add/modify/delete inside an open store transaction (slot idempotency).
fn apply_request_tx(
    tx: &Transaction<'_>,
    req: &SlotRequest,
    operation: SlotOperation,
    request_id: &str,
    max_events: usize,
) -> SlotResult<SlotResponse> {
    let slot_id = req.slot_id.clone();
    let payload_v = req
        .payload
        .clone()
        .ok_or_else(|| SlotError::Invalid("missing payload".into()))?;
    let payload = SlotPayload::from_value(&payload_v).map_err(SlotError::Invalid)?;
    let app_name = payload
        .app_name
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SlotError::Invalid("payload.app_name is required".into()))?;

    // Optional ack advance (any op that knows the timer id).
    if let (Some(ack), Some(tid)) = (payload.ack_through, payload.resolved_timer_id()) {
        check_ownership_tx(tx, tid, &app_name)?;
        Store::ack_run_events_in_tx(tx, tid, ack).map_err(SlotError::from)?;
    }

    match operation {
        SlotOperation::Add => {
            let new = new_timer_from_payload(&payload).map_err(SlotError::Invalid)?;
            let timer = Store::create_timer_in_tx(tx, new).map_err(SlotError::from)?;
            Store::set_timer_owner_in_tx(tx, timer.id, &app_name).map_err(SlotError::from)?;
            if let Some(ack) = payload.ack_through {
                Store::ack_run_events_in_tx(tx, timer.id, ack).map_err(SlotError::from)?;
            }
            let events = events_for_tx(tx, timer.id, max_events)?;
            Ok(SlotResponse::ok(
                slot_id,
                request_id,
                Some(timer.id),
                timer.next_fire_utc,
                events,
            ))
        }
        SlotOperation::Modify => {
            let timer_id = payload.resolved_timer_id().ok_or_else(|| {
                SlotError::Invalid("modify requires payload.timer_id (or id)".into())
            })?;
            check_ownership_tx(tx, timer_id, &app_name)?;
            let current = Store::get_timer_in_tx(tx, timer_id)
                .map_err(SlotError::from)?
                .ok_or_else(|| SlotError::Invalid(format!("timer not found: {timer_id}")))?;
            let patch = patch_from_payload(&payload).map_err(SlotError::Invalid)?;
            let timer = Store::update_timer_in_tx(
                tx,
                TimerUpdate {
                    id: timer_id,
                    expected_revision: current.revision,
                    patch,
                },
            )
            .map_err(SlotError::from)?;
            let events = events_for_tx(tx, timer.id, max_events)?;
            Ok(SlotResponse::ok(
                slot_id,
                request_id,
                Some(timer.id),
                timer.next_fire_utc,
                events,
            ))
        }
        SlotOperation::Delete => {
            let timer_id = payload.resolved_timer_id().ok_or_else(|| {
                SlotError::Invalid("delete requires payload.timer_id (or id)".into())
            })?;
            check_ownership_tx(tx, timer_id, &app_name)?;
            let timer = Store::get_timer_in_tx(tx, timer_id).map_err(SlotError::from)?;
            let events = events_for_tx(tx, timer_id, max_events)?;

            // R10: cancel the open run INSIDE the delete transaction —
            // cancelled event + lifecycle close + timer/owner/cursor delete
            // commit together. The unresolved test reads owner and run state
            // BEFORE anything is deleted, so an owned run with a finished
            // action claim but an open app lifecycle is still cancelled.
            if let Some(timer) = &timer {
                let prev = crate::store::runs_for_timer_conn(tx, timer_id)
                    .map_err(SlotError::from)?
                    .last()
                    .cloned();
                if let Some(prev) = &prev {
                    let row = crate::store::get_run_state_conn(tx, prev.run_id)
                        .map_err(SlotError::from)?;
                    let unresolved = match &row {
                        Some(row) => !row.is_terminal(),
                        // Ownership was just proven — an owned run without a
                        // lifecycle row is conservatively open (IK3 rule).
                        None => true,
                    };
                    if unresolved {
                        tx.enqueue_event(
                            &crate::events::EventRecord::new(crate::events::RunState::Cancelled)
                                .with_timer(timer.id, timer.name.clone())
                                .with_run(prev.run_id)
                                .with_scheduled_for(prev.scheduled_for)
                                .with_message("timer deleted while its run was open"),
                        )
                        .map_err(SlotError::from)?;
                        if let Some(mut row) = row {
                            row.state = crate::events::RunState::Cancelled.as_str().to_string();
                            row.pickup_deadline = None;
                            row.watchdog_deadline = None;
                            crate::store::update_run_state_conn(tx, &row)
                                .map_err(SlotError::from)?;
                        }
                    }
                }
            }

            let existed = Store::delete_timer_in_tx(tx, timer_id).map_err(SlotError::from)?;
            Store::clear_timer_owner_in_tx(tx, timer_id).map_err(SlotError::from)?;
            // Drop ack cursor with the timer.
            let _ = tx.execute(
                "DELETE FROM slot_event_acks WHERE timer_id = ?1",
                rusqlite::params![timer_id.to_string()],
            );
            if !existed {
                return Ok(SlotResponse::err(
                    slot_id,
                    request_id,
                    format!("timer not found: {timer_id}"),
                ));
            }
            Ok(SlotResponse::ok(
                slot_id,
                request_id,
                Some(timer_id),
                None,
                events,
            ))
        }
    }
}

fn check_ownership_tx(tx: &Transaction<'_>, timer_id: TimerId, app_name: &str) -> SlotResult<()> {
    match Store::get_timer_owner_in_tx(tx, timer_id).map_err(SlotError::from)? {
        Some(owner) if owner == app_name => Ok(()),
        Some(owner) => Err(SlotError::Invalid(format!(
            "ownership denied: timer {timer_id} owned by '{owner}', not '{app_name}'"
        ))),
        None => Err(SlotError::Invalid(format!(
            "ownership denied: timer {timer_id} has no slot owner (not created via slots)"
        ))),
    }
}

fn events_for_tx(
    tx: &Transaction<'_>,
    timer_id: TimerId,
    max_events: usize,
) -> SlotResult<Vec<SlotRunEvent>> {
    let runs = Store::unacked_runs_for_timer_in_tx(tx, timer_id, max_events)
        .map_err(SlotError::from)?;
    Ok(runs.iter().map(SlotRunEvent::from_claim).collect())
}

/// Build a ready-to-publish add request.
pub fn make_add_request(
    app_name: &str,
    timer_name: &str,
    occurrence_kind: &str,
    time: Option<&str>,
    every_secs: Option<u64>,
) -> SlotRequest {
    let mut payload = serde_json::json!({
        "app_name": app_name,
        "timer_name": timer_name,
        "tz": "UTC",
        "occurrence": {
            "kind": occurrence_kind,
        }
    });
    if let Some(t) = time {
        payload["occurrence"]["time"] = serde_json::json!(t);
        payload["time"] = serde_json::json!(t);
    }
    if let Some(e) = every_secs {
        payload["occurrence"]["every_secs"] = serde_json::json!(e);
        payload["every_secs"] = serde_json::json!(e);
    }
    SlotRequest {
        schema: super::envelope::SCHEMA_V1.to_string(),
        slot_id: String::new(), // filled on publish from free stub
        request_id: Some(Uuid::new_v4().to_string()),
        logged_at: Some(Utc::now()),
        operation: Some(SlotOperation::Add),
        payload: Some(payload),
    }
}

/// Status helper for tests.
pub fn response_is_ok(r: &SlotResponse) -> bool {
    r.status == SlotStatus::Ok
}
