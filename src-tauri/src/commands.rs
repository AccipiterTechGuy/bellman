//! Tauri commands — every IPC call the webview can invoke.
//!
//! Each command is small and passes through to the underlying core service
//! or store. No scheduling logic lives here; the engine runs in its own
//! thread and the webview only pushes/pulls state.

use std::str::FromStr;

use bellman_core::events::EventRecord;
use bellman_core::store::{Action, Timer, TimerPatch, TimerUpdate};
use bellman_core::Occurrence;
use bellman_core::RunNowOptions;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::config::Config;
use crate::first_run::{WizardChoice, WizardStatus};
use crate::occurrence_input::{self, CreateTimerInput, OccurrenceInput, PreviewFire};
use crate::state::{AppState, RunNowResponse};

/// `Timer` shape the webview consumes. Mirrors the core type but with a
/// `tz_name` shortcut so the UI does not have to dig into `occurrence.tz`.
///
/// Serialized as **camelCase** so the webview's idiomatic JS bindings
/// (`timer.nextFireUtc`, `timer.lastFired`) match the Rust field names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    let outcome =
        bellman_core::run_now(&mut store, &db_path, id, &opts).map_err(|e| e.to_string())?;
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
/// Recent events from the JSONL log. camelCase at the IPC boundary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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
    let path = state.data_dir.join("logs").join("events.current.jsonl");
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
    let (events, stats) =
        bellman_core::read_log_tail(&path, tid, limit).map_err(|e| e.to_string())?;
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
    // Keep the tray's "Pause all" check item in sync with the in-window
    // toggle. Silently no-ops when the tray is not installed.
    crate::tray::set_tray_pause_check(&app, paused);
    // Emit a bool payload (consistent with the tray-side emit).
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
        },
    }
}

/// `app_info` — app metadata the UI uses in headers / settings. camelCase
/// at the IPC boundary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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

// ── C8 calendar UI commands ──────────────────────────────────────────
//
// The dialog / Week / Month / Run history pages read and mutate timers
// through these. They are intentionally thin wrappers around `Store`
// (and `Occurrence::preview` for the live next-5 preview) so the same
// validation surface backs both the GUI and the CLI (`bellman add|edit|rm|next`).

/// `create_timer` — insert a fresh timer. Mirrors `bellman add`.
#[tauri::command]
pub fn create_timer(
    state: State<'_, AppState>,
    input: CreateTimerInput,
) -> Result<TimerDto, String> {
    let new = input.into_new_timer()?;
    let mut store = state.store.lock();
    let timer = store.create_timer(new).map_err(|e| e.to_string())?;
    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    Ok(TimerDto::from(timer))
}

/// `update_timer` — apply a partial patch. Mirrors `bellman edit`.
/// The caller passes the current revision (from `listTimers`) so concurrent
/// edits stay consistent with the store's optimistic-update contract.
#[tauri::command]
pub fn update_timer(
    state: State<'_, AppState>,
    id: String,
    expected_revision: i64,
    patch: TimerPatchDto,
) -> Result<TimerDto, String> {
    let id = Uuid::from_str(&id).map_err(|e| format!("invalid id: {e}"))?;
    let core_patch = patch.into_core_patch()?;
    let mut store = state.store.lock();
    let updated = store
        .update_timer(TimerUpdate {
            id,
            expected_revision,
            patch: core_patch,
        })
        .map_err(|e| e.to_string())?;
    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    Ok(TimerDto::from(updated))
}

/// `delete_timer` — remove by id. Mirrors `bellman rm <name|id>`.
#[tauri::command]
pub fn delete_timer(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    let id = Uuid::from_str(&id).map_err(|e| format!("invalid id: {e}"))?;
    let mut store = state.store.lock();
    let n = store.delete_timer(id).map_err(|e| e.to_string())?;
    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    Ok(n)
}

/// `preview_fires` — return the next-N fires for a draft occurrence. Wired
/// to the dialog's live preview pane; identical math to `bellman next`.
#[derive(Debug, Deserialize)]
pub struct PreviewArgs {
    pub input: OccurrenceInput,
    #[serde(default = "default_preview_n")]
    pub n: usize,
}

fn default_preview_n() -> usize {
    5
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewResponseDto {
    pub fires: Vec<PreviewFireDto>,
    /// DST / month-day-clamp warnings keyed off the user's current input.
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFireDto {
    pub utc: DateTime<Utc>,
    pub local_date: String,
    pub local_time: String,
    pub offset: String,
    pub tz_name: String,
}

impl From<PreviewFire> for PreviewFireDto {
    fn from(p: PreviewFire) -> Self {
        Self {
            utc: p.utc,
            local_date: p.local_date,
            local_time: p.local_time,
            offset: p.offset,
            tz_name: p.tz_name,
        }
    }
}

#[tauri::command]
pub fn preview_fires(
    input: OccurrenceInput,
    n: Option<usize>,
) -> Result<PreviewResponseDto, String> {
    let n = n.unwrap_or(5);
    let occ = input.clone().build()?;
    let fires = occurrence_input::preview_fires(&input, n)?;
    let mut warnings: Vec<String> = Vec::new();
    if let Some(w) = occurrence_input::dst_warning(occ.kind(), occ.tz_name()) {
        warnings.push(w);
    }
    Ok(PreviewResponseDto {
        fires: fires.into_iter().map(PreviewFireDto::from).collect(),
        warnings,
    })
}

/// Optional fields on the dialog's edit patch. Every field is optional;
/// `None` means "leave unchanged" (matching `TimerPatch::None` semantics).
///
/// We accept the loose JSON shape and translate into the typed
/// `TimerPatch` only after building an `Occurrence` (so all validation
/// runs in one place). Only the fields the dialog exposes are
/// serialized — adding more is a non-breaking wire change.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimerPatchDto {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    /// Replacement occurrence (must be a complete `OccurrenceInput`).
    /// Sent as the full new shape rather than a delta so the GUI cannot
    /// desync a partial edit.
    pub occurrence: Option<OccurrenceInput>,
    pub action: Option<Action>,
}

impl TimerPatchDto {
    fn into_core_patch(self) -> Result<TimerPatch, String> {
        let occurrence = match self.occurrence {
            None => None,
            Some(input) => {
                let occ: Occurrence = input.build()?;
                Some(occ)
            }
        };
        Ok(TimerPatch {
            name: self.name,
            enabled: self.enabled,
            occurrence,
            action: self.action,
            ..Default::default()
        })
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
