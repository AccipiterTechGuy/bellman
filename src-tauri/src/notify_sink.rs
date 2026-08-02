//! Real desktop-notification sink wrapping `tauri-plugin-notification`.
//!
//! Bellman-core's `ActionRunner` calls `show(title, body)` whenever a
//! `Action::Notify` timer fires. The Tauri shell installs this sink so the
//! user sees a real toast (the C6 CLI keeps the stub instead).

use std::sync::Arc;

use bellman_core::NotifyOutcome;
use tauri::AppHandle;
use tauri::Runtime;
use tauri_plugin_notification::NotificationExt;

/// NotifySink impl backed by the Tauri notification plugin.
pub struct TauriNotifySink<R: Runtime> {
    handle: AppHandle<R>,
}

impl<R: Runtime> TauriNotifySink<R> {
    /// Wrap a Tauri app handle as the engine's desktop-notification sink.
    pub fn new(handle: AppHandle<R>) -> Arc<Self> {
        Arc::new(Self { handle })
    }
}

impl<R: Runtime + 'static> bellman_core::NotifySink for TauriNotifySink<R> {
    fn show(&self, title: &str, body: &str) -> NotifyOutcome {
        // Best-effort: the plugin's `builder().show()` is async; we fire and
        // forget. A failure to display a toast is not a fire-path error.
        if let Err(e) = self
            .handle
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
        {
            log::warn!("bellman: notification failed: {e}");
        }
        NotifyOutcome {
            title: title.to_string(),
            body: body.to_string(),
            stubbed: false,
        }
    }
}
