//! First-run wizard data (the question the modal asks the user).

use serde::{Deserialize, Serialize};

/// The three questions the wizard asks. The webview ships a single
/// `wizard_set_choice` command carrying this struct.
///
/// camelCase at the IPC boundary so `choice.startMinimized` (JS) lines up
/// with `start_minimized` (Rust) without a hand-written rename.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WizardChoice {
    /// "Launch Bellman automatically when I log in?" (XDG autostart / macOS
    /// Login Item / Windows Run key).
    pub autostart: bool,
    /// "Start hidden in the system tray, or show the window?" Persists as
    /// `start_minimized` in the config.
    pub start_minimized: bool,
    /// "Try to wake this machine from sleep so timers fire on time?"
    /// Stored for C11; C7 itself does not implement wake yet.
    pub wake_enabled: bool,
}

/// What the webview asks for at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WizardStatus {
    /// True when the wizard has been completed at least once.
    pub completed: bool,
    /// Default values to pre-fill the modal (last answer or product default).
    pub defaults: WizardChoice,
}
