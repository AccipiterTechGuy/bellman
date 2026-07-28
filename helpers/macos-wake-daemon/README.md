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

## Build

Standalone package (excluded from the root workspace; has its own `[workspace]`):

```sh
cd helpers/macos-wake-daemon
cargo test          # pure dispatch + foreign-tag refusal (any OS)
cargo build --release
# On macOS packaging: codesign + install into app bundle / LaunchDaemons
```

On non-macOS, `schedule` still updates the in-process ledger so protocol tests
pass; IOPMSchedulePowerEvent is cfg-gated to macOS. Client code-sig validation
rejects non-Bellman peers on macOS (test mode: `BELLMAN_WAKE_REJECT_CLIENTS=1`).
