//! IK2 — the per-timer folder tree (`docs/todo/json_normalization.md`,
//! "The per-timer folder tree").
//!
//! A human-browsable **view** of state under `<data_dir>/timers/`:
//!
//! ```text
//! timers/
//! ├── README.txt
//! └── bulb-test-3f1a/
//!     ├── timer.json             what the timer IS   (bellman-timer/1)
//!     ├── status.json            the CURRENT run     (bellman-run/1)
//!     └── reply-<run_id>.json    where the app answers (integration-owned only)
//! ```
//!
//! The database owns timers; the event log owns retained history. This tree
//! can be deleted or rebuilt without losing either — every write here is a
//! projection from store state, and a failure to project must never break
//! the primary operation (call sites log and continue).
//!
//! Slug rules are identical on all three platforms (see [`slugify`]): the
//! sanitise → strip → escape → suffix pipeline never relies on the OS to
//! reject or trim anything.

use crate::events::{EventLog, EventRecord, RunState};
use crate::occurrence::OccurrenceKind;
use crate::scheduler::FireKind;
use crate::slots::atomic_write_json;
use crate::store::{ClaimStatus, RunClaim, Store, Timer, TimerId};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Directory name under the data root holding the per-timer folders.
pub const TREE_DIR_NAME: &str = "timers";
/// `timer.json` wire schema (R1).
pub const TIMER_SCHEMA_V1: &str = "bellman-timer/1";
/// `status.json` wire schema (R1).
pub const RUN_SCHEMA_V1: &str = "bellman-run/1";
/// File names inside a timer folder.
pub const TIMER_FILE_NAME: &str = "timer.json";
pub const STATUS_FILE_NAME: &str = "status.json";
/// Root explainer for whoever opens the folder in a file manager.
pub const README_FILE_NAME: &str = "README.txt";
/// Every `timer.json` carries this note: the file is readable, not
/// authoritative — hand edits are ignored, the database wins.
pub const TIMER_NOTE: &str =
    "Written by Bellman. The database is the source of truth — editing this file has no effect.";

/// Root `README.txt`: the layout, which file answers the question, and the
/// honest retention window. History is "30-day history (configurable)",
/// never "permanent".
const README_TEXT: &str = "\
Bellman timers — a human-readable view of what your timers are doing.

Each folder here is one timer, named <name>-<id> (the id keeps it unique;
renaming a timer does NOT rename the folder — integrations depend on the
path, and the live name is always in timer.json).

Inside a folder:
  timer.json            what the timer IS. Bellman writes it, you read it.
                        Hand edits have no effect — the database is the
                        source of truth.
  status.json           the CURRENT run. This is the truth, right now.
  reply-<run_id>.json   where an integrating app answers (only present for
                        timers owned by an app; a fresh file per run).

Which file answers the question \"did it work?\": status.json is the truth;
the reply file is only the app's side. They diverge whenever Bellman judged
a run (no_ack, watchdog expiry) and the app did not speak.

A new fire overwrites status.json — a folder holds the current run only,
there is no history here. History lives in ../logs/events.current.jsonl and
its archives as 30-day history (configurable); deleting or rebuilding this
tree never loses it. Deleting a timer deletes its folder.
";

/// Errors from tree projections. View failures are always recoverable: the
/// database state is already committed when these run.
#[derive(Debug)]
pub enum TreeError {
    Io(String),
    Serialize(String),
    Store(crate::store::StoreError),
}

impl std::fmt::Display for TreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(s) => write!(f, "timer tree io: {s}"),
            Self::Serialize(s) => write!(f, "timer tree json: {s}"),
            Self::Store(e) => write!(f, "timer tree store: {e}"),
        }
    }
}

impl std::error::Error for TreeError {}

impl From<io::Error> for TreeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<crate::store::StoreError> for TreeError {
    fn from(e: crate::store::StoreError) -> Self {
        Self::Store(e)
    }
}

pub type TreeResult<T> = Result<T, TreeError>;

// ── Slug rules ──────────────────────────────────────────────────────────
//
// Verified against Microsoft Learn (2026-07-30), mirrored by the IK2 card:
// - Windows silently STRIPS trailing dots and spaces (inconsistently across
//   APIs), so `backup.` and `backup` collapse into one folder. Strip them
//   ourselves; never rely on the OS to reject them.
// - Reserved device names apply to directories too: CON PRN AUX NUL,
//   COM1..COM9 + COM¹ COM² COM³, LPT1..LPT9 + LPT¹ LPT² LPT³, matched
//   case-insensitively on the exact stem (optionally + .ext). COM0/LPT0 are
//   ambiguous in Microsoft's own docs — blocked anyway, it costs nothing.
// - Windows-illegal characters: < > : " / \ | ? * plus ASCII control
//   characters 0x00-0x1F. On macOS `:` must also be avoided (Finder shows it
//   as `/`). The sanitiser removes all of them.
//
// Pipeline: sanitise → strip trailing dots and spaces → escape a reserved
// stem → append `-<hexid>` (done by [`folder_name`]).

/// Windows reserved device stems (lowercase; matching is case-insensitive).
/// Includes the ISO-8859-1 superscript forms and the ambiguous COM0/LPT0.
const RESERVED_STEMS: &[&str] = &[
    "con", "prn", "aux", "nul", //
    "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", //
    "com¹", "com²", "com³", //
    "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", //
    "lpt¹", "lpt²", "lpt³",
];

/// True when `stem` (already sanitised) is a Windows reserved device name.
/// Case-insensitive; `CONX` was never reserved and is left alone.
fn is_reserved_stem(stem: &str) -> bool {
    let lower = stem.to_lowercase();
    RESERVED_STEMS.contains(&lower.as_str())
}

/// Sanitise a timer name into a portable folder slug, identical on Linux /
/// macOS / Windows.
///
/// 1. Remove Windows-illegal and control characters (`sanitize-filename`
///    with the same rules on every OS — the `windows` flag is NOT used so
///    its built-in reserved-name erasure cannot eat the stem; that check is
///    owned here).
/// 2. Strip trailing dots and spaces (Windows strips them silently and
///    would collide `backup.` with `backup`).
/// 3. Escape a reserved stem (whole slug, or stem before the first `.` for
///    inputs like `CON.txt`) with a leading underscore — the `-<hexid>`
///    suffix must not be the only defence.
/// 4. Empty results fall back to `timer`.
pub fn slugify(name: &str) -> String {
    let sanitized = sanitize_filename::sanitize_with_options(
        name,
        sanitize_filename::Options {
            windows: false,
            truncate: true,
            replacement: "",
        },
    );
    let stripped = sanitized.trim_end_matches(['.', ' ']);
    let mut slug = if stripped.is_empty() {
        "timer".to_string()
    } else {
        stripped.to_string()
    };
    let stem = slug.split('.').next().unwrap_or(&slug);
    if is_reserved_stem(stem) {
        slug = format!("_{slug}");
    }
    slug
}

/// First 4 hex digits of the timer id — the uniqueness suffix in
/// `bulb-test-3f1a`.
pub fn short_id(id: TimerId) -> String {
    let simple = id.simple().to_string();
    simple[..4].to_string()
}

/// `<slug>-<short-id>` — the folder name for a timer.
pub fn folder_name(name: &str, id: TimerId) -> String {
    format!("{}-{}", slugify(name), short_id(id))
}

// ── The tree ────────────────────────────────────────────────────────────

/// Handle to the `<data_dir>/timers/` view root.
#[derive(Debug, Clone)]
pub struct TimersTree {
    root: PathBuf,
}

impl TimersTree {
    /// Tree root under a data directory (`<data_dir>/timers`).
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join(TREE_DIR_NAME),
        }
    }

    /// Tree rooted at an explicit path (tests).
    pub fn at_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Write the root `README.txt` (idempotent; content is static).
    pub fn ensure_readme(&self) -> TreeResult<()> {
        fs::create_dir_all(&self.root)?;
        let path = self.root.join(README_FILE_NAME);
        let tmp = self.root.join(".README.txt.tmp");
        fs::write(&tmp, README_TEXT)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Locate the folder for a timer id without knowing its name (renames do
    /// not rename folders). Matches the `-<short-id>` suffix; on the rare
    /// 4-hex collision, disambiguates via `timer.json`'s `timer_id`.
    pub fn folder_for(&self, id: TimerId) -> Option<PathBuf> {
        let suffix = format!("-{}", short_id(id));
        let mut matches: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&self.root).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(&suffix) {
                matches.push(path);
            }
        }
        match matches.len() {
            0 => None,
            1 => matches.pop(),
            _ => matches.into_iter().find(|p| {
                fs::read_to_string(p.join(TIMER_FILE_NAME))
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| v.get("timer_id")?.as_str().map(str::to_string))
                    .is_some_and(|tid| tid == id.to_string())
            }),
        }
    }

    /// Create the folder for a new timer (README + `timer.json`). Returns the
    /// folder path.
    pub fn create_for_timer(&self, timer: &Timer, owner: Option<&str>) -> TreeResult<PathBuf> {
        self.ensure_readme()?;
        let folder = self.root.join(folder_name(&timer.name, timer.id));
        fs::create_dir_all(&folder)?;
        write_timer_json(&folder, timer, owner)?;
        Ok(folder)
    }

    /// Rewrite `timer.json` after an update (rename, pause/resume, schedule
    /// change). The folder path never changes; a missing folder is recreated
    /// (self-healing view).
    pub fn sync_timer_json(&self, timer: &Timer, owner: Option<&str>) -> TreeResult<PathBuf> {
        self.ensure_readme()?;
        let folder = match self.folder_for(timer.id) {
            Some(f) => f,
            None => {
                let f = self.root.join(folder_name(&timer.name, timer.id));
                fs::create_dir_all(&f)?;
                f
            }
        };
        write_timer_json(&folder, timer, owner)?;
        Ok(folder)
    }

    /// Delete a timer's folder (after the caller logged `cancelled` for any
    /// unresolved run). Returns true when a folder was removed.
    pub fn remove_for(&self, id: TimerId) -> TreeResult<bool> {
        match self.folder_for(id) {
            Some(folder) => {
                fs::remove_dir_all(&folder)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Create the per-run reply stub `reply-<run_id>.json` with `O_EXCL`
    /// (create-only; an existing path is never overwritten — a lost race to a
    /// real reply is the correct outcome). Only integration-owned timers get
    /// a stub. Returns the stub path (created or already present).
    pub fn create_reply_stub(&self, folder: &Path, run_id: Uuid) -> TreeResult<PathBuf> {
        let path = folder.join(reply_file_name(run_id));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(path),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete `reply-*.json` files that do not belong to `keep_run` (a new
    /// fire supersedes the previous run's channel; different runs never share
    /// a path and a stale file is never overwritten).
    pub fn remove_stale_replies(&self, folder: &Path, keep_run: Uuid) -> TreeResult<()> {
        let keep = reply_file_name(keep_run);
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("reply-") && name.ends_with(".json") && name != keep {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    /// Orphan sweep: remove folders whose timer no longer exists in the
    /// database (a crash between the database delete and the folder delete
    /// left them behind). Returns the removed folder paths so the caller can
    /// log each removal — never silent.
    pub fn sweep_orphans(&self, live_ids: &HashSet<TimerId>) -> TreeResult<Vec<PathBuf>> {
        let mut removed = Vec::new();
        if !self.root.is_dir() {
            return Ok(removed);
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(hex4) = name.rsplit('-').next() else {
                continue;
            };
            // Folder ids are a 4-hex prefix of the timer UUID; collect every
            // live id sharing the prefix before calling a folder an orphan.
            let looks_like_timer_folder = hex4.len() == 4 && hex4.chars().all(|c| c.is_ascii_hexdigit());
            if !looks_like_timer_folder {
                continue;
            }
            let alive = live_ids
                .iter()
                .any(|id| id.simple().to_string().starts_with(hex4));
            if !alive {
                fs::remove_dir_all(&path)?;
                removed.push(path);
            }
        }
        Ok(removed)
    }
}

/// `reply-<full-run_id>.json` — per-run filename; full id, never a prefix.
pub fn reply_file_name(run_id: Uuid) -> String {
    format!("reply-{run_id}.json")
}

// ── timer.json (bellman-timer/1) ────────────────────────────────────────

/// Project a store timer into the `bellman-timer/1` shape and write it
/// atomically. Readable, not authoritative — carries the `note` field.
fn write_timer_json(folder: &Path, timer: &Timer, owner: Option<&str>) -> TreeResult<()> {
    let mut v = serde_json::json!({
        "schema": TIMER_SCHEMA_V1,
        "timer_id": timer.id,
        "name": timer.name,
        "enabled": timer.enabled,
        "tz": timer.tz,
        "occurrence": occurrence_view(timer.occurrence.kind()),
        "action": timer.action,
        "note": TIMER_NOTE,
    });
    if let Some(app) = owner {
        v["integration"] = serde_json::json!({ "app_name": app });
    }
    if let Some(next) = timer.next_fire_utc {
        v["next_fire_at"] = serde_json::json!(next);
    }
    atomic_write_json(folder, TIMER_FILE_NAME, &v)
        .map(|_| ())
        .map_err(|e| TreeError::Serialize(e.to_string()))
}

/// Reshape the store's `{"occ": "daily", "at": ...}` occurrence serde into
/// the design's `{"kind": "daily", "time": ...}` view shape.
fn occurrence_view(kind: &OccurrenceKind) -> serde_json::Value {
    let mut v = serde_json::to_value(kind).unwrap_or_else(|_| serde_json::json!({}));
    let obj = v.as_object_mut();
    let Some(map) = obj else {
        return v;
    };
    map.remove("occ");
    // Wall-clock kinds name their clock time `time` in the view shape;
    // `once` keeps `at` (a full local datetime), `interval` keeps
    // `every_secs`/`anchor`, `cron` keeps `expr`.
    match kind.kind_label() {
        "daily" | "weekly" | "monthly" | "yearly" => {
            if let Some(at) = map.remove("at") {
                map.insert("time".into(), at);
            }
        }
        _ => {}
    }
    let mut out = serde_json::Map::new();
    out.insert("kind".into(), serde_json::json!(kind.kind_label()));
    out.extend(map.clone());
    serde_json::Value::Object(out)
}

// ── status.json (bellman-run/1) ─────────────────────────────────────────

/// The current-run mirror. Optional fields the app never sent are simply
/// absent — never rendered as empty or "never".
#[derive(Debug, Clone, Serialize)]
pub struct RunStatus {
    pub schema: String,
    pub state: String,
    pub run_id: Uuid,
    pub timer_id: Uuid,
    pub timer_name: String,
    pub occurrence_kind: String,
    pub scheduled_for: DateTime<Utc>,
    pub fired_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl RunStatus {
    /// Fresh snapshot at fire time.
    pub fn fired(timer: &Timer, claim: &RunClaim, kind: &FireKind, owner: Option<&str>) -> Self {
        Self {
            schema: RUN_SCHEMA_V1.to_string(),
            state: fire_state(kind).to_string(),
            run_id: claim.run_id,
            timer_id: timer.id,
            timer_name: timer.name.clone(),
            occurrence_kind: timer.occurrence.kind().kind_label().to_string(),
            scheduled_for: claim.scheduled_for,
            fired_at: claim.claimed_at,
            app_name: owner.map(str::to_string),
            completed_at: None,
            failed_at: None,
            reason: None,
        }
    }
}

/// R5 state for the fire snapshot (`fired` / `fired_late` / `coalesced`).
fn fire_state(kind: &FireKind) -> &'static str {
    match kind {
        FireKind::OnTime | FireKind::CatchUp { .. } => RunState::Fired.as_str(),
        FireKind::Late { .. } => RunState::FiredLate.as_str(),
        FireKind::Coalesced { .. } => RunState::Coalesced.as_str(),
    }
}

/// Write `status.json` into the timer's folder (located by id). No-op when
/// the folder is gone (timer deleted mid-run is not an error here).
pub fn write_status(tree: &TimersTree, timer: &Timer, status: &RunStatus) -> TreeResult<()> {
    let Some(folder) = tree.folder_for(timer.id) else {
        return Ok(());
    };
    atomic_write_json(&folder, STATUS_FILE_NAME, status)
        .map(|_| ())
        .map_err(|e| TreeError::Serialize(e.to_string()))
}

// ── Fire / delete projections (called from the scheduler, run_now, CRUD) ─

/// Fire-time projection, after the run claim committed and before/with the
/// action run:
///
/// 1. Any still-open previous run (`claimed`, not this run) is logged
///    `superseded` — loudly; it means the interval is shorter than the app
///    takes. The first firing's reply path is never overwritten: its stale
///    reply file is deleted and the new run gets its own.
/// 2. `status.json` is rewritten fresh for this run (`fired` /
///    `fired_late` / `coalesced`).
/// 3. Integration-owned timers get a fresh per-run reply stub (`O_EXCL`).
pub fn project_run_started(
    tree: &TimersTree,
    store: &Store,
    timer: &Timer,
    claim: &RunClaim,
    kind: &FireKind,
    log: &mut EventLog,
) -> TreeResult<()> {
    let owner = store.get_timer_owner(timer.id)?;
    let folder = tree.sync_timer_json(timer, owner.as_deref())?;

    // 1. Supersede still-open previous runs.
    let runs = store.runs_for_timer(timer.id)?;
    for run in &runs {
        if run.run_id != claim.run_id && run.status == ClaimStatus::Claimed {
            log.emit(
                EventRecord::new(RunState::Superseded)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(run.run_id)
                    .with_scheduled_for(run.scheduled_for)
                    .with_message("superseded by a new firing while still unresolved"),
            )
            .map_err(|e| TreeError::Io(e.to_string()))?;
        }
    }
    // The new run owns the channel: drop stale reply files (never overwrite),
    // then write the fresh status snapshot.
    tree.remove_stale_replies(&folder, claim.run_id)?;
    let status = RunStatus::fired(timer, claim, kind, owner.as_deref());
    write_status(tree, timer, &status)?;

    // 3. Integration-owned run: fresh per-run reply stub.
    if owner.is_some() {
        tree.create_reply_stub(&folder, claim.run_id)?;
    }
    Ok(())
}

/// Run-close projection: fold the recorded outcome into `status.json`.
/// Success → `completed` with `completed_at`; failure → `failed` with
/// `failed_at` and the error as `reason`. `duration_ms` stays on the log
/// event only — it is never written here.
pub fn project_run_finished(
    tree: &TimersTree,
    store: &Store,
    timer: &Timer,
    claim: &RunClaim,
    kind: &FireKind,
    failure: Option<&str>,
) -> TreeResult<()> {
    let owner = store.get_timer_owner(timer.id)?;
    let mut status = RunStatus::fired(timer, claim, kind, owner.as_deref());
    let now = Utc::now();
    match failure {
        None => {
            status.state = RunState::Completed.as_str().to_string();
            status.completed_at = Some(now);
        }
        Some(err) => {
            status.state = RunState::Failed.as_str().to_string();
            status.failed_at = Some(now);
            status.reason = Some(err.chars().take(1024).collect());
        }
    }
    write_status(tree, timer, &status)
}

/// Delete-time projection, before/with the database delete: any unresolved
/// run (`claimed` — the app lifecycle is IK3; today the claim is the open
/// run) is logged `cancelled` FIRST, so an app whose `status.json` vanishes
/// reads the run as cancelled, not missing.
pub fn log_cancelled_for_open_runs(
    store: &Store,
    timer: &Timer,
    log: &mut EventLog,
) -> TreeResult<usize> {
    let mut n = 0;
    for run in store.runs_for_timer(timer.id)? {
        if run.status == ClaimStatus::Claimed {
            log.emit(
                EventRecord::new(RunState::Cancelled)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(run.run_id)
                    .with_scheduled_for(run.scheduled_for)
                    .with_message("timer deleted while its run was open"),
            )
            .map_err(|e| TreeError::Io(e.to_string()))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Folder reconciliation: ensure every live timer has a folder + fresh
/// `timer.json` (covers timers created before the tree existed and rebuilds
/// a deleted tree). Orphan folders are the caller's concern
/// ([`TimersTree::sweep_orphans`]).
pub fn reconcile_folders(tree: &TimersTree, store: &Store) -> TreeResult<usize> {
    let mut synced = 0;
    for timer in store.list_timers()? {
        let owner = store.get_timer_owner(timer.id)?;
        tree.sync_timer_json(&timer, owner.as_deref())?;
        synced += 1;
    }
    Ok(synced)
}

#[cfg(test)]
mod tests;
