# P7 packaging amendment — wake daemon + wizard

## All three builds

- First-run wizard ships in deb / AppImage / NSIS / MSI / dmg (webview +
  `wizard_*` Tauri commands). Wizard-driven steps are the **only** elevation
  surfaces (Windows powercfg UAC, macOS Login Items approval, Linux manual udev).

## macOS dmg

1. Build `helpers/macos-wake-daemon` → `bellman-wake-daemon` binary.
2. Codesign with the same identity as the app.
3. Install into the app bundle as an SMAppService daemon:
   - Launchd label: `com.bellman.wake-daemon`
   - Bundle id / service id matches `platform::wake::macos::daemon_service_id()`.
4. Enrollment is **not** forced at install time. Wizard "Yes" / Settings
   "Enroll" calls SMAppService register → user approves under System Settings →
   Login Items. Decline ⇒ `Disabled(HelperAwaitingApproval)` (optional, not broken).

## Linux

No extra package payload. Ambient CAP_WAKE_ALARM on systemd ≥254 desktop
sessions; udev rule snippet is copy-only from Settings.

## Windows

No extra package payload. Waitable timers are in-process; policy fix-it is
user-initiated elevated `powercfg`.
