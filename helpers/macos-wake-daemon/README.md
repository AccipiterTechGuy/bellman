# macos-wake-daemon

Tiny SMAppService root daemon for Bellman P7 (macOS 13+).

## Contract (synthesis §2-macOS)

- XPC / unix-socket surface:
  - `schedule_wake(epoch, tag="com.bellman.wake")`
  - `cancel_my_wakes(tag="com.bellman.wake")`
- Client **code-signature validation** (only the signed Bellman app).
- Calls `IOPMSchedulePowerEvent(kIOPMAutoWake, "com.bellman.wake")`.
- **Own-tag one-shots only.** Never `pmset repeat`. Never `cancelall`.
- `IORegisterForSystemPower` lives here (pre-suspend refresh).
- Launchd label / SMAppService id: `com.bellman.wake-daemon`.
- Socket path: `/var/run/bellman-wake.sock` (root-owned).

## Packaging

Bundled into the signed+notarized dmg (P6 machinery). Enrollment is
wizard/Settings-driven via SMAppService — never installer-forced.
Declining Login Items approval leaves the feature as
`Disabled(HelperAwaitingApproval)` (optional enhancement, never broken).

## Build (on macOS)

```sh
cd helpers/macos-wake-daemon
cargo build --release
# codesign + install into app bundle Frameworks / LaunchDaemons
```

This helper is a separate binary from the Tauri app. On non-macOS hosts the
crate is not built; unit tests for the decision tree live in `bellman-core`.
