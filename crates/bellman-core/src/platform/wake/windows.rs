//! Windows wake: SetWaitableTimer(fResume=TRUE), absolute UTC FILETIME.
//!
//! Probe: SystemPowerCapabilities + GUID_ALLOW_RTC_WAKE per rail + live arm test.
//! See synthesis §2-Windows.

use super::windows_decision::{decide, WindowsProbeFacts};
use super::{MachineWake, PowerRail, WakeCapability, WakeError};
use chrono::{DateTime, Utc};
use std::sync::Mutex;

pub struct WindowsWake {
    state: Mutex<WinState>,
}

struct WinState {
    capability: WakeCapability,
    /// Waitable-timer HANDLE, kept as an integer so the state stays `Send`
    /// (a raw pointer would not be, and `MachineWake` crosses threads).
    #[cfg(windows)]
    timer: Option<isize>,
    armed: Option<DateTime<Utc>>,
    hold_awake: bool,
}

impl WindowsWake {
    pub fn probe() -> Self {
        let capability = probe_live();
        Self {
            state: Mutex::new(WinState {
                capability,
                #[cfg(windows)]
                timer: None,
                armed: None,
                hold_awake: false,
            }),
        }
    }
}

fn probe_live() -> WakeCapability {
    #[cfg(windows)]
    {
        let facts = collect_facts();
        decide(&facts)
    }
    #[cfg(not(windows))]
    {
        // Should never be constructed off-Windows; decision tree is unit-tested.
        WakeCapability::Disabled {
            reason: super::DisabledReason::UnsupportedOs,
        }
    }
}

#[cfg(windows)]
fn collect_facts() -> WindowsProbeFacts {
    use windows::core::GUID;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::System::Power::{
        CallNtPowerInformation, GetSystemPowerStatus, PowerGetActiveScheme, PowerReadACValueIndex,
        PowerReadDCValueIndex, SYSTEM_POWER_CAPABILITIES, SYSTEM_POWER_STATUS,
    };

    // Defaults if probes fail.
    let mut facts = WindowsProbeFacts {
        ao_ac: false,
        rtc_wake_ok: true,
        ac_rtcwake: 1,
        dc_rtcwake: 0,
        ac_line_status: 1,
        arm_test: Ok(()),
    };

    // Power status (AC line).
    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status).is_ok() {
            facts.ac_line_status = status.ACLineStatus;
        }
    }

    // SystemPowerCapabilities via CallNtPowerInformation.
    unsafe {
        let mut caps = SYSTEM_POWER_CAPABILITIES::default();
        let size = std::mem::size_of::<SYSTEM_POWER_CAPABILITIES>() as u32;
        // SystemPowerCapabilities = 4
        if CallNtPowerInformation(
            windows::Win32::System::Power::POWER_INFORMATION_LEVEL(4),
            None,
            0,
            Some(&mut caps as *mut _ as *mut _),
            size,
        )
        .is_ok()
        {
            facts.ao_ac = caps.AoAc.0 != 0;
            // RtcWake: non-zero means some sleep state can wake via RTC.
            facts.rtc_wake_ok = caps.RtcWake.0 != 0 || caps.SystemS3.0 != 0 || caps.SystemS4.0 != 0;
        }
    }

    // GUID_ALLOW_RTC_WAKE = bd3b718a-0680-4d9d-8ab2-e1d2b4ac806d
    // GUID_SLEEP_SUBGROUP  = 238C9FA8-0AAD-41ED-83F4-97BE242C8F20
    let allow_rtc = GUID::from_u128(0xbd3b718a_0680_4d9d_8ab2_e1d2b4ac806d);
    let sleep_sub = GUID::from_u128(0x238C9FA8_0AAD_41ED_83F4_97BE242C8F20);

    unsafe {
        let mut scheme: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(None, &mut scheme).is_ok() && !scheme.is_null() {
            let mut ac: u32 = 1;
            let mut dc: u32 = 0;
            let _ = PowerReadACValueIndex(
                None,
                Some(scheme as *const GUID),
                Some(&sleep_sub),
                Some(&allow_rtc),
                &mut ac,
            );
            let _ = PowerReadDCValueIndex(
                None,
                Some(scheme as *const GUID),
                Some(&sleep_sub),
                Some(&allow_rtc),
                &mut dc,
            );
            facts.ac_rtcwake = ac.min(255) as u8;
            facts.dc_rtcwake = dc.min(255) as u8;
            let _ = LocalFree(HLOCAL(scheme.cast()));
        }
    }

    // Live arm test with a far-future absolute timer.
    facts.arm_test = live_arm_test();
    facts
}

#[cfg(windows)]
fn live_arm_test() -> Result<(), bool> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CancelWaitableTimer, CreateWaitableTimerExW, SetWaitableTimer, TIMER_ALL_ACCESS,
    };

    unsafe {
        let timer = match CreateWaitableTimerExW(
            None,
            None,
            // No HIGH_RESOLUTION for resume timers (synthesis).
            0,
            TIMER_ALL_ACCESS.0,
        ) {
            Ok(h) => h,
            Err(_) => return Err(false),
        };

        // Far future: now + 30 days as absolute FILETIME (100-ns since 1601).
        let due = Utc::now() + chrono::Duration::days(30);
        let due_time = utc_to_due_time(due);
        // fResume = TRUE
        let ok = SetWaitableTimer(timer, &due_time, 0, None, None, true);
        let result = if ok.is_ok() {
            Ok(())
        } else {
            // ERROR_NOT_SUPPORTED = 50
            let err = windows::Win32::Foundation::GetLastError();
            if err.0 == 50 {
                Err(true)
            } else {
                Err(false)
            }
        };
        let _ = CancelWaitableTimer(timer);
        let _ = CloseHandle(timer);
        result
    }
}

#[cfg(windows)]
fn utc_to_due_time(at: DateTime<Utc>) -> i64 {
    // SetWaitableTimer due time: a positive value is absolute UTC FILETIME,
    // 100-ns intervals since 1601-01-01. Unix epoch is 11644473600 s later.
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
    (at.timestamp() + EPOCH_DIFF_SECS) * 10_000_000 + i64::from(at.timestamp_subsec_nanos() / 100)
}

impl MachineWake for WindowsWake {
    fn capability(&self) -> WakeCapability {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .capability
            .clone()
    }

    fn re_probe(&self) -> WakeCapability {
        let cap = probe_live();
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .capability = cap.clone();
        cap
    }

    fn program_wake(&self, at: DateTime<Utc>) -> Result<(), WakeError> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !st.capability.is_enabled() {
            return Ok(());
        }
        #[cfg(windows)]
        {
            arm_timer(&mut st, at)?;
            st.armed = Some(at);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = at;
            Err(WakeError::Unsupported)
        }
    }

    fn cancel_wake(&self) -> Result<(), WakeError> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(windows)]
        {
            cancel_timer(&mut st);
        }
        st.armed = None;
        Ok(())
    }

    fn hold_system_awake(&self, hold: bool) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.hold_awake = hold;
        #[cfg(windows)]
        {
            use windows::Win32::System::Power::{
                SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
            };
            unsafe {
                if hold {
                    let _ = SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
                } else {
                    let _ = SetThreadExecutionState(ES_CONTINUOUS);
                }
            }
        }
    }
}

#[cfg(windows)]
fn arm_timer(st: &mut WinState, at: DateTime<Utc>) -> Result<(), WakeError> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        CreateWaitableTimerExW, SetWaitableTimer, TIMER_ALL_ACCESS,
    };

    unsafe {
        if st.timer.is_none() {
            let h = CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS.0)
                .map_err(|e| WakeError::Io(format!("CreateWaitableTimerExW: {e}")))?;
            st.timer = Some(h.0 as isize);
        }
        let handle = HANDLE(st.timer.unwrap() as *mut _);
        let due_time = utc_to_due_time(at);
        SetWaitableTimer(handle, &due_time, 0, None, None, true)
            .map_err(|e| WakeError::Io(format!("SetWaitableTimer: {e}")))?;
    }
    Ok(())
}

#[cfg(windows)]
fn cancel_timer(st: &mut WinState) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::CancelWaitableTimer;
    if let Some(raw) = st.timer.take() {
        unsafe {
            let handle = HANDLE(raw as *mut _);
            let _ = CancelWaitableTimer(handle);
            let _ = CloseHandle(handle);
        }
    }
}

/// Elevated fix-it command (user-initiated only) when policy-blocked.
pub fn powercfg_fix_command(rail: PowerRail) -> String {
    match rail {
        PowerRail::Ac => {
            "powercfg /setacvalueindex SCHEME_CURRENT SUB_SLEEP RTCWAKE 1 && powercfg /setactive SCHEME_CURRENT"
                .into()
        }
        PowerRail::Dc => {
            "powercfg /setdcvalueindex SCHEME_CURRENT SUB_SLEEP RTCWAKE 1 && powercfg /setactive SCHEME_CURRENT"
                .into()
        }
    }
}
