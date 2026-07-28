//! macOS wake: XPC client to SMAppService root daemon → IOPMSchedulePowerEvent.
//!
//! Probe: SMAppService.daemon.status + through-daemon sentinel schedule/cancel.
//! See synthesis §2-macOS. Daemon lives in `helpers/macos-wake-daemon/`.

use super::macos_decision::{decide, HelperStatus, MacosProbeFacts};
use super::{MachineWake, WakeCapability, WakeError};
use chrono::{DateTime, Utc};
use std::sync::Mutex;

const OWN_TAG: &str = "com.bellman.wake";

pub struct MacosWake {
    state: Mutex<MacState>,
}

struct MacState {
    capability: WakeCapability,
    /// Exact (time, id, type) for cancel-by-match.
    armed: Option<ArmedWake>,
}

#[derive(Clone)]
struct ArmedWake {
    at: DateTime<Utc>,
    tag: String,
}

impl MacosWake {
    pub fn probe() -> Self {
        let capability = probe_live();
        Self {
            state: Mutex::new(MacState {
                capability,
                armed: None,
            }),
        }
    }
}

fn probe_live() -> WakeCapability {
    #[cfg(target_os = "macos")]
    {
        let facts = collect_facts();
        decide(&facts)
    }
    #[cfg(not(target_os = "macos"))]
    {
        WakeCapability::Disabled {
            reason: super::DisabledReason::UnsupportedOs,
        }
    }
}

#[cfg(target_os = "macos")]
fn collect_facts() -> MacosProbeFacts {
    // SMAppService status via a small FFI/objc path. When the helper binary
    // is not bundled (dev builds), report NotRegistered so the UI can enroll.
    let helper = smapp_helper_status();
    let (sentinel_ok, sentinel_error) = if helper == HelperStatus::Enabled {
        match daemon_sentinel_roundtrip() {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e)),
        }
    } else {
        (false, None)
    };
    MacosProbeFacts {
        helper,
        sentinel_ok,
        apple_silicon: is_apple_silicon(),
        sentinel_error,
    }
}

#[cfg(target_os = "macos")]
fn smapp_helper_status() -> HelperStatus {
    // Best-effort: if the daemon socket / launchd label is absent → NotRegistered.
    // Full SMAppService.objc binding is exercised on real macOS CI (C11).
    let label = "com.bellman.wake-daemon";
    // Check launchctl print for the system domain service.
    let out = std::process::Command::new("launchctl")
        .args(["print", &format!("system/{label}")])
        .output();
    match out {
        Ok(o) if o.status.success() => HelperStatus::Enabled,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stderr.contains("Could not find") || stderr.contains("no such process") {
                HelperStatus::NotRegistered
            } else {
                // Ambiguous — treat as awaiting approval when service exists but inactive.
                HelperStatus::RequiresApproval
            }
        }
        Err(_) => HelperStatus::NotRegistered,
    }
}

#[cfg(target_os = "macos")]
fn daemon_sentinel_roundtrip() -> Result<(), String> {
    // Schedule a far-future wake via the XPC/daemon path, then cancel.
    // Dev stub: use IOPMCopyScheduledPowerEvents presence as a smoke check
    // when the daemon is not yet talking XPC in this build.
    let far = Utc::now() + chrono::Duration::days(30);
    xpc_schedule(far)?;
    xpc_cancel_mine()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_apple_silicon() -> bool {
    std::env::consts::ARCH == "aarch64"
}

#[cfg(target_os = "macos")]
fn xpc_schedule(at: DateTime<Utc>) -> Result<(), String> {
    // Placeholder until the daemon XPC client is fully linked: call through
    // a private pipe protocol file if present, else error with a clear hint.
    let sock = std::path::Path::new("/var/run/bellman-wake.sock");
    if !sock.exists() {
        return Err("wake daemon socket not present".into());
    }
    // Socket protocol: one-line JSON { "op":"schedule", "epoch":N, "tag":"com.bellman.wake" }
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let mut s = UnixStream::connect(sock).map_err(|e| e.to_string())?;
    let msg = format!(
        "{{\"op\":\"schedule\",\"epoch\":{},\"tag\":\"{OWN_TAG}\"}}\n",
        at.timestamp()
    );
    s.write_all(msg.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    if buf.contains("\"ok\"") {
        Ok(())
    } else {
        Err(format!("daemon schedule failed: {buf}"))
    }
}

#[cfg(target_os = "macos")]
fn xpc_cancel_mine() -> Result<(), String> {
    let sock = std::path::Path::new("/var/run/bellman-wake.sock");
    if !sock.exists() {
        return Ok(());
    }
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let mut s = UnixStream::connect(sock).map_err(|e| e.to_string())?;
    let msg = format!("{{\"op\":\"cancel_my_wakes\",\"tag\":\"{OWN_TAG}\"}}\n");
    s.write_all(msg.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    Ok(())
}

impl MachineWake for MacosWake {
    fn capability(&self) -> WakeCapability {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).capability.clone()
    }

    fn re_probe(&self) -> WakeCapability {
        let cap = probe_live();
        self.state.lock().unwrap_or_else(|e| e.into_inner()).capability = cap.clone();
        cap
    }

    fn program_wake(&self, at: DateTime<Utc>) -> Result<(), WakeError> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !st.capability.is_enabled() {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            xpc_schedule(at).map_err(WakeError::Io)?;
            st.armed = Some(ArmedWake {
                at,
                tag: OWN_TAG.into(),
            });
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = at;
            Err(WakeError::Unsupported)
        }
    }

    fn cancel_wake(&self) -> Result<(), WakeError> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(target_os = "macos")]
        {
            if st.armed.is_some() {
                let _ = xpc_cancel_mine();
            }
        }
        st.armed = None;
        Ok(())
    }
}

/// Deep-link to System Settings → Login Items (user-initiated).
pub fn login_items_deeplink() -> &'static str {
    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"
}

/// Launchd label / SMAppService identifier for the bundled daemon.
pub fn daemon_service_id() -> &'static str {
    "com.bellman.wake-daemon"
}
