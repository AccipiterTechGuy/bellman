//! Notify-based watcher (hint only) + periodic rescan (truth).
//!
//! OS file-watch backends drop events; every hint and every tick of the
//! periodic timer must run a full `SlotService::poll`.

use super::error::{SlotError, SlotResult};
use super::service::SlotService;
use crate::store::Store;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

/// Wake reason for the slot loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotWake {
    /// Filesystem notify hint (may be lossy).
    NotifyHint,
    /// Periodic full rescan (source of truth).
    PeriodicRescan,
    /// Explicit kick from the control plane / tests.
    Manual,
}

/// Handle that signals a background slot loop to stop.
#[derive(Debug, Clone)]
pub struct SlotWatcherStop {
    tx: Sender<()>,
}

impl SlotWatcherStop {
    pub fn stop(&self) {
        let _ = self.tx.send(());
    }
}

/// One-shot helper: poll once (no watcher). Preferred path for tests and for
/// embedding in an existing event loop that already has a timer.
pub fn poll_once(service: &SlotService, store: &mut Store) -> SlotResult<usize> {
    service.poll(store)
}

/// Run a blocking slot loop: notify hints + periodic rescan.
///
/// Returns when `stop` is signaled (call [`SlotWatcherStop::stop`] from another
/// thread). The returned handle is also what the caller uses to stop the loop
/// after spawning this on a worker thread — for in-process use, pass a clone
/// of the stop channel before calling.
///
/// For most embeds prefer [`spawn_slot_thread`], which owns the store connection.
pub fn run_slot_loop(
    service: &SlotService,
    store: &mut Store,
    poll_interval: Duration,
    stop_rx: mpsc::Receiver<()>,
    mut on_wake: impl FnMut(SlotWake, usize),
) -> SlotResult<()> {
    let free_dir = service.layout().free_dir();
    let work_dir = service.layout().work_dir();

    let (hint_tx, hint_rx) = mpsc::channel::<SlotWake>();
    let mut watcher = build_watcher(hint_tx.clone())?;
    watcher
        .watch(&free_dir, RecursiveMode::NonRecursive)
        .map_err(|e| SlotError::Io(format!("watch free/: {e}")))?;
    watcher
        .watch(&work_dir, RecursiveMode::NonRecursive)
        .map_err(|e| SlotError::Io(format!("watch work/: {e}")))?;

    // Initial full scan (truth on startup).
    let n = service.poll(store)?;
    on_wake(SlotWake::Manual, n);

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        let wake = match hint_rx.recv_timeout(poll_interval) {
            Ok(w) => w,
            Err(RecvTimeoutError::Timeout) => SlotWake::PeriodicRescan,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        // Coalesce bursty notify events into one poll.
        while hint_rx.try_recv().is_ok() {}
        if stop_rx.try_recv().is_ok() {
            break;
        }
        let n = service.poll(store)?;
        on_wake(wake, n);
    }
    // Keep watcher alive until loop ends.
    drop(watcher);
    Ok(())
}

/// Spawn a background rescan+notify loop on a dedicated thread.
///
/// Opens its own store connection at `db_path` (SQLite connections are not
/// shared across threads).
pub fn spawn_slot_thread(
    slots_root: impl AsRef<Path>,
    db_path: impl AsRef<Path>,
    poll_interval: Duration,
) -> SlotResult<SlotWatcherStop> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let slots_root = slots_root.as_ref().to_path_buf();
    let db_path = db_path.as_ref().to_path_buf();

    std::thread::Builder::new()
        .name("bellman-slots".into())
        .spawn(move || {
            let mut store = match Store::open(&db_path) {
                Ok(s) => s,
                Err(_) => return,
            };
            let service = match SlotService::open(&slots_root, Default::default()) {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = run_slot_loop(&service, &mut store, poll_interval, stop_rx, |_, _| {});
        })
        .map_err(|e| SlotError::Io(format!("spawn slot thread: {e}")))?;

    Ok(SlotWatcherStop { tx: stop_tx })
}

fn build_watcher(hint_tx: Sender<SlotWake>) -> SlotResult<RecommendedWatcher> {
    RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| match res {
            Ok(event) => {
                if !event.paths.is_empty() {
                    let _ = hint_tx.send(SlotWake::NotifyHint);
                }
            }
            Err(_) => {
                let _ = hint_tx.send(SlotWake::NotifyHint);
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| SlotError::Io(format!("notify watcher: {e}")))
}

/// Watch only `free/` and invoke `on_hint` (for embeds that own their own poll loop).
pub fn watch_free_dir(
    free_dir: impl AsRef<Path>,
    on_hint: impl Fn() + Send + 'static,
) -> SlotResult<RecommendedWatcher> {
    let free_dir = free_dir.as_ref().to_path_buf();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if res.is_ok() {
                on_hint();
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| SlotError::Io(format!("notify watcher: {e}")))?;
    watcher
        .watch(&free_dir, RecursiveMode::NonRecursive)
        .map_err(|e| SlotError::Io(format!("watch {}: {e}", free_dir.display())))?;
    Ok(watcher)
}

/// Ensure PathBuf is used (silence unused if only Path appears in signatures).
#[allow(dead_code)]
fn _path_buf_identity(p: PathBuf) -> PathBuf {
    p
}
