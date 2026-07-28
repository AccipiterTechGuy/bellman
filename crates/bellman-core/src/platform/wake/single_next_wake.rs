//! Single-next-wake bridge (synthesis §4).
//!
//! Election: `next = min(next_due(t) for t where t.wake_machine && t.enabled)`.
//! One `rearm()` (cancel-then-program, idempotent) from store mutations,
//! pre-suspend, resume, and start. Arms at `wake_utc − 45 s`; wake never fires actions.

use super::{arm_instant, MachineWake, PowerEvent, WakeCapability, WakeError};
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex};

/// Candidate timer for the single-next-wake election.
#[derive(Debug, Clone)]
pub struct WakeCandidate {
    pub enabled: bool,
    pub wake_machine: bool,
    pub next_fire_utc: Option<DateTime<Utc>>,
}

/// Elect the earliest due time among wake-enabled timers.
pub fn elect_next_wake<'a, I>(timers: I) -> Option<DateTime<Utc>>
where
    I: IntoIterator<Item = &'a WakeCandidate>,
{
    timers
        .into_iter()
        .filter(|t| t.enabled && t.wake_machine)
        .filter_map(|t| t.next_fire_utc)
        .min()
}

/// Process-wide bridge: owns the platform `MachineWake` and last-armed target.
#[derive(Clone)]
pub struct SingleNextWake {
    inner: Arc<Mutex<BridgeState>>,
}

struct BridgeState {
    wake: Box<dyn MachineWake>,
    /// Master toggle from config.json (`wake.enabled`).
    master_enabled: bool,
    last_target: Option<DateTime<Utc>>,
    last_capability: WakeCapability,
    /// Last status line we emitted (for transition detection).
    last_status_line: String,
}

impl SingleNextWake {
    pub fn new(wake: Box<dyn MachineWake>, master_enabled: bool) -> Self {
        let cap = wake.capability();
        let line = cap.status_line();
        Self {
            inner: Arc::new(Mutex::new(BridgeState {
                wake,
                master_enabled,
                last_target: None,
                last_capability: cap,
                last_status_line: line,
            })),
        }
    }

    pub fn with_platform_default(master_enabled: bool) -> Self {
        Self::new(super::create_wake(), master_enabled)
    }

    pub fn set_master_enabled(&self, enabled: bool) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).master_enabled = enabled;
    }

    pub fn master_enabled(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).master_enabled
    }

    /// Effective capability: platform probe gated by master toggle.
    pub fn capability(&self) -> WakeCapability {
        let st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        effective_cap(&st.wake.capability(), st.master_enabled)
    }

    pub fn re_probe(&self) -> WakeCapability {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cap = st.wake.re_probe();
        st.last_capability = cap.clone();
        effective_cap(&cap, st.master_enabled)
    }

    /// Status line for Settings / JSONL (respects master toggle).
    pub fn status_line(&self) -> String {
        self.capability().status_line()
    }

    /// True when the status line changed since last call (startup or transition).
    pub fn take_status_transition(&self) -> Option<(String, WakeCapability)> {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let eff = effective_cap(&st.wake.capability(), st.master_enabled);
        let line = eff.status_line();
        if line != st.last_status_line {
            st.last_status_line = line.clone();
            st.last_capability = eff.clone();
            Some((line, eff))
        } else {
            None
        }
    }

    /// Force-record current status as the baseline (call after emitting startup event).
    pub fn mark_status_emitted(&self) {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let eff = effective_cap(&st.wake.capability(), st.master_enabled);
        st.last_status_line = eff.status_line();
        st.last_capability = eff;
    }

    /// Cancel-then-program the single next wake from the elected due time.
    pub fn rearm(&self, next_due: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Result<(), WakeError> {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let eff = effective_cap(&st.wake.capability(), st.master_enabled);
        if !eff.is_enabled() {
            let _ = st.wake.cancel_wake();
            st.last_target = None;
            return Ok(());
        }
        match next_due {
            None => {
                st.wake.cancel_wake()?;
                st.last_target = None;
                Ok(())
            }
            Some(due) => {
                let at = arm_instant(due, now);
                // Idempotent: skip re-program if same second already armed.
                if st.last_target == Some(at) {
                    return Ok(());
                }
                st.wake.cancel_wake()?;
                match st.wake.program_wake(at) {
                    Ok(()) => {
                        st.last_target = Some(at);
                        Ok(())
                    }
                    Err(e) => {
                        // Arm failure → re-probe once (synthesis §3).
                        let new_cap = st.wake.re_probe();
                        st.last_capability = new_cap;
                        st.last_target = None;
                        Err(e)
                    }
                }
            }
        }
    }

    /// Convenience: elect from candidates and rearm.
    pub fn rearm_from_candidates(
        &self,
        candidates: &[WakeCandidate],
        now: DateTime<Utc>,
    ) -> Result<(), WakeError> {
        let next = elect_next_wake(candidates);
        self.rearm(next, now)
    }

    pub fn cancel(&self) -> Result<(), WakeError> {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        st.last_target = None;
        st.wake.cancel_wake()
    }

    pub fn on_power_event(&self, ev: PowerEvent, candidates: &[WakeCandidate], now: DateTime<Utc>) {
        match ev {
            PowerEvent::Suspending => {
                let _ = self.rearm_from_candidates(candidates, now);
            }
            PowerEvent::Resumed => {
                {
                    let st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    st.wake.on_resumed();
                    let _ = st.wake.re_probe();
                }
                let _ = self.rearm_from_candidates(candidates, now);
            }
            PowerEvent::PowerSourceChanged => {
                let _ = self.re_probe();
                let _ = self.rearm_from_candidates(candidates, now);
            }
        }
    }

    pub fn last_armed(&self) -> Option<DateTime<Utc>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).last_target
    }
}

fn effective_cap(platform: &WakeCapability, master_enabled: bool) -> WakeCapability {
    if !master_enabled {
        return WakeCapability::Disabled {
            reason: super::DisabledReason::MasterDisabled,
        };
    }
    platform.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::wake::{DisabledReason, WakeMechanism};
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockWake {
        cap: Mutex<WakeCapability>,
        programs: AtomicUsize,
        cancels: AtomicUsize,
        last: Mutex<Option<DateTime<Utc>>>,
    }

    impl MockWake {
        fn enabled() -> Self {
            Self {
                cap: Mutex::new(WakeCapability::Enabled {
                    mechanism: WakeMechanism::LinuxAlarmTimerfd,
                    caveats: vec![],
                }),
                programs: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
                last: Mutex::new(None),
            }
        }
        fn disabled() -> Self {
            Self {
                cap: Mutex::new(WakeCapability::Disabled {
                    reason: DisabledReason::NoPermission {
                        hint: "test".into(),
                    },
                }),
                programs: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
                last: Mutex::new(None),
            }
        }
    }

    impl MachineWake for MockWake {
        fn capability(&self) -> WakeCapability {
            self.cap.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
        fn re_probe(&self) -> WakeCapability {
            self.capability()
        }
        fn program_wake(&self, at: DateTime<Utc>) -> Result<(), WakeError> {
            self.programs.fetch_add(1, Ordering::SeqCst);
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
            Ok(())
        }
        fn cancel_wake(&self) -> Result<(), WakeError> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Ok(())
        }
    }

    #[test]
    fn elect_min_of_wake_machine_enabled() {
        let now = Utc.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap();
        let cands = vec![
            WakeCandidate {
                enabled: true,
                wake_machine: true,
                next_fire_utc: Some(now + chrono::Duration::hours(2)),
            },
            WakeCandidate {
                enabled: true,
                wake_machine: true,
                next_fire_utc: Some(now + chrono::Duration::hours(1)),
            },
            WakeCandidate {
                enabled: false,
                wake_machine: true,
                next_fire_utc: Some(now + chrono::Duration::minutes(5)),
            },
            WakeCandidate {
                enabled: true,
                wake_machine: false,
                next_fire_utc: Some(now + chrono::Duration::minutes(1)),
            },
        ];
        assert_eq!(
            elect_next_wake(&cands),
            Some(now + chrono::Duration::hours(1))
        );
    }

    #[test]
    fn rearm_applies_slack() {
        let mock = MockWake::enabled();
        let bridge = SingleNextWake::new(Box::new(mock), true);
        let now = Utc.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap();
        let due = now + chrono::Duration::hours(1);
        bridge.rearm(Some(due), now).unwrap();
        assert_eq!(
            bridge.last_armed(),
            Some(due - chrono::Duration::seconds(45))
        );
    }

    #[test]
    fn rearm_skips_when_disabled() {
        let mock = MockWake::disabled();
        let programs_before = 0;
        let bridge = SingleNextWake::new(Box::new(mock), true);
        let now = Utc.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap();
        bridge
            .rearm(Some(now + chrono::Duration::hours(1)), now)
            .unwrap();
        assert!(bridge.last_armed().is_none());
        let _ = programs_before;
    }

    #[test]
    fn master_toggle_disables() {
        let mock = MockWake::enabled();
        let bridge = SingleNextWake::new(Box::new(mock), false);
        assert!(!bridge.capability().is_enabled());
        assert!(bridge.status_line().contains("turned off"));
    }

    #[test]
    fn empty_election_cancels() {
        let mock = MockWake::enabled();
        let bridge = SingleNextWake::new(Box::new(mock), true);
        let now = Utc.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap();
        bridge
            .rearm(Some(now + chrono::Duration::hours(1)), now)
            .unwrap();
        assert!(bridge.last_armed().is_some());
        bridge.rearm(None, now).unwrap();
        assert!(bridge.last_armed().is_none());
    }
}
