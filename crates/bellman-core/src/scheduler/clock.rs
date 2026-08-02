//! Mockable wall + monotonic clock pair.
//!
//! The engine compares wall-clock delta vs monotonic delta each tick to detect
//! suspend / NTP jumps. Tests use [`SimulatedClock`] so no real sleeping is
//! required.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Monotonic reading as duration since an arbitrary per-clock epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonoTime(pub Duration);

impl MonoTime {
    /// The reading as a `Duration` since this clock's origin.
    pub fn as_duration(self) -> Duration {
        self.0
    }

    /// Elapsed time since `other`, clamped at zero. A monotonic clock does
    /// not go backwards, so a negative result would be a bug, not a fact.
    pub fn saturating_sub(self, other: MonoTime) -> Duration {
        self.0.saturating_sub(other.0)
    }
}

/// Wall + monotonic clocks used by the scheduler loop.
pub trait Clock: Send {
    /// Current wall-clock time (UTC).
    fn wall_now(&self) -> DateTime<Utc>;

    /// Current monotonic reading.
    fn mono_now(&self) -> MonoTime;

    /// Block (or advance simulated time) by `d`. Wall and mono both move by `d`.
    fn sleep(&self, d: Duration);

    /// Whether wall time advances during an OS blocking wait (e.g. `recv_timeout`).
    ///
    /// - [`SystemClock`]: `true` — real wall clock moves while the thread blocks.
    /// - [`SimulatedClock`]: `false` — must call [`Clock::sleep`] to advance.
    fn uses_os_time(&self) -> bool {
        true
    }
}

/// Production clock: `Utc::now` + `Instant`.
#[derive(Debug, Clone)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// A clock reading the real wall and monotonic clocks.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall_now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn mono_now(&self) -> MonoTime {
        MonoTime(self.origin.elapsed())
    }

    fn sleep(&self, d: Duration) {
        if !d.is_zero() {
            std::thread::sleep(d);
        }
    }
}

#[derive(Debug)]
struct SimState {
    wall: DateTime<Utc>,
    mono: Duration,
}

/// In-memory clock for tests. Safe to share via [`Arc`].
#[derive(Debug)]
pub struct SimulatedClock {
    state: Mutex<SimState>,
}

impl SimulatedClock {
    /// Start at the given wall time with mono = 0.
    pub fn new(wall: DateTime<Utc>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SimState {
                wall,
                mono: Duration::ZERO,
            }),
        })
    }

    /// Advance wall and mono together (normal time passage / sleep).
    pub fn advance(&self, d: Duration) {
        let mut g = self.state.lock().expect("sim clock lock");
        g.wall = add_wall(g.wall, d);
        g.mono = g.mono.saturating_add(d);
    }

    /// Advance wall only (mono frozen) — models suspend / lid-close oversleep.
    pub fn advance_wall_only(&self, d: Duration) {
        let mut g = self.state.lock().expect("sim clock lock");
        g.wall = add_wall(g.wall, d);
    }

    /// Jump wall backward (NTP / manual set). Mono is unchanged.
    pub fn jump_wall_backward(&self, d: Duration) {
        let mut g = self.state.lock().expect("sim clock lock");
        g.wall = sub_wall(g.wall, d);
    }

    /// Absolute wall set (mono unchanged). Useful for precise test fixtures.
    pub fn set_wall(&self, wall: DateTime<Utc>) {
        let mut g = self.state.lock().expect("sim clock lock");
        g.wall = wall;
    }

    /// The simulated wall clock.
    pub fn wall(&self) -> DateTime<Utc> {
        self.state.lock().expect("sim clock lock").wall
    }

    /// The simulated monotonic clock. Advancing wall without mono is how a
    /// clock jump is staged in tests; advancing mono without wall is a
    /// suspend.
    pub fn mono(&self) -> Duration {
        self.state.lock().expect("sim clock lock").mono
    }
}

impl Clock for SimulatedClock {
    fn wall_now(&self) -> DateTime<Utc> {
        self.state.lock().expect("sim clock lock").wall
    }

    fn mono_now(&self) -> MonoTime {
        MonoTime(self.state.lock().expect("sim clock lock").mono)
    }

    fn sleep(&self, d: Duration) {
        self.advance(d);
    }

    fn uses_os_time(&self) -> bool {
        false
    }
}

impl Clock for Arc<SimulatedClock> {
    fn wall_now(&self) -> DateTime<Utc> {
        (**self).wall_now()
    }

    fn mono_now(&self) -> MonoTime {
        (**self).mono_now()
    }

    fn sleep(&self, d: Duration) {
        (**self).sleep(d);
    }

    fn uses_os_time(&self) -> bool {
        false
    }
}

fn add_wall(wall: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    wall + chrono_from_std(d)
}

fn sub_wall(wall: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    wall - chrono_from_std(d)
}

fn chrono_from_std(d: Duration) -> ChronoDuration {
    ChronoDuration::from_std(d).unwrap_or_else(|_| ChronoDuration::seconds(i64::MAX / 4))
}
