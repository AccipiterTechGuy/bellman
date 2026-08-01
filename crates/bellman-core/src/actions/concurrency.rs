//! Global action backpressure: `max_concurrent_actions` + overflow queue.
//!
//! A resume mass-fire (hundreds of overdue timers) must not fork-bomb the
//! box. Every wake action acquires a permit from [`ActionLimiter`]; when the
//! cap is reached further starts wait on the overflow queue until a permit
//! is released. Peak concurrency is recorded for tests.

use std::sync::{Condvar, Mutex};
use std::time::Instant;

/// Default global concurrent action cap (product / config default).
pub const DEFAULT_MAX_CONCURRENT_ACTIONS: usize = 16;

/// Snapshot of limiter counters (tests / diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LimiterStats {
    /// Maximum number of actions observed in flight at once.
    pub peak_in_flight: usize,
    /// Total actions that completed (acquired + released).
    pub completed: usize,
    /// Total times a caller had to wait because the cap was reached.
    pub queue_waits: usize,
}

#[derive(Debug)]
struct LimiterInner {
    in_flight: usize,
    peak_in_flight: usize,
    completed: usize,
    queue_waits: usize,
    /// Callers currently blocked waiting for a permit.
    waiters: usize,
}

/// Fair-ish semaphore over concurrent wake actions.
///
/// `run(f)` acquires a permit (blocking when at capacity), runs `f`, then
/// releases. Thread-safe; intended to wrap process launches and any other
/// side-effecting wake work that must not stampede.
#[derive(Debug)]
pub struct ActionLimiter {
    max: usize,
    inner: Mutex<LimiterInner>,
    cv: Condvar,
}

impl ActionLimiter {
    /// Build a limiter with the given concurrency cap (minimum 1).
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            max: max_concurrent.max(1),
            inner: Mutex::new(LimiterInner {
                in_flight: 0,
                peak_in_flight: 0,
                completed: 0,
                queue_waits: 0,
                waiters: 0,
            }),
            cv: Condvar::new(),
        }
    }

    pub fn max_concurrent(&self) -> usize {
        self.max
    }

    /// Run `f` under a concurrency permit. Blocks when at capacity.
    pub fn run<R>(&self, f: impl FnOnce() -> R) -> R {
        self.acquire();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        self.release();
        match result {
            Ok(r) => r,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// Non-blocking attempt: returns `None` if at capacity (caller may queue
    /// the work externally). Prefer [`Self::run`] for normal use.
    pub fn try_run<R>(&self, f: impl FnOnce() -> R) -> Option<R> {
        if !self.try_acquire() {
            return None;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        self.release();
        match result {
            Ok(r) => Some(r),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn acquire(&self) {
        let mut g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.in_flight >= self.max {
            g.queue_waits = g.queue_waits.saturating_add(1);
            g.waiters = g.waiters.saturating_add(1);
            while g.in_flight >= self.max {
                g = self
                    .cv
                    .wait(g)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            g.waiters = g.waiters.saturating_sub(1);
        }
        g.in_flight = g.in_flight.saturating_add(1);
        if g.in_flight > g.peak_in_flight {
            g.peak_in_flight = g.in_flight;
        }
    }

    fn try_acquire(&self) -> bool {
        let mut g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.in_flight >= self.max {
            return false;
        }
        g.in_flight = g.in_flight.saturating_add(1);
        if g.in_flight > g.peak_in_flight {
            g.peak_in_flight = g.in_flight;
        }
        true
    }

    fn release(&self) {
        let mut g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.in_flight = g.in_flight.saturating_sub(1);
        g.completed = g.completed.saturating_add(1);
        drop(g);
        self.cv.notify_one();
    }

    /// Current stats snapshot.
    pub fn stats(&self) -> LimiterStats {
        let g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        LimiterStats {
            peak_in_flight: g.peak_in_flight,
            completed: g.completed,
            queue_waits: g.queue_waits,
        }
    }

    /// Reset counters (tests).
    pub fn reset_stats(&self) {
        let mut g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.peak_in_flight = g.in_flight;
        g.completed = 0;
        g.queue_waits = 0;
    }
}

/// Spawn `n` threads each running `work` under a limiter of `max_concurrent`.
///
/// Returns the limiter stats after every thread has completed. Used by the
/// 500-timer resume acceptance test.
pub fn run_parallel_under_cap(
    max_concurrent: usize,
    n: usize,
    work: impl Fn(usize) + Send + Sync + 'static,
) -> LimiterStats {
    let limiter = std::sync::Arc::new(ActionLimiter::new(max_concurrent));
    let work = std::sync::Arc::new(work);
    let start = Instant::now();
    let mut handles = Vec::with_capacity(n);
    for i in 0..n {
        let lim = std::sync::Arc::clone(&limiter);
        let w = work.clone();
        handles.push(std::thread::spawn(move || {
            lim.run(|| w(i));
        }));
    }
    for h in handles {
        h.join().expect("worker panic");
    }
    let _ = start;
    limiter.stats()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn peak_never_exceeds_cap_under_500_parallel() {
        let cap = 16;
        let concurrent = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let stats = run_parallel_under_cap(cap, 500, {
            let concurrent = concurrent.clone();
            let peak = peak.clone();
            move |_| {
                let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(2));
                concurrent.fetch_sub(1, Ordering::SeqCst);
            }
        });

        let observed_peak = peak.load(Ordering::SeqCst);
        assert!(
            observed_peak <= cap,
            "peak concurrency {observed_peak} exceeded cap {cap}"
        );
        assert_eq!(stats.completed, 500);
        assert!(stats.peak_in_flight <= cap);
        assert!(
            stats.queue_waits > 0,
            "expected some queueing with 500 tasks and cap {cap}"
        );
    }

    #[test]
    fn try_run_returns_none_when_full() {
        let limiter = ActionLimiter::new(1);
        limiter.acquire();
        assert!(limiter.try_run(|| 1).is_none());
        limiter.release();
        assert_eq!(limiter.try_run(|| 7), Some(7));
    }
}
