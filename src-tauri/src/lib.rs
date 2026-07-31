//! Bellman desktop shell — Tauri v2.
//!
//! The shell is a THIN view over `bellman-core`. All scheduling, firing, and
//! persistence happen in the Rust engine. The webview renders state pushed
//! from Tauri commands and events; it never owns the clock.
//!
//! ## Layout
//!
//! - [`state`] — process-wide state (store handle, notify sink, config).
//! - [`notify_sink`] — real `tauri-plugin-notification` backend.
//! - [`commands`] — every `#[tauri::command]` the webview can invoke.
//! - [`first_run`] — first-launch autostart ask-the-user (config in `~/.bellman/config.json`).
//! - [`tray`] — system tray icon + menu (Open / Pause all / Quit).
//! - [`run`] — build + run the Tauri application.

pub mod commands;
pub mod config;
pub mod demo;
pub mod first_run;
pub mod neighbours;
pub mod notify_sink;
pub mod occurrence_input;
pub mod state;
pub mod tray;
pub mod wake_fixit;
pub mod web;

#[cfg(test)]
mod dto_serde_tests;

use std::path::PathBuf;

use tauri::Emitter as _;
use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_notification::NotificationExt;

use crate::first_run::WizardChoice;
use crate::state::AppState;

/// Application entry point. Builds the Tauri runtime and the bellman engine
/// inside the setup hook, then runs the event loop until the user quits.
pub fn run() {
    let cli_args: Vec<String> = std::env::args().collect();

    let mut builder = tauri::Builder::default()
        // Single-instance: a second launch focuses the existing main window
        // (per spec: "second launch focuses window").
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ));

    builder = register_commands(builder);

    builder
        .setup(move |app| {
            // Bellman data directory: ~/.bellman (Linux/macOS) / %APPDATA% (Windows).
            let data_dir = resolve_data_dir(app.handle())?;
            std::fs::create_dir_all(&data_dir).map_err(setup_err("create data dir"))?;
            let db_path = data_dir.join("timers.db");
            let logs_dir = data_dir.join("logs");
            let slots_dir = data_dir.join("slots");
            std::fs::create_dir_all(&logs_dir).map_err(setup_err("create logs dir"))?;
            std::fs::create_dir_all(&slots_dir).map_err(setup_err("create slots dir"))?;

            // Load (or create) the user config — the wizard's persisted answer.
            let config = config::Config::load(&data_dir)?;
            log::info!(
                "bellman: data_dir={} wizard_completed={} autostart_enabled={:?}",
                data_dir.display(),
                config.wizard_completed,
                config.autostart_enabled
            );

            // Open the store. This applies the schema (v4) and PRAGMAs.
            let store = bellman_core::open_store(&db_path)
                .map_err(|e| setup_err_str(format!("open store: {e}")))?;
            // Persist a complete config.json on first run so packaging / hand
            // edits see every documented key (horizon, retention, slots, cap).
            if !config::config_path(&data_dir).exists() {
                let _ = config.save(&data_dir);
            }
            // `update_timer` + `claim_run` are &mut on the store; all the
            // scheduler / Tauri commands share one Store via Mutex.

            // Read the persisted global pause-all flag (if any) and apply
            // it to the engine (the scheduler starts unpaused by default).
            let pause_all = config::read_pause_all_flag(&data_dir);

            let state = AppState::new(
                store,
                data_dir.clone(),
                config.clone(),
                pause_all,
                notify_sink::TauriNotifySink::new(app.handle().clone()),
            );

            // IK5: one `run-status-changed` invalidation after every accepted
            // status projection (reply ingest, pickup/watchdog deadlines,
            // fire). It carries only the timer id — the webview refetches the
            // affected row via `list_run_states`; no second copy of state.
            {
                let handle = app.handle().clone();
                state.set_status_listener(bellman_core::reply::StatusListener(
                    std::sync::Arc::new(move |timer_id: uuid::Uuid| {
                        let _ = handle.emit("run-status-changed", timer_id.to_string());
                    }),
                ));
            }

            // Build the scheduler + start the background tick thread.
            // The thread is the only writer to the engine; Tauri commands
            // mutate the store directly (and the next tick observes via
            // `Refill` control messages).
            state.start_scheduler();

            // P7: wake capability JSONL at startup + single-next-wake rearm.
            state.emit_wake_capability_startup();
            state.rearm_wake();

            // Stash the AppState under a managed handle BEFORE the tray
            // is installed — the tray's pause-toggle calls into
            // AppState, and the set_pause_all command needs to push
            // updates back into the tray's CheckMenuItem.
            app.manage(state);

            // Linux: login1 delay inhibitor + PrepareForSleep → rearm bridge.
            #[cfg(target_os = "linux")]
            {
                let handle = app.handle().clone();
                start_linux_power_watch(handle);
            }

            // Tray icon + menu (Open / Pause all / Quit).
            tray::install(app.handle())?;

            // First-run wizard: when `wizard_completed` is false, the window
            // is shown with the wizard modal open. The webview asks
            // `wizard_status()` and persists the answer with `wizard_set_choice()`.
            if !config.wizard_completed {
                // Ensure the main window is visible for the wizard.
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
                log::info!("bellman: first-run wizard will prompt on webview open");
            } else {
                // Honor the user's wizard answer:
                // - autostart: enable/disable per `config.autostart_enabled`
                // - on macOS the window would otherwise be hidden by default
                apply_autostart_from_config(app.handle(), &config)?;
                if config.start_minimized {
                    if let Some(win) = app.get_webview_window("main") {
                        let _ = win.hide();
                    }
                } else if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }

            // Pre-request notification permission so the first fire is
            // visible (best-effort — no UI prompt, no error if denied).
            if let Ok(state) = app.notification().permission_state() {
                log::info!("bellman: notification permission state = {state:?}");
                if matches!(state, tauri_plugin_notification::PermissionState::Prompt) {
                    let _ = app.notification().request_permission();
                }
            }

            // CLI arg `--run-now <name-or-id>` (headless, no window) — used by
            // CI scripts and the wizard's "open & fire" shortcut.
            if let Some(id_or_name) = parse_run_now_arg(&cli_args) {
                if let Some(state) = app.try_state::<AppState>() {
                    if let Err(e) = state.cli_run_now(&id_or_name) {
                        log::error!("bellman: --run-now failed: {e}");
                    }
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // On-demand window: closing the window does NOT quit the app
            // (per spec). Tray stays, engine keeps firing.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running bellman");
}

fn parse_run_now_arg(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--run-now" {
            return it.next().cloned();
        }
    }
    None
}

fn register_commands(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    builder.invoke_handler(tauri::generate_handler![
        commands::list_timers,
        commands::get_timer,
        commands::list_run_states,
        commands::set_enabled,
        commands::run_now,
        commands::list_log_tail,
        commands::list_calendar_truth,
        commands::create_timer,
        commands::update_timer,
        commands::delete_timer,
        commands::preview_fires,
        commands::query_neighbours,
        commands::get_pause_all,
        commands::set_pause_all,
        commands::wizard_status,
        commands::wizard_set_choice,
        commands::wizard_re_run,
        commands::demo_info,
        commands::demo_launch,
        commands::demo_open_docs,
        commands::set_demo_opt_in,
        commands::app_info,
        commands::wake_status,
        commands::wake_reprobe,
        commands::set_wake_enabled,
        commands::set_autostart_enabled,
        commands::set_max_concurrent_actions,
        commands::dependency_check,
        commands::wake_fix_powercfg,
        commands::wake_enroll_macos,
        commands::wake_open_login_items,
        commands::get_misfire_defaults,
        commands::set_misfire_defaults,
    ])
}

/// Background thread: login1 `PrepareForSleep` + delay inhibitor (synthesis §2).
/// Budget ≤5 s on the suspending path — only rearm, no heavy work.
#[cfg(target_os = "linux")]
fn start_linux_power_watch(app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("bellman-login1".into())
        .spawn(move || {
            if let Err(e) = linux_power_watch_loop(&app) {
                log::warn!("bellman: login1 power watch exited: {e}");
            }
        })
        .ok();
}

#[cfg(target_os = "linux")]
fn linux_power_watch_loop(app: &tauri::AppHandle) -> Result<(), String> {
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::OwnedFd;

    let conn = Connection::system().map_err(|e| format!("system bus: {e}"))?;
    let proxy = Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .map_err(|e| format!("login1 proxy: {e}"))?;

    // Delay inhibitor is held only across the PrepareForSleep(true) handler
    // (synthesis §2-Linux: ≤5 s budget, then release). Re-take after resume so
    // the next suspend is also delayed. Holding it for the process lifetime
    // stalls every suspend for the full InhibitDelayMaxUSec.
    let take_inhibit = || -> Result<OwnedFd, String> {
        proxy
            .call_method(
                "Inhibit",
                &(
                    "sleep",
                    "Bellman",
                    "Rearm RTC wake before suspend",
                    "delay",
                ),
            )
            .and_then(|m| m.body().deserialize())
            .map_err(|e| format!("Inhibit: {e}"))
    };

    let mut inhibit: Option<OwnedFd> = take_inhibit().ok();

    let mut msgs = proxy
        .receive_signal("PrepareForSleep")
        .map_err(|e| format!("PrepareForSleep match: {e}"))?;

    for msg in &mut msgs {
        let preparing: bool = match msg.body().deserialize() {
            Ok(b) => b,
            Err(_) => continue,
        };
        let Some(state) = app.try_state::<AppState>() else {
            continue;
        };
        let cands = state.wake_candidates();
        let now = chrono::Utc::now();
        if preparing {
            // Work inside the delay budget, then release so suspend proceeds.
            state
                .wake
                .on_power_event(bellman_core::PowerEvent::Suspending, &cands, now);
            drop(inhibit.take());
        } else {
            state
                .wake
                .on_power_event(bellman_core::PowerEvent::Resumed, &cands, now);
            state.emit_wake_capability_if_changed();
            // Re-arm the delay inhibitor for the next suspend.
            inhibit = take_inhibit().ok();
        }
    }
    Ok(())
}

fn resolve_data_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(format!("resolve data dir: {e}"))))
}

fn apply_autostart_from_config<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    cfg: &config::Config,
) -> tauri::Result<()> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    let enabled = mgr.is_enabled().unwrap_or(false);
    if cfg.autostart_enabled && !enabled {
        let _ = mgr.enable();
    } else if !cfg.autostart_enabled && enabled {
        let _ = mgr.disable();
    }
    Ok(())
}

fn setup_err<E: std::fmt::Display>(ctx: &'static str) -> impl FnOnce(E) -> tauri::Error {
    move |e| tauri::Error::Anyhow(anyhow::anyhow!(format!("{ctx}: {e}")))
}

fn setup_err_str(s: String) -> tauri::Error {
    tauri::Error::Anyhow(anyhow::anyhow!(s))
}

/// Record the user's wizard answer (called by the webview when the modal
/// closes; we keep the public function so the command module can call it).
#[allow(dead_code)]
pub(crate) fn persist_wizard_choice(
    data_dir: &std::path::Path,
    choice: WizardChoice,
) -> anyhow::Result<()> {
    let _ = (data_dir, choice);
    Ok(())
}
