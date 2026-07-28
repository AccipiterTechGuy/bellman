//! RTC wake-from-sleep: capability probe, per-OS arm, single-next-wake bridge.
//!
//! Design source: `docs/research/rtc_wake_synthesis.md` (§2–§5).
//!
//! Contract:
//! - Probe is always in-process and prompt-free.
//! - Arms at `wake_utc − 45 s`; the wake event **never** fires actions.
//! - When capability is Disabled, `program_wake` is a silent skip.

pub mod single_next_wake;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// Decision-tree helpers always compile so unit tests run on every host.
pub mod windows_decision;
pub mod macos_decision;

#[cfg(target_os = "linux")]
pub use linux::LinuxWake;
#[cfg(target_os = "macos")]
pub use macos::MacosWake;
#[cfg(target_os = "windows")]
pub use windows::WindowsWake;

pub use single_next_wake::{elect_next_wake, SingleNextWake};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::mpsc::Receiver;

/// Slack between the RTC arm instant and the true timer due time (seconds).
/// Covers macOS 30 s round-down, resume latency, and clock settle.
pub const ARM_SLACK_SECS: i64 = 45;

/// Hardware / OS mechanism that will resume the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeMechanism {
    LinuxAlarmTimerfd,
    LinuxWakealarmSysfs,
    WindowsWaitableTimer,
    MacPmDaemon,
}

impl WakeMechanism {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinuxAlarmTimerfd => "Linux timerfd (CLOCK_REALTIME_ALARM)",
            Self::LinuxWakealarmSysfs => "Linux sysfs wakealarm",
            Self::WindowsWaitableTimer => "Windows waitable timer",
            Self::MacPmDaemon => "macOS power-management daemon",
        }
    }
}

impl fmt::Display for WakeMechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Soft caveats attached to an Enabled capability (never block the feature).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Caveat {
    /// Windows S0ix / Modern Standby — wake runs screen-off.
    ModernStandbyScreenOff,
    /// Apple Silicon field reliability is non-deterministic.
    AppleSiliconBestEffort,
    /// Active power rail is battery and DC wake timers are off (Windows).
    BatteryRailExcluded,
    /// Hibernate/S4 is firmware-dependent and not guaranteed.
    HibernateNotGuaranteed,
}

impl Caveat {
    pub fn note(self) -> &'static str {
        match self {
            Self::ModernStandbyScreenOff => "wake runs screen-off (Modern Standby)",
            Self::AppleSiliconBestEffort => "Apple Silicon best-effort",
            Self::BatteryRailExcluded => "battery rail excludes wake timers",
            Self::HibernateNotGuaranteed => "hibernate not guaranteed",
        }
    }
}

/// Active Windows power rail (AC vs DC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerRail {
    Ac,
    Dc,
}

impl PowerRail {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ac => "AC",
            Self::Dc => "battery",
        }
    }
}

/// Why wake is unavailable. Every variant carries a user-facing sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DisabledReason {
    UnsupportedOs,
    NoRtc,
    RtcNotWakeCapable,
    NoPermission { hint: String },
    WakeTimersDisabledByPolicy { rail: PowerRail, value: u8 },
    ResumeTimersUnsupported,
    HelperNotInstalled,
    HelperAwaitingApproval,
    ProbeFailed { detail: String },
    /// Master config toggle is off (`wake.enabled = false`).
    MasterDisabled,
}

impl DisabledReason {
    /// Exact sentence used by the Settings status line and the JSONL event.
    pub fn sentence(&self) -> String {
        match self {
            Self::UnsupportedOs => "this OS build does not support machine wake".into(),
            Self::NoRtc => "no RTC device found".into(),
            Self::RtcNotWakeCapable => "RTC is present but not wake-capable".into(),
            Self::NoPermission { hint } => {
                format!("no permission to arm a wake timer ({hint})")
            }
            Self::WakeTimersDisabledByPolicy { rail, value } => format!(
                "wake timers disabled by power policy on {} rail (RTCWAKE={})",
                rail.as_str(),
                value
            ),
            Self::ResumeTimersUnsupported => {
                "this machine does not support resume timers (ERROR_NOT_SUPPORTED)".into()
            }
            Self::HelperNotInstalled => {
                "macOS wake helper is not installed — enroll from Settings".into()
            }
            Self::HelperAwaitingApproval => {
                "macOS wake helper is waiting for Login Items approval".into()
            }
            Self::ProbeFailed { detail } => format!("wake probe failed: {detail}"),
            Self::MasterDisabled => "wake from sleep is turned off in Settings".into(),
        }
    }

    /// Optional fix-it hint for the UI (copy-paste / elevated button).
    pub fn fix_hint(&self) -> Option<&'static str> {
        match self {
            Self::NoPermission { .. } => Some(
                "Launch Bellman from a desktop session (XDG autostart preserves CAP_WAKE_ALARM on systemd ≥254), or install a udev rule granting write access to /sys/class/rtc/rtc0/wakealarm.",
            ),
            Self::WakeTimersDisabledByPolicy { .. } => Some(
                "powercfg /setacvalueindex SCHEME_CURRENT SUB_SLEEP RTCWAKE 1  (and setdcvalueindex for battery)",
            ),
            Self::HelperNotInstalled | Self::HelperAwaitingApproval => {
                Some("Enroll the Bellman wake daemon and approve it under System Settings → Login Items.")
            }
            Self::MasterDisabled => Some("Enable “Allow Bellman to wake this machine” in Settings."),
            _ => None,
        }
    }
}

/// Result of the in-process capability probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WakeCapability {
    Enabled {
        mechanism: WakeMechanism,
        #[serde(default)]
        caveats: Vec<Caveat>,
    },
    Disabled {
        reason: DisabledReason,
    },
}

impl WakeCapability {
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }

    /// Settings / JSONL status line — exact product sentence.
    pub fn status_line(&self) -> String {
        status_line(self)
    }
}

/// Build the status-line sentence for a capability.
pub fn status_line(cap: &WakeCapability) -> String {
    match cap {
        WakeCapability::Enabled { mechanism, caveats } => {
            if caveats.is_empty() {
                format!("Wake from sleep: ON via {mechanism}")
            } else {
                let notes: Vec<&str> = caveats.iter().map(|c| c.note()).collect();
                format!(
                    "Wake from sleep: ON via {mechanism} ({})",
                    notes.join("; ")
                )
            }
        }
        WakeCapability::Disabled { reason } => {
            format!("Wake from sleep: OFF — {}", reason.sentence())
        }
    }
}

/// Errors from arm / cancel (never elevation prompts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeError {
    Disabled,
    Io(String),
    Unsupported,
}

impl fmt::Display for WakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("wake capability disabled"),
            Self::Io(s) => write!(f, "wake io: {s}"),
            Self::Unsupported => f.write_str("wake unsupported on this build"),
        }
    }
}

impl std::error::Error for WakeError {}

/// Pre-suspend / resume events surfaced to the bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    /// System is about to suspend (refresh arm, ≤5 s Linux / ~2 s Windows).
    Suspending,
    /// System has resumed (misfire pass + rearm).
    Resumed,
    /// Power rail flipped (Windows AC↔DC) — re-probe.
    PowerSourceChanged,
}

/// One process-wide wake controller. Implementations are armed-state-aware;
/// `program_wake` replaces any previous wake (cancel-then-arm, idempotent).
pub trait MachineWake: Send + Sync {
    /// Cached capability; call [`re_probe`](Self::re_probe) after failures /
    /// resume / power-source change.
    fn capability(&self) -> WakeCapability;

    /// Re-run the in-process probe and update the cache.
    fn re_probe(&self) -> WakeCapability;

    /// Arm a single absolute wake at `at` (already slack-adjusted by the
    /// bridge). Silent-skip when Disabled. Never fires actions.
    fn program_wake(&self, at: DateTime<Utc>) -> Result<(), WakeError>;

    /// Cancel any armed wake. `Ok(())` when nothing was armed.
    fn cancel_wake(&self) -> Result<(), WakeError>;

    /// Optional channel of suspend/resume/power events. Default: closed.
    fn power_events(&self) -> Option<Receiver<PowerEvent>> {
        None
    }

    /// After a successful auto-wake on Windows, hold the system awake while
    /// actions run. Default no-op.
    fn hold_system_awake(&self, _hold: bool) {}

    /// Restore a displaced foreign sysfs alarm (Linux fallback). Default no-op.
    fn on_resumed(&self) {}
}

/// Build the platform-native wake implementation for this host.
pub fn create_wake() -> Box<dyn MachineWake> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxWake::probe())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(WindowsWake::probe())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(MacosWake::probe())
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Box::new(UnsupportedWake)
    }
}

/// Fallback for exotic targets.
#[allow(dead_code)]
struct UnsupportedWake;

impl MachineWake for UnsupportedWake {
    fn capability(&self) -> WakeCapability {
        WakeCapability::Disabled {
            reason: DisabledReason::UnsupportedOs,
        }
    }
    fn re_probe(&self) -> WakeCapability {
        self.capability()
    }
    fn program_wake(&self, _at: DateTime<Utc>) -> Result<(), WakeError> {
        Err(WakeError::Unsupported)
    }
    fn cancel_wake(&self) -> Result<(), WakeError> {
        Ok(())
    }
}

/// Apply the −45 s slack. Clamps to "now + 1 s" when the due time is too near.
pub fn arm_instant(wake_utc: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    let target = wake_utc - chrono::Duration::seconds(ARM_SLACK_SECS);
    let floor = now + chrono::Duration::seconds(1);
    if target < floor {
        floor
    } else {
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn arm_instant_subtracts_slack() {
        let now = Utc.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap();
        let due = now + chrono::Duration::hours(1);
        let arm = arm_instant(due, now);
        assert_eq!(arm, due - chrono::Duration::seconds(45));
    }

    #[test]
    fn arm_instant_clamps_near_due() {
        let now = Utc.with_ymd_and_hms(2026, 7, 28, 12, 0, 0).unwrap();
        let due = now + chrono::Duration::seconds(10);
        let arm = arm_instant(due, now);
        assert_eq!(arm, now + chrono::Duration::seconds(1));
    }

    #[test]
    fn status_line_enabled_with_caveat() {
        let cap = WakeCapability::Enabled {
            mechanism: WakeMechanism::WindowsWaitableTimer,
            caveats: vec![Caveat::ModernStandbyScreenOff],
        };
        assert_eq!(
            cap.status_line(),
            "Wake from sleep: ON via Windows waitable timer (wake runs screen-off (Modern Standby))"
        );
    }

    #[test]
    fn status_line_disabled_permission() {
        let cap = WakeCapability::Disabled {
            reason: DisabledReason::NoPermission {
                hint: "CAP_WAKE_ALARM / udev rule".into(),
            },
        };
        assert!(cap.status_line().starts_with("Wake from sleep: OFF — "));
        assert!(cap.status_line().contains("no permission"));
    }

    #[test]
    fn create_wake_probes_without_panic() {
        let w = create_wake();
        let cap = w.capability();
        // Just ensure we get a well-formed status line.
        let line = cap.status_line();
        assert!(line.starts_with("Wake from sleep:"));
    }
}
