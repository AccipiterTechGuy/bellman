//! Windows capability decision tree — pure function over probe facts.
//!
//! Unit-tested against mocked API answers (real hardware QA lands in C11).
//! See synthesis §2-Windows and §3.

use super::{Caveat, DisabledReason, PowerRail, WakeCapability, WakeMechanism};

/// Snapshot of Windows power-policy / capability APIs used by the probe.
#[derive(Debug, Clone)]
pub struct WindowsProbeFacts {
    /// `SYSTEM_POWER_CAPABILITIES.AoAc` — Modern Standby (S0ix).
    pub ao_ac: bool,
    /// `RtcWake` capability enum (≥ PowerSystemHibernate predicts S4 wake).
    pub rtc_wake_ok: bool,
    /// GUID_ALLOW_RTC_WAKE AC value (0=disable, 1=enable, 2=important-only).
    pub ac_rtcwake: u8,
    /// GUID_ALLOW_RTC_WAKE DC value.
    pub dc_rtcwake: u8,
    /// `GetSystemPowerStatus().ACLineStatus`: 0=offline, 1=online, 255=unknown.
    pub ac_line_status: u8,
    /// Result of a far-future `SetWaitableTimer(fResume=TRUE)` arm test.
    /// `Ok(())` = clean; `Err(true)` = ERROR_NOT_SUPPORTED; `Err(false)` = other.
    pub arm_test: Result<(), bool>,
}

/// Pure decision tree matching synthesis §2-Windows.
pub fn decide(facts: &WindowsProbeFacts) -> WakeCapability {
    if !facts.rtc_wake_ok {
        return WakeCapability::Disabled {
            reason: DisabledReason::ResumeTimersUnsupported,
        };
    }

    let (rail, value) = match facts.ac_line_status {
        1 => (PowerRail::Ac, facts.ac_rtcwake),
        0 => (PowerRail::Dc, facts.dc_rtcwake),
        // Unknown → treat as AC for the gate, still report honestly via caveats.
        _ => (PowerRail::Ac, facts.ac_rtcwake),
    };

    // 0 = disable, 2 = important-only (app timers aren't "important").
    if value == 0 || value == 2 {
        return WakeCapability::Disabled {
            reason: DisabledReason::WakeTimersDisabledByPolicy { rail, value },
        };
    }

    match facts.arm_test {
        Err(true) => WakeCapability::Disabled {
            reason: DisabledReason::ResumeTimersUnsupported,
        },
        Err(false) => WakeCapability::Disabled {
            reason: DisabledReason::ProbeFailed {
                detail: "SetWaitableTimer arm test failed".into(),
            },
        },
        Ok(()) => {
            let mut caveats = vec![Caveat::HibernateNotGuaranteed];
            if facts.ao_ac {
                caveats.push(Caveat::ModernStandbyScreenOff);
            }
            if facts.ac_line_status == 0 && facts.dc_rtcwake == 1 {
                // On battery but DC allows — fine; no BatteryRailExcluded.
            }
            WakeCapability::Enabled {
                mechanism: WakeMechanism::WindowsWaitableTimer,
                caveats,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> WindowsProbeFacts {
        WindowsProbeFacts {
            ao_ac: false,
            rtc_wake_ok: true,
            ac_rtcwake: 1,
            dc_rtcwake: 0,
            ac_line_status: 1,
            arm_test: Ok(()),
        }
    }

    #[test]
    fn ac_enabled_clean() {
        let cap = decide(&base());
        assert!(cap.is_enabled());
        match cap {
            WakeCapability::Enabled { mechanism, caveats } => {
                assert_eq!(mechanism, WakeMechanism::WindowsWaitableTimer);
                assert!(caveats.contains(&Caveat::HibernateNotGuaranteed));
            }
            _ => panic!("expected Enabled"),
        }
    }

    #[test]
    fn modern_standby_adds_caveat() {
        let mut f = base();
        f.ao_ac = true;
        let cap = decide(&f);
        match cap {
            WakeCapability::Enabled { caveats, .. } => {
                assert!(caveats.contains(&Caveat::ModernStandbyScreenOff));
            }
            _ => panic!("expected Enabled"),
        }
    }

    #[test]
    fn policy_disabled_ac() {
        let mut f = base();
        f.ac_rtcwake = 0;
        match decide(&f) {
            WakeCapability::Disabled {
                reason: DisabledReason::WakeTimersDisabledByPolicy { rail, value },
            } => {
                assert_eq!(rail, PowerRail::Ac);
                assert_eq!(value, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn important_only_is_disabled() {
        let mut f = base();
        f.ac_rtcwake = 2;
        assert!(!decide(&f).is_enabled());
    }

    #[test]
    fn dc_rail_policy() {
        let mut f = base();
        f.ac_line_status = 0;
        f.dc_rtcwake = 0;
        match decide(&f) {
            WakeCapability::Disabled {
                reason: DisabledReason::WakeTimersDisabledByPolicy { rail, .. },
            } => assert_eq!(rail, PowerRail::Dc),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn error_not_supported() {
        let mut f = base();
        f.arm_test = Err(true);
        match decide(&f) {
            WakeCapability::Disabled {
                reason: DisabledReason::ResumeTimersUnsupported,
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn status_line_stable_for_policy() {
        let mut f = base();
        f.ac_rtcwake = 0;
        let line = decide(&f).status_line();
        assert!(line.starts_with("Wake from sleep: OFF — "));
        assert!(line.contains("power policy"));
    }
}
