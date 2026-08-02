//! Desktop notification — interface stub until C7 wires the Tauri plugin.

/// Result of a notify attempt (always "stubbed" in this phase).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyOutcome {
    /// Notification title; the part every desktop shows.
    pub title: String,
    /// Notification body; may be empty.
    pub body: String,
    /// Always true for the stub — records that the interface was invoked.
    pub stubbed: bool,
}

/// Record a desktop notification request without showing a real toast.
///
/// C7 replaces this with the Tauri notification plugin. Callers still get a
/// successful wake path so timers with `Action::Notify` exercise the full
/// claim → act → complete loop.
pub fn notify_stub(title: impl Into<String>, body: impl Into<String>) -> NotifyOutcome {
    let title = title.into();
    let body = body.into();
    // Visible breadcrumb for run-now / demos (stderr keeps JSON stdout clean).
    eprintln!("bellman: notify stub title={title:?} body={body:?}");
    NotifyOutcome {
        title,
        body,
        stubbed: true,
    }
}
