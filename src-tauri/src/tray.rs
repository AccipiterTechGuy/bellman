//! System tray icon + menu: Open / Pause all / Quit.
//!
//! Mirrors the build_plan product menu (`open`, `pause-all`, `quit`). The
//! pause-all menu item reads the current `AppState` flag and toggles it
//! in place; the tray stays in sync with the in-window toggle on the
//! All-timers page.

use std::sync::Arc;

use parking_lot::Mutex;
use tauri::menu::{CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

const ID_OPEN: &str = "bellman-open";
const ID_PAUSE: &str = "bellman-pause-all";
const ID_QUIT: &str = "bellman-quit";

/// Install the system tray icon + menu. Idempotent — only one tray per app.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
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
    if let Some(state) = app.try_state::<AppState>() {
        let _ = pause_check.set_checked(state.pause_all());
    }
    // Keep a handle to the check item so menu events can update its label
    // and the in-memory flag together.
    let pause_check_arc: Arc<Mutex<Option<CheckMenuItem<R>>>> = Arc::new(Mutex::new(Some(pause_check)));

    let _tray = TrayIconBuilder::with_id("bellman-tray")
        .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
            tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
                .expect("tray.png present")
        }))
        .tooltip("Bellman")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event({
            let app2 = app.clone();
            move |_tray, event| {
                if let TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } = event
                {
                    show_main(&app2);
                } else if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    ..
                } = event {
                    show_main(&app2);
                }
            }
        })
        .on_menu_event({
            let app2 = app.clone();
            let pause_check = pause_check_arc.clone();
            move |app, event| match event.id().as_ref() {
                ID_OPEN => show_main(app),
                ID_QUIT => app.exit(0),
                ID_PAUSE => {
                    let Some(state) = app.try_state::<AppState>() else {
                        return;
                    };
                    let next = !state.pause_all();
                    state.set_pause_all(next);
                    if let Some(item) = pause_check.lock().as_ref() {
                        let _ = item.set_checked(next);
                    }
                    let _ = app2.emit(
                        "pause-all-changed",
                        serde_json::json!({ "paused": next }),
                    );
                }
                _ => {}
            }})
            .build(app)?;
    Ok(())
}

fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}
