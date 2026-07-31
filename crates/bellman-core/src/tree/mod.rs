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

use crate::events::{EventRecord, RunState};
use crate::occurrence::OccurrenceKind;
use crate::scheduler::FireKind;
use crate::slots::atomic_write_json;
use crate::store::{RunClaim, Store, Timer, TimerId};
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

A run without a reply-<run_id>.json spoke over IPC (the local socket) —
that is normal, not a missing file: this run spoke over IPC; status.json
is still the truth.

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

/// First 4 hex digits of the timer id — the default uniqueness suffix in
/// `bulb-test-3f1a`.
pub fn short_id(id: TimerId) -> String {
    let simple = id.simple().to_string();
    simple[..4].to_string()
}

/// `<slug>-<short-id>` — the default (4-hex) folder name for a timer. When
/// that name is already taken by a DIFFERENT timer (same slug + same first 4
/// hex digits), allocation lengthens the suffix — see
/// [`TimersTree::allocate_folder`].
pub fn folder_name(name: &str, id: TimerId) -> String {
    format!("{}-{}", slugify(name), short_id(id))
}

/// Suffix lengths tried in order when allocating a folder name. A collision
/// (same slug + same prefix owned by another timer) lengthens the hex until
/// the name is unique — 32 hex digits is the full UUID, so this always
/// terminates.
const SUFFIX_LENS: [usize; 8] = [4, 6, 8, 12, 16, 20, 28, 32];

/// Read `timer_id` out of a folder's `timer.json`, if present and parseable.
fn folder_timer_id(folder: &Path) -> Option<String> {
    fs::read_to_string(folder.join(TIMER_FILE_NAME))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("timer_id")?.as_str().map(str::to_string))
}

/// True when a directory name looks like `<slug>-<hexsuffix>`; returns the
/// hex suffix. The suffix is 4..=32 hex digits (see [`SUFFIX_LENS`]).
fn folder_suffix(name: &str) -> Option<&str> {
    let suffix = name.rsplit('-').next()?;
    if (4..=32).contains(&suffix.len())
        && suffix.len() < name.len()
        && suffix.chars().all(|c| c.is_ascii_hexdigit())
    {
        Some(suffix)
    } else {
        None
    }
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
    /// not rename folders, and collision-allocation may have lengthened the
    /// suffix). A candidate's hex suffix must be a PREFIX of the timer's id
    /// (so `…-a001` and `…-a001b2` are both candidates of id `a001b2…`);
    /// several candidates are disambiguated via `timer.json`'s `timer_id`. A
    /// single candidate whose `timer.json` names a different timer is not
    /// ours — the folder was allocated away in a collision and ours was
    /// never created.
    pub fn folder_for(&self, id: TimerId) -> Option<PathBuf> {
        let hex = id.simple().to_string();
        let mut candidates: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&self.root).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(suffix) = folder_suffix(&name) {
                if hex.starts_with(suffix) {
                    candidates.push(path);
                }
            }
        }
        match candidates.len() {
            0 => None,
            1 => {
                let folder = candidates.pop()?;
                match folder_timer_id(&folder) {
                    Some(tid) if tid != id.to_string() => None,
                    _ => Some(folder),
                }
            }
            _ => candidates
                .into_iter()
                .find(|p| folder_timer_id(p).as_deref() == Some(id.to_string().as_str())),
        }
    }

    /// Allocate a fresh folder for `timer`, lengthening the hex suffix until
    /// the name is unique. Idempotent for the same timer (an existing folder
    /// whose `timer.json` names this id is reused). `create_dir` (not
    /// `create_dir_all`) makes the existence check atomic across processes.
    fn allocate_folder(&self, timer: &Timer) -> TreeResult<PathBuf> {
        let slug = slugify(&timer.name);
        let hex = timer.id.simple().to_string();
        for len in SUFFIX_LENS {
            let path = self.root.join(format!("{slug}-{}", &hex[..len]));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if folder_timer_id(&path).as_deref() == Some(timer.id.to_string().as_str()) {
                        return Ok(path);
                    }
                    // Collision with a different timer: lengthen the suffix.
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(TreeError::Io(format!(
            "could not allocate a unique folder for timer {}",
            timer.id
        )))
    }

    /// Create the folder for a new timer (README + `timer.json`). Returns the
    /// folder path.
    pub fn create_for_timer(&self, timer: &Timer, owner: Option<&str>) -> TreeResult<PathBuf> {
        self.ensure_readme()?;
        let folder = self.allocate_folder(timer)?;
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
            None => self.allocate_folder(timer)?,
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

    /// Create the per-run reply stub `reply-<run_id>.json`, pre-filled with
    /// everything the app should not have to know (`schema`, `run_id`,
    /// `app_name`, `state: null`, hint) — the app edits; it does not
    /// reconstruct. Create-only semantics: the content is written to a temp
    /// file and hard-linked into place, so an existing path (an app that
    /// already wrote a real reply) is never overwritten — a lost race to a
    /// real reply is the correct outcome. Only integration-owned timers get
    /// a stub. Returns the stub path (created or already present).
    pub fn create_reply_stub(
        &self,
        folder: &Path,
        run_id: Uuid,
        app_name: &str,
    ) -> TreeResult<PathBuf> {
        let path = folder.join(reply_file_name(run_id));
        if path.exists() {
            return Ok(path);
        }
        let tmp = folder.join(format!(".reply-{run_id}.tmp"));
        let bytes = crate::reply::stub_bytes(run_id, app_name);
        fs::write(&tmp, &bytes)?;
        if let Ok(f) = fs::File::open(&tmp) {
            let _ = f.sync_all();
        }
        match fs::hard_link(&tmp, &path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            // Filesystems without hard links: fall back to O_EXCL create.
            Err(_) => match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(&bytes)?;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(e.into());
                }
            },
        }
        let _ = fs::remove_file(&tmp);
        Ok(path)
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
            // Folder suffixes are a 4..32-hex prefix of the timer UUID.
            let Some(suffix) = folder_suffix(&name) else {
                continue;
            };
            // Exact identity wins when timer.json is readable (prefix
            // sharing between live timers must not keep a dead folder
            // alive); otherwise fall back to the suffix-prefix check.
            let alive = match folder_timer_id(&path) {
                Some(tid) => live_ids
                    .iter()
                    .any(|id| id.to_string() == tid),
                None => live_ids
                    .iter()
                    .any(|id| id.simple().to_string().starts_with(suffix)),
            };
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
        "transport": { "mode": timer.transport.as_str() },
        "note": TIMER_NOTE,
    });
    if let Some(app) = owner {
        v["integration"] = serde_json::json!({ "app_name": app });
    }
    // IK6: connection info is data, not code — the one socket path, only
    // while the IPC server is up. Never a generated adapter file.
    if let Some(socket) = crate::ipc::advertised_socket() {
        v["ipc"] = serde_json::json!({ "socket": socket });
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

/// The current-run mirror. `cat status.json` must always show the truth
/// right now — Bellman's own fields plus everything the app has reported,
/// accumulated (a reply that omits an earlier field never retracts it).
/// Optional fields the app never sent are simply absent — never rendered as
/// empty or "never".
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
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detection: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_ack_at: Option<DateTime<Utc>>,
    /// IK6: the effective delivery transport of this run (`json` | `ipc` |
    /// `ipc_fallback`) — the mirror stays transport-independent (written for
    /// both), but the human reading it can see how the app was spoken to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

impl RunStatus {
    /// Fresh snapshot at fire time.
    pub fn fired(
        timer: &Timer,
        claim: &RunClaim,
        kind: &FireKind,
        owner: Option<&str>,
        transport: Option<&str>,
    ) -> Self {
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
            acknowledged_at: None,
            expected_secs: None,
            error_detection: None,
            heartbeat_at: None,
            progress: None,
            completed_at: None,
            result: None,
            result_truncated: None,
            failed_at: None,
            reason: None,
            failure_kind: None,
            no_ack_at: None,
            transport: transport.map(str::to_string),
        }
    }

    /// Project the accumulated database row (IK3) into the mirror. This is
    /// how `status.json` is rebuilt after a crash and how every accepted
    /// reply folds in: the database is the truth, the file is the present.
    pub fn from_run_state(
        timer: &Timer,
        claim: &RunClaim,
        row: &crate::store::RunStateRow,
    ) -> Self {
        Self {
            schema: RUN_SCHEMA_V1.to_string(),
            state: row.state.clone(),
            run_id: claim.run_id,
            timer_id: timer.id,
            timer_name: timer.name.clone(),
            occurrence_kind: timer.occurrence.kind().kind_label().to_string(),
            scheduled_for: claim.scheduled_for,
            fired_at: row.fired_at,
            app_name: Some(row.app_name.clone()),
            acknowledged_at: row.acknowledged_at,
            expected_secs: row.expected_secs,
            error_detection: row.error_detection,
            heartbeat_at: row.heartbeat_at,
            progress: row.progress.clone(),
            completed_at: row.completed_at,
            result: row.result_json.clone(),
            result_truncated: row.result_truncated.then_some(true),
            failed_at: row.failed_at,
            reason: row.reason.clone(),
            failure_kind: row.failure_kind.map(|k| k.as_str().to_string()),
            no_ack_at: row.no_ack_at,
            transport: row.transport.clone(),
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

/// The one current run that a new firing replaces or a deletion cancels, if
/// it is still unresolved by the exact R5 test:
///
/// - **Owned run** (integration owner): the app lifecycle is the reply
///   channel. Pre-IK3 nothing records a terminal app state, so the run is
///   open until a reply closes it — the claim ledger is delivery bookkeeping
///   only (a `finished` claim with outcome `wake_delivered` never means the
///   app finished; see store/models.rs).
/// - **Unowned run**: unresolved means the action claim is not finished
///   (`pending` / `active`).
///
/// `exclude_run` skips the just-claimed run when called from a fire.
///
/// The exact R5 unresolved test:
///
/// - **Owned run with a lifecycle row** (IK3): unresolved means the app
///   lifecycle is non-terminal (`completed` / `failed` / `no_ack` /
///   `cancelled` / `superseded` close it — provisionally for the first three,
///   but terminal enough that the next firing supersedes regardless).
/// - **Owned run without a row** (predates IK3): conservatively open.
/// - **Unowned run**: unresolved means the action claim is not finished
///   (`claimed`).
fn current_unresolved_run(
    store: &Store,
    timer: &Timer,
    exclude_run: Option<Uuid>,
) -> TreeResult<Option<RunClaim>> {
    let owner = store.get_timer_owner(timer.id)?;
    let runs = store.runs_for_timer(timer.id)?;
    let latest = runs
        .iter()
        .rev()
        .find(|r| Some(r.run_id) != exclude_run);
    let Some(prev) = latest else {
        return Ok(None);
    };
    let unresolved = match store.get_run_state(prev.run_id)? {
        Some(row) => !row.is_terminal(),
        None => {
            if owner.is_some() {
                // No lifecycle row (predates IK3) — an owned run stays open.
                true
            } else {
                prev.is_unfinished()
            }
        }
    };
    Ok(unresolved.then(|| prev.clone()))
}

/// The R10 fire transaction + projections. One call does, in order:
///
/// 1. **Acquire the per-timer gate** (REQUIRED — a lock failure aborts the
///    fire; lifecycle mutation without the gate is not permitted).
/// 2. **Pre-fire barrier**: the previous current run's reply file is
///    synchronously read and parsed (bounded: one debounce window), so a
///    valid `completed` the watcher has not processed yet is folded in
///    BEFORE the run can be superseded.
/// 3. **One SQLite transaction** carries: the barrier ingest (previous run's
///    final known state + its transition events), the `superseded` event and
///    lifecycle-row update if the previous run is still unresolved *after*
///    the barrier, the new claim (`UNIQUE(timer_id, scheduled_for)` still
///    guards double-fire), the new run's lifecycle row (owner snapshotted,
///    pickup deadline persisted), and the `fired` event. Crash before the
///    commit and the previous firing is still current — nothing was
///    half-started; crash after it and the projections below are rebuilt by
///    the startup/periodic reconciler.
/// 4. **Post-commit, still under the gate**: monotonic pickup deadline +
///    duration anchor registration, stale reply removal, then projections
///    in the R10 order — fresh `status.json` → create-only pre-filled reply
///    stub → fire notification under `slots/fires/`. Projection failures
///    surface (stderr + reconciler) but never un-commit the fire.
///
/// Returns the new claim. `StoreError::AlreadyClaimed` propagates (inside
/// `TreeError::Store`) so the scheduler can run its crash-recovery path.
#[allow(clippy::too_many_arguments)]
pub fn project_fire(
    tree: &TimersTree,
    store: &mut Store,
    timer: &Timer,
    scheduled_for: DateTime<Utc>,
    kind: &FireKind,
    engine: &crate::reply::ReplyEngine,
    now: DateTime<Utc>,
) -> TreeResult<RunClaim> {
    use crate::reply::RunDb;
    use crate::store::{claim_run_conn, insert_run_state_conn, update_run_state_conn};

    // 1. The gate is required: every lifecycle mutation happens under it.
    let _gate = crate::reply::gate::acquire(&engine.data_dir, timer.id)
        .map_err(|e| TreeError::Io(format!("per-timer gate: {e}")))?;

    let owner = store.get_timer_owner(timer.id)?;
    let folder = tree.sync_timer_json(timer, owner.as_deref())?;
    // IK6: the transport choice is made HERE, at fire, recorded on the run,
    // and never changes mid-firing. The next firing chooses fresh.
    let selected = crate::reply::publication::select_transport(timer, engine);

    // 2. Barrier READ (file I/O — outside the transaction, bounded). The
    //    barrier enforces the same filename/document identity rule as the
    //    ordinary watcher; a forged or invalid document is quarantined
    //    (copy-only) and never reaches the transaction.
    let prev = store.runs_for_timer(timer.id)?.last().cloned();
    let barrier = prev.as_ref().map(|p| {
        let read = crate::reply::barrier_read(engine, store, timer, &folder, p.run_id);
        (p.clone(), read)
    });

    // 3. The one fire transaction.
    let mut prev_superseded = None;
    let mut post_quarantine: Option<(PathBuf, Uuid, Vec<u8>, &'static str)> = None;
    let mut transport: Option<crate::store::TransportProjection> = None;
    let claim = {
        let tx = store.transaction()?;
        // Barrier ingest: fold the previous run's final outcome in FIRST.
        if let Some((prev, crate::reply::BarrierRead::Valid { doc, digest, bytes })) = &barrier
        {
            let outcome = engine
                .ingest_as_current(&tx, timer, doc, digest, prev.run_id, now, std::time::Instant::now())
                .map_err(|e| TreeError::Io(e.to_string()))?;
            if let crate::reply::IngestOutcome::Rejected(reason) = outcome {
                engine
                    .log_rejection(&tx, timer, Some(prev.run_id), reason.as_str())
                    .map_err(|e| TreeError::Io(e.to_string()))?;
                post_quarantine = Some((
                    folder.join(reply_file_name(prev.run_id)),
                    prev.run_id,
                    bytes.clone(),
                    reason.as_str(),
                ));
            }
        }
        // Supersede the previous run when still unresolved AFTER the barrier.
        if let Some(prev) = &prev {
            let unresolved = match tx.get_run_state(prev.run_id)? {
                Some(row) => !row.is_terminal(),
                None => {
                    if owner.is_some() {
                        true
                    } else {
                        prev.is_unfinished()
                    }
                }
            };
            if unresolved {
                tx.enqueue_event(
                    &EventRecord::new(RunState::Superseded)
                        .with_timer(timer.id, timer.name.clone())
                        .with_run(prev.run_id)
                        .with_scheduled_for(prev.scheduled_for)
                        .with_message("superseded by a new firing while still unresolved"),
                )?;
                if let Some(mut row) = tx.get_run_state(prev.run_id)? {
                    row.state = RunState::Superseded.as_str().to_string();
                    row.pickup_deadline = None;
                    row.watchdog_deadline = None;
                    update_run_state_conn(&tx, &row)?;
                }
                prev_superseded = Some(prev.run_id);
            }
        }
        // The new claim (UNIQUE guard) + fired event + SCH1 overlap
        // disposition + lifecycle row. The fired event commits FIRST so the
        // log never reads backwards for a skipped run; the disposition is
        // decided HERE, at fire commit, from the older executable claims —
        // never later at dequeue.
        let claim = claim_run_conn(&tx, timer.id, scheduled_for)?;
        tx.enqueue_event(&fire_event(kind, timer, &claim))?;
        let claim = apply_overlap_disposition(&tx, timer, claim)?;
        if let Some(app_name) = owner.as_deref() {
            let deadline = now
                + chrono::Duration::from_std(engine.pickup_grace)
                    .unwrap_or_else(|_| chrono::Duration::seconds(60));
            let row = crate::store::RunStateRow::fired(
                claim.run_id,
                timer.id,
                app_name,
                fire_state(kind),
                claim.claimed_at,
                deadline,
            )
            .with_transport(selected.as_str());
            insert_run_state_conn(&tx, &row)?;
        }
        // SCH1: the durable transport projection for the fire notification
        // (routing/retry state for this run; the fixed-target cursor advances
        // in the same commit).
        if let Some(app_name) = owner.as_deref() {
            let order = crate::store::next_publication_order_conn(&tx)?;
            let proj = crate::reply::publication::new_projection(
                engine,
                timer,
                &claim,
                fire_state(kind),
                &folder,
                app_name,
                order,
                now,
                selected,
            )
            .map_err(crate::store::StoreError::Internal)?;
            crate::store::insert_transport_projection_conn(&tx, &proj)?;
            transport = Some(proj);
        }
        tx.commit().map_err(|e| TreeError::Io(e.to_string()))?;
        claim
    };

    // 4. Post-commit, still under the gate.
    if owner.is_some() {
        engine.register_fire(claim.run_id, now);
    }
    if let Some(prev_id) = prev_superseded {
        engine.clear_deadlines(prev_id);
    }
    // The barrier's semantic rejection also gets its quarantine COPY (the
    // event went out with the transaction; the artifact is idempotent).
    if let Some((path, run_id, bytes, reason)) = post_quarantine {
        crate::reply::quarantine_rejected_bytes(engine, timer, &path, run_id, &bytes, reason);
    }

    // Projections: surface failures, never un-commit the fire. The bounded
    // reconciler repairs them (status.json is Bellman's alone; a missing
    // stub is created O_EXCL only).
    let projection = (|| -> TreeResult<()> {
        tree.remove_stale_replies(&folder, claim.run_id)?;
        let status = RunStatus::fired(
            timer,
            &claim,
            kind,
            owner.as_deref(),
            owner.as_deref().map(|_| selected.as_str()),
        );
        write_status(tree, timer, &status)?;
        // The reply stub exists only for firings that selected the file
        // transport (IK6): an IPC firing deliberately has no stub — the
        // folder README explains "this run spoke over IPC; status.json is
        // still the truth".
        if selected == crate::reply::publication::SelectedTransport::Json {
            if let Some(app_name) = owner.as_deref() {
                tree.create_reply_stub(&folder, claim.run_id, app_name)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = projection {
        eprintln!("bellman: fire projection failed (reconciler will repair): {e}");
    }
    // IK5: the fire committed either way — invalidate the live-run view so
    // the GUI shows the new `fired` run (and the superseded previous one)
    // without waiting for the app to answer.
    engine.notify_status_changed(timer.id);
    // The gate must be released BEFORE the publication attempt: the attempt
    // takes the timer shard itself (then the target shard — the fixed lock
    // order), and flock is per open-file-description, so re-acquiring here
    // would deadlock against our own guard.
    drop(_gate);
    // SCH1: the immediate publication attempt — only after R10 projected
    // `status.json` and the reply stub above (a notification naming missing
    // run files is a broken notification). A bounded local failure stays
    // `pending` for the publication pump / startup recovery, never for an
    // action worker.
    if let Some(proj) = &transport {
        crate::reply::publication::attempt(&engine.data_dir, store, proj, engine.ipc.as_ref());
    }
    Ok(claim)
}

/// SCH1: the durable overlap admission decision, made inside the fire
/// transaction — never later at dequeue. Examines the timer's older
/// executable claims (`pending` or `active`) and applies the policy:
///
/// - `Skip`: any older unfinished claim → the new claim finishes
///   `skipped_misfire(overlap_skip)` immediately.
/// - `QueueOne`: at most one executable follow-up beyond the oldest
///   active/pending action; excess finishes `skipped_misfire(overlap_queue_full)`.
/// - `Parallel { cap }`: admit while fewer than `cap` older claims are
///   unfinished; excess finishes `skipped_misfire(overlap_parallel_cap)`
///   (`cap: 0` admits none).
/// - `Replace`: every older `pending` claim finishes
///   `wake_failed(overlap_replace_before_start)`, every older `active` claim
///   is marked `cancel_requested` (the dispatcher signals the worker tokens);
///   the newest claim stays `pending`, eligible only after the active
///   predecessors finish, so they never overlap.
///
/// Publication is NOT affected: the record of the fire (claim, lifecycle
/// row, `fired` event, status/notification) commits either way — the policy
/// governs the action, never the record.
fn apply_overlap_disposition(
    tx: &rusqlite::Transaction<'_>,
    timer: &Timer,
    claim: RunClaim,
) -> crate::store::StoreResult<RunClaim> {
    use crate::store::{
        finish_run_conn, get_run_conn, pending_claims_for_timer_conn, request_cancel_active_conn,
        unfinished_claims_count_conn, OverlapPolicy, RunOutcome,
    };
    use crate::reply::RunDb;

    let skip_reason: Option<&'static str> = match &timer.overlap {
        OverlapPolicy::Skip => {
            (unfinished_claims_count_conn(tx, timer.id, claim.run_id)? >= 1).then_some("overlap_skip")
        }
        OverlapPolicy::QueueOne => {
            (unfinished_claims_count_conn(tx, timer.id, claim.run_id)? >= 2)
                .then_some("overlap_queue_full")
        }
        OverlapPolicy::Parallel { cap } => {
            (unfinished_claims_count_conn(tx, timer.id, claim.run_id)? >= *cap as usize)
                .then_some("overlap_parallel_cap")
        }
        OverlapPolicy::Replace => {
            for p in pending_claims_for_timer_conn(tx, timer.id, claim.run_id)? {
                // Loses the race to a worker that already committed — the
                // worker's truthful outcome stands, no replace event.
                if finish_run_conn(tx, p.run_id, RunOutcome::WakeFailed, "overlap_replace_before_start")? {
                    tx.enqueue_event(
                        &EventRecord::new(RunState::WakeFailed)
                            .with_timer(timer.id, timer.name.clone())
                            .with_run(p.run_id)
                            .with_scheduled_for(p.scheduled_for)
                            .with_message("overlap_replace_before_start"),
                    )?;
                }
            }
            let _ = request_cancel_active_conn(tx, timer.id, claim.run_id)?;
            None
        }
    };
    let Some(reason) = skip_reason else {
        return Ok(claim);
    };
    let _ = finish_run_conn(tx, claim.run_id, RunOutcome::SkippedMisfire, reason)?;
    tx.enqueue_event(
        &EventRecord::new(RunState::SkippedMisfire)
            .with_timer(timer.id, timer.name.clone())
            .with_run(claim.run_id)
            .with_scheduled_for(claim.scheduled_for)
            .with_message(reason),
    )?;
    // Return the finished row (status + outcome) to the caller.
    Ok(get_run_conn(tx, claim.run_id)?.unwrap_or(claim))
}

/// The `fired` event for the fire transaction (mirrors the fire kind).
fn fire_event(kind: &FireKind, timer: &Timer, claim: &RunClaim) -> EventRecord {
    let base = || {
        EventRecord::new(RunState::Fired)
            .with_timer(timer.id, timer.name.clone())
            .with_run(claim.run_id)
            .with_scheduled_for(claim.scheduled_for)
    };
    match kind {
        FireKind::OnTime => base(),
        FireKind::Late { lateness } => EventRecord::new(RunState::FiredLate)
            .with_timer(timer.id, timer.name.clone())
            .with_run(claim.run_id)
            .with_scheduled_for(claim.scheduled_for)
            .with_duration_ms(lateness.num_milliseconds()),
        FireKind::Coalesced { missed_count } => EventRecord::new(RunState::Coalesced)
            .with_timer(timer.id, timer.name.clone())
            .with_run(claim.run_id)
            .with_scheduled_for(claim.scheduled_for)
            .with_count(*missed_count),
        FireKind::CatchUp { index } => base().with_count(*index).with_message("catch_up"),
    }
}

/// Delete-time projection, before/with the database delete: if the current
/// run is unresolved by the exact R5 test (owned: app lifecycle open;
/// unowned: action claim unfinished), it is logged `cancelled` FIRST, so an
/// app whose `status.json` vanishes reads the run as cancelled, not missing.
/// The event is enqueued (R11); the elected publisher appends it.
pub fn log_cancelled_for_open_runs(store: &Store, timer: &Timer) -> TreeResult<usize> {
    let mut n = 0;
    if let Some(run) = current_unresolved_run(store, timer, None)? {
        store.enqueue_event(
            &EventRecord::new(RunState::Cancelled)
                .with_timer(timer.id, timer.name.clone())
                .with_run(run.run_id)
                .with_scheduled_for(run.scheduled_for)
                .with_message("timer deleted while its run was open"),
        )?;
        // Close the lifecycle row too: its deadlines must not fire `no_ack`
        // or a watchdog for a timer that no longer exists.
        if let Some(mut row) = store.get_run_state(run.run_id)? {
            row.state = RunState::Cancelled.as_str().to_string();
            row.pickup_deadline = None;
            row.watchdog_deadline = None;
            store.update_run_state(&row)?;
        }
        n += 1;
    }
    Ok(n)
}

/// The atomic delete lifecycle (R10): `cancelled` event + lifecycle-row
/// close + timer delete + owner/cursor cleanup commit in ONE IMMEDIATE
/// transaction, under the caller's REQUIRED per-timer gate. The folder is
/// removed only after this commits.
///
/// The unresolved test reads owner and run state BEFORE anything is
/// deleted, so an owned run with a finished action claim but an open app
/// lifecycle is still `cancelled` (the order that previously lost it).
/// Returns `(deleted, cancelled_count)`.
pub fn delete_timer_lifecycle(store: &mut Store, timer: &Timer) -> TreeResult<(bool, usize)> {
    use crate::reply::RunDb;
    use crate::store::{get_run_state_conn, runs_for_timer_conn, update_run_state_conn};

    let owner = store.get_timer_owner(timer.id)?;
    let tx = store.transaction()?;

    let mut cancelled = 0;
    let prev = runs_for_timer_conn(&tx, timer.id)?.last().cloned();
    if let Some(prev) = &prev {
        let unresolved = match get_run_state_conn(&tx, prev.run_id)? {
            Some(row) => !row.is_terminal(),
            None => {
                if owner.is_some() {
                    true
                } else {
                    prev.is_unfinished()
                }
            }
        };
        if unresolved {
            tx.enqueue_event(
                &EventRecord::new(RunState::Cancelled)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(prev.run_id)
                    .with_scheduled_for(prev.scheduled_for)
                    .with_message("timer deleted while its run was open"),
            )?;
            if let Some(mut row) = get_run_state_conn(&tx, prev.run_id)? {
                row.state = RunState::Cancelled.as_str().to_string();
                row.pickup_deadline = None;
                row.watchdog_deadline = None;
                update_run_state_conn(&tx, &row)?;
            }
            cancelled = 1;
        }
    }

    let deleted = Store::delete_timer_in_tx(&tx, timer.id)?;
    crate::store::Store::clear_timer_owner_in_tx(&tx, timer.id)?;
    // Drop the ack cursor and the SCH1 transport projections with the timer;
    // target cursors with no remaining projection go too.
    tx.execute(
        "DELETE FROM slot_event_acks WHERE timer_id = ?1",
        rusqlite::params![timer.id.to_string()],
    )
    .map_err(crate::store::StoreError::from)?;
    tx.execute(
        "DELETE FROM transport_projections WHERE timer_id = ?1",
        rusqlite::params![timer.id.to_string()],
    )
    .map_err(crate::store::StoreError::from)?;
    tx.execute(
        "DELETE FROM target_cursors
         WHERE NOT EXISTS (
             SELECT 1 FROM transport_projections tp
             WHERE tp.target_path = target_cursors.target_path
         )",
        [],
    )
    .map_err(crate::store::StoreError::from)?;
    tx.commit().map_err(|e| TreeError::Io(e.to_string()))?;
    Ok((deleted, cancelled))
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
