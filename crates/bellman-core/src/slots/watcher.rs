//! Notify-based watcher (hint only) + periodic rescan (truth).
//!
//! Stack (BUILD_PLAN locked): **notify v8** + **notify-debouncer-full**.
//! OS file-watch backends drop events; every debounced hint and every tick of
//! the periodic timer must run a full `SlotService::poll`.

use super::error::{SlotError, SlotResult};
use super::service::SlotService;
use crate::store::Store;
use chrono::Utc;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

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

// ── The ONE background watcher (IK3) ────────────────────────────────────
//
// One thread, one debouncer, one store connection, one event publisher.
// The same loop drives: the slot channel poll, the reply-channel scan and
// monotonic deadline pass, the folder reconciler, and the R11 publisher
// safety tick. Anything file-shaped is a latency hint; the periodic full
// rescan is truth. No second watcher exists — the reply channel reuses
// this mechanism.

/// Configuration for [`spawn_watch_thread`].
#[derive(Clone)]
pub struct WatchConfig {
    /// Slot channel root (`<data_dir>/slots`).
    pub slots_root: PathBuf,
    /// Data root (reply tree, logs, locks).
    pub data_dir: PathBuf,
    /// SQLite database path (the thread opens its own connection).
    pub db_path: PathBuf,
    /// The reply engine; `None` runs the slot channel only.
    pub reply_engine: Option<crate::reply::ReplyEngine>,
    /// Scheduler control handle for deadline-heap hints (IK3): armed
    /// pickup/watchdog deadlines are forwarded so the scheduler wakes at the
    /// exact instant. `None` (no scheduler in this process) leaves the
    /// periodic poll as the deadline driver — always correct, just coarser.
    pub scheduler: Option<crate::scheduler::ControlHandle>,
    /// Periodic rescan cadence — also the deadline granularity and the
    /// publisher safety tick (R11 requires no slower than 1 s).
    pub poll_interval: Duration,
}

/// Handle to stop the background watcher thread (joins on drop-stop).
pub struct WatchStop {
    tx: Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl WatchStop {
    /// Signal shutdown and join the thread.
    pub fn stop(mut self) {
        let _ = self.tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the ONE background watcher: slot channel + reply channel +
/// publisher tick on a single thread with a single debouncer.
pub fn spawn_watch_thread(cfg: WatchConfig) -> SlotResult<WatchStop> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let handle = std::thread::Builder::new()
        .name("bellman-watch".into())
        .spawn(move || {
            if let Err(e) = run_watch_loop(cfg, stop_rx) {
                eprintln!("bellman: watcher exited: {e}");
            }
        })
        .map_err(|e| SlotError::Io(format!("spawn watch thread: {e}")))?;
    Ok(WatchStop {
        tx: stop_tx,
        handle: Some(handle),
    })
}

fn run_watch_loop(cfg: WatchConfig, stop_rx: mpsc::Receiver<()>) -> SlotResult<()> {
    let mut store = Store::open_with(
        &cfg.db_path,
        crate::store::OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .map_err(|e| SlotError::Io(format!("watcher store open: {e}")))?;
    // IK5: the slot `ack_through` hook projects status through its own
    // engine — hand it the same invalidation sink the reply engine carries.
    let slot_cfg = crate::slots::SlotConfig {
        status_listener: cfg
            .reply_engine
            .as_ref()
            .and_then(|e| e.status_listener.clone()),
        ..Default::default()
    };
    let service = SlotService::open(&cfg.slots_root, slot_cfg)?
        .with_timers_tree(crate::tree::TimersTree::new(&cfg.data_dir));
    let mut publisher = crate::events::EventPublisher::open(&cfg.data_dir)
        .map_err(|e| SlotError::Io(format!("watcher publisher: {e}")))?;
    let mut tracker = crate::reply::InvalidTracker::default();

    let (hint_tx, hint_rx) = mpsc::channel::<SlotWake>();
    let mut debouncer = build_debouncer(hint_tx, DEFAULT_DEBOUNCE)?;
    debouncer
        .watch(service.layout().free_dir(), RecursiveMode::NonRecursive)
        .map_err(|e| SlotError::Io(format!("watch free/: {e}")))?;
    debouncer
        .watch(service.layout().work_dir(), RecursiveMode::NonRecursive)
        .map_err(|e| SlotError::Io(format!("watch work/: {e}")))?;
    if let Some(engine) = &cfg.reply_engine {
        debouncer
            .watch(engine.tree.root(), RecursiveMode::Recursive)
            .map_err(|e| SlotError::Io(format!("watch timers/: {e}")))?;
    }

    // Initial full pass (truth on startup), then the loop.
    match service.poll(&mut store) {
        Ok(n) => refill_if_mutated(&cfg.scheduler, n),
        Err(e) => eprintln!("bellman: watcher: initial slot poll: {e}"),
    }
    if let Some(engine) = &cfg.reply_engine {
        let stats =
            crate::reply::poll_once(engine, &store, Utc::now(), Instant::now(), &mut tracker);
        if stats.errors > 0 {
            eprintln!(
                "bellman: watcher: initial reply pass: {} error(s)",
                stats.errors
            );
        }
        crate::reply::reconcile(engine, &store);
    }
    publisher.publish_cycle(&store);

    let mut polls: u32 = 0;
    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        match hint_rx.recv_timeout(cfg.poll_interval) {
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Coalesce bursty debounced hints into one pass.
        while hint_rx.try_recv().is_ok() {}
        if stop_rx.try_recv().is_ok() {
            break;
        }

        // Slot channel.
        match service.poll(&mut store) {
            Ok(n) => refill_if_mutated(&cfg.scheduler, n),
            Err(e) => eprintln!("bellman: watcher: slot poll: {e}"),
        }
        // Reply channel + monotonic deadlines.
        if let Some(engine) = &cfg.reply_engine {
            let stats =
                crate::reply::poll_once(engine, &store, Utc::now(), Instant::now(), &mut tracker);
            if stats.errors > 0 {
                eprintln!("bellman: watcher: reply pass: {} error(s)", stats.errors);
            }
            // Forward armed deadlines to the scheduler heap so it wakes at
            // the exact instant instead of the next poll tick.
            let hints = engine.take_deadline_hints();
            if let Some(scheduler) = &cfg.scheduler {
                for (run_id, kind, wall_at) in hints {
                    scheduler.arm_deadline(run_id, kind, wall_at);
                }
            }
            polls += 1;
            if polls.is_multiple_of(crate::reply::RECONCILE_EVERY_POLLS) {
                crate::reply::reconcile(engine, &store);
            }
        }
        // R11 publisher safety tick: a row committed by ANY process (CLI
        // enqueue, this loop's own transitions) is drained within one tick,
        // with no in-process signal required.
        publisher.publish_cycle(&store);
    }
    drop(debouncer);
    Ok(())
}

/// SCH2 (Path A): a processed slot request may have added, modified or
/// deleted a timer on this connection — the running scheduler's horizon heap
/// is stale until it rebuilds. Refill only when a request was actually
/// processed; idle polls must not cost the scheduler a store query.
fn refill_if_mutated(scheduler: &Option<crate::scheduler::ControlHandle>, processed: usize) {
    if processed > 0 {
        if let Some(s) = scheduler {
            s.refill();
        }
    }
}

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
