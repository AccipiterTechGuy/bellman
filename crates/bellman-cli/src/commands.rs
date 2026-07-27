//! Command handlers: store CRUD + run-now fire path + slot-submit.

use crate::output;
use crate::parse;
use crate::resolve::{resolve_timer, ResolveError};
use bellman_core::actions::{ActionRunner, ActionRunnerConfig};
use bellman_core::events::{EventLog, EventLogConfig, EventRecord, EventKind};
use bellman_core::scheduler::{FireAction, FireContext, FireKind};
use bellman_core::slots::{
    SlotConfig, SlotRequest, SlotService, SlotStatus, SCHEMA_V1,
};
use bellman_core::store::{
    NewTimer, OpenOptions, Store, StoreError, Timer, TimerPatch, TimerUpdate,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ── Result types ────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct CliError {
    pub command: &'static str,
    pub code: &'static str,
    pub message: String,
}

impl CliError {
    pub fn new(command: &'static str, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            command,
            code,
            message: message.into(),
        }
    }
}

impl From<ResolveError> for CliError {
    fn from(e: ResolveError) -> Self {
        // command filled by caller via with_command
        Self::new("", e.code(), e.to_string())
    }
}

trait WithCommand {
    fn with_command(self, command: &'static str) -> CliError;
}

impl WithCommand for ResolveError {
    fn with_command(self, command: &'static str) -> CliError {
        CliError::new(command, self.code(), self.to_string())
    }
}

impl WithCommand for StoreError {
    fn with_command(self, command: &'static str) -> CliError {
        let code = match &self {
            StoreError::NotFound(_) => "not_found",
            StoreError::StaleRevision { .. } => "stale_revision",
            StoreError::InvalidOccurrence(_) => "invalid_occurrence",
            StoreError::NetworkFilesystem(_) => "network_filesystem",
            StoreError::AlreadyClaimed { .. } => "already_claimed",
            _ => "store_error",
        };
        CliError::new(command, code, self.to_string())
    }
}

/// Successful command payload (JSON + human).
pub enum CommandPayload {
    Timer {
        command: &'static str,
        timer: Timer,
    },
    Timers {
        command: &'static str,
        timers: Vec<Timer>,
    },
    Deleted {
        command: &'static str,
        id: Uuid,
        name: String,
    },
    Next {
        command: &'static str,
        id: Uuid,
        name: String,
        n: usize,
        fires: Vec<DateTime<Utc>>,
    },
    RunNow {
        command: &'static str,
        id: Uuid,
        name: String,
        run_id: Uuid,
        scheduled_for: DateTime<Utc>,
        message: String,
        timer: Timer,
    },
    SlotSubmit {
        command: &'static str,
        request_id: String,
        slot_id: String,
        status: String,
        timer_id: Option<Uuid>,
        next_fire: Option<DateTime<Utc>>,
        error: Option<String>,
        processed: usize,
        response: Value,
    },
}

impl CommandPayload {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Timer { command, timer } => json!({
                "command": command,
                "timer": output::timer_json(timer),
            }),
            Self::Timers { command, timers } => json!({
                "command": command,
                "count": timers.len(),
                "timers": timers.iter().map(output::timer_json).collect::<Vec<_>>(),
            }),
            Self::Deleted { command, id, name } => json!({
                "command": command,
                "id": id,
                "name": name,
                "deleted": true,
            }),
            Self::Next {
                command,
                id,
                name,
                n,
                fires,
            } => json!({
                "command": command,
                "id": id,
                "name": name,
                "n": n,
                "fires": fires.iter().map(chrono::DateTime::to_rfc3339).collect::<Vec<_>>(),
            }),
            Self::RunNow {
                command,
                id,
                name,
                run_id,
                scheduled_for,
                message,
                timer,
            } => json!({
                "command": command,
                "id": id,
                "name": name,
                "run_id": run_id,
                "scheduled_for": scheduled_for.to_rfc3339(),
                "message": message,
                "timer": output::timer_json(timer),
            }),
            Self::SlotSubmit {
                command,
                request_id,
                slot_id,
                status,
                timer_id,
                next_fire,
                error,
                processed,
                response,
            } => json!({
                "command": command,
                "request_id": request_id,
                "slot_id": slot_id,
                "status": status,
                "timer_id": timer_id,
                "next_fire": next_fire.map(|t| t.to_rfc3339()),
                "error": error,
                "processed": processed,
                "response": response,
            }),
        }
    }

    pub fn to_human(&self) -> String {
        match self {
            Self::Timer { command, timer } => format!(
                "{command}: id={} name={:?} enabled={} next_fire={} revision={}",
                timer.id,
                timer.name,
                timer.enabled,
                timer
                    .next_fire_utc
                    .map_or_else(|| "-".into(), |t| t.to_rfc3339()),
                timer.revision
            ),
            Self::Timers { timers, .. } => {
                if timers.is_empty() {
                    return "no timers".into();
                }
                let mut lines = Vec::with_capacity(timers.len());
                for t in timers {
                    lines.push(format!(
                        "{}\t{}\tenabled={}\tnext={}",
                        t.id,
                        t.name,
                        t.enabled,
                        t.next_fire_utc
                            .map_or_else(|| "-".into(), |x| x.to_rfc3339())
                    ));
                }
                lines.join("\n")
            }
            Self::Deleted { id, name, .. } => format!("deleted id={id} name={name:?}"),
            Self::Next {
                name, id, fires, ..
            } => {
                let mut lines = vec![format!("next fires for {name:?} ({id}):")];
                for (i, f) in fires.iter().enumerate() {
                    lines.push(format!("  {}: {}", i + 1, f.to_rfc3339()));
                }
                if fires.is_empty() {
                    lines.push("  (none)".into());
                }
                lines.join("\n")
            }
            Self::RunNow {
                name,
                id,
                run_id,
                scheduled_for,
                message,
                ..
            } => format!(
                "run-now name={name:?} id={id} run_id={run_id} scheduled_for={}\n{message}",
                scheduled_for.to_rfc3339()
            ),
            Self::SlotSubmit {
                request_id,
                slot_id,
                status,
                timer_id,
                next_fire,
                error,
                processed,
                ..
            } => {
                let mut s = format!(
                    "slot-submit status={status} request_id={request_id} slot_id={slot_id} processed={processed}"
                );
                if let Some(id) = timer_id {
                    s.push_str(&format!(" timer_id={id}"));
                }
                if let Some(nf) = next_fire {
                    s.push_str(&format!(" next_fire={}", nf.to_rfc3339()));
                }
                if let Some(e) = error {
                    s.push_str(&format!(" error={e}"));
                }
                s
            }
        }
    }
}

// ── Store open ──────────────────────────────────────────────────────────

fn open_store(db: &Path) -> Result<Store, CliError> {
    // CLI always allows local paths; network FS still refused by default.
    Store::open_with(
        db,
        OpenOptions {
            refuse_network_fs: true,
            ..OpenOptions::default()
        },
    )
    .map_err(|e| e.with_command("open"))
}

// ── Commands ────────────────────────────────────────────────────────────

pub struct AddArgs {
    pub name: String,
    pub occurrence: String,
    pub time: Option<String>,
    pub tz: Option<String>,
    pub every_secs: Option<u64>,
    pub days: Option<String>,
    pub day: Option<u8>,
    pub month: Option<u8>,
    pub cron: Option<String>,
    pub tags: Vec<String>,
}

pub fn add(db: &Path, args: AddArgs) -> Result<CommandPayload, CliError> {
    const CMD: &str = "add";
    let occ = parse::build_occurrence(&parse::BuildOccurrence {
        kind: args.occurrence,
        time: args.time,
        tz: args.tz,
        every_secs: args.every_secs,
        days: args.days,
        day: args.day,
        month: args.month,
        cron: args.cron,
    })
    .map_err(|m| CliError::new(CMD, "invalid_args", m))?;

    let mut new = NewTimer::new(args.name, occ);
    new.tags = args.tags;

    let mut store = open_store(db).map_err(|mut e| {
        e.command = CMD;
        e
    })?;
    let timer = store
        .create_timer(new)
        .map_err(|e| e.with_command(CMD))?;

    // Lifecycle: every successful registration appends a `registered` line.
    let logs_dir = resolve_logs_dir(db);
    if let Ok(mut log) = EventLog::open(EventLogConfig::new(&logs_dir)) {
        let _ = log.emit(
            EventRecord::new(EventKind::Registered)
                .with_timer(timer.id, timer.name.clone())
                .with_message("cli add"),
        );
    }

    Ok(CommandPayload::Timer {
        command: CMD,
        timer,
    })
}

pub fn list(db: &Path) -> Result<CommandPayload, CliError> {
    const CMD: &str = "list";
    let store = open_store(db).map_err(|mut e| {
        e.command = CMD;
        e
    })?;
    let timers = store.list_timers().map_err(|e| e.with_command(CMD))?;
    Ok(CommandPayload::Timers {
        command: CMD,
        timers,
    })
}

pub struct EditArgs {
    pub name: Option<String>,
    pub time: Option<String>,
    pub every_secs: Option<u64>,
    pub cron: Option<String>,
    pub days: Option<String>,
    pub day: Option<u8>,
    pub month: Option<u8>,
    pub enabled: Option<String>,
}

pub fn edit(db: &Path, name_or_id: &str, args: EditArgs) -> Result<CommandPayload, CliError> {
    const CMD: &str = "edit";
    let mut store = open_store(db).map_err(|mut e| {
        e.command = CMD;
        e
    })?;
    let timer = resolve_timer(&store, name_or_id).map_err(|e| e.with_command(CMD))?;

    let enabled = match args.enabled.as_deref() {
        Some(s) => Some(parse::parse_enabled(s).map_err(|m| CliError::new(CMD, "invalid_args", m))?),
        None => None,
    };

    let occurrence = parse::patch_occurrence(
        &timer.occurrence,
        &parse::PatchOccurrence {
            time: args.time,
            every_secs: args.every_secs,
            cron: args.cron,
            days: args.days,
            day: args.day,
            month: args.month,
        },
    )
    .map_err(|m| CliError::new(CMD, "invalid_args", m))?;

    if args.name.is_none() && occurrence.is_none() && enabled.is_none() {
        return Err(CliError::new(
            CMD,
            "invalid_args",
            "nothing to edit (pass --name, --time, --enabled, …)",
        ));
    }

    let timer = store
        .update_timer(TimerUpdate {
            id: timer.id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                name: args.name,
                enabled,
                occurrence,
                ..Default::default()
            },
        })
        .map_err(|e| e.with_command(CMD))?;

    Ok(CommandPayload::Timer {
        command: CMD,
        timer,
    })
}

pub fn rm(db: &Path, name_or_id: &str) -> Result<CommandPayload, CliError> {
    const CMD: &str = "rm";
    let mut store = open_store(db).map_err(|mut e| {
        e.command = CMD;
        e
    })?;
    let timer = resolve_timer(&store, name_or_id).map_err(|e| e.with_command(CMD))?;
    let deleted = store
        .delete_timer(timer.id)
        .map_err(|e| e.with_command(CMD))?;
    if !deleted {
        return Err(CliError::new(
            CMD,
            "not_found",
            format!("timer not found: {}", timer.id),
        ));
    }
    Ok(CommandPayload::Deleted {
        command: CMD,
        id: timer.id,
        name: timer.name,
    })
}

pub fn next(db: &Path, name_or_id: &str, n: usize) -> Result<CommandPayload, CliError> {
    const CMD: &str = "next";
    let store = open_store(db).map_err(|mut e| {
        e.command = CMD;
        e
    })?;
    let timer = resolve_timer(&store, name_or_id).map_err(|e| e.with_command(CMD))?;
    let tz = timer.occurrence.timezone();
    let after = Utc::now().with_timezone(&tz);
    let local_fires = timer.occurrence.preview(after, n);
    let fires: Vec<DateTime<Utc>> = local_fires
        .into_iter()
        .map(|dt| dt.with_timezone(&Utc))
        .collect();
    Ok(CommandPayload::Next {
        command: CMD,
        id: timer.id,
        name: timer.name,
        n,
        fires,
    })
}

pub fn pause(db: &Path, name_or_id: &str) -> Result<CommandPayload, CliError> {
    set_enabled(db, name_or_id, false, "pause")
}

pub fn resume(db: &Path, name_or_id: &str) -> Result<CommandPayload, CliError> {
    set_enabled(db, name_or_id, true, "resume")
}

fn set_enabled(
    db: &Path,
    name_or_id: &str,
    enabled: bool,
    command: &'static str,
) -> Result<CommandPayload, CliError> {
    let mut store = open_store(db).map_err(|mut e| {
        e.command = command;
        e
    })?;
    let timer = resolve_timer(&store, name_or_id).map_err(|e| e.with_command(command))?;
    let timer = store
        .update_timer(TimerUpdate {
            id: timer.id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                enabled: Some(enabled),
                ..Default::default()
            },
        })
        .map_err(|e| e.with_command(command))?;
    Ok(CommandPayload::Timer { command, timer })
}

/// Execute the timer action immediately via claim → [`ActionRunner`] → complete.
///
/// Uses the real launch / notify / write-slot path with event-log recording
/// under `<db parent>/logs/` (or `$BELLMAN_LOGS` when set). When a slots root
/// is available (`$BELLMAN_SLOTS` or `<db parent>/slots`), fire delivery also
/// writes trigger data into `slots/done/` (launch + write JSON).
pub fn run_now(db: &Path, name_or_id: &str) -> Result<CommandPayload, CliError> {
    const CMD: &str = "run-now";
    let mut store = open_store(db).map_err(|mut e| {
        e.command = CMD;
        e
    })?;
    let timer = resolve_timer(&store, name_or_id).map_err(|e| e.with_command(CMD))?;
    let scheduled_for = Utc::now();

    let claim = store
        .claim_run(timer.id, scheduled_for)
        .map_err(|e| e.with_command(CMD))?;

    let logs_dir = resolve_logs_dir(db);
    let event_log = EventLog::open(EventLogConfig::new(&logs_dir)).map_err(|e| {
        CliError::new(CMD, "store_error", format!("open event log: {e}"))
    })?;

    // Resolve slots root for write-output-slot (integrating apps' done/ files).
    let slots_root = resolve_slots_root_optional(db);
    let slot_rec = store
        .latest_slot_request_for_timer(timer.id)
        .map_err(|e| e.with_command(CMD))?;
    let write_slot_file = slot_rec
        .as_ref()
        .map(|r| format!("slot-{}.json", r.slot_id));
    let write_slot_dir = slots_root.as_ref().map(|r| r.join("done"));

    let mut action = ActionRunner::new(ActionRunnerConfig {
        write_slot_dir: write_slot_dir.clone(),
        write_slot_file: write_slot_file.clone(),
        ..Default::default()
    })
    .with_event_log(event_log);

    let ctx = FireContext {
        timer: &timer,
        scheduled_for: claim.scheduled_for,
        run_id: claim.run_id,
        kind: FireKind::OnTime,
        claimed_at: claim.claimed_at,
    };
    let action_res = action.on_fire(&ctx);
    let message = action
        .last_message
        .clone()
        .unwrap_or_else(|| "action completed".into());

    // Always complete so recovery does not loop (mirrors scheduler).
    store
        .complete_run(claim.run_id)
        .map_err(|e| e.with_command(CMD))?;

    if let Err(e) = action_res {
        return Err(CliError::new(CMD, "action_failed", e));
    }

    // Advance last_fired + record_run so next_fire moves past this slot —
    // same bookkeeping as the scheduler's mark_fired path.
    let mut occ = timer.occurrence.clone();
    occ.record_run();
    let timer = store
        .update_timer(TimerUpdate {
            id: timer.id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                last_fired: Some(Some(scheduled_for)),
                occurrence: Some(occ),
                ..Default::default()
            },
        })
        .map_err(|e| e.with_command(CMD))?;

    // Rewrite integrator output slot with unacked run events (status + next_fire).
    // The ActionRunner already wrote bellman-fire/1 payload into the same path
    // when write_slot_file was set; overlay the full SlotResponse so consumers
    // that parse done/ as bellman-slot/1 still see events + next_fire.
    if let (Some(root), Some(rec)) = (slots_root.as_ref(), slot_rec.as_ref()) {
        let _ = publish_fire_slot_response(&store, root, &timer, rec);
    }

    Ok(CommandPayload::RunNow {
        command: CMD,
        id: timer.id,
        name: timer.name.clone(),
        run_id: claim.run_id,
        scheduled_for,
        message,
        timer,
    })
}

/// Publish an updated `done/slot-<id>.json` with unacked run events after a fire.
fn publish_fire_slot_response(
    store: &Store,
    slots_root: &Path,
    timer: &Timer,
    rec: &bellman_core::store::SlotRequestRecord,
) -> Result<(), String> {
    use bellman_core::slots::{atomic_write_json, SlotResponse, SlotRunEvent, SlotStatus, SCHEMA_V1};

    let runs = store
        .unacked_runs_for_timer(timer.id, 64)
        .map_err(|e| e.to_string())?;
    let events: Vec<SlotRunEvent> = runs
        .into_iter()
        .map(|run| SlotRunEvent {
            event_sequence: run.event_sequence,
            run_id: run.run_id,
            timer_id: run.timer_id,
            scheduled_for: run.scheduled_for,
            status: run.status.as_str().to_string(),
            claimed_at: run.claimed_at,
            completed_at: run.completed_at,
        })
        .collect();

    let response = SlotResponse {
        schema: SCHEMA_V1.to_string(),
        slot_id: rec.slot_id.clone(),
        request_id: rec.request_id.clone(),
        status: SlotStatus::Ok,
        timer_id: Some(timer.id),
        next_fire: timer.next_fire_utc,
        error: None,
        events,
    };
    let name = format!("slot-{}.json", rec.slot_id);
    let done = slots_root.join("done");
    atomic_write_json(&done, &name, &response).map_err(|e| e.to_string())?;
    Ok(())
}

/// Prefer `$BELLMAN_SLOTS`, else `<db parent>/slots` when that directory exists.
fn resolve_slots_root_optional(db: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BELLMAN_SLOTS") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let candidate = db.parent()?.join("slots");
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

/// Publish a slot request JSON file and process it against the local store.
///
/// Reads `request_path` (a complete `bellman-slot/1` envelope or a minimal
/// payload that is wrapped), publishes into `slots_dir/free/`, then runs one
/// [`SlotService::poll`] so the timer lands without a long-running daemon.
pub fn slot_submit(
    db: &Path,
    request_path: &Path,
    slots_dir: &Path,
) -> Result<CommandPayload, CliError> {
    const CMD: &str = "slot-submit";
    let mut store = open_store(db).map_err(|mut e| {
        e.command = CMD;
        e
    })?;

    let raw = fs::read_to_string(request_path).map_err(|e| {
        CliError::new(
            CMD,
            "invalid_args",
            format!("read {}: {e}", request_path.display()),
        )
    })?;
    let mut request: SlotRequest = serde_json::from_str(&raw).map_err(|e| {
        CliError::new(
            CMD,
            "invalid_args",
            format!("parse slot request JSON: {e}"),
        )
    })?;
    if request.schema.is_empty() {
        request.schema = SCHEMA_V1.to_string();
    }
    if request.operation.is_none() {
        return Err(CliError::new(
            CMD,
            "invalid_args",
            "request.operation is required (add|modify|delete)",
        ));
    }
    if request
        .request_id
        .as_ref()
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        request.request_id = Some(Uuid::new_v4().to_string());
    }
    if request.ts.is_none() {
        request.ts = Some(Utc::now());
    }

    let service = SlotService::open(slots_dir, SlotConfig::default()).map_err(|e| {
        CliError::new(CMD, "store_error", format!("open slots: {e}"))
    })?;

    let request_id = request
        .request_id
        .clone()
        .expect("request_id filled above");

    service.publish(request).map_err(|e| {
        CliError::new(CMD, "store_error", format!("publish: {e}"))
    })?;

    let processed = service.poll(&mut store).map_err(|e| {
        CliError::new(CMD, "store_error", format!("poll slots: {e}"))
    })?;

    let rec = store
        .get_slot_request(&request_id)
        .map_err(|e| e.with_command(CMD))?
        .ok_or_else(|| {
            CliError::new(
                CMD,
                "store_error",
                format!("no ledger row for request_id={request_id}"),
            )
        })?;

    let response: bellman_core::slots::SlotResponse =
        serde_json::from_str(&rec.response_json).map_err(|e| {
            CliError::new(
                CMD,
                "store_error",
                format!("parse response JSON: {e}"),
            )
        })?;
    let response_val =
        serde_json::to_value(&response).unwrap_or(json!(null));
    let status = response.status.as_str().to_string();
    let slot_id = response.slot_id.clone();
    let timer_id = response.timer_id;
    let next_fire = response.next_fire;
    let error = response.error.clone();

    // Log registration when a timer was created/updated successfully.
    if response.status == SlotStatus::Ok {
        if let Some(tid) = timer_id {
            let logs_dir = resolve_logs_dir(db);
            if let Ok(mut log) = EventLog::open(EventLogConfig::new(&logs_dir)) {
                let name = store
                    .get_timer(tid)
                    .ok()
                    .flatten()
                    .map(|t| t.name)
                    .unwrap_or_default();
                let _ = log.emit(
                    EventRecord::new(EventKind::Registered)
                        .with_timer(tid, name)
                        .with_message(format!("slot-submit {request_id}")),
                );
            }
        }
    }

    Ok(CommandPayload::SlotSubmit {
        command: CMD,
        request_id,
        slot_id,
        status,
        timer_id,
        next_fire,
        error,
        processed,
        response: response_val,
    })
}

fn resolve_logs_dir(db: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("BELLMAN_LOGS") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    db.parent()
        .map(|p| p.join("logs"))
        .unwrap_or_else(|| PathBuf::from("logs"))
}
