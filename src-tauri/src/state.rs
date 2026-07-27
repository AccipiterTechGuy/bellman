//! Process-wide application state.
//!
//! One `Store` (sqlite, WAL) + one background scheduler thread + the
//! persisted config + the global pause-all flag. Wrapped in `Arc<Mutex<…>>`
//! for the Tauri command handlers.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bellman_core::scheduler::{ControlHandle, Scheduler, SchedulerConfig, SystemClock};
use bellman_core::store::Store;
use bellman_core::{ActionRunner, ActionRunnerConfig, NotifySink, RunNowOptions};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;

/// State owned by the running app. Stored under a Tauri `State<>` handle.
pub struct AppState {
    /// The SQLite-backed store. The scheduler thread holds its own
    /// `&mut Store` view via the scheduler; commands acquire the same lock
    /// when they need to mutate.
    pub store: Arc<Mutex<Store>>,
    /// Bellman data directory (config, logs, slots, db).
    pub data_dir: PathBuf,
    /// Persisted user preferences (wizard answer, autostart toggle, etc).
    pub config: Mutex<Config>,
    /// Global pause-all flag (distinct from per-timer `enabled`).
    pub pause_all: Mutex<bool>,
    /// Notification sink the engine uses for `Action::Notify` timers.
    pub notify_sink: Arc<dyn NotifySink>,
    /// Control handle for the scheduler (set after `start_scheduler`).
    pub control_handle: Mutex<Option<ControlHandle>>,
    /// Handle to the tray's "Pause all" CheckMenuItem, set when the
    /// tray is installed. Used by the `set_pause_all` Tauri command
    /// to keep the tray in sync with the in-window toggle. The `Wry`
    /// runtime is the default Tauri runtime. `CheckMenuItem: Clone`,
    /// so the inner `Mutex` is not needed.
    pub tray_pause_check: parking_lot::Mutex<Option<tauri::menu::CheckMenuItem<tauri::Wry>>>,
}

impl AppState {
    pub fn new(
        store: Store,
        data_dir: PathBuf,
        config: Config,
        pause_all: bool,
        notify_sink: Arc<dyn NotifySink>,
    ) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            data_dir,
            config: Mutex::new(config),
            pause_all: Mutex::new(pause_all),
            notify_sink,
            control_handle: Mutex::new(None),
            tray_pause_check: parking_lot::Mutex::new(None),
        }
    }

    /// Spawn the background scheduler thread. The thread is the only writer
    /// to its own `Store`/`ActionRunner` handles; commands mutate the shared
    /// store directly and send `Refill` so the next tick rebuilds the heap.
    pub fn start_scheduler(&self) {
        let mut store = self.store.lock();
        let cfg = SchedulerConfig::default();
        // Open the JSONL event log once and hand it to the runner so
        // scheduled fires (not just run-now) land in events.current.jsonl.
        // Without this, the per-timer "live log tail" panel in the GUI
        // would only see events from manual run-now invocations, and the
        // docs/QA_P3.md "window close leaves engine firing" check would
        // be unprovable.
        let event_log = match bellman_core::EventLog::open_under(&self.data_dir) {
            Ok(log) => Some(log),
            Err(e) => {
                log::error!("bellman: could not open event log under {}: {e}", self.data_dir.display());
                None
            }
        };
        let mut runner = ActionRunner::new(ActionRunnerConfig::default())
            .with_notify_sink(self.notify_sink.clone());
        if let Some(log) = event_log {
            runner = runner.with_event_log(log);
        }
        // Same db path: the scheduler opens its own connection on a clone.
        // (SQLite is happy with multiple readers + one writer; WAL mode is
        // enabled by `Store::open_with`.)
        let scheduler_store_path = self.data_dir.join("timers.db");
        let sched_store = bellman_core::open_store(&scheduler_store_path)
            .expect("scheduler store open");
        let pause_all = *self.pause_all.lock();
        let mut sched = if pause_all {
            Scheduler::new_paused(sched_store, SystemClock::new(), runner, cfg)
        } else {
            Scheduler::new(sched_store, SystemClock::new(), runner, cfg)
        };
        sched.boot().expect("scheduler boot");
        let handle = sched.control_handle();
        *self.control_handle.lock() = Some(handle);
        drop(store); // release the AppState lock for the thread

        thread::Builder::new()
            .name("bellman-scheduler".into())
            .spawn(move || {
                log::info!("bellman: scheduler thread started");
                let r = sched.run_until_shutdown();
                if let Err(e) = r {
                    log::error!("bellman: scheduler stopped with error: {e}");
                } else {
                    log::info!("bellman: scheduler thread exited cleanly");
                }
            })
            .expect("scheduler thread spawn");
    }

    /// Apply a pause-all change to the running scheduler. Sends the
    /// control message so the loop observes the flag at the next tick.
    pub fn set_pause_all(&self, paused: bool) {
        *self.pause_all.lock() = paused;
        if let Some(h) = self.control_handle.lock().as_ref() {
            h.set_pause_all(paused);
        }
        let _ = crate::config::write_pause_all_flag(&self.data_dir, paused);
    }

    pub fn pause_all(&self) -> bool {
        *self.pause_all.lock()
    }

    /// CLI `--run-now <name-or-id>` entry point: run a timer now through
    /// the real fire path. Logs the outcome to stderr; the GUI is unaffected.
    pub fn cli_run_now(&self, name_or_id: &str) -> Result<RunNowResponse, String> {
        let timer = resolve_timer(&self.store.lock(), name_or_id)?;
        let mut store = self.store.lock();
        let opts = RunNowOptions {
            notify_sink: Some(self.notify_sink.clone()),
            ..Default::default()
        };
        let outcome = bellman_core::run_now(&mut store, &self.data_dir.join("timers.db"), timer.id, &opts)
            .map_err(|e| e.to_string())?;
        // Wake the scheduler so any UI-driven run-now is also visible to it.
        if let Some(h) = self.control_handle.lock().as_ref() {
            h.refill();
        }
        Ok(RunNowResponse::from(outcome))
    }
}

/// Look up a timer by name-or-id (mirrors the CLI's resolver).
pub fn resolve_timer(
    store: &parking_lot::MutexGuard<'_, Store>,
    name_or_id: &str,
) -> Result<bellman_core::Timer, String> {
    use std::str::FromStr;
    if let Ok(id) = Uuid::from_str(name_or_id) {
        if let Ok(Some(t)) = store.get_timer(id) {
            return Ok(t);
        }
    }
    let all = store.list_timers().map_err(|e| e.to_string())?;
    let mut matches: Vec<_> = all.into_iter().filter(|t| t.name == name_or_id).collect();
    match matches.len() {
        0 => Err(format!("timer not found: {name_or_id}")),
        1 => Ok(matches.pop().unwrap()),
        _ => Err(format!("ambiguous name: {name_or_id}")),
    }
}

/// Subset of `RunNowOutcome` we expose over Tauri. Simpler than the full
/// internal shape; the webview only needs id, name, run_id, message, and
/// the updated timer's enabled/next_fire fields. camelCase at the IPC
/// boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunNowResponse {
    pub timer_id: Uuid,
    pub name: String,
    pub run_id: Uuid,
    pub scheduled_for: DateTime<Utc>,
    pub message: String,
    pub enabled: bool,
    pub next_fire_utc: Option<DateTime<Utc>>,
}

impl From<bellman_core::RunNowOutcome> for RunNowResponse {
    fn from(o: bellman_core::RunNowOutcome) -> Self {
        Self {
            timer_id: o.timer.id,
            name: o.timer.name.clone(),
            run_id: o.run_id,
            scheduled_for: o.scheduled_for,
            message: o.message,
            enabled: o.timer.enabled,
            next_fire_utc: o.timer.next_fire_utc,
        }
    }
}

/// Small helper for callers that need to know whether the scheduler is up.
pub fn tick_interval_hint() -> Duration {
    Duration::from_millis(500)
}
