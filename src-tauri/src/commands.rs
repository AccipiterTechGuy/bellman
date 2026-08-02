//! Tauri commands — every IPC call the webview can invoke.
// Tauri injects `State<'_, T>` / `AppHandle` by value into every command;
// taking them by reference is not an option for the `#[tauri::command]` ABI.
#![allow(clippy::needless_pass_by_value)]
//!
//! Each command is small and passes through to the underlying core service
//! or store. No scheduling logic lives here; the engine runs in its own
//! thread and the webview only pushes/pulls state.
//!
//! The IPC wire shape is the deliberate `web`-module DTO set
//! (`WebTimerDto`, `WebOccurrenceDto`, `WebActionDto`). Re-exported here
//! under their `#[tauri::command]` arg/return types so the dialog and
//! the calendar pages have a single flat shape (no nested serde enum).

use std::str::FromStr;

use bellman_core::events::EventRecord;
use bellman_core::store::NewTimer;
use bellman_core::RunNowOptions;
use chrono::offset::Offset as _Offset;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::config;
use crate::demo::DemoInfoDto;
use crate::first_run::{WizardChoice, WizardStatus};
use crate::occurrence_input::{self, PreviewFire};
use crate::state::{AppState, RunNowResponse};
use crate::web::WebTimerPatchDto;

/// Re-export the deliberate UI-shaped DTO under its command-facing name.
/// See `src-tauri/src/web.rs` for the wire contract (fixture-pinned by
/// `web::tests::weekly_dto_matches_pinned_json_fixture`).
pub use crate::web::WebTimerDto as TimerDto;

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

/// IK5: the current run of an integration-owned timer, projected from the
/// `run_states` table — the same truth `status.json` mirrors. Absent
/// optional fields are omitted entirely (absence is not a state; the GUI
/// renders nothing for them). camelCase at the IPC boundary.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStateDto {
    /// The timer this run belongs to.
    pub timer_id: String,
    /// Its display name, so the row needs no second lookup.
    pub timer_name: String,
    /// Identity of the firing.
    pub run_id: String,
    /// R5 wire state (`fired` / `acknowledged` / `running` / `completed` /
    /// `failed` / `no_ack` / `cancelled`).
    pub state: String,
    /// The integration owner snapshotted at fire.
    pub app_name: String,
    /// The overdue label's anchor: `overdue` ⇔ `now − fired_at >
    /// expected_secs`, computed by the GUI at render time.
    pub fired_at: DateTime<Utc>,
    /// When the app picked the run up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    /// The app's estimate; the overdue label compares against it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_secs: Option<u64>,
    /// Whether the app opted into the silence watchdog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detection: Option<bool>,
    /// Last liveness ping — visible here and nowhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_at: Option<DateTime<Utc>>,
    /// The app's own progress text, rendered verbatim in the row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// When the app reported success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// When the run was recorded as failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<DateTime<Utc>>,
    /// `reported` (the app said it) vs `timed_out` (the opt-in watchdog).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    /// The app's failure text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When the pickup grace lapsed unanswered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_ack_at: Option<DateTime<Utc>>,
    /// The app's result payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Set when `result` was trimmed to fit the cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_truncated: Option<bool>,
}

impl RunStateDto {
    pub(crate) fn from_row(timer: &bellman_core::Timer, row: &bellman_core::RunStateRow) -> Self {
        Self {
            timer_id: timer.id.to_string(),
            timer_name: timer.name.clone(),
            run_id: row.run_id.to_string(),
            state: row.state.clone(),
            app_name: row.app_name.clone(),
            fired_at: row.fired_at,
            acknowledged_at: row.acknowledged_at,
            expected_secs: row.expected_secs,
            error_detection: row.error_detection,
            heartbeat_at: row.heartbeat_at,
            progress: row.progress.clone(),
            completed_at: row.completed_at,
            failed_at: row.failed_at,
            failure_kind: row.failure_kind.map(|k| k.as_str().to_string()),
            reason: row.reason.clone(),
            no_ack_at: row.no_ack_at,
            result: row.result_json.clone(),
            result_truncated: row.result_truncated.then_some(true),
        }
    }
}

/// `list_run_states` — IK5: the CURRENT run of every integration-owned timer
/// (or of one timer when `timer_id` is given — the `run-status-changed`
/// invalidation refetches only the affected row). Reads the database, never
/// the event log. Unowned action-only timers have no lifecycle row and never
/// appear here: their `status.json: fired` is a firing snapshot, not an app
/// claiming to work.
#[tauri::command]
pub fn list_run_states(
    state: State<'_, AppState>,
    timer_id: Option<String>,
) -> Result<Vec<RunStateDto>, String> {
    let store = state.store.lock();
    collect_run_states(&store, timer_id.as_deref())
}

fn collect_run_states(
    store: &bellman_core::store::Store,
    timer_id: Option<&str>,
) -> Result<Vec<RunStateDto>, String> {
    let timers: Vec<bellman_core::Timer> = match timer_id {
        None | Some("") => store.list_timers().map_err(|e| e.to_string())?,
        Some(s) => {
            let id = Uuid::from_str(s).map_err(|e| format!("invalid timer_id: {e}"))?;
            store
                .get_timer(id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .collect()
        }
    };
    let mut out = Vec::new();
    for timer in &timers {
        if let Some(row) = store
            .current_run_state(timer.id)
            .map_err(|e| e.to_string())?
        {
            out.push(RunStateDto::from_row(timer, &row));
        }
    }
    Ok(out)
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
        .update_timer(bellman_core::store::TimerUpdate {
            id,
            expected_revision,
            patch: bellman_core::store::TimerPatch {
                enabled: Some(enabled),
                ..Default::default()
            },
        })
        .map_err(|e| e.to_string())?;
    // IK2: enabled flag lives in timer.json too — resync the view.
    {
        let tree = bellman_core::TimersTree::new(&state.data_dir);
        let owner = store.get_timer_owner(id).ok().flatten();
        if let Err(e) = tree.sync_timer_json(&updated, owner.as_deref()) {
            log::warn!("bellman: timer.json sync failed: {e}");
        }
    }
    // Wake the scheduler so the next tick picks up the change.
    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    drop(store);
    state.rearm_wake();
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
        anchors: Some(state.reply_anchors.clone()),
        deadlines: Some(state.reply_deadlines.clone()),
        dispatcher: state.dispatcher.lock().clone(),
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
    // IK5: run_now builds its own engine without the status listener (it is
    // also the CLI path) — emit the live-run invalidation directly.
    let _ = app.emit("run-status-changed", &outcome.timer.id.to_string());
    Ok(outcome.into())
}

/// `list_log_tail` — recent events from the JSONL log, optionally filtered
/// to a single timer.
#[derive(Debug, Deserialize)]
pub struct ListLogTailArgs {
    /// Restrict to one timer; `None` returns every timer's events.
    pub timer_id: Option<String>,
    /// How many of the most recent events to return.
    pub limit: Option<usize>,
}
/// Recent events from the JSONL log. camelCase at the IPC boundary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogTailDto {
    /// The events, oldest first.
    pub events: Vec<EventRecord>,
    /// How many lines the reader saw in total.
    pub total_records: usize,
    /// How many were unparseable and skipped. Surfaced rather than hidden:
    /// a tolerant reader that silently drops lines is a liar.
    pub skipped: usize,
}

/// Read the tail of the event log, optionally for one timer.
#[tauri::command]
pub fn list_log_tail(
    state: State<'_, AppState>,
    timer_id: Option<String>,
    limit: Option<usize>,
) -> Result<LogTailDto, String> {
    let logs_dir = state.data_dir.join("logs");
    let tid = match timer_id.as_deref() {
        None | Some("") => None,
        Some(s) => Some(Uuid::from_str(s).map_err(|e| format!("invalid timer_id: {e}"))?),
    };
    // One centralized read path (R11): archives + .rotating + current,
    // deduped by event_id, filtered and limited AFTER dedupe. A missing
    // live file (mid-rotation) yields history, never a false empty.
    let (events, stats) =
        bellman_core::read_log_tail(&logs_dir, tid, limit).map_err(|e| e.to_string())?;

    Ok(LogTailDto {
        total_records: stats.records,
        skipped: stats.skipped,
        events,
    })
}

/// `list_calendar_truth` — Week / Month view truth model.
///
/// Past instants: only durable recorded outcomes (JSONL events + run ledger).
/// Future instants: schedule projections. Never fabricates past recurrence.
///
/// Args are individual Tauri command parameters (camelCase at the IPC
/// boundary via the generate_handler arg mapping).
#[tauri::command]
pub fn list_calendar_truth(
    state: State<'_, AppState>,
    from: String,
    to: String,
    timezone: Option<String>,
) -> Result<bellman_core::TruthWindow, String> {
    do_list_calendar_truth(&state, &from, &to, timezone.as_deref(), Utc::now())
}

/// Testable implementation of [`list_calendar_truth`] (injectable `now`).
fn do_list_calendar_truth(
    state: &AppState,
    from: &str,
    to: &str,
    timezone: Option<&str>,
    now_utc: DateTime<Utc>,
) -> Result<bellman_core::TruthWindow, String> {
    use chrono::NaiveDate;

    let from_d = NaiveDate::parse_from_str(from.trim(), "%Y-%m-%d")
        .map_err(|e| format!("invalid from date '{from}': {e}"))?;
    let to_d = NaiveDate::parse_from_str(to.trim(), "%Y-%m-%d")
        .map_err(|e| format!("invalid to date '{to}': {e}"))?;
    let tz = match timezone.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => bellman_core::system_tz_name(),
    };

    // Current + retained archives (rotation must not erase Week/Month truth).
    let logs_dir = state.data_dir.join("logs");
    let events = {
        let (evs, _) = bellman_core::read_log_history(&logs_dir).map_err(|e| e.to_string())?;
        evs
    };

    let store = state.store.lock();
    let tasks = bellman_core::tasks_from_store(&store)?;
    // Broad claim window padded by a day so DST edges near the range still
    // reach the truth builder (which re-filters in display tz).
    let pad_start = from_d
        .pred_opt()
        .unwrap_or(from_d)
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| format!("invalid from date {from_d}"))?;
    let pad_end = to_d
        .succ_opt()
        .and_then(|d| d.succ_opt())
        .unwrap_or(to_d)
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| format!("invalid to date {to_d}"))?;
    let range_from = DateTime::<Utc>::from_naive_utc_and_offset(pad_start, Utc);
    let range_to = DateTime::<Utc>::from_naive_utc_and_offset(pad_end, Utc);
    let claims = store
        .runs_in_range(range_from, range_to)
        .map_err(|e| e.to_string())?;
    drop(store);

    let opts = bellman_core::TruthBuildOptions {
        from: from_d,
        to: to_d,
        timezone: tz,
        now_utc,
        caps: bellman_core::CalendarCaps::default(),
    };
    bellman_core::build_truth_window(&tasks, &events, &claims, &opts)
}

/// `get_pause_all` / `set_pause_all` — the global pause-all flag.
#[tauri::command]
pub fn get_pause_all(state: State<'_, AppState>) -> bool {
    state.pause_all()
}

/// Set the global pause-all flag (vacation mode) and mirror it into the tray.
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
            demo: cfg.demo_opt_in,
        },
    }
}

/// Record the first-run wizard's answers, apply the autostart and wake bits
/// immediately, and return the resulting status.
#[tauri::command]
pub fn wizard_set_choice(
    app: AppHandle,
    state: State<'_, AppState>,
    choice: WizardChoice,
) -> Result<WizardStatus, String> {
    let cfg = config::record_wizard_choice(&state.data_dir, choice).map_err(|e| e.to_string())?;
    *state.config.lock() = cfg.clone();
    // Apply the autostart bit immediately.
    let _ = apply_autostart(&app, cfg.autostart_enabled);
    // Master wake toggle from wizard answer.
    state.set_wake_master(cfg.wake_enabled);
    state.rearm_wake();
    Ok(WizardStatus {
        completed: cfg.wizard_completed,
        defaults: WizardChoice {
            autostart: cfg.autostart_enabled,
            start_minimized: cfg.start_minimized,
            wake_enabled: cfg.wake_enabled,
            demo: cfg.demo_opt_in,
        },
    })
}

/// Re-open the wizard from Settings, pre-filled with the current settings.
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
            demo: cfg.demo_opt_in,
        },
    }
}

// ── WIZ1 demo commands ─────────────────────────────────────────────────
//
// The wizard panel and the Settings demo entry read `demo_info` and act via
// `demo_launch` / `demo_open_docs`. Bellman explains and launches; it never
// provisions the demo's timer (the demo claims its own through the slot
// protocol — see `crate::demo` and WIZ1's "must NOT do").

/// `demo_info` — everything the demo panel renders: resolved demo dir,
/// copyable command, interpreter probe, slots root, docs path.
#[tauri::command]
pub fn demo_info(state: State<'_, AppState>) -> DemoInfoDto {
    let opt_in = state.config.lock().demo_opt_in;
    crate::demo::gather(opt_in, &state.data_dir.join("slots"))
}

/// `demo_launch` — spawn the demo detached against this install's slots
/// root. Returns the child pid. Refuses when the demo or the interpreter
/// (python3 + tkinter) is not actually runnable.
#[tauri::command]
pub fn demo_launch(state: State<'_, AppState>) -> Result<u32, String> {
    crate::demo::launch_demo(&state.data_dir.join("slots"))
}

/// `demo_open_docs` — open `docs/INTEGRATION.md` with the platform handler.
#[tauri::command]
pub fn demo_open_docs() -> Result<(), String> {
    crate::demo::open_integration_doc()
}

/// `set_demo_opt_in` — persist the Settings-side mirror of the wizard tick.
#[tauri::command]
pub fn set_demo_opt_in(state: State<'_, AppState>, enabled: bool) -> Result<bool, String> {
    let mut cfg = state.config.lock().clone();
    cfg.demo_opt_in = enabled;
    cfg.save(&state.data_dir).map_err(|e| e.to_string())?;
    *state.config.lock() = cfg;
    Ok(enabled)
}

/// `app_info` — app metadata the UI uses in headers / settings. camelCase
/// at the IPC boundary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// This app's live data directory — what Settings → Data shows, and the
    /// answer to "which store am I looking at?".
    pub data_dir: String,
    /// The SQLite database inside it.
    pub db_path: String,
    /// Where the event log and its archives live.
    pub logs_dir: String,
    /// The slot channel root an integrating app is given.
    pub slots_dir: String,
    /// Whether the first-run wizard has been dismissed.
    pub wizard_completed: bool,
    /// Whether launch-on-login is on.
    pub autostart_enabled: bool,
    /// Whether vacation mode is on.
    pub pause_all: bool,
    /// The wake-from-sleep master toggle.
    pub wake_enabled: bool,
    /// The wake status sentence, identical to the JSONL event's.
    pub wake_status_line: String,
    /// The global wake-action concurrency cap.
    pub max_concurrent_actions: usize,
    /// Default misfire policy for new calendar timers.
    pub default_misfire_policy: String,
    /// Default grace for that policy, in seconds.
    pub default_misfire_grace_secs: u64,
}

/// Paths and settings the UI shows in its header and Settings page.
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
        wake_enabled: cfg.wake_enabled,
        wake_status_line: state.wake_status_line(),
        max_concurrent_actions: cfg.max_concurrent_actions,
        default_misfire_policy: cfg.default_misfire_policy.clone(),
        default_misfire_grace_secs: cfg.default_misfire_grace_secs,
    }
}

// ── P7 wake / Settings commands ──────────────────────────────────────

/// Snapshot returned by `wake_status` / `wake_reprobe`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeStatusDto {
    /// The one sentence shown in Settings and written to the log.
    pub status_line: String,
    /// Effective capability (platform AND master toggle).
    pub enabled: bool,
    /// Master config toggle (`wake.enabled`).
    pub master_enabled: bool,
    /// Platform probe only (ignores master) — drives greying of the master toggle.
    pub platform_enabled: bool,
    /// Which platform answered, so the UI can pick the right fix-it.
    pub platform: String,
    /// What would make wake work here, in one sentence.
    pub fix_hint: Option<String>,
    /// `linux_udev` | `windows_powercfg` | `macos_enroll` | `macos_login_items`
    pub fix_action: Option<String>,
    /// The udev rule to copy on older Linux; the admin applies it by hand.
    pub udev_snippet: Option<String>,
    /// The elevated `powercfg` line behind the Windows fix-it button.
    pub powercfg_command: Option<String>,
    /// Deep link to macOS Login Items for helper approval.
    pub login_items_url: Option<String>,
    /// The raw probe result, for anyone who wants the detail.
    pub capability: serde_json::Value,
}

/// The current wake capability, without re-probing.
#[tauri::command]
pub fn wake_status(state: State<'_, AppState>) -> WakeStatusDto {
    state.wake_status_dto()
}

/// Re-run the platform probe — the Settings "Re-probe" button, and what the
/// resume and power-source hooks call.
#[tauri::command]
pub fn wake_reprobe(state: State<'_, AppState>) -> WakeStatusDto {
    let _ = state.wake_reprobe();
    state.emit_wake_capability_if_changed();
    state.wake_status_dto()
}

/// Flip the wake-from-sleep master toggle and re-arm the next wake.
#[tauri::command]
pub fn set_wake_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<WakeStatusDto, String> {
    let mut cfg = state.config.lock().clone();
    cfg.wake_enabled = enabled;
    cfg.save(&state.data_dir).map_err(|e| e.to_string())?;
    *state.config.lock() = cfg;
    state.set_wake_master(enabled);
    state.rearm_wake();
    state.emit_wake_capability_if_changed();
    Ok(state.wake_status_dto())
}

/// Turn launch-on-login on or off, applying it to the OS immediately.
#[tauri::command]
pub fn set_autostart_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    let mut cfg = state.config.lock().clone();
    cfg.autostart_enabled = enabled;
    cfg.save(&state.data_dir).map_err(|e| e.to_string())?;
    *state.config.lock() = cfg;
    apply_autostart(&app, enabled).map_err(|e| e.to_string())?;
    Ok(enabled)
}

/// Change the global wake-action concurrency cap (clamped to 1..=256).
#[tauri::command]
pub fn set_max_concurrent_actions(
    state: State<'_, AppState>,
    value: usize,
) -> Result<usize, String> {
    let mut cfg = state.config.lock().clone();
    cfg.max_concurrent_actions = value.clamp(1, 256);
    let saved = cfg.max_concurrent_actions;
    cfg.save(&state.data_dir).map_err(|e| e.to_string())?;
    *state.config.lock() = cfg;
    Ok(saved)
}

/// Dependency check for the first-run wizard (informational only).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyCheckDto {
    /// One row per optional dependency. Nothing here blocks anything: a
    /// missing item is a hint, not a failure.
    pub items: Vec<DepItemDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// One optional dependency the wizard reports on.
pub struct DepItemDto {
    /// What was checked, in the user's words.
    pub name: String,
    /// Whether it was found.
    pub ok: bool,
    /// How to get it, when it was not.
    pub hint: Option<String>,
}

/// User-initiated Windows powercfg fix-it (UAC). Rail: `"ac"` | `"dc"`.
#[tauri::command]
pub fn wake_fix_powercfg(
    state: State<'_, AppState>,
    rail: Option<String>,
) -> Result<crate::commands::WakeStatusDto, String> {
    let rail = match rail.as_deref() {
        Some("dc") | Some("battery") => bellman_core::PowerRail::Dc,
        _ => bellman_core::PowerRail::Ac,
    };
    crate::wake_fixit::run_windows_powercfg_fix(rail)?;
    let _ = state.wake_reprobe();
    state.emit_wake_capability_if_changed();
    Ok(state.wake_status_dto())
}

/// User-initiated macOS SMAppService enroll + Login Items deep-link.
#[tauri::command]
pub fn wake_enroll_macos(state: State<'_, AppState>) -> Result<WakeStatusDto, String> {
    let msg = crate::wake_fixit::enroll_macos_wake_daemon()?;
    log::info!("bellman: {msg}");
    let _ = state.wake_reprobe();
    state.emit_wake_capability_if_changed();
    Ok(state.wake_status_dto())
}

/// Open macOS Login Items (helper awaiting approval).
#[tauri::command]
pub fn wake_open_login_items(state: State<'_, AppState>) -> Result<WakeStatusDto, String> {
    let msg = crate::wake_fixit::open_macos_login_items()?;
    log::info!("bellman: {msg}");
    let _ = state.wake_reprobe();
    Ok(state.wake_status_dto())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// The misfire defaults new calendar timers inherit.
pub struct MisfireDefaultsDto {
    /// `skip` | `coalesce` | `catch_up`.
    pub policy: String,
    /// The grace window for that policy, in seconds.
    pub grace_secs: u64,
}

/// The misfire defaults new calendar timers will be created with.
#[tauri::command]
pub fn get_misfire_defaults(state: State<'_, AppState>) -> MisfireDefaultsDto {
    let cfg = state.config.lock().clone();
    MisfireDefaultsDto {
        policy: cfg.default_misfire_policy,
        grace_secs: cfg.default_misfire_grace_secs,
    }
}

/// Change those defaults. Existing timers keep the policy they were made
/// with; this only affects the next one.
#[tauri::command]
pub fn set_misfire_defaults(
    state: State<'_, AppState>,
    policy: String,
    grace_secs: u64,
) -> Result<MisfireDefaultsDto, String> {
    let mut cfg = state.config.lock().clone();
    cfg.default_misfire_policy = policy;
    cfg.default_misfire_grace_secs = grace_secs;
    cfg = cfg.sanitized();
    cfg.save(&state.data_dir).map_err(|e| e.to_string())?;
    let out = MisfireDefaultsDto {
        policy: cfg.default_misfire_policy.clone(),
        grace_secs: cfg.default_misfire_grace_secs,
    };
    *state.config.lock() = cfg;
    Ok(out)
}

/// Probe the optional per-OS dependencies the wizard reports on.
#[tauri::command]
pub fn dependency_check() -> DependencyCheckDto {
    #[cfg(target_os = "linux")]
    {
        let mut items = Vec::new();
        // webkit2gtk is a link-time dep of the app — if we're running, it's present.
        items.push(DepItemDto {
            name: "webkit2gtk".into(),
            ok: true,
            hint: None,
        });
        // AppIndicator: best-effort via presence of the shared lib.
        let indicator =
            std::path::Path::new("/usr/lib/x86_64-linux-gnu/libayatana-appindicator3.so.1")
                .exists()
                || std::path::Path::new("/usr/lib/libayatana-appindicator3.so.1").exists();
        items.push(DepItemDto {
            name: "tray AppIndicator".into(),
            ok: indicator,
            hint: if indicator {
                None
            } else {
                Some(
                    "GNOME: install the AppIndicator extension, or use KDE/others with tray support."
                        .into(),
                )
            },
        });
        DependencyCheckDto { items }
    }
    #[cfg(target_os = "windows")]
    {
        DependencyCheckDto {
            items: vec![DepItemDto {
                name: "WebView2 runtime".into(),
                ok: true, // evergreen bootstrapper in installer
                hint: None,
            }],
        }
    }
    #[cfg(target_os = "macos")]
    {
        DependencyCheckDto { items: vec![] }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        DependencyCheckDto { items: vec![] }
    }
}

// ── C8 calendar UI commands ──────────────────────────────────────────
//
// The dialog / Week / Month / Run history pages read and mutate timers
// through these. They are intentionally thin wrappers around `Store`
// (and `Occurrence::preview` for the live next-5 preview) so the same
// validation surface backs both the GUI and the CLI (`bellman add|edit|rm|next`).

/// Inputs for `create_timer`: the dialog sends a deliberate web DTO
/// (flat weekly-days, tagged action, kind discriminant). We convert to
/// a core `NewTimer` inside the command so the store layer sees the
/// same types as the CLI.
///
/// `enabled` defaults to `true` when absent (matches `Store::create_timer`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTimerInput {
    /// Display name.
    pub name: String,
    /// The recurrence, in the webview's flattened shape.
    pub occurrence: crate::web::WebOccurrenceDto,
    /// What to do when it fires.
    pub action: crate::web::WebActionDto,
    /// Create it scheduled, or create it paused.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Include it in the wake-from-sleep election.
    #[serde(default)]
    pub wake_machine: bool,
}

fn default_enabled() -> bool {
    true
}

impl CreateTimerInput {
    /// Convert the webview shape into the core `NewTimer` the store takes,
    /// so the store layer never sees a UI type.
    pub fn into_new_timer(self) -> Result<NewTimer, String> {
        let occurrence = self.occurrence.into_core_occurrence()?;
        let action = self.action.into_core_action();
        let mut new = NewTimer::new(self.name, occurrence);
        new.action = action;
        new.enabled = self.enabled;
        new.wake_machine = self.wake_machine;
        Ok(new)
    }

    /// Build a `NewTimer` applying Settings misfire defaults from `cfg`
    /// (calendar kinds only; intervals stay Skip).
    pub fn into_new_timer_with_config(
        self,
        cfg: &bellman_core::AppConfig,
    ) -> Result<NewTimer, String> {
        let mut new = self.into_new_timer()?;
        new.misfire = cfg.misfire_for_occurrence(&new.occurrence);
        Ok(new)
    }
}

/// `create_timer` — insert a fresh timer. Mirrors `bellman add`.
#[tauri::command]
pub fn create_timer(
    state: State<'_, AppState>,
    input: CreateTimerInput,
) -> Result<TimerDto, String> {
    do_create_timer(&state, input)
}

pub(crate) fn do_create_timer(
    state: &AppState,
    input: CreateTimerInput,
) -> Result<TimerDto, String> {
    let cfg = state.config.lock().clone();
    let new = input.into_new_timer_with_config(&cfg)?;
    let timer = {
        let mut store = state.store.lock();
        store.create_timer(new).map_err(|e| e.to_string())?
    };

    // Lifecycle: enqueue the Registered event (R11; the publisher appends).
    {
        let store = state.store.lock();
        if let Err(e) = store.enqueue_event(
            &bellman_core::events::EventRecord::new(bellman_core::events::RunState::Registered)
                .with_timer(timer.id, timer.name.clone())
                .with_message("gui create"),
        ) {
            log::warn!("bellman: registered enqueue failed: {e}");
        }
    }

    // IK2: project the per-timer folder (README + timer.json). View-only.
    {
        let tree = bellman_core::TimersTree::new(&state.data_dir);
        let owner = state.store.lock().get_timer_owner(timer.id).ok().flatten();
        if let Err(e) = tree.create_for_timer(&timer, owner.as_deref()) {
            log::warn!("bellman: timer folder projection failed: {e}");
        }
    }

    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    state.rearm_wake();
    Ok(TimerDto::from(timer))
}

/// `update_timer` — apply a partial patch. Mirrors `bellman edit`.
/// The caller passes the current revision (from `list_timers`) so
/// concurrent edits stay consistent with the store's optimistic-update
/// contract.
#[tauri::command]
pub fn update_timer(
    state: State<'_, AppState>,
    id: String,
    expected_revision: i64,
    patch: WebTimerPatchDto,
) -> Result<TimerDto, String> {
    let id = Uuid::from_str(&id).map_err(|e| format!("invalid id: {e}"))?;
    let core_patch = patch.into_core_patch()?;
    let mut store = state.store.lock();
    let updated = store
        .update_timer(bellman_core::store::TimerUpdate {
            id,
            expected_revision,
            patch: core_patch,
        })
        .map_err(|e| e.to_string())?;
    // IK2: rename/edit rewrites timer.json; the folder path never changes.
    {
        let tree = bellman_core::TimersTree::new(&state.data_dir);
        let owner = store.get_timer_owner(id).ok().flatten();
        if let Err(e) = tree.sync_timer_json(&updated, owner.as_deref()) {
            log::warn!("bellman: timer.json sync failed: {e}");
        }
    }
    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    drop(store);
    state.rearm_wake();
    Ok(TimerDto::from(updated))
}

/// `delete_timer` — remove by id. Mirrors `bellman rm <name|id>`.
#[tauri::command]
pub fn delete_timer(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    let id = Uuid::from_str(&id).map_err(|e| format!("invalid id: {e}"))?;
    let mut store = state.store.lock();
    // R10: deletion is a lifecycle mutator — the per-timer gate is REQUIRED;
    // a lock failure aborts the delete.
    let _gate = bellman_core::reply::gate::acquire(&state.data_dir, id)
        .map_err(|e| format!("per-timer gate: {e}"))?;
    let timer = store.get_timer(id).map_err(|e| e.to_string())?;
    let n = match &timer {
        Some(timer) => {
            let (deleted, _cancelled) =
                bellman_core::tree::delete_timer_lifecycle(&mut store, timer)
                    .map_err(|e| e.to_string())?;
            deleted
        }
        None => false,
    };
    if n {
        let tree = bellman_core::TimersTree::new(&state.data_dir);
        if let Err(e) = tree.remove_for(id) {
            log::warn!("bellman: timer folder removal failed: {e}");
        }
    }
    if let Some(h) = state.control_handle.lock().as_ref() {
        h.refill();
    }
    drop(store);
    state.rearm_wake();
    Ok(n)
}

/// `preview_fires` — return the next-N fires for a draft occurrence.
/// Wired to the dialog's live preview pane; identical math to
/// `bellman next`. Accepts the deliberate `WebOccurrenceDto` shape
/// (flat weekly-days, tagged action, kind discriminant) and converts
/// it to the core occurrence inside the command so the GUI never has
/// to know about `chrono::Weekdays` or the core tagged enum.
#[derive(Debug, Deserialize)]
pub struct PreviewArgs {
    /// The occurrence being edited, in the webview's flattened shape.
    pub input: crate::web::WebOccurrenceDto,
    /// How many upcoming fires to return (default 5).
    #[serde(default = "default_preview_n")]
    pub n: usize,
}

fn default_preview_n() -> usize {
    5
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// The next-fires preview shown live in the edit dialog.
pub struct PreviewResponseDto {
    /// The next N fires of the schedule as currently edited.
    pub fires: Vec<PreviewFireDto>,
    /// DST / month-day-clamp warnings keyed off the user's current input.
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// One previewed fire, in UTC and in local time with its offset.
pub struct PreviewFireDto {
    /// The instant, in UTC.
    pub utc: DateTime<Utc>,
    /// Its local date, `YYYY-MM-DD`.
    pub local_date: String,
    /// Its local time of day.
    pub local_time: String,
    /// The UTC offset in force then — how a DST step shows up in the list.
    pub offset: String,
    /// The zone abbreviation in force then.
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

/// The next N fires of a schedule being edited, with DST and month-day
/// warnings, so the dialog can show consequences before saving.
#[tauri::command]
pub fn preview_fires(
    input: crate::web::WebOccurrenceDto,
    n: Option<usize>,
) -> Result<PreviewResponseDto, String> {
    let n = n.unwrap_or(5);
    let occ = input.into_core_occurrence()?;
    let fires = preview_fires_from_occurrence(&occ, n);
    let mut warnings: Vec<String> = Vec::new();
    if let Some(w) = occurrence_input::dst_warning(occ.kind(), occ.tz_name()) {
        warnings.push(w);
    }
    Ok(PreviewResponseDto {
        fires: fires
            .into_iter()
            .map(|(utc, local_dt, offset_secs)| PreviewFireDto {
                utc,
                local_date: local_dt.date().format("%Y-%m-%d").to_string(),
                local_time: local_dt.time().format("%H:%M:%S").to_string(),
                offset: format_offset_secs(offset_secs),
                tz_name: occ.tz_name().to_string(),
            })
            .collect(),
        warnings,
    })
}

fn format_offset_secs(secs: i32) -> String {
    if secs == 0 {
        "UTC".to_string()
    } else {
        let sign = if secs >= 0 { '+' } else { '-' };
        let abs = secs.unsigned_abs();
        let h = abs / 3600;
        let m = (abs % 3600) / 60;
        format!("{sign}{h:02}:{m:02}")
    }
}

fn preview_fires_from_occurrence(
    occ: &bellman_core::Occurrence,
    n: usize,
) -> Vec<(DateTime<Utc>, chrono::NaiveDateTime, i32)> {
    let after = Utc::now()
        .with_timezone(&occ.timezone())
        .naive_utc()
        .and_local_timezone(occ.timezone())
        .single()
        .map_or_else(
            || Utc::now().with_timezone(&occ.timezone()),
            |d| d.with_timezone(&occ.timezone()),
        );
    occ.preview(after, n)
        .into_iter()
        .map(|local_dt| {
            let utc = local_dt.with_timezone(&Utc);
            let naive = local_dt.naive_local();
            let offset_secs = local_dt.offset().fix().local_minus_utc();
            (utc, naive, offset_secs)
        })
        .collect()
}

/// `query_neighbours` — store-aware "what else fires at/near these instants?"
///
/// Used by the timer dialog (Next 5 fires collision names + nearby panel) and
/// by the All timers list (shared next-fire density). Reads the store and
/// expands via the same `Occurrence::preview` path as `preview_fires`.
/// Does **not** change scheduling, store schema, or occurrence semantics.
///
/// Thresholds / caps: see [`crate::neighbours`] named constants
/// (`NEIGHBOUR_WINDOW_SECS`, `NEIGHBOUR_HORIZON_SECS`,
/// `NEIGHBOUR_MAX_FIRES_PER_TIMER`).
#[tauri::command]
pub fn query_neighbours(
    state: State<'_, AppState>,
    candidates: Vec<DateTime<Utc>>,
    exclude_timer_id: Option<String>,
    window_secs: Option<i64>,
) -> Result<crate::neighbours::QueryNeighboursResponse, String> {
    let exclude = crate::neighbours::parse_exclude_id(exclude_timer_id.as_deref())?;
    let window = window_secs.unwrap_or(crate::neighbours::NEIGHBOUR_WINDOW_SECS);
    let store = state.store.lock();
    let timers = store.list_timers().map_err(|e| e.to_string())?;
    drop(store);
    Ok(crate::neighbours::query_neighbours_from_timers(
        &timers,
        &candidates,
        exclude,
        window,
        Utc::now(),
    ))
}

// `TimerPatchDto` was removed in rework #2: the GUI now sends the
// `WebTimerPatchDto` from `crate::web` directly. See `crate::web` for
// the flat wire shape and round-trip logic.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use bellman_core::NotifySink;
    use std::fs;
    use std::io::Read;
    use std::sync::Arc;
    use tempfile::tempdir;

    struct DummySink;
    impl NotifySink for DummySink {
        fn show(&self, _title: &str, _body: &str) -> bellman_core::NotifyOutcome {
            bellman_core::NotifyOutcome {
                title: _title.into(),
                body: _body.into(),
                stubbed: true,
            }
        }
    }

    fn create_test_state(data_dir: std::path::PathBuf) -> AppState {
        let store = bellman_core::open_store(&data_dir.join("timers.db")).unwrap();
        AppState::new(
            store,
            data_dir,
            Config::default(),
            false,
            Arc::new(DummySink),
        )
    }

    fn dummy_input() -> CreateTimerInput {
        CreateTimerInput {
            wake_machine: false,
            name: "test timer".into(),
            occurrence: crate::web::WebOccurrenceDto {
                occ: "once".into(),
                tz: "UTC".into(),
                once_at: Some("2050-01-01T00:00:00".into()),
                at: None,
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: None,
                days: None,
            },
            action: crate::web::WebActionDto::Notify {
                title: "title".into(),
                body: "body".into(),
            },
            enabled: true,
        }
    }

    #[test]
    fn test_create_timer_logs_registered_event() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let state = create_test_state(data_dir.clone());

        let result = do_create_timer(&state, dummy_input());
        assert!(result.is_ok());
        let dto = result.unwrap();

        // R11: the GUI path ENQUEUES the event; the elected publisher appends
        // it. No scheduler runs in a unit test, so drive the same best-effort
        // drain one-shot producers (the CLI) use, then verify the log.
        {
            let store = state.store.lock();
            bellman_core::events::EventPublisher::drain_best_effort(&data_dir, &store);
        }

        // Verify the log was written
        let log_path = data_dir.join("logs").join("events.current.jsonl");
        assert!(log_path.exists(), "log file should be created");

        let mut file = fs::File::open(&log_path).unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).unwrap();

        assert!(
            content.contains("\"registered\""),
            "should contain registered event"
        );
        assert!(
            content.contains("\"gui create\""),
            "should contain gui create message"
        );
        assert!(content.contains(&dto.id), "should contain timer id");
        assert!(content.contains(&dto.name), "should contain timer name");
    }

    #[test]
    fn test_create_timer_succeeds_when_log_unwritable() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let state = create_test_state(data_dir.clone());

        // Make the logs directory unwritable
        let logs_dir = data_dir.join("logs");
        fs::create_dir_all(&logs_dir).unwrap();
        let mut perms = fs::metadata(&logs_dir).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&logs_dir, perms).unwrap();

        // The command should still succeed
        let result = do_create_timer(&state, dummy_input());
        assert!(result.is_ok(), "command should succeed even if log fails");
    }

    /// Settings "Misfire defaults" must actually land on newly created calendar
    /// timers (supervisor R1). Interval timers stay Skip.
    #[test]
    fn create_timer_applies_misfire_defaults_from_config() {
        use bellman_core::store::MisfirePolicy;
        use std::str::FromStr;
        use uuid::Uuid;

        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let state = create_test_state(data_dir.clone());

        // Simulate Settings → set_misfire_defaults(catch_up, 77).
        {
            let mut cfg = state.config.lock().clone();
            cfg.default_misfire_policy = "catch_up".into();
            cfg.default_misfire_grace_secs = 77;
            cfg = cfg.sanitized();
            cfg.save(&state.data_dir).unwrap();
            *state.config.lock() = cfg;
        }

        let input = CreateTimerInput {
            wake_machine: false,
            name: "cal-default".into(),
            occurrence: crate::web::WebOccurrenceDto {
                occ: "daily".into(),
                tz: "UTC".into(),
                once_at: None,
                at: Some("09:00:00".into()),
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: None,
                days: None,
            },
            action: crate::web::WebActionDto::None,
            enabled: true,
        };
        let dto = do_create_timer(&state, input).expect("create calendar timer");
        let id = Uuid::from_str(&dto.id).unwrap();

        let store = state.store.lock();
        let timer = store.get_timer(id).unwrap().expect("timer row");
        assert_eq!(
            timer.misfire,
            MisfirePolicy::CatchUp {
                grace_secs: 77,
                max_catch_up: 10,
            },
            "calendar timer must store Settings misfire defaults"
        );

        // Interval still Skip even when config says coalesce.
        drop(store);
        {
            let mut cfg = state.config.lock().clone();
            cfg.default_misfire_policy = "coalesce".into();
            cfg.default_misfire_grace_secs = 3600;
            *state.config.lock() = cfg;
        }
        let interval = CreateTimerInput {
            wake_machine: false,
            name: "int-skip".into(),
            occurrence: crate::web::WebOccurrenceDto {
                occ: "interval".into(),
                tz: "UTC".into(),
                once_at: None,
                at: None,
                every_secs: Some(30),
                anchor: None,
                day: None,
                month: None,
                expr: None,
                days: None,
            },
            action: crate::web::WebActionDto::None,
            enabled: true,
        };
        let dto2 = do_create_timer(&state, interval).expect("create interval");
        let id2 = Uuid::from_str(&dto2.id).unwrap();
        let store = state.store.lock();
        let t2 = store.get_timer(id2).unwrap().unwrap();
        assert_eq!(t2.misfire, MisfirePolicy::Skip);
    }

    /// Production path: archive JSONL + claim ledger through list_calendar_truth.
    #[test]
    fn list_calendar_truth_merges_archive_and_ledger() {
        use bellman_core::events::{EventRecord, RunState};
        use bellman_core::store::NewTimer;
        use bellman_core::{Occurrence, OccurrenceKind, OutcomeLabel, TruthSource};
        use chrono::{NaiveTime, TimeZone, Utc};

        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let state = create_test_state(data_dir.clone());

        let at = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let occ = Occurrence::new(OccurrenceKind::Daily { at }, "UTC").unwrap();
        let mut nt = NewTimer::new("NEW CURRENT NAME", occ);
        nt.enabled = false;

        let (tid, run_fail_id, run_ledger_id) = {
            let mut store = state.store.lock();
            let t = store.create_timer(nt).unwrap();
            let sched_fail = Utc.with_ymd_and_hms(2026, 7, 10, 9, 0, 0).unwrap();
            let sched_ledger = Utc.with_ymd_and_hms(2026, 7, 11, 9, 0, 0).unwrap();
            let c_fail = store.claim_run(t.id, sched_fail).unwrap();
            store.complete_run(c_fail.run_id).unwrap();
            let c_ledger = store.claim_run(t.id, sched_ledger).unwrap();
            store.complete_run(c_ledger.run_id).unwrap();
            (t.id, c_fail.run_id, c_ledger.run_id)
        };

        // Archived failure only (empty current) — simulates rotate after fire.
        let logs = data_dir.join("logs");
        let archive_dir = logs.join("archive");
        fs::create_dir_all(&archive_dir).unwrap();
        let sched_fail = Utc.with_ymd_and_hms(2026, 7, 10, 9, 0, 0).unwrap();
        let line = serde_json::to_string(
            &EventRecord::new(RunState::WakeFailed)
                .with_timer(tid, "Historical Failed")
                .with_run(run_fail_id)
                .with_scheduled_for(sched_fail)
                .with_logged_at(sched_fail)
                .with_error("boom"),
        )
        .unwrap();
        fs::write(
            archive_dir.join("events-2026-W28.jsonl"),
            format!("{line}\n"),
        )
        .unwrap();
        fs::write(logs.join("events.current.jsonl"), "").unwrap();

        let now = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        let win = do_list_calendar_truth(&state, "2026-07-01", "2026-07-31", Some("UTC"), now)
            .expect("list_calendar_truth");

        let recorded: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.source == TruthSource::Recorded)
            .collect();
        assert_eq!(
            recorded.len(),
            2,
            "one archived failed + one ledger-only unknown, got {recorded:?}"
        );

        let failed = recorded
            .iter()
            .find(|e| e.outcome == OutcomeLabel::Failed)
            .expect("failed from archive");
        assert_eq!(failed.name, "Historical Failed");
        assert_ne!(failed.name, "NEW CURRENT NAME");
        assert!(failed.kind.is_none());
        assert!(failed.enabled.is_none());
        let fail_run = run_fail_id.to_string();
        assert_eq!(failed.run_id.as_deref(), Some(fail_run.as_str()));

        let unknown = recorded
            .iter()
            .find(|e| e.outcome == OutcomeLabel::Unknown)
            .expect("ledger-only unknown");
        assert_ne!(unknown.name, "NEW CURRENT NAME");
        assert!(unknown.kind.is_none());
        assert!(unknown.enabled.is_none());
        let ledger_run = run_ledger_id.to_string();
        assert_eq!(unknown.run_id.as_deref(), Some(ledger_run.as_str()));

        assert!(recorded
            .iter()
            .all(|e| e.outcome != OutcomeLabel::Delivered));
    }
}

#[cfg(test)]
mod run_states_tests {
    use super::*;
    use bellman_core::occurrence::{Occurrence, OccurrenceKind};
    use bellman_core::store::NewTimer;
    use bellman_core::NotifySink;
    use chrono::NaiveTime;
    use std::sync::Arc;
    use tempfile::tempdir;

    use crate::config::Config;

    struct DummySink;
    impl NotifySink for DummySink {
        fn show(&self, _title: &str, _body: &str) -> bellman_core::NotifyOutcome {
            bellman_core::NotifyOutcome {
                title: _title.into(),
                body: _body.into(),
                stubbed: true,
            }
        }
    }

    fn make_state(data_dir: std::path::PathBuf) -> AppState {
        let store = bellman_core::open_store(&data_dir.join("timers.db")).unwrap();
        AppState::new(
            store,
            data_dir,
            Config::default(),
            false,
            Arc::new(DummySink),
        )
    }

    fn add_timer(state: &AppState, name: &str, owner: Option<&str>) -> bellman_core::Timer {
        let occ = Occurrence::new(
            OccurrenceKind::Daily {
                at: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
            },
            "UTC",
        )
        .unwrap();
        let mut store = state.store.lock();
        let timer = store.create_timer(NewTimer::new(name, occ)).unwrap();
        if let Some(app) = owner {
            store.set_timer_owner(timer.id, app).unwrap();
        }
        timer
    }

    fn fire_run(
        state: &AppState,
        timer: &bellman_core::Timer,
        app: &str,
    ) -> bellman_core::RunStateRow {
        let mut store = state.store.lock();
        let claim = store.claim_run(timer.id, Utc::now()).unwrap();
        let row = bellman_core::RunStateRow::fired(
            claim.run_id,
            timer.id,
            app,
            "fired",
            claim.claimed_at,
            claim.claimed_at + chrono::Duration::seconds(60),
        );
        store.insert_run_state(&row).unwrap();
        row
    }

    #[test]
    fn list_run_states_returns_current_owned_runs_only() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path().to_path_buf());
        let owned = add_timer(&state, "bulb-test", Some("lightbulb"));
        let unowned = add_timer(&state, "plain-backup", None);
        let idle = add_timer(&state, "idle-owned", Some("other-app"));

        // No runs yet → nothing to show.
        {
            let store = state.store.lock();
            assert!(collect_run_states(&store, None).unwrap().is_empty());
        }

        // Unowned timer fires: no lifecycle row is ever created for it.
        {
            let mut store = state.store.lock();
            let claim = store.claim_run(unowned.id, Utc::now()).unwrap();
            assert!(store.get_run_state(claim.run_id).unwrap().is_none());
        }

        let mut row = fire_run(&state, &owned, "lightbulb");
        {
            let store = state.store.lock();
            let all = collect_run_states(&store, None).unwrap();
            assert_eq!(all.len(), 1, "only the owned timer with a run appears");
            assert_eq!(all[0].timer_id, owned.id.to_string());
            assert_eq!(all[0].run_id, row.run_id.to_string());
            assert_eq!(all[0].state, "fired");
            assert_eq!(all[0].app_name, "lightbulb");
            assert!(all[0].progress.is_none());
            // The idle owned timer and the unowned timer are absent.
            assert!(all.iter().all(|d| d.timer_id != unowned.id.to_string()));
            assert!(all.iter().all(|d| d.timer_id != idle.id.to_string()));
            // Single-timer refetch (the run-status-changed path).
            let one = collect_run_states(&store, Some(&owned.id.to_string())).unwrap();
            assert_eq!(one.len(), 1);
            let none = collect_run_states(&store, Some(&unowned.id.to_string())).unwrap();
            assert!(none.is_empty(), "unowned timer never has a run state");
        }

        // A second firing supersedes the first: only the LATEST run is current.
        row.state = "superseded".into();
        {
            let store = state.store.lock();
            store.update_run_state(&row).unwrap();
        }
        let row2 = fire_run(&state, &owned, "lightbulb");
        {
            let store = state.store.lock();
            let all = collect_run_states(&store, None).unwrap();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].run_id, row2.run_id.to_string(), "latest run wins");
            assert_eq!(all[0].state, "fired");
        }
    }
}
