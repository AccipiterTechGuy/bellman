//! Pluggable desktop-notification sink.
//!
//! C6 shipped a `notify_stub` that just logs the title/body. C7 (Tauri shell)
//! wants the real toast via `tauri-plugin-notification`, so we keep the sink
//! behind a trait — `bellman-cli` keeps the stub implementation, the Tauri
//! binary injects a wrapper around the plugin.
//!
//! The [`ActionRunner`] resolves a sink at construction time. When the sink is
//! the stub, the behaviour is identical to C6; when it is the real one, the
//! toast appears in the user's notification centre.

use super::notify::NotifyOutcome;

/// Pluggable notification sink.
pub trait NotifySink: Send + Sync {
    /// Show a desktop notification. The outcome reports the displayed title and
    /// body; the trait does not fail — a failure is just a missed toast and
    /// does not block the fire path. Implementations should swallow and log.
    fn show(&self, title: &str, body: &str) -> NotifyOutcome;

    /// Whether this sink is the C6 stub. Used to keep the legacy
    /// `last_message` format (`"notify stub title=…"`) intact for the CLI.
    /// Real sinks return `false` so the runner emits a `"notify title=…"`
    /// message that doesn't claim a real toast was attempted.
    fn is_stub(&self) -> bool {
        false
    }
}

/// Default sink: the C6 stub. No native toast; logs the title/body on stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubNotifySink;

impl NotifySink for StubNotifySink {
    fn show(&self, title: &str, body: &str) -> NotifyOutcome {
        let outcome = super::notify::notify_stub(title, body);
        // Mirror the C6 log line on stderr for the stub path (so it stays
        // visible to integrators who watch stderr).
        eprintln!(
            "bellman: notify stub title={:?} body={:?}",
            outcome.title, outcome.body
        );
        outcome
    }

    fn is_stub(&self) -> bool {
        true
    }
}
