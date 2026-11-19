//! User-initiated fix-it actions for wake capability (the ONLY elevation paths).
//!
//! Windows: elevated powercfg for RTCWAKE policy.
//! macOS: SMAppService enroll + Login Items deep-link.
//! Linux: udev rule is copy-only (no elevation from the app).

use bellman_core::PowerRail;

/// Elevated powercfg one-liner for the active (or given) rail.
pub fn powercfg_command_for_rail(rail: PowerRail) -> String {
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

/// System Settings → Login Items deep-link (macOS 13+).
pub fn login_items_deeplink() -> &'static str {
    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"
}

/// Launchd / SMAppService service id for the bundled wake daemon.
pub fn daemon_service_id() -> &'static str {
    "com.bellman.wake-daemon"
}

/// Run the Windows elevated powercfg fix-it (user-initiated UAC).
///
/// Returns a human message. On non-Windows, returns an error string.
pub fn run_windows_powercfg_fix(rail: PowerRail) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let cmd = powercfg_command_for_rail(rail);
        // Shell out via PowerShell Start-Process -Verb RunAs for UAC.
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Start-Process -FilePath cmd.exe -Verb RunAs -ArgumentList '/c {}' -Wait",
                    cmd.replace('\'', "''")
                ),
            ])
            .status()
            .map_err(|e| format!("failed to launch elevated powercfg: {e}"))?;
        if status.success() {
            Ok("powercfg RTCWAKE enabled (elevated). Re-probe to confirm.".into())
        } else {
            Err(format!("elevated powercfg exited with {status}"))
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = rail;
        Err("powercfg fix-it is only available on Windows".into())
    }
}

/// Enroll the macOS SMAppService wake daemon.
///
/// The privileged helper daemon (`helpers/macos-wake-daemon`) is not built,
/// signed, or bundled in this build, so enrollment cannot succeed and is
/// refused honestly instead of opening Login Items or invoking `launchctl`.
pub fn enroll_macos_wake_daemon() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        Err(
            "wake-from-sleep is not available in this macOS build (the privileged wake helper daemon is not bundled)"
                .into(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("macOS daemon enrollment is only available on macOS".into())
    }
}

/// Open the Login Items pane (no enroll attempt).
pub fn open_macos_login_items() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let url = login_items_deeplink();
        std::process::Command::new("open")
            .arg(url)
            .status()
            .map_err(|e| format!("open Login Items: {e}"))?;
        Ok("Opened System Settings → Login Items.".into())
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Still return the URL so the UI can show/copy it in cross-platform builds.
        Ok(format!("Login Items URL: {}", login_items_deeplink()))
    }
}
