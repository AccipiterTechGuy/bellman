//! System tray icon + menu: Open / Pause all / Quit.
//!
//! Mirrors the build_plan product menu (`open`, `pause-all`, `quit`). The
//! pause-all menu item reads the current `AppState` flag and toggles it
//! in place; the tray stays in sync with the in-window toggle on the
//! All-timers page via the shared `AppState::tray_pause_check` slot.

use tauri::menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

const ID_OPEN: &str = "bellman-open";
const ID_PAUSE: &str = "bellman-pause-all";
const ID_QUIT: &str = "bellman-quit";

/// Install the system tray icon + menu. Must be called AFTER
/// `app.manage(state)` so the tray's pause-toggle can call
/// `AppState::set_pause_all` AND stash the pause-check item into
/// `AppState::tray_pause_check` for cross-sync from the Tauri command.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let pause_check = CheckMenuItemBuilder::with_id(ID_PAUSE, "Pause all")
        .checked(false)
        .build(app)?;
    let open = MenuItemBuilder::with_id(ID_OPEN, "Open Bellman").build(app)?;
    let quit = MenuItemBuilder::with_id(ID_QUIT, "Quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&open)
        .item(&pause_check)
        .separator()
        .item(&quit)
        .build()?;

    // Mirror the current pause-all flag into the check item at install time.
    let initial_paused = if let Some(state) = app.try_state::<AppState>() {
        let _ = pause_check.set_checked(state.pause_all());
        state.pause_all()
    } else {
        false
    };

    // Stash the handle in AppState so the set_pause_all command can
    // mirror the flag back into the tray UI when the window toggles.
    if let Some(state) = app.try_state::<AppState>() {
        *state.tray_pause_check.lock() = Some(pause_check.clone());
    }

    let _tray = TrayIconBuilder::with_id("bellman-tray")
        .icon(
            app.default_window_icon()
                .cloned()
                .unwrap_or_else(|| {
                    tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
                        .expect("tray.png present")
                }),
        )
        .tooltip("Bellman")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event({
            let app2 = app.clone();
            move |_tray, event| match event {
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
                | TrayIconEvent::Click {
                    button: MouseButton::Left,
                    ..
                } => show_main(&app2),
                _ => {}
            }
        })
        .on_menu_event(move |app, event| match event.id().as_ref() {
            ID_OPEN => show_main(app),
            ID_QUIT => app.exit(0),
            ID_PAUSE => {
                let Some(state) = app.try_state::<AppState>() else {
                    return;
                };
                let next = !state.pause_all();
                state.set_pause_all(next);
                let tray_item = state.tray_pause_check.lock().clone();
                drop(state);
                if let Some(item) = tray_item {
                    let _ = item.set_checked(next);
                }
                // Emit a bool payload (consistent with the set_pause_all
                // command — the UI reads `event.payload` as a bool).
                let _ = app.emit("pause-all-changed", next);
            }
            _ => {}
        })
        .build(app)?;

    log::info!(
        "bellman: tray installed (paused={}, tray_check=…)",
        initial_paused
    );
    Ok(())
}

/// Update the tray's "Pause all" check item from outside the tray
/// callback (e.g. from the `set_pause_all` Tauri command). Silently
/// no-ops when the tray is not installed (e.g. on a system that
/// never registered it).
pub fn set_tray_pause_check(app: &AppHandle, paused: bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let item = state.tray_pause_check.lock().clone();
    drop(state);
    if let Some(item) = item {
        let _ = item.set_checked(paused);
    }
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}
