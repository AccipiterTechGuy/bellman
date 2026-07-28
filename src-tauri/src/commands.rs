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
    pub wake_enabled: bool,
    pub wake_status_line: String,
    pub max_concurrent_actions: usize,
    pub default_misfire_policy: String,
    pub default_misfire_grace_secs: u64,
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
    pub status_line: String,
    /// Effective capability (platform AND master toggle).
    pub enabled: bool,
    /// Master config toggle (`wake.enabled`).
    pub master_enabled: bool,
    /// Platform probe only (ignores master) — drives greying of the master toggle.
    pub platform_enabled: bool,
    pub platform: String,
    pub fix_hint: Option<String>,
    /// `linux_udev` | `windows_powercfg` | `macos_enroll` | `macos_login_items`
    pub fix_action: Option<String>,
    pub udev_snippet: Option<String>,
    pub powercfg_command: Option<String>,
    pub login_items_url: Option<String>,
    pub capability: serde_json::Value,
}

#[tauri::command]
pub fn wake_status(state: State<'_, AppState>) -> WakeStatusDto {
    state.wake_status_dto()
}

#[tauri::command]
pub fn wake_reprobe(state: State<'_, AppState>) -> WakeStatusDto {
    let _ = state.wake_reprobe();
    state.emit_wake_capability_if_changed();
    state.wake_status_dto()
}

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
    pub items: Vec<DepItemDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepItemDto {
    pub name: String,
    pub ok: bool,
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
pub struct MisfireDefaultsDto {
    pub policy: String,
    pub grace_secs: u64,
}

#[tauri::command]
pub fn get_misfire_defaults(state: State<'_, AppState>) -> MisfireDefaultsDto {
    let cfg = state.config.lock().clone();
    MisfireDefaultsDto {
        policy: cfg.default_misfire_policy,
        grace_secs: cfg.default_misfire_grace_secs,
    }
}

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
        let indicator = std::path::Path::new("/usr/lib/x86_64-linux-gnu/libayatana-appindicator3.so.1")
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
    pub name: String,
    pub occurrence: crate::web::WebOccurrenceDto,
    pub action: crate::web::WebActionDto,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub wake_machine: bool,
}

fn default_enabled() -> bool {
    true
}

impl CreateTimerInput {
    pub fn into_new_timer(self) -> Result<NewTimer, String> {
        let occurrence = self.occurrence.into_core_occurrence()?;
        let action = self.action.into_core_action();
        let mut new = NewTimer::new(self.name, occurrence);
        new.action = action;
        new.enabled = self.enabled;
        new.wake_machine = self.wake_machine;
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

pub(crate) fn do_create_timer(state: &AppState, input: CreateTimerInput) -> Result<TimerDto, String> {
    let new = input.into_new_timer()?;
    let timer = {
        let mut store = state.store.lock();
        store.create_timer(new).map_err(|e| e.to_string())?
    };
    
    // Lifecycle: emit Registered event (analogous to the CLI)
    if let Ok(mut log) = bellman_core::EventLog::open_under(&state.data_dir) {
        let _ = log.emit(
            bellman_core::events::EventRecord::new(bellman_core::events::EventKind::Registered)
                .with_timer(timer.id, timer.name.clone())
                .with_message("gui create"),
        );
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
    let n = store.delete_timer(id).map_err(|e| e.to_string())?;
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
    pub input: crate::web::WebOccurrenceDto,
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
        .single().map_or_else(|| Utc::now().with_timezone(&occ.timezone()), |d| d.with_timezone(&occ.timezone()));
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
    use bellman_core::NotifySink;
    use crate::config::Config;
    use tempfile::tempdir;
    use std::fs;
    use std::sync::Arc;
    use std::io::Read;

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

        // Verify the log was written
        let log_path = data_dir.join("logs").join("events.current.jsonl");
        assert!(log_path.exists(), "log file should be created");

        let mut file = fs::File::open(&log_path).unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).unwrap();

        assert!(content.contains("\"registered\""), "should contain registered event");
        assert!(content.contains("\"gui create\""), "should contain gui create message");
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
}
