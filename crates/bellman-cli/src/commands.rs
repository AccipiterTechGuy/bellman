//! Command handlers: store CRUD + run-now fire path.

use crate::output;
use crate::parse;
use crate::resolve::{resolve_timer, ResolveError};
use bellman_core::scheduler::{FireAction, FireContext, FireKind};
use bellman_core::store::{
    NewTimer, OpenOptions, Store, StoreError, Timer, TimerPatch, TimerUpdate,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::path::Path;
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
                "fires": fires.iter().map(|t| t.to_rfc3339()).collect::<Vec<_>>(),
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
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "-".into()),
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
                            .map(|x| x.to_rfc3339())
                            .unwrap_or_else(|| "-".into())
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

/// Execute the timer action immediately via claim → [`FireAction`] → complete.
///
/// Real launch/notify actions land in C6. Until then the stub
/// [`LogLineAction`] writes one log line (also returned in JSON).
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

    let mut action = LogLineAction::default();
    let ctx = FireContext {
        timer: &timer,
        scheduled_for: claim.scheduled_for,
        run_id: claim.run_id,
        kind: FireKind::OnTime,
        claimed_at: claim.claimed_at,
    };
    if let Err(e) = action.on_fire(&ctx) {
        // Still complete so recovery does not loop (mirrors scheduler).
        let _ = store.complete_run(claim.run_id);
        return Err(CliError::new(CMD, "action_failed", e));
    }

    store
        .complete_run(claim.run_id)
        .map_err(|e| e.with_command(CMD))?;

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

    Ok(CommandPayload::RunNow {
        command: CMD,
        id: timer.id,
        name: timer.name.clone(),
        run_id: claim.run_id,
        scheduled_for,
        message: action
            .message
            .unwrap_or_else(|| "stub action completed".into()),
        timer,
    })
}

/// Stub wake action used until C6 real actions land.
#[derive(Debug, Default)]
struct LogLineAction {
    message: Option<String>,
}

impl FireAction for LogLineAction {
    fn on_fire(&mut self, ctx: &FireContext<'_>) -> Result<(), String> {
        let msg = format!(
            "bellman: run-now stub action timer_name={:?} timer_id={} run_id={} scheduled_for={}",
            ctx.timer.name,
            ctx.timer.id,
            ctx.run_id,
            ctx.scheduled_for.to_rfc3339()
        );
        // Human-visible log line on stderr so JSON stdout stays clean.
        eprintln!("{msg}");
        self.message = Some(msg);
        Ok(())
    }
}
