//! Linux RTC wake: timerfd CLOCK_REALTIME_ALARM primary, sysfs cooperative fallback.
//!
//! Probe order (synthesis §2-Linux):
//! 1. `/sys/class/rtc/rtc0` exists
//! 2. `device/power/wakeup == enabled`
//! 3. `timerfd_create(CLOCK_REALTIME_ALARM)` succeeds → Enabled(LinuxAlarmTimerfd)
//! 4. else `access(wakealarm, W_OK)` → Enabled(LinuxWakealarmSysfs)
//! 5. else Disabled(NoPermission{…})

use super::{
    Caveat, DisabledReason, MachineWake, WakeCapability, WakeError, WakeMechanism,
};
use chrono::{DateTime, Utc};
use std::fs;
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

const RTC_DIR: &str = "/sys/class/rtc/rtc0";
const WAKEALARM: &str = "/sys/class/rtc/rtc0/wakealarm";
const WAKEUP: &str = "/sys/class/rtc/rtc0/device/power/wakeup";

const NO_PERM_HINT: &str = "systemd ≥254 local session / AmbientCapabilities=CAP_WAKE_ALARM / setcap / udev rule";

/// Linux in-process wake controller.
pub struct LinuxWake {
    state: Mutex<LinuxState>,
}

struct LinuxState {
    capability: WakeCapability,
    /// Held open when mechanism is timerfd (kernel multiplexes alarms).
    timerfd: Option<OwnedFd>,
    /// Absolute epoch seconds we last programmed (sysfs or timerfd).
    armed_epoch: Option<i64>,
    /// Foreign sysfs alarm we displaced (restore on resume).
    displaced_foreign: Option<i64>,
    /// Whether we own the current sysfs wakealarm value.
    sysfs_owned: bool,
}

impl LinuxWake {
    /// Run the ordered probe and build a controller.
    pub fn probe() -> Self {
        let (capability, timerfd) = probe_inner();
        Self {
            state: Mutex::new(LinuxState {
                capability,
                timerfd,
                armed_epoch: None,
                displaced_foreign: None,
                sysfs_owned: false,
            }),
        }
    }

    /// Pure probe facts for tests (no side effects beyond timerfd create).
    pub fn probe_capability() -> WakeCapability {
        probe_inner().0
    }
}

fn probe_inner() -> (WakeCapability, Option<OwnedFd>) {
    let rtc = Path::new(RTC_DIR);
    if !rtc.exists() {
        return (
            WakeCapability::Disabled {
                reason: DisabledReason::NoRtc,
            },
            None,
        );
    }

    match read_trimmed(WAKEUP) {
        Ok(v) if v == "enabled" => {}
        Ok(_) => {
            return (
                WakeCapability::Disabled {
                    reason: DisabledReason::RtcNotWakeCapable,
                },
                None,
            );
        }
        Err(_) => {
            // Some kernels omit the wakeup node when the RTC is wake-capable
            // by default — continue probing timerfd/sysfs.
        }
    }

    match create_alarm_timerfd() {
        Ok(fd) => {
            let cap = WakeCapability::Enabled {
                mechanism: WakeMechanism::LinuxAlarmTimerfd,
                caveats: vec![Caveat::HibernateNotGuaranteed],
            };
            (cap, Some(fd))
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            if wakealarm_writable() {
                let cap = WakeCapability::Enabled {
                    mechanism: WakeMechanism::LinuxWakealarmSysfs,
                    caveats: vec![Caveat::HibernateNotGuaranteed],
                };
                (cap, None)
            } else {
                (
                    WakeCapability::Disabled {
                        reason: DisabledReason::NoPermission {
                            hint: NO_PERM_HINT.into(),
                        },
                    },
                    None,
                )
            }
        }
        Err(e) => (
            WakeCapability::Disabled {
                reason: DisabledReason::ProbeFailed {
                    detail: format!("timerfd_create: {e}"),
                },
            },
            None,
        ),
    }
}

fn create_alarm_timerfd() -> io::Result<OwnedFd> {
    // rustix TimerfdClockId::RealtimeAlarm == CLOCK_REALTIME_ALARM.
    use rustix::time::{timerfd_create, TimerfdClockId, TimerfdFlags};
    let fd = timerfd_create(TimerfdClockId::RealtimeAlarm, TimerfdFlags::CLOEXEC)
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))?;
    Ok(fd)
}

fn wakealarm_writable() -> bool {
    Path::new(WAKEALARM).exists()
        && fs::OpenOptions::new()
            .write(true)
            .open(WAKEALARM)
            .is_ok()
}

fn read_trimmed(path: &str) -> io::Result<String> {
    let s = fs::read_to_string(path)?;
    Ok(s.trim().to_string())
}

fn epoch_secs(at: DateTime<Utc>) -> i64 {
    at.timestamp()
}

impl MachineWake for LinuxWake {
    fn capability(&self) -> WakeCapability {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).capability.clone()
    }

    fn re_probe(&self) -> WakeCapability {
        let (cap, fd) = probe_inner();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // Keep an existing armed timerfd if still Enabled via timerfd.
        if matches!(
            &cap,
            WakeCapability::Enabled {
                mechanism: WakeMechanism::LinuxAlarmTimerfd,
                ..
            }
        ) {
            if st.timerfd.is_none() {
                st.timerfd = fd;
            }
        } else {
            st.timerfd = fd;
            st.armed_epoch = None;
        }
        st.capability = cap.clone();
        cap
    }

    fn program_wake(&self, at: DateTime<Utc>) -> Result<(), WakeError> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let cap = st.capability.clone();
        match cap {
            WakeCapability::Disabled { .. } => Ok(()), // silent skip
            WakeCapability::Enabled {
                mechanism: WakeMechanism::LinuxAlarmTimerfd,
                ..
            } => {
                let fd = st.timerfd.as_ref().ok_or_else(|| {
                    WakeError::Io("timerfd missing despite Enabled".into())
                })?;
                set_timerfd_absolute(fd, at).map_err(|e| WakeError::Io(e.to_string()))?;
                st.armed_epoch = Some(epoch_secs(at));
                Ok(())
            }
            WakeCapability::Enabled {
                mechanism: WakeMechanism::LinuxWakealarmSysfs,
                ..
            } => {
                program_sysfs(&mut st, at).map_err(|e| WakeError::Io(e.to_string()))
            }
            WakeCapability::Enabled { mechanism, .. } => {
                Err(WakeError::Io(format!(
                    "linux controller cannot arm via {mechanism}"
                )))
            }
        }
    }

    fn cancel_wake(&self) -> Result<(), WakeError> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match &st.capability {
            WakeCapability::Enabled {
                mechanism: WakeMechanism::LinuxAlarmTimerfd,
                ..
            } => {
                if let Some(fd) = st.timerfd.as_ref() {
                    // Disarm: zero itimerspec.
                    disarm_timerfd(fd).map_err(|e| WakeError::Io(e.to_string()))?;
                }
                st.armed_epoch = None;
                Ok(())
            }
            WakeCapability::Enabled {
                mechanism: WakeMechanism::LinuxWakealarmSysfs,
                ..
            } => {
                cancel_sysfs(&mut st).map_err(|e| WakeError::Io(e.to_string()))
            }
            _ => {
                st.armed_epoch = None;
                Ok(())
            }
        }
    }

    fn on_resumed(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let restore = plan_sysfs_restore(st.sysfs_owned, st.displaced_foreign);
        if st.sysfs_owned || st.displaced_foreign.is_some() {
            // Some(epoch) → restore foreign; None while owned → clear our one-shot.
            let _ = write_wakealarm_clear_set(restore);
            st.displaced_foreign = None;
            st.sysfs_owned = false;
            st.armed_epoch = None;
        }
    }
}

fn set_timerfd_absolute(fd: &OwnedFd, at: DateTime<Utc>) -> io::Result<()> {
    use rustix::time::{timerfd_settime, Itimerspec, TimerfdTimerFlags, Timespec};
    let secs = at.timestamp();
    let nsecs = at.timestamp_subsec_nanos() as i64;
    let new_value = Itimerspec {
        it_interval: Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: Timespec {
            tv_sec: secs as _,
            tv_nsec: nsecs as _,
        },
    };
    // TIMER_ABSTIME — absolute CLOCK_REALTIME_ALARM.
    timerfd_settime(fd, TimerfdTimerFlags::ABSTIME, &new_value)
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))?;
    Ok(())
}

fn disarm_timerfd(fd: &OwnedFd) -> io::Result<()> {
    use rustix::time::{timerfd_settime, Itimerspec, TimerfdTimerFlags, Timespec};
    let zero = Itimerspec {
        it_interval: Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
    };
    timerfd_settime(fd, TimerfdTimerFlags::empty(), &zero)
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))?;
    Ok(())
}

/// Pure cooperative-sysfs decision (testable without /sys).
///
/// Returns `(write_epoch, displace_foreign, own_slot)`:
/// - `write_epoch`: value to program (None = leave foreign alone)
/// - `displace_foreign`: foreign alarm we must restore later
/// - `own_slot`: whether we now own the register
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysfsProgramPlan {
    pub write_epoch: Option<i64>,
    pub displace_foreign: Option<i64>,
    pub own_slot: bool,
}

/// Synthesis §1 #3: displace only later foreign alarms; keep earlier ones.
pub fn plan_sysfs_program(current: Option<i64>, ours: i64) -> SysfsProgramPlan {
    match current {
        Some(foreign) if foreign > 0 && foreign <= ours => SysfsProgramPlan {
            write_epoch: None,
            displace_foreign: None,
            own_slot: false,
        },
        Some(foreign) if foreign > 0 && foreign > ours => SysfsProgramPlan {
            write_epoch: Some(ours),
            displace_foreign: Some(foreign),
            own_slot: true,
        },
        _ => SysfsProgramPlan {
            write_epoch: Some(ours),
            displace_foreign: None,
            own_slot: true,
        },
    }
}

/// On resume: restore displaced foreign alarm, or clear our one-shot.
pub fn plan_sysfs_restore(
    sysfs_owned: bool,
    displaced_foreign: Option<i64>,
) -> Option<i64> {
    if !sysfs_owned && displaced_foreign.is_none() {
        return None;
    }
    // Some(epoch) = write that epoch; Some(0) sentinel handled by caller as clear.
    // We return the epoch to write, or None meaning clear (0).
    match displaced_foreign {
        Some(f) => Some(f),
        None if sysfs_owned => None, // clear
        None => None,
    }
}

/// Cooperative sysfs protocol (synthesis §1 #3):
/// read current → displace only if ours is earlier → restore displaced on resume.
fn program_sysfs(st: &mut LinuxState, at: DateTime<Utc>) -> io::Result<()> {
    let ours = epoch_secs(at);
    let current = read_wakealarm_epoch()?;
    let plan = plan_sysfs_program(current, ours);
    if let Some(e) = plan.write_epoch {
        write_wakealarm_clear_set(Some(e))?;
    }
    st.armed_epoch = plan.write_epoch;
    st.sysfs_owned = plan.own_slot;
    st.displaced_foreign = plan.displace_foreign;
    Ok(())
}

fn cancel_sysfs(st: &mut LinuxState) -> io::Result<()> {
    if st.sysfs_owned {
        if let Some(foreign) = st.displaced_foreign.take() {
            write_wakealarm_clear_set(Some(foreign))?;
        } else {
            write_wakealarm_clear_set(None)?;
        }
        st.sysfs_owned = false;
    }
    st.armed_epoch = None;
    Ok(())
}

fn read_wakealarm_epoch() -> io::Result<Option<i64>> {
    if !Path::new(WAKEALARM).exists() {
        return Ok(None);
    }
    let s = read_trimmed(WAKEALARM)?;
    if s.is_empty() || s == "0" {
        return Ok(None);
    }
    Ok(s.parse::<i64>().ok())
}

/// Two-step clear+set vs EBUSY (kernel requires clear before re-set).
fn write_wakealarm_clear_set(epoch: Option<i64>) -> io::Result<()> {
    let path = PathBuf::from(WAKEALARM);
    // Clear first.
    if let Ok(mut f) = fs::OpenOptions::new().write(true).open(&path) {
        let _ = f.write_all(b"0\n");
        let _ = f.flush();
    }
    if let Some(e) = epoch {
        let mut f = fs::OpenOptions::new().write(true).open(&path)?;
        write!(f, "{e}")?;
        f.flush()?;
    }
    Ok(())
}

/// Linux udev rule snippet shown in Settings / wizard when sysfs path is needed.
pub fn udev_rule_snippet() -> &'static str {
    r#"# /etc/udev/rules.d/99-bellman-wakealarm.rules
KERNEL=="rtc0", GROUP="bellman", MODE="0660"
# then: groupadd bellman && usermod -aG bellman $USER && udevadm control --reload"#
}

/// Informational RTC / power facts (not part of the capability decision).
pub fn informational_facts() -> LinuxInfo {
    LinuxInfo {
        rtc_present: Path::new(RTC_DIR).exists(),
        wakeup: read_trimmed(WAKEUP).ok(),
        power_state: fs::read_to_string("/sys/power/state")
            .ok()
            .map(|s| s.trim().to_string()),
        mem_sleep: fs::read_to_string("/sys/power/mem_sleep")
            .ok()
            .map(|s| s.trim().to_string()),
        since_epoch: fs::read_to_string(format!("{RTC_DIR}/since_epoch"))
            .ok()
            .and_then(|s| s.trim().parse().ok()),
        wall_epoch: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64),
    }
}

#[derive(Debug, Clone)]
pub struct LinuxInfo {
    pub rtc_present: bool,
    pub wakeup: Option<String>,
    pub power_state: Option<String>,
    pub mem_sleep: Option<String>,
    pub since_epoch: Option<i64>,
    pub wall_epoch: Option<i64>,
}

/// Decision-tree helper for unit tests (inject probe outcomes).
#[derive(Debug, Clone)]
pub struct LinuxProbeFacts {
    pub rtc_present: bool,
    pub wakeup_enabled: Option<bool>,
    pub timerfd_result: Result<(), i32>, // Ok or errno
    pub wakealarm_writable: bool,
}

pub fn decide_from_facts(facts: &LinuxProbeFacts) -> WakeCapability {
    if !facts.rtc_present {
        return WakeCapability::Disabled {
            reason: DisabledReason::NoRtc,
        };
    }
    if facts.wakeup_enabled == Some(false) {
        return WakeCapability::Disabled {
            reason: DisabledReason::RtcNotWakeCapable,
        };
    }
    match facts.timerfd_result {
        Ok(()) => WakeCapability::Enabled {
            mechanism: WakeMechanism::LinuxAlarmTimerfd,
            caveats: vec![Caveat::HibernateNotGuaranteed],
        },
        Err(libc_eperm) if libc_eperm == 1 /* EPERM */ || libc_eperm == 13 /* EACCES */ => {
            if facts.wakealarm_writable {
                WakeCapability::Enabled {
                    mechanism: WakeMechanism::LinuxWakealarmSysfs,
                    caveats: vec![Caveat::HibernateNotGuaranteed],
                }
            } else {
                WakeCapability::Disabled {
                    reason: DisabledReason::NoPermission {
                        hint: NO_PERM_HINT.into(),
                    },
                }
            }
        }
        Err(code) => WakeCapability::Disabled {
            reason: DisabledReason::ProbeFailed {
                detail: format!("timerfd_create errno {code}"),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_timerfd_primary() {
        let cap = decide_from_facts(&LinuxProbeFacts {
            rtc_present: true,
            wakeup_enabled: Some(true),
            timerfd_result: Ok(()),
            wakealarm_writable: false,
        });
        match cap {
            WakeCapability::Enabled {
                mechanism: WakeMechanism::LinuxAlarmTimerfd,
                ..
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decision_sysfs_fallback() {
        let cap = decide_from_facts(&LinuxProbeFacts {
            rtc_present: true,
            wakeup_enabled: Some(true),
            timerfd_result: Err(1),
            wakealarm_writable: true,
        });
        match cap {
            WakeCapability::Enabled {
                mechanism: WakeMechanism::LinuxWakealarmSysfs,
                ..
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decision_no_permission() {
        let cap = decide_from_facts(&LinuxProbeFacts {
            rtc_present: true,
            wakeup_enabled: Some(true),
            timerfd_result: Err(1),
            wakealarm_writable: false,
        });
        match cap {
            WakeCapability::Disabled {
                reason: DisabledReason::NoPermission { .. },
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decision_no_rtc() {
        let cap = decide_from_facts(&LinuxProbeFacts {
            rtc_present: false,
            wakeup_enabled: None,
            timerfd_result: Err(1),
            wakealarm_writable: false,
        });
        assert!(matches!(
            cap,
            WakeCapability::Disabled {
                reason: DisabledReason::NoRtc
            }
        ));
    }

    #[test]
    fn live_probe_returns_well_formed_capability() {
        let cap = LinuxWake::probe_capability();
        let line = cap.status_line();
        assert!(line.starts_with("Wake from sleep:"));
        // On this agent shell ambient CAP_WAKE_ALARM is typically absent →
        // either NoPermission or sysfs/timerfd if the environment allows.
        match &cap {
            WakeCapability::Enabled { mechanism, .. } => {
                assert!(matches!(
                    mechanism,
                    WakeMechanism::LinuxAlarmTimerfd | WakeMechanism::LinuxWakealarmSysfs
                ));
            }
            WakeCapability::Disabled { reason } => {
                assert!(matches!(
                    reason,
                    DisabledReason::NoPermission { .. }
                        | DisabledReason::NoRtc
                        | DisabledReason::RtcNotWakeCapable
                        | DisabledReason::ProbeFailed { .. }
                ));
            }
        }
    }

    #[test]
    fn live_daemon_descended_shell_is_not_timerfd_when_no_ambient() {
        // Research observation: a daemon-descended / non-session shell without
        // ambient CAP_WAKE_ALARM gets EPERM on timerfd. This agent process has
        // CapAmb=0 — so we must not claim LinuxAlarmTimerfd unless the create
        // actually succeeded (which would mean caps are present).
        let w = LinuxWake::probe();
        let cap = w.capability();
        if let WakeCapability::Enabled {
            mechanism: WakeMechanism::LinuxAlarmTimerfd,
            ..
        } = &cap
        {
            // Caps present — fine; Enabled is correct.
            let _ = w.program_wake(Utc::now() + chrono::Duration::hours(2));
            let _ = w.cancel_wake();
        } else if let WakeCapability::Disabled {
            reason: DisabledReason::NoPermission { hint },
        } = &cap
        {
            assert!(hint.contains("CAP_WAKE_ALARM") || hint.contains("udev"));
        }
    }

    #[test]
    fn sysfs_displaces_only_later_foreign() {
        let plan = plan_sysfs_program(Some(2_000_000_100), 2_000_000_000);
        assert_eq!(
            plan,
            SysfsProgramPlan {
                write_epoch: Some(2_000_000_000),
                displace_foreign: Some(2_000_000_100),
                own_slot: true,
            }
        );
    }

    #[test]
    fn sysfs_keeps_earlier_foreign() {
        let plan = plan_sysfs_program(Some(2_000_000_000), 2_000_000_100);
        assert_eq!(
            plan,
            SysfsProgramPlan {
                write_epoch: None,
                displace_foreign: None,
                own_slot: false,
            }
        );
    }

    #[test]
    fn sysfs_empty_slot_taken() {
        let plan = plan_sysfs_program(None, 2_000_000_000);
        assert!(plan.own_slot);
        assert_eq!(plan.write_epoch, Some(2_000_000_000));
        assert!(plan.displace_foreign.is_none());
    }

    #[test]
    fn sysfs_restore_displaced_foreign_on_resume() {
        // After we displaced foreign=100, resume restores 100.
        assert_eq!(
            plan_sysfs_restore(true, Some(2_000_000_100)),
            Some(2_000_000_100)
        );
        // Owned with no foreign → clear (None means write 0).
        assert_eq!(plan_sysfs_restore(true, None), None);
        // Not owned → nothing to do.
        assert_eq!(plan_sysfs_restore(false, None), None);
    }

    #[test]
    fn no_permission_fix_hint_is_honest_about_xdg() {
        let hint = DisabledReason::NoPermission {
            hint: "test".into(),
        }
        .fix_hint()
        .unwrap();
        assert!(
            hint.contains("setcap") || hint.contains("AmbientCapabilities"),
            "hint must name actionable paths: {hint}"
        );
        assert!(
            hint.to_lowercase().contains("does not") || hint.contains("not grant"),
            "hint must not claim plain XDG autostart always works: {hint}"
        );
    }
}
