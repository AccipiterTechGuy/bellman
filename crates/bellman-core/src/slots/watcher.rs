//! Notify-based watcher (hint only) + periodic rescan (truth).
//!
//! Stack (BUILD_PLAN locked): **notify v8** + **notify-debouncer-full**.
//! OS file-watch backends drop events; every debounced hint and every tick of
//! the periodic timer must run a full `SlotService::poll`.

use super::error::{SlotError, SlotResult};
use super::service::SlotService;
use crate::store::Store;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{
    new_debouncer, DebounceEventResult, Debouncer, RecommendedCache,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

/// Default debounce window for notify hints (latency sugar, not truth).
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(200);

/// Wake reason for the slot loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotWake {
    /// Debounced filesystem notify hint (may still be lossy).
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

/// Run a blocking slot loop: debounced notify hints + periodic rescan.
///
/// Returns when `stop` is signaled (call [`SlotWatcherStop::stop`] from another
/// thread). For most embeds prefer [`spawn_slot_thread`], which owns the store
/// connection.
pub fn run_slot_loop(
    service: &SlotService,
    store: &mut Store,
    poll_interval: Duration,
    stop_rx: mpsc::Receiver<()>,
    on_wake: impl FnMut(SlotWake, usize),
) -> SlotResult<()> {
    run_slot_loop_with_debounce(
        service,
        store,
        poll_interval,
        DEFAULT_DEBOUNCE,
        stop_rx,
        on_wake,
    )
}

/// Same as [`run_slot_loop`] with an explicit debounce window.
pub fn run_slot_loop_with_debounce(
    service: &SlotService,
    store: &mut Store,
    poll_interval: Duration,
    debounce: Duration,
    stop_rx: mpsc::Receiver<()>,
    mut on_wake: impl FnMut(SlotWake, usize),
) -> SlotResult<()> {
    let free_dir = service.layout().free_dir();
    let work_dir = service.layout().work_dir();

    let (hint_tx, hint_rx) = mpsc::channel::<SlotWake>();
    let mut debouncer = build_debouncer(hint_tx, debounce)?;
    debouncer
        .watch(&free_dir, RecursiveMode::NonRecursive)
        .map_err(|e| SlotError::Io(format!("watch free/: {e}")))?;
    debouncer
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
        // Coalesce bursty debounced hints into one poll.
        while hint_rx.try_recv().is_ok() {}
        if stop_rx.try_recv().is_ok() {
            break;
        }
        let n = service.poll(store)?;
        on_wake(wake, n);
    }
    // Keep debouncer (and its inner watcher) alive until loop ends.
    drop(debouncer);
    Ok(())
}

/// Spawn a background rescan+debounced-notify loop on a dedicated thread.
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
            // IK2: slot add/modify/delete also projects the per-timer folder tree.
            let service = match db_path.parent() {
                Some(data_dir) => service.with_timers_tree(crate::tree::TimersTree::new(data_dir)),
                None => service,
            };
            let _ = run_slot_loop(&service, &mut store, poll_interval, stop_rx, |_, _| {});
        })
        .map_err(|e| SlotError::Io(format!("spawn slot thread: {e}")))?;

    Ok(SlotWatcherStop { tx: stop_tx })
}

type SlotDebouncer = Debouncer<RecommendedWatcher, RecommendedCache>;

fn build_debouncer(hint_tx: Sender<SlotWake>, debounce: Duration) -> SlotResult<SlotDebouncer> {
    new_debouncer(debounce, None, move |res: DebounceEventResult| match res {
        Ok(events) => {
            if !events.is_empty() {
                let _ = hint_tx.send(SlotWake::NotifyHint);
            }
        }
        Err(_errors) => {
            // Notify errors are still a reason to rescan (truth is poll).
            let _ = hint_tx.send(SlotWake::NotifyHint);
        }
    })
    .map_err(|e| SlotError::Io(format!("notify debouncer: {e}")))
}

/// Watch only `free/` with debouncing and invoke `on_hint` (embeds that own
/// their own poll loop). Returns the live debouncer — keep it alive.
pub fn watch_free_dir(
    free_dir: impl AsRef<Path>,
    on_hint: impl Fn() + Send + 'static,
) -> SlotResult<SlotDebouncer> {
    watch_free_dir_with_debounce(free_dir, DEFAULT_DEBOUNCE, on_hint)
}

/// Same as [`watch_free_dir`] with an explicit debounce window.
pub fn watch_free_dir_with_debounce(
    free_dir: impl AsRef<Path>,
    debounce: Duration,
    on_hint: impl Fn() + Send + 'static,
) -> SlotResult<SlotDebouncer> {
    let free_dir = free_dir.as_ref().to_path_buf();
    let mut debouncer = new_debouncer(debounce, None, move |res: DebounceEventResult| {
        if res.is_ok() {
            on_hint();
        }
    })
    .map_err(|e| SlotError::Io(format!("notify debouncer: {e}")))?;
    debouncer
        .watch(&free_dir, RecursiveMode::NonRecursive)
        .map_err(|e| SlotError::Io(format!("watch {}: {e}", free_dir.display())))?;
    Ok(debouncer)
}

/// Ensure PathBuf is used (silence unused if only Path appears in signatures).
#[allow(dead_code)]
fn _path_buf_identity(p: PathBuf) -> PathBuf {
    p
}
