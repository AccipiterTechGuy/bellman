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
use bellman_core::{
    ActionRunner, ActionRunnerConfig, NotifySink, RunNowOptions, SingleNextWake, WakeCandidate,
    WakeCapability,
};
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
    /// Single-next-wake bridge (RTC arm / capability probe).
    pub wake: SingleNextWake,
    /// IK3 reply watcher thread handle (set after `start_scheduler`).
    pub reply_watcher: Mutex<Option<bellman_core::slots::WatchStop>>,
    /// IK3 duration anchors shared by the scheduler, `run_now` and the reply
    /// watcher so `duration_ms` stays monotonic across threads.
    pub reply_anchors: bellman_core::reply::SharedAnchors,
    /// IK3 monotonic deadline book shared by the scheduler, `run_now` and
    /// the watcher (persisted wall deadlines are only the restart fallback).
    pub reply_deadlines: bellman_core::reply::SharedDeadlines,
}

impl AppState {
    pub fn new(
        store: Store,
        data_dir: PathBuf,
        config: Config,
        pause_all: bool,
        notify_sink: Arc<dyn NotifySink>,
    ) -> Self {
        let wake = SingleNextWake::with_platform_default(config.wake_enabled);
        Self {
            store: Arc::new(Mutex::new(store)),
            data_dir,
            config: Mutex::new(config),
            pause_all: Mutex::new(pause_all),
            notify_sink,
            control_handle: Mutex::new(None),
            tray_pause_check: parking_lot::Mutex::new(None),
            wake,
            reply_watcher: Mutex::new(None),
            reply_anchors: bellman_core::reply::new_anchors(),
            reply_deadlines: bellman_core::reply::new_deadlines(),
        }
    }

    pub fn set_wake_master(&self, enabled: bool) {
        self.wake.set_master_enabled(enabled);
    }

    pub fn wake_status_line(&self) -> String {
        self.wake.status_line()
    }

    pub fn wake_reprobe(&self) -> WakeCapability {
        self.wake.re_probe()
    }

    /// Collect wake candidates from the store and rearm the single next wake.
    pub fn rearm_wake(&self) {
        let cands = self.wake_candidates();
        let now = Utc::now();
        if let Err(e) = self.wake.rearm_from_candidates(&cands, now) {
            log::warn!("bellman: wake rearm failed: {e}");
        }
    }

    pub fn wake_candidates(&self) -> Vec<WakeCandidate> {
        let store = self.store.lock();
        match store.list_timers() {
            Ok(timers) => timers
                .into_iter()
                .map(|t| WakeCandidate {
                    enabled: t.enabled,
                    wake_machine: t.wake_machine,
                    next_fire_utc: t.next_fire_utc,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Emit a `wake_capability` JSONL event when the status line changes.
    pub fn emit_wake_capability_if_changed(&self) {
        if let Some((line, _cap)) = self.wake.take_status_transition() {
            self.emit_wake_capability_line(&line);
        }
    }

    pub fn emit_wake_capability_startup(&self) {
        let line = self.wake.status_line();
        self.emit_wake_capability_line(&line);
        self.wake.mark_status_emitted();
    }

    fn emit_wake_capability_line(&self, line: &str) {
        let store = self.store.lock();
        if let Err(e) = store.enqueue_event(
            &bellman_core::events::EventRecord::new(
                bellman_core::events::RunState::WakeCapability,
            )
            .with_message(line),
        ) {
            log::warn!("bellman: wake_capability enqueue failed: {e}");
        }
        drop(store);
        log::info!("bellman: {line}");
    }

    pub fn wake_status_dto(&self) -> crate::commands::WakeStatusDto {
        let cap = self.wake.capability();
        let master = self.wake.master_enabled();
        let platform_cap = self.wake.platform_capability();
        let platform_enabled = platform_cap.is_enabled();

        let (fix_hint, fix_action) = match &platform_cap {
            WakeCapability::Disabled { reason } => (
                reason.fix_hint().map(|s| s.to_string()),
                reason.fix_action().map(|s| s.to_string()),
            ),
            _ if !master => (
                Some("Enable “Allow Bellman to wake this machine” in Settings.".into()),
                None,
            ),
            _ => (None, None),
        };

        #[cfg(target_os = "linux")]
        let udev_snippet = Some(
            bellman_core::platform::wake::linux::udev_rule_snippet().to_string(),
        );
        #[cfg(not(target_os = "linux"))]
        let udev_snippet = None;

        let powercfg_command = match &platform_cap {
            WakeCapability::Disabled {
                reason: bellman_core::DisabledReason::WakeTimersDisabledByPolicy { rail, .. },
            } => Some(crate::wake_fixit::powercfg_command_for_rail(*rail)),
            _ => {
                #[cfg(target_os = "windows")]
                {
                    Some(crate::wake_fixit::powercfg_command_for_rail(
                        bellman_core::PowerRail::Ac,
                    ))
                }
                #[cfg(not(target_os = "windows"))]
                {
                    None
                }
            }
        };

        #[cfg(target_os = "macos")]
        let login_items_url = Some(crate::wake_fixit::login_items_deeplink().to_string());
        #[cfg(not(target_os = "macos"))]
        let login_items_url = if matches!(
            fix_action.as_deref(),
            Some("macos_enroll") | Some("macos_login_items")
        ) {
            Some(crate::wake_fixit::login_items_deeplink().to_string())
        } else {
            None
        };

        let platform = std::env::consts::OS.to_string();
        crate::commands::WakeStatusDto {
            status_line: cap.status_line(),
            enabled: cap.is_enabled(),
            master_enabled: master,
            platform_enabled,
            platform,
            fix_hint,
            fix_action,
            udev_snippet,
            powercfg_command,
            login_items_url,
            capability: serde_json::to_value(&cap).unwrap_or(serde_json::Value::Null),
        }
    }

    /// Spawn the background scheduler thread. The thread is the only writer
    /// to its own `Store`/`ActionRunner` handles; commands mutate the shared
    /// store directly and send `Refill` so the next tick rebuilds the heap.
    pub fn start_scheduler(&self) {
        let store = self.store.lock();
        // Engine tunables from config.json (horizon, retention, concurrency…).
        let app_cfg = self.config.lock().clone();
        // IK3: one duration-anchor registry shared between the fire paths
        // (scheduler thread, run_now) and the reply watcher thread, so
        // `duration_ms` stays monotonic across both.
        let anchors = self.reply_anchors.clone();
        let deadlines = self.reply_deadlines.clone();
        let cfg = SchedulerConfig::from_app_config(&app_cfg)
            .with_data_dir(self.data_dir.clone())
            .with_anchors(anchors.clone())
            .with_deadlines(deadlines.clone());
        // R11: the runner enqueues into the outbox (its own store
        // connection); the elected publisher appends, so scheduled fires
        // land in events.current.jsonl exactly like run-now events.
        let runner_cfg = ActionRunnerConfig {
            max_concurrent_actions: app_cfg.max_concurrent_actions,
            ..ActionRunnerConfig::default()
        };
        let mut runner = ActionRunner::new(runner_cfg)
            .with_notify_sink(self.notify_sink.clone());
        match bellman_core::open_store(&self.data_dir.join("timers.db")) {
            Ok(sink) => runner = runner.with_event_sink(sink),
            Err(e) => log::error!("bellman: could not open runner event sink: {e}"),
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

        // IK3: the ONE background watcher — slot channel, reply channel
        // (ingest, monotonic deadlines, reconciler) and the R11 publisher
        // safety tick on a single thread.
        {
            let engine = bellman_core::reply::ReplyEngine {
                tree: bellman_core::TimersTree::new(&self.data_dir),
                data_dir: self.data_dir.clone(),
                pickup_grace: app_cfg.pickup_grace(),
                watchdog_factor: app_cfg.watchdog_factor,
                anchors,
                deadlines,
            };
            match bellman_core::slots::spawn_watch_thread(bellman_core::slots::WatchConfig {
                slots_root: self.data_dir.join("slots"),
                data_dir: self.data_dir.clone(),
                db_path: self.data_dir.join("timers.db"),
                reply_engine: Some(engine),
                poll_interval: bellman_core::reply::DEFAULT_POLL_INTERVAL,
            }) {
                Ok(stop) => *self.reply_watcher.lock() = Some(stop),
                Err(e) => log::error!("bellman: watcher spawn failed: {e}"),
            }
        }
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
