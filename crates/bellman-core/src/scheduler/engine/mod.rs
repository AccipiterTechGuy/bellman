//! Heap-loop scheduler engine.
//!
//! - `BinaryHeap<Reverse<(fire_at_utc, TimerId)>>` over the near-horizon window
//! - Chunked sleeps `min(next_fire − now, max_sleep)` re-reading the wall clock
//! - Wall-vs-monotonic clock-jump detector → misfire pass + horizon rebuild
//! - Per-timer misfire policy (skip / coalesce / catch_up)
//! - Claim-before-work via the store run ledger
//! - Channel-driven refill on insert/edit
//!
//! This module owns the [`Scheduler`] struct, its constructor and its
//! accessors. The behaviour is split across sibling files by responsibility:
//!
//! - [`types`] — control messages, tick/fire results, errors, heap entry
//! - [`drive`] — the loop: boot / tick / sleep / run / control messages
//! - [`misfire`] — overdue scan, due drain, and per-policy grace windows
//! - [`delivery`] — the claim → act → record path
//! - [`horizon`] — heap and horizon maintenance

mod delivery;
mod drive;
mod horizon;
mod misfire;
mod types;

pub use types::{
    ControlHandle, ControlMsg, DeliveredFire, SchedulerError, SchedulerResult, TickResult,
};

use self::types::HeapEntry;
use crate::scheduler::action::FireAction;
use crate::scheduler::clock::{Clock, MonoTime};
use crate::scheduler::config::SchedulerConfig;
use crate::store::{Store, TimerId};
use chrono::{DateTime, Utc};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::mpsc::{self, Receiver, Sender};

/// Near-horizon timer engine.
pub struct Scheduler<C: Clock, A: FireAction> {
    store: Store,
    clock: C,
    action: A,
    config: SchedulerConfig,
    heap: BinaryHeap<Reverse<HeapEntry>>,
    last_wall: DateTime<Utc>,
    last_mono: MonoTime,
    control_tx: Sender<ControlMsg>,
    control_rx: Receiver<ControlMsg>,
    booted: bool,
    /// Global pause-all (distinct from per-timer `enabled`): when true the loop
    /// keeps the heap warm but does not deliver fires. Toggle via
    /// [`ControlHandle::set_pause_all`] or the env override at construction.
    pause_all: bool,
    /// SCH2: store `PRAGMA data_version` snapshot taken at the last horizon
    /// rebuild. A different value means another connection committed — the
    /// heap may be stale even though no control message arrived.
    last_data_version: i64,
    /// SCH2: monotonic time of the last horizon rebuild (any cause); drives
    /// the unconditional `external_rebuild_interval` floor.
    last_rebuild_mono: MonoTime,
}

impl<C: Clock, A: FireAction> Scheduler<C, A> {
    /// Build a scheduler. Call [`Self::boot`] before ticking.
    pub fn new(store: Store, clock: C, action: A, config: SchedulerConfig) -> Self {
        Self::new_with_pause(store, clock, action, config, false)
    }

    /// Build a scheduler that starts with the global pause-all flag set.
    pub fn new_paused(store: Store, clock: C, action: A, config: SchedulerConfig) -> Self {
        Self::new_with_pause(store, clock, action, config, true)
    }

    fn new_with_pause(
        store: Store,
        clock: C,
        action: A,
        config: SchedulerConfig,
        pause_all: bool,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::channel();
        let wall = clock.wall_now();
        let mono = clock.mono_now();
        Self {
            store,
            clock,
            action,
            config,
            heap: BinaryHeap::new(),
            last_wall: wall,
            last_mono: mono,
            control_tx,
            control_rx,
            booted: false,
            pause_all,
            last_data_version: 0,
            last_rebuild_mono: mono,
        }
    }

    /// Handle used to signal refill / shutdown from outside the loop.
    pub fn control_handle(&self) -> ControlHandle {
        ControlHandle {
            tx: self.control_tx.clone(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    pub fn action(&self) -> &A {
        &self.action
    }

    pub fn action_mut(&mut self) -> &mut A {
        &mut self.action
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    pub fn heap_len(&self) -> usize {
        self.heap.len()
    }

    /// Peek the next wake on the heap (if any): a timer fire or a lifecycle
    /// deadline. Deadline entries report the nil id — only `fire_at` drives
    /// sleep sizing.
    pub fn peek_next(&self) -> Option<(DateTime<Utc>, TimerId)> {
        self.heap.peek().map(|Reverse(e)| {
            let id = match e.kind {
                types::HeapKind::Fire { timer_id } => timer_id,
                types::HeapKind::Deadline { .. } => TimerId::nil(),
            };
            (e.fire_at, id)
        })
    }

    /// Consume the scheduler, returning the action sink (tests read recorded fires).
    pub fn into_action(self) -> A {
        self.action
    }

    /// Current value of the global pause-all flag (read-only).
    pub fn pause_all(&self) -> bool {
        self.pause_all
    }

    /// Set the global pause-all flag in place (does not require a control
    /// message; the next `tick` observes it).
    pub fn set_pause_all_now(&mut self, paused: bool) {
        self.pause_all = paused;
    }
}
