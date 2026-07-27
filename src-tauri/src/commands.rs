//! Tauri commands — every IPC call the webview can invoke.
//!
//! Each command is small and passes through to the underlying core service
//! or store. No scheduling logic lives here; the engine runs in its own
//! thread and the webview only pushes/pulls state.

use std::str::FromStr;

use bellman_core::events::EventRecord;
use bellman_core::store::{Timer, TimerPatch, TimerUpdate};
use bellman_core::RunNowOptions;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::config::Config;
use crate::first_run::{WizardChoice, WizardStatus};
use crate::state::{AppState, RunNowResponse};

/// `Timer` shape the webview consumes. Mirrors the core type but with a
/// `tz_name` shortcut so the UI does not have to dig into `occurrence.tz`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerDto {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub tz: String,
    pub next_fire_utc: Option<DateTime<Utc>>,
    pub last_fired: Option<DateTime<Utc>>,
    /// Pretty kind label: "interval", "daily", "cron", ...
    pub kind: String,
    /// Human-readable occurrence summary (e.g. "every 5s", "daily 09:30").
    pub summary: String,
    /// Pretty action label: "none", "launch: /usr/bin/true", "notify: hello".
    pub action: String,
    pub revision: i64,
}

impl From<Timer> for TimerDto {
    fn from(t: Timer) -> Self {
        let kind = kind_label(&t);
        let summary = summary_for(&t);
        let action = action_label(&t);
        Self {
            id: t.id,
            name: t.name,
            enabled: t.enabled,
            tz: t.tz,
            next_fire_utc: t.next_fire_utc,
            last_fired: t.last_fired,
            kind,
            summary,
            action,
            revision: t.revision,
        }
    }
}

fn kind_label(t: &Timer) -> String {
    use bellman_core::OccurrenceKind::*;
    match t.occurrence.kind() {
        Once { .. } => "once".into(),
        Interval { every_secs, .. } => format!("interval ({every_secs}s)"),
        Daily { .. } => "daily".into(),
        Weekly { .. } => "weekly".into(),
        Monthly { .. } => "monthly".into(),
        Yearly { .. } => "yearly".into(),
        Cron { .. } => "cron".into(),
    }
}

fn summary_for(t: &Timer) -> String {
    use bellman_core::OccurrenceKind::*;
    let tz = &t.tz;
    match t.occurrence.kind() {
        Interval { every_secs, .. } => format!("every {every_secs}s"),
        Daily { at } => format!("daily {} {tz}", at.format("%H:%M:%S")),
        Weekly { days, at } => format!(
            "weekly {} {at} {tz}",
            days.iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ),
        Monthly { day, at } => format!("monthly day {day} {at} {tz}"),
        Yearly { month, day, at } => format!("yearly {month}-{day} {at} {tz}"),
        Once { at } => format!("once {at} {tz}"),
        Cron { expr } => format!("cron `{expr}` {tz}"),
    }
}

fn action_label(t: &Timer) -> String {
    use bellman_core::Action;
    match &t.action {
        Action::None => "none".into(),
        Action::Launch { command, args, .. } => {
            if args.is_empty() {
                format!("launch: {command}")
            } else {
                format!("launch: {command} {}", args.join(" "))
            }
        }
        Action::Notify { title, .. } => format!("notify: {title}"),
    }
}

/// `list_timers` — return all timers in the store.
#[tauri::command]
pub fn list_timers(state: State<'_, AppState>) -> Result<Vec<TimerDto>, String> {
    let store = state.store.lock();
    let timers = store.list_timers().map_err(|e| e.to_string())?;
    Ok(timers.into_iter().map(TimerDto::from).collect())
}

/// `get_timer` — single timer by id.
#[tauri::command]
pub fn get_timer(state: State<'_, AppState>, id: String) -> Result<TimerDto, String> {
    let id = Uuid::from_str(&id).map_err(|e| format!("invalid id: {e}"))?;
    let store = state.store.lock();
    let timer = store
        .get_timer(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("timer not found: {id}"))?;
    Ok(TimerDto::from(timer))
}

/// `set_enabled` — toggle a timer's per-timer enabled flag (NOT the global
/// pause-all). Optimistic revision check; the caller passes the current
/// revision (webview already received it via list_timers).
#[tauri::command]
pub fn set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
    expected_revision: i64,
) -> Result<TimerDto, String> {
    let id = Uuid::from_str(&id).map_err(|e| format!("invalid id: {e}"))?;
    let mut store = state.store.lock();
    let updated = store
        .update_timer(TimerUpdate {
            id,
            expected_revision,
            patch: TimerPatch {
                enabled: Some(enabled),
                ..Default::default()
            },
        })
        .map_err(|e| e.to_string())?;
    // Wake the scheduler so the next tick picks up the change.
    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    Ok(TimerDto::from(updated))
}

/// `run_now` — execute the timer's action immediately through the real
/// fire path. Returns the updated timer + a one-line message describing
/// what happened.
#[tauri::command]
pub fn run_now(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<RunNowResponse, String> {
    let id = Uuid::from_str(&id).map_err(|e| format!("invalid id: {e}"))?;
    let db_path = state.data_dir.join("timers.db");
    let opts = RunNowOptions {
        notify_sink: Some(state.notify_sink.clone()),
        ..Default::default()
    };
    let mut store = state.store.lock();
    let outcome = bellman_core::run_now(&mut store, &db_path, id, &opts)
        .map_err(|e| e.to_string())?;
    drop(store);
    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    // Fire-and-forget UI hint: the webview re-polls list_timers after this
    // returns, so we don't need a separate event.
    let _ = app.emit("timer-fired", &outcome.timer.id.to_string());
    Ok(outcome.into())
}

/// `list_log_tail` — recent events from the JSONL log, optionally filtered
/// to a single timer.
#[derive(Debug, Deserialize)]
pub struct ListLogTailArgs {
    pub timer_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct LogTailDto {
    pub events: Vec<EventRecord>,
    pub total_records: usize,
    pub skipped: usize,
}

#[tauri::command]
pub fn list_log_tail(
    state: State<'_, AppState>,
    timer_id: Option<String>,
    limit: Option<usize>,
) -> Result<LogTailDto, String> {
    use std::path::Path;
    let path = state
        .data_dir
        .join("logs")
        .join("events.current.jsonl");
    if !Path::new(&path).exists() {
        return Ok(LogTailDto {
            events: vec![],
            total_records: 0,
            skipped: 0,
        });
    }
    let tid = match timer_id.as_deref() {
        None | Some("") => None,
        Some(s) => Some(Uuid::from_str(s).map_err(|e| format!("invalid timer_id: {e}"))?),
    };
    let (events, stats) = bellman_core::read_log_tail(&path, tid, limit)
        .map_err(|e| e.to_string())?;
    Ok(LogTailDto {
        total_records: stats.records,
        skipped: stats.skipped,
        events,
    })
}

/// `get_pause_all` / `set_pause_all` — the global pause-all flag.
#[tauri::command]
pub fn get_pause_all(state: State<'_, AppState>) -> bool {
    state.pause_all()
}

#[tauri::command]
pub fn set_pause_all(
    app: AppHandle,
    state: State<'_, AppState>,
    paused: bool,
) -> Result<bool, String> {
    state.set_pause_all(paused);
    let _ = app.emit("pause-all-changed", paused);
    Ok(state.pause_all())
}

/// `wizard_status` / `wizard_set_choice` / `wizard_re_run` — first-run
/// wizard. The webview calls these; we persist + apply.
#[tauri::command]
pub fn wizard_status(state: State<'_, AppState>) -> WizardStatus {
    let cfg = state.config.lock().clone();
    WizardStatus {
        completed: cfg.wizard_completed,
        defaults: WizardChoice {
            autostart: cfg.autostart_enabled,
            start_minimized: cfg.start_minimized,
            wake_enabled: cfg.wake_enabled,
        },
    }
}

#[tauri::command]
pub fn wizard_set_choice(
    app: AppHandle,
    state: State<'_, AppState>,
    choice: WizardChoice,
) -> Result<WizardStatus, String> {
    let cfg = Config::record_wizard_choice(&state.data_dir, choice).map_err(|e| e.to_string())?;
    *state.config.lock() = cfg.clone();
    // Apply the autostart bit immediately.
    let _ = apply_autostart(&app, cfg.autostart_enabled);
    Ok(WizardStatus {
        completed: cfg.wizard_completed,
        defaults: WizardChoice {
            autostart: cfg.autostart_enabled,
            start_minimized: cfg.start_minimized,
            wake_enabled: cfg.wake_enabled,
        },
    })
}

#[tauri::command]
pub fn wizard_re_run(state: State<'_, AppState>) -> WizardStatus {
    let mut cfg = state.config.lock().clone();
    cfg.wizard_completed = false;
    WizardStatus {
        completed: false,
        defaults: WizardChoice {
            autostart: cfg.autostart_enabled,
            start_minimized: cfg.start_minimized,
            wake_enabled: cfg.wake_enabled,
        }
    }
}

/// `app_info` — app metadata the UI uses in headers / settings.
#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub data_dir: String,
    pub db_path: String,
    pub logs_dir: String,
    pub slots_dir: String,
    pub wizard_completed: bool,
    pub autostart_enabled: bool,
    pub pause_all: bool,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    let cfg = state.config.lock().clone();
    AppInfo {
        data_dir: state.data_dir.display().to_string(),
        db_path: state.data_dir.join("timers.db").display().to_string(),
        logs_dir: state.data_dir.join("logs").display().to_string(),
        slots_dir: state.data_dir.join("slots").display().to_string(),
        wizard_completed: cfg.wizard_completed,
        autostart_enabled: cfg.autostart_enabled,
        pause_all: state.pause_all(),
    }
}

// --- helpers (not commands) ---

/// Enable / disable autostart to match the wizard answer.
fn apply_autostart(app: &AppHandle, enabled: bool) -> tauri::Result<()> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    let cur = mgr.is_enabled().unwrap_or(false);
    if enabled && !cur {
        let _ = mgr.enable();
    } else if !enabled && cur {
        let _ = mgr.disable();
    }
    Ok(())
}
