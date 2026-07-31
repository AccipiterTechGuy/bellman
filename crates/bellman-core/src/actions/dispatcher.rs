//! SCH1 — the bounded fire dispatcher and its worker lanes.
//!
//! ```text
//! timer heap  ──due──▶  claim in SQLite  ──▶  bounded fire queue  ──▶  worker lanes  ──▶  action
//!      ▲                                                                                   │
//!      └──────────────── advance next_fire immediately, do not wait ────────────────────────┘
//! ```
//!
//! The fire producer (scheduler loop, GUI `run_now`, CLI `run-now`) commits
//! the claim durably and only *nudges* this dispatcher. The dispatcher's
//! pump — running on worker dequeue/completion, enqueue failure, startup and
//! a periodic safety tick (≤ 1 s) — scans `pending` claims, reserves lane
//! order per timer, and sends disposable `run_id` hints into a bounded
//! channel (`2 × max_concurrent_actions`). A full channel drops the hint,
//! never the fire: the claim stays `pending` for the next pump pass.
//!
//! Workers own their SQLite connections, CAS `pending → active` before
//! executing (duplicate/stale hints lose and do nothing), run only the
//! configured action via [`ActionExecutor`], and commit the claim result plus
//! the R11 outbox event in ONE transaction. Workers never write
//! `status.json`, fire notifications or the JSONL log.
//!
//! Only the process holding the dispatcher OS lock
//! (`<data_dir>/locks/dispatcher.lock`) may pump; on acquisition the previous
//! owner's `active` claims return to `pending` and are re-queued with the
//! same `run_id` (at-least-once across a crash).

use super::cancel::CancellationToken;
use super::concurrency::ActionLimiter;
use super::executor::{ActionExecutor, ExecOutcome, ExecutorConfig};
use super::notify_sink::NotifySink;
use crate::events::{EventRecord, RunState};
use crate::scheduler::{FireAction, FireContext};
use crate::store::{ClaimStatus, OverlapPolicy, RunOutcome, Store, StoreResult, TimerId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use uuid::Uuid;

/// Lock file name under `<data_dir>/locks/` — the dispatcher OS lock.
pub const DISPATCHER_LOCK_NAME: &str = "dispatcher.lock";

const PRIMED: u8 = 0;
const STARTED: u8 = 1;
const STOPPING: u8 = 2;

/// Dispatcher configuration (derived from the existing settings — SCH1 adds
/// no config/JSON field).
#[derive(Clone)]
pub struct DispatcherConfig {
    /// The timers database (workers open their own connections to it).
    pub db_path: PathBuf,
    /// Data root when this process may own the dispatcher OS lock and run the
    /// publication pump. `None` (pure-store unit tests) → in-memory only,
    /// every claim is pumped without an OS lock.
    pub data_dir: Option<PathBuf>,
    /// Worker lanes = the existing `max_concurrent_actions` (minimum 1). The
    /// hint queue holds `2 ×` this (minimum 1).
    pub max_concurrent_actions: usize,
    /// Notification sink for `Action::Notify` timers.
    pub notify_sink: Arc<dyn NotifySink>,
    /// Executor tunables (timeouts / caps / test knobs).
    pub executor: ExecutorConfig,
    /// Periodic pump safety tick — the backstop for a lost in-process
    /// wakeup. Production: 1 s; tests may shrink it.
    pub tick: Duration,
}

impl std::fmt::Debug for DispatcherConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatcherConfig")
            .field("db_path", &self.db_path)
            .field("data_dir", &self.data_dir)
            .field("max_concurrent_actions", &self.max_concurrent_actions)
            .field("executor", &self.executor)
            .field("tick", &self.tick)
            .finish_non_exhaustive()
    }
}

/// The dispatcher: cloneable handle around the shared lane state. Clones
/// refer to the same worker pool.
#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: DispatcherConfig,
    limiter: Arc<ActionLimiter>,
    executor: ActionExecutor,
    /// Hint queue (disposable `run_id`s; SQLite is the record). `None` once
    /// shutdown closes the channel so workers drain and exit.
    tx: Mutex<Option<SyncSender<Uuid>>>,
    rx: Mutex<Receiver<Uuid>>,
    lanes: Mutex<LaneBook>,
    wake: Arc<(Mutex<()>, Condvar)>,
    state: AtomicU8,
    /// True while this process holds the dispatcher OS lock (or runs
    /// lock-free without a data dir). A follower re-tries acquisition on its
    /// pump tick: if the owner dies, the normal `active` recovery rule
    /// continues the same claims here.
    owns_lock: AtomicBool,
    /// The dispatcher OS lock guard (held while `owns_lock` is true).
    lock: Mutex<Option<crate::reply::gate::GateGuard>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

/// In-memory lane reservations: which run ids are queued (hint sent, not yet
/// active) or active per timer, plus their cancellation tokens. Disposable —
/// never persisted (`pending`/`active` in SQLite is the durable truth).
#[derive(Default)]
struct LaneBook {
    per_timer: HashMap<TimerId, Lane>,
}

#[derive(Default)]
struct Lane {
    /// Hints sent, oldest first, not yet reported `pending → active`.
    queued: VecDeque<Uuid>,
    /// Claims a worker activated and is executing.
    active: HashSet<Uuid>,
    /// Cancellation tokens by run id (queued or active).
    tokens: HashMap<Uuid, Arc<CancellationToken>>,
}

impl Lane {
    fn in_lane(&self) -> usize {
        self.queued.len() + self.active.len()
    }
}

fn open_store(db_path: &Path) -> StoreResult<Store> {
    Store::open_with(
        db_path,
        crate::store::OpenOptions {
            refuse_network_fs: true,
            ..crate::store::OpenOptions::default()
        },
    )
}

/// Try to take the dispatcher OS lock (no-op when already held or in
/// lock-free in-memory mode). Returns true while this process is the owner.
fn try_take_lock(inner: &Arc<Inner>) -> std::io::Result<bool> {
    if inner.owns_lock.load(Ordering::SeqCst) {
        return Ok(true);
    }
    let Some(dir) = &inner.cfg.data_dir else {
        inner.owns_lock.store(true, Ordering::SeqCst);
        return Ok(true);
    };
    match crate::reply::gate::try_acquire_file(&dir.join("locks").join(DISPATCHER_LOCK_NAME))? {
        Some(guard) => {
            *inner.lock.lock().unwrap() = Some(guard);
            inner.owns_lock.store(true, Ordering::SeqCst);
            eprintln!("bellman: dispatcher lock acquired");
            Ok(true)
        }
        None => Ok(false),
    }
}

impl Dispatcher {
    /// Spawn the worker pool and the pump thread. Tries to take the
    /// dispatcher OS lock; without it this process is a follower — producers
    /// still commit claims durably, but only the lock holder pumps.
    pub fn spawn(cfg: DispatcherConfig) -> std::io::Result<Self> {
        let workers = cfg.max_concurrent_actions.max(1);
        let queue_cap = (2 * workers).max(1);
        let limiter = Arc::new(ActionLimiter::new(workers));
        let executor = ActionExecutor::new(
            cfg.executor.clone(),
            cfg.notify_sink.clone(),
            Arc::clone(&limiter),
        );
        let owns_lock = cfg.data_dir.is_none();
        let (tx, rx) = sync_channel::<Uuid>(queue_cap);
        let inner = Arc::new(Inner {
            limiter,
            executor,
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(rx),
            lanes: Mutex::new(LaneBook::default()),
            wake: Arc::new((Mutex::new(()), Condvar::new())),
            state: AtomicU8::new(PRIMED),
            owns_lock: AtomicBool::new(owns_lock),
            lock: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
            cfg,
        });
        if inner.cfg.data_dir.is_some() {
            try_take_lock(&inner)?;
        }

        let mut threads = Vec::new();
        for i in 0..workers {
            let inner = Arc::clone(&inner);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("bellman-action-{i}"))
                    .spawn(move || worker_loop(&inner))
                    .expect("action worker spawn"),
            );
        }
        {
            let inner = Arc::clone(&inner);
            threads.push(
                std::thread::Builder::new()
                    .name("bellman-dispatch-pump".into())
                    .spawn(move || pump_loop(&inner))
                    .expect("dispatch pump spawn"),
            );
        }
        *inner.threads.lock().unwrap() = threads;
        Ok(Self { inner })
    }

    /// Whether this process holds the dispatcher OS lock (or runs lock-free
    /// without a data dir). Only the holder may pump scheduled claims.
    pub fn owns_lock(&self) -> bool {
        self.inner.owns_lock.load(Ordering::SeqCst)
    }

    /// Number of worker lanes.
    pub fn workers(&self) -> usize {
        self.inner.cfg.max_concurrent_actions.max(1)
    }

    /// Shared global permit/statistics gate.
    pub fn limiter(&self) -> &ActionLimiter {
        &self.inner.limiter
    }

    /// Startup recovery, called only AFTER R10 completed (reply scan/ingest,
    /// outbox recovery, folder reconciliation). The lock holder returns the
    /// previous owner's `active` claims to `pending` — acquiring the lock
    /// proves that dispatcher is gone — then the pump starts.
    ///
    /// A follower (another process holds the dispatcher OS lock) stays an
    /// idle PRIMED follower: only the lock holder may pump scheduled claims.
    /// Its producers still commit claims durably — the owner's periodic pump
    /// (≤ 1 s) picks them up.
    pub fn begin_startup(&self) {
        match try_take_lock(&self.inner) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "bellman: dispatcher lock held by another process; follower stays idle"
                );
                return;
            }
            Err(e) => {
                eprintln!("bellman: dispatcher lock acquire: {e}");
                return;
            }
        }
        if self.inner.cfg.data_dir.is_some() {
            match open_store(&self.inner.cfg.db_path) {
                Ok(mut store) => match store.repend_all_active() {
                    Ok(n) if n > 0 => {
                        eprintln!("bellman: dispatcher returned {n} active claim(s) to pending");
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("bellman: dispatcher active-claim recovery: {e}"),
                },
                Err(e) => eprintln!("bellman: dispatcher recovery store open: {e}"),
            }
        }
        self.inner.state.store(STARTED, Ordering::SeqCst);
        self.wake_pump();
    }

    /// Fire-producer nudge: a new claim is durable. The pump sends it as
    /// soon as the lane allows; the periodic tick is the backstop.
    pub fn submit(&self, _run_id: Uuid) {
        self.wake_pump();
    }

    fn wake_pump(&self) {
        let (lock, cv) = &*self.inner.wake;
        let _g = lock.lock().unwrap();
        cv.notify_one();
    }

    /// Wait until one claim reaches `finished` (polls the durable row — the
    /// executor may live in another process), up to `timeout`. Returns the
    /// finished claim, or `None` on timeout (the claim keeps its state).
    pub fn wait_finished(
        &self,
        store: &Store,
        run_id: Uuid,
        timeout: Duration,
    ) -> StoreResult<Option<crate::store::RunClaim>> {
        let start = std::time::Instant::now();
        loop {
            if let Some(claim) = store.get_run(run_id)? {
                if claim.status == ClaimStatus::Finished {
                    return Ok(Some(claim));
                }
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Shutdown drains: stop accepting new jobs, let in-flight lanes finish
    /// (bounded by their action timeout + retries), then drain pending
    /// outbox rows through the R11 publisher.
    pub fn shutdown_drain(&self) {
        if self.inner.state.swap(STOPPING, Ordering::SeqCst) == STOPPING {
            return;
        }
        // Close the channel: workers finish the current job and exit.
        self.inner.tx.lock().unwrap().take();
        self.wake_pump();
        let threads = std::mem::take(&mut *self.inner.threads.lock().unwrap());
        for t in threads {
            let _ = t.join();
        }
        // Drain pending outbox rows so the exit does not truncate the log.
        if let Some(data_dir) = &self.inner.cfg.data_dir {
            if let Ok(store) = open_store(&self.inner.cfg.db_path) {
                crate::events::EventPublisher::drain_best_effort(data_dir, &store);
            }
        }
        // Release the OS lock: this dispatcher is done, a waiting follower
        // takes over immediately ("run the bounded dispatcher until the
        // claim finishes, then release it").
        self.inner.lock.lock().unwrap().take();
        self.inner.owns_lock.store(false, Ordering::SeqCst);
    }
}

impl FireAction for Dispatcher {
    /// The fire-producer hook: nudge the pump. Never runs the action.
    fn on_fire(&mut self, ctx: &FireContext<'_>) -> Result<(), String> {
        self.submit(ctx.run_id);
        Ok(())
    }

    fn executes_inline(&self) -> bool {
        false
    }

    fn boot_complete(&mut self) {
        self.begin_startup();
    }

    fn shutdown(&mut self) {
        self.shutdown_drain();
    }
}

// ── Pump ────────────────────────────────────────────────────────────────

fn pump_loop(inner: &Arc<Inner>) {
    let (lock, cv) = &*inner.wake;
    let mut guard = lock.lock().unwrap();
    loop {
        if inner.state.load(Ordering::SeqCst) == STOPPING {
            return;
        }
        // Follower whose owner died: re-try the OS lock on the tick; on
        // acquisition the normal `active` recovery rule continues the same
        // claims here (a waiting CLI never strands its run_id).
        if inner.cfg.data_dir.is_some()
            && !inner.owns_lock.load(Ordering::SeqCst)
            && try_take_lock(inner).unwrap_or(false)
        {
            let state = inner.state.load(Ordering::SeqCst);
            if state != STOPPING {
                // Run the same startup recovery the boot path would have.
                if let Ok(mut store) = open_store(&inner.cfg.db_path) {
                    let _ = store.repend_all_active();
                }
                inner.state.store(STARTED, Ordering::SeqCst);
            }
        }
        if inner.state.load(Ordering::SeqCst) == STARTED {
            drop(guard);
            pump_once(inner);
            guard = lock.lock().unwrap();
        }
        let tick = inner.cfg.tick.max(Duration::from_millis(10));
        let (g, _timeout) = cv.wait_timeout(guard, tick).unwrap();
        guard = g;
    }
}

/// One pump pass: signal cancellation tokens, dispatch eligible pending
/// claims in lane order, then run the publication pump.
fn pump_once(inner: &Arc<Inner>) {
    // Only the process holding the dispatcher OS lock may pump scheduled
    // claims (pure in-memory test dispatchers run lock-free). A follower
    // must never execute beside the owner — SQLite is the handoff, and two
    // pumps would break per-timer lane serialization.
    if !inner.owns_lock.load(Ordering::SeqCst) && inner.cfg.data_dir.is_some() {
        return;
    }
    let mut store = match open_store(&inner.cfg.db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bellman: dispatch pump store open: {e}");
            return;
        }
    };

    // 1. A `Replace` fire transaction (possibly in another process) marked
    //    active claims `cancel_requested` — signal their tokens.
    match store.cancel_requested_active() {
        Ok(ids) if !ids.is_empty() => {
            let lanes = inner.lanes.lock().unwrap();
            for lane in lanes.per_timer.values() {
                for id in &ids {
                    if let Some(token) = lane.tokens.get(id) {
                        token.cancel();
                    }
                }
            }
        }
        Ok(_) => {}
        Err(e) => eprintln!("bellman: dispatch pump cancel scan: {e}"),
    }

    // 2. Dispatch scan. SQLite, not the queue, is the source of truth.
    let pending = match store.pending_claims() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bellman: dispatch pump pending scan: {e}");
            return;
        }
    };
    // Per timer, oldest event_sequence first.
    let mut per_timer: HashMap<TimerId, Vec<crate::store::RunClaim>> = HashMap::new();
    for claim in pending {
        per_timer.entry(claim.timer_id).or_default().push(claim);
    }
    for claims in per_timer.values_mut() {
        claims.sort_by_key(|c| c.event_sequence);
    }

    {
        let mut lanes = inner.lanes.lock().unwrap();
        'timers: for (timer_id, claims) in per_timer {
            for claim in claims {
                // Skip claims this process already reserved.
                let reserved = lanes
                    .per_timer
                    .get(&timer_id)
                    .is_some_and(|l| l.queued.contains(&claim.run_id) || l.active.contains(&claim.run_id));
                if reserved {
                    continue;
                }
                let Some(timer) = (match store.get_timer(timer_id) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("bellman: dispatch pump timer load: {e}");
                        continue 'timers;
                    }
                }) else {
                    // Timer gone — close the orphan claim so it does not loop
                    // (never recorded as success).
                    let _ = store.finish_run(
                        claim.run_id,
                        RunOutcome::WakeFailed,
                        Some("timer_deleted"),
                    );
                    continue;
                };
                let limit = match &timer.overlap {
                    // Serial policies: "never concurrently, in order".
                    OverlapPolicy::Skip | OverlapPolicy::QueueOne | OverlapPolicy::Replace => 1,
                    // Parallel admits up to cap of THIS timer, still inside
                    // the global max_concurrent_actions.
                    OverlapPolicy::Parallel { cap } => *cap as usize,
                };
                if limit == 0 {
                    continue;
                }
                let lane = lanes.per_timer.entry(timer_id).or_default();
                if lane.in_lane() >= limit {
                    // Lane busy: only the oldest eligible may go next.
                    continue 'timers;
                }
                let run_id = claim.run_id;
                let send = {
                    let tx = inner.tx.lock().unwrap();
                    tx.as_ref().map(|tx| tx.try_send(run_id))
                };
                match send {
                    Some(Ok(())) => {
                        // Successful send keeps the reservation until the
                        // worker reports its CAS result.
                        lane.queued.push_back(run_id);
                        lane.tokens.entry(run_id).or_default();
                    }
                    Some(Err(std::sync::mpsc::TrySendError::Full(_))) => {
                        // Backpressure: the claim stays `pending` — a full
                        // queue never drops a fire. The next completion/tick
                        // pumps again.
                        break 'timers;
                    }
                    Some(Err(std::sync::mpsc::TrySendError::Disconnected(_))) | None => return,
                }
            }
        }
    }

    // 3. The transport-publication pump (bounded per pass).
    if let Some(data_dir) = &inner.cfg.data_dir {
        crate::reply::publication::pump(data_dir, &store, 8);
    }
}

// ── Workers ─────────────────────────────────────────────────────────────

fn worker_loop(inner: &Arc<Inner>) {
    let mut store = match open_store(&inner.cfg.db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bellman: action worker store open: {e}");
            return;
        }
    };
    loop {
        let run_id = {
            let rx = inner.rx.lock().unwrap();
            rx.recv()
        };
        let Ok(run_id) = run_id else {
            return; // channel closed: shutdown.
        };
        run_job(inner, &mut store, run_id);
    }
}

/// Execute one hinted claim: CAS `pending → active`, run the configured
/// action, commit the durable result + R11 outbox event in one transaction.
fn run_job(inner: &Arc<Inner>, store: &mut Store, run_id: Uuid) {
    let claim = match store.get_run(run_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            release_queued(inner, run_id);
            return;
        }
        Err(e) => {
            eprintln!("bellman: worker claim load {run_id}: {e}");
            release_queued(inner, run_id);
            return;
        }
    };
    let timer_id = claim.timer_id;
    // Stale or duplicate hints lose the transition and do nothing.
    if claim.status != ClaimStatus::Pending {
        release_queued(inner, run_id);
        return;
    }
    match store.activate_run(run_id) {
        Ok(true) => {}
        Ok(false) => {
            release_queued(inner, run_id);
            return;
        }
        Err(e) => {
            eprintln!("bellman: worker activate {run_id}: {e}");
            release_queued(inner, run_id);
            return;
        }
    }
    // The reservation moves queued → active until completion.
    let token = {
        let mut lanes = inner.lanes.lock().unwrap();
        let lane = lanes.per_timer.entry(timer_id).or_default();
        lane.queued.retain(|id| *id != run_id);
        lane.active.insert(run_id);
        lane.tokens
            .entry(run_id)
            .or_default()
            .clone()
    };

    let timer = match store.get_timer(timer_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            finish_claim(
                store,
                &claim,
                None,
                RunOutcome::WakeFailed,
                "timer_deleted",
                EventRecord::new(RunState::WakeFailed)
                    .with_timer(timer_id, timer_id.to_string())
                    .with_run(run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_message("timer_deleted"),
            );
            finish_lane(inner, timer_id, run_id);
            return;
        }
        Err(e) => {
            eprintln!("bellman: worker timer load {timer_id}: {e}");
            let _ = store.repend_run(run_id);
            finish_lane(inner, timer_id, run_id);
            return;
        }
    };

    // The worker supervisor: a panic returns the unfinished claim to
    // `pending` (at-least-once) and wakes the pump. The worker thread keeps
    // serving the pool — a panicking action must not shrink the lanes.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        inner.executor.execute(&timer, run_id, &token)
    }));
    let outcome = match outcome {
        Ok(o) => o,
        Err(_) => {
            eprintln!("bellman: action worker panicked on {run_id}; claim re-queued");
            let _ = store.repend_run(run_id);
            finish_lane(inner, timer_id, run_id);
            return;
        }
    };

    match outcome {
        ExecOutcome::Delivered { message, attempts } => {
            finish_claim(
                store,
                &claim,
                Some(&timer),
                RunOutcome::WakeDelivered,
                &message,
                EventRecord::new(RunState::WakeDelivered)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_message(message.clone())
                    .with_count(attempts),
            );
        }
        ExecOutcome::Failed { error, attempts } => {
            finish_claim(
                store,
                &claim,
                Some(&timer),
                RunOutcome::WakeFailed,
                &error,
                EventRecord::new(RunState::WakeFailed)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_error(error.clone())
                    .with_message("FAILED")
                    .with_count(attempts)
                    .with_detail(serde_json::json!({ "status": "FAILED" })),
            );
        }
        ExecOutcome::Cancelled => {
            // `Replace` actually interrupted this action.
            finish_claim(
                store,
                &claim,
                Some(&timer),
                RunOutcome::WakeFailed,
                "overlap_replace",
                EventRecord::new(RunState::WakeFailed)
                    .with_timer(timer.id, timer.name.clone())
                    .with_run(run_id)
                    .with_scheduled_for(claim.scheduled_for)
                    .with_message("overlap_replace"),
            );
        }
    }
    finish_lane(inner, timer_id, run_id);
}

/// The worker completion transaction: claim result + R11 outbox event commit
/// together. Workers never write run files or the JSONL log; the elected
/// publisher drains the outbox row. If a `Replace` fire transaction already
/// finished the claim, its outcome won — this worker emits nothing.
fn finish_claim(
    store: &mut Store,
    claim: &crate::store::RunClaim,
    _timer: Option<&crate::store::Timer>,
    outcome: RunOutcome,
    reason: &str,
    event: EventRecord,
) {
    let result = (|| -> StoreResult<()> {
        let tx = store.transaction()?;
        let transitioned = crate::store::finish_run_conn(&tx, claim.run_id, outcome, reason)?;
        if transitioned {
            Store::enqueue_event_in_tx(&tx, &event)?;
        }
        tx.commit()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("bellman: worker result commit for {}: {e}", claim.run_id);
    }
}

/// Release a reservation whose hint turned out stale (claim already active
/// elsewhere or finished) and wake the pump.
fn release_queued(inner: &Arc<Inner>, run_id: Uuid) {
    {
        let mut lanes = inner.lanes.lock().unwrap();
        for lane in lanes.per_timer.values_mut() {
            if lane.queued.contains(&run_id) {
                lane.queued.retain(|id| *id != run_id);
                lane.tokens.remove(&run_id);
                break;
            }
        }
    }
    wake_from_worker(inner);
}

/// Worker completion/CAS-failure: release the lane and wake the pump.
fn finish_lane(inner: &Arc<Inner>, timer_id: TimerId, run_id: Uuid) {
    {
        let mut lanes = inner.lanes.lock().unwrap();
        if let Some(lane) = lanes.per_timer.get_mut(&timer_id) {
            lane.queued.retain(|id| *id != run_id);
            lane.active.remove(&run_id);
            lane.tokens.remove(&run_id);
            if lane.in_lane() == 0 {
                lanes.per_timer.remove(&timer_id);
            }
        }
    }
    wake_from_worker(inner);
}

fn wake_from_worker(inner: &Arc<Inner>) {
    let (lock, cv) = &*inner.wake;
    let _g = lock.lock().unwrap();
    cv.notify_one();
}
