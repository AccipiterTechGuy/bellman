# R2' Research — RTC wake-machine-from-sleep for Bellman (3-OS + capability detection)

Researcher: ① (kimi) — slot `1_kimi`
Date: 2026-07-27
Scope: Bellman v1 wake-from-sleep, strictly capability-gated, never crashing, never
prompting unexpectedly, never requiring elevation merely to run.

**One-paragraph verdict:** No mainstream OS lets an unprivileged app wake a sleeping
machine *unconditionally*. Linux: the RTC wakealarm is root-only by default; a one-time
udev rule at install makes it group-writable and is the clean path. Windows: wake timers
need **no** privilege, but are gated by the per-scheme "Allow wake timers" power setting
(disabled by default on battery, OEM-dependent on AC) and are unreliable on Modern
Standby (S0ix) laptops. macOS: **no unprivileged wake-scheduling API exists at all** —
`pmset schedule` and `IOPMSchedulePowerEvent` both require root; the realistic route is
a one-time-authorized root helper (SMAppService daemon) or degraded mode. Therefore
Bellman's design must be: **runtime OS detect → mechanism pick → probe → enabled /
degraded / disabled with a surfaced reason**, with the misfire-on-resume pass as the
always-on safety net. This matches the operator's "best-effort, capability-gated"
decision exactly.

---

## Q1 — Linux: rtcwake vs wakealarm vs systemd

### Mechanism survey with explicit picks

| Option | Unprivileged? | Verdict |
|---|---|---|
| `rtcwake` (util-linux) | No — needs write to `/dev/rtc0` (`0600 root:root`) and `/sys/power/state`; no setuid, no polkit integration shipped by any distro | **Do not shell out.** Also the wrong shape: rtcwake *suspends the machine itself*; Bellman needs to arm an alarm and keep running |
| Direct `/sys/class/rtc/rtc0/wakealarm` writes | No by default (`0644 root:root`, kernel default, no distro relaxes it); **yes** after a one-time install-time udev rule | **PICK — primary mechanism.** Three `std::fs` ops, no crate needed |
| systemd facilities | `systemd.timer WakeSystem=true` exists but is *system-manager only* ("this functionality requires privileges and is thus generally only available in the system service manager" — systemd.timer(5)); logind has **no** schedule-a-wake D-Bus API (verified against the full v257 method list); UPower has none; `systemd-inhibit` only delays/blocks suspend | Not usable unprivileged. Note for sharing analysis below: **`systemd-suspend-then-hibernate` programs the RTC alarm at every suspend** (systemd-sleep.conf(5)) |
| polkit | No `org.freedesktop.login1.*` action covers RTC wake — verified against the complete action list | Dead end |
| Custom root D-Bus service + polkit action | The "proper" way, but requires shipping a system daemon | v1.1+ optional; not v1 |

**Pick: direct `wakealarm` writes, gated by probe; installer offers (optionally,
skippable) a udev rule.** Canonical rule (sysfs attributes ignore `MODE=`/`GROUP=`,
so `RUN` is required):

```
# /etc/udev/rules.d/60-bellman-rtc.rules
ACTION=="add", SUBSYSTEM=="rtc", KERNEL=="rtc0", \
  RUN+="/bin/chgrp bellman-wake /sys/class/rtc/rtc0/wakealarm", \
  RUN+="/bin/chmod g+w /sys/class/rtc/rtc0/wakealarm"
```

If the user declines the rule (or installs via AppImage without it), Bellman runs in
degraded mode: timers fire via the misfire pass on resume. This is the same pattern
real projects use (Marathon Shell udev rule; MythTV/tvheadend use the cruder
sudoers-NOPASSWD-on-rtcwake variant).

### wakealarm semantics that must be coded exactly

- **Two-step arm is mandatory.** `wakealarm_store()` returns `-EBUSY` if you write a
  future time while an alarm is enabled. Always: write `0` (clear — harmless when
  empty), then write the epoch (kernel `drivers/rtc/sysfs.c`).
- **Format:** seconds since epoch; `+N` = N seconds from now. Reading returns the
  enabled alarm's epoch, or empty when none.
- **One-shot.** The alarm is consumed on firing; re-arm after every resume.
- **Existence = capability.** The attribute only exists when the RTC can wake the
  system (`device_can_wakeup() && RTC_FEATURE_ALARM`); absent ⇒ wake impossible via
  this RTC.
- **RTC timezone trap.** Dual-boot-with-Windows machines often keep the RTC in local
  time, which shifts `wakealarm` semantics. Probe: compare
  `/sys/class/rtc/rtc0/since_epoch` against `SystemTime::now()`; apply the offset.
- `CAP_SYS_TIME` is **not** needed (only `RTC_SET_TIME` checks it — kernel
  `drivers/rtc/dev.c`); the whole privilege question is filesystem permission.

### Sharing the single RTC alarm (no clobbering)

There is exactly one alarm; the kernel deliberately refuses a second writer with
`-EBUSY`. Writers in the wild: Bellman, system timers with `WakeSystem=true`,
**`systemd-suspend-then-hibernate`** (default suspend action on many battery systems —
Fedora, recent Ubuntu laptop profiles — re-arms the RTC at *every* suspend for its
hibernate timer), and BIOS "wake on RTC" features. No project implements
restore-on-exit (the alarm is one-shot anyway).

**Merge policy (pick):**
1. Read `wakealarm`. If an enabled alarm exists and is **earlier** than Bellman's next
   wake-enabled timer → keep it; set an in-process timer for that earlier instant and
   re-evaluate on resume (Bellman's own timer will be caught by the misfire pass).
2. If the existing alarm is later, or none → take over: clear, write our epoch.
3. Never "restore on exit"; recompute after every `PrepareForSleep(false)`.
4. Arm inside the `PrepareForSleep(true)` delay-inhibitor window (below) — this is
   *before* systemd-sleep's RTC programming in the suspend sequence, and merging (rule
   1) makes us safe even against suspend-then-hibernate's hibernate alarm: we honor
   any earlier alarm including its.

### Suspend vs hibernate

- S3 (`mem`/`deep`) and s2idle: RTC wake works; s2idle wakeup-source set is
  per-machine.
- S4 hibernate: works *if firmware keeps the RTC as a wake source* — vendor lottery.
- S5 poweroff: often works on desktops ("not officially supported by ACPI, but it
  usually works" — rtcwake(8)).
- Probe states via `/sys/power/state`, `/sys/power/mem_sleep`, `/sys/power/disk`.

### Pre-suspend hook (unprivileged, standard pattern)

logind delay inhibitor + `PrepareForSleep` (systemd.io/INHIBITOR_LOCKS):

1. `Inhibit("sleep", "Bellman", "Rearming RTC wake alarm", "delay")` → keep fd open.
2. On `PrepareForSleep(true)`: merge+arm wakealarm, **close the fd**. Budget:
   `InhibitDelayMaxUSec` (default 5 s) — a few syscalls only.
3. On `PrepareForSleep(false)`: re-take the lock, recompute next wake.

Watching the signal *without* the delay lock is racy — the lock is what guarantees the
handler completes. Default polkit grants `org.freedesktop.login1.inhibit-delay-sleep`
to active local sessions, non-interactively. Works identically on elogind
(Void/Artix/Gentoo). Caveat: only logind-initiated sleeps signal; a root
`echo mem > /sys/power/state` bypasses everything (acceptable).

---

## Q2 — Windows: waitable timers vs Task Scheduler

### Mechanism survey with explicit picks

| Option | Unprivileged? | Verdict |
|---|---|---|
| `CreateWaitableTimerEx` + `SetWaitableTimer(Ex)`, `fResume=TRUE` | **Yes — zero privileges** (no `SE_SYSTEM_TIME_NAME`; that privilege is for `SetSystemTime`). Docs: SetWaitableTimer needs only `TIMER_MODIFY_STATE` on the handle | **PICK — primary.** Bonus: if resume is unsupported the call succeeds but `GetLastError()==ERROR_NOT_SUPPORTED` — a built-in live probe |
| Task Scheduler task with `WakeToRun=true` | **Yes** — a standard user can register in their own folder via COM `RegisterTaskDefinition` + `TASK_LOGON_INTERACTIVE_TOKEN` (no password), or `schtasks /create /xml` (plain switches can't set WakeToRun). Only boot triggers need admin | **PICK — optional secondary.** Keeps the machine awake until the task completes (no unattended-idle race) and survives Bellman not running |
| Changing the power setting ourselves | **No** — writing scheme values (`powercfg /setacvalueindex`) requires admin | Offer a one-click *elevated helper* in UI instead |

**The gate: "Allow wake timers" (RTCWAKE).** Subgroup `238c9fa8-0aad-41ed-83f4-97be242c8f20`
(SUB_SLEEP) → setting `bd3b718a-0680-4d9d-8ab2-e1d2b4ac806d`. Values: 0=Disable,
1=Enable, 2=Important-only. Defaults **vary by OEM and build**: AC=1/DC=0 on some
Win11 25H2 machines, AC=0/DC=0 on some Asus laptops. **Probe, never assume.**
"Important wake timers" are an OS-internal class (Windows Update reboots); there is no
API for an app to mark its timer important — Bellman's timers and WakeToRun tasks are
ordinary wake timers, suppressed when the setting is 0 or 2.

**Implementation specifics:**
- Use `SetWaitableTimer` with **absolute UTC** FILETIME and `fResume=TRUE`. On Win8+
  *relative* due times freeze while asleep — absolute only. Keep `TolerableDelay`
  small/zero for wake-critical timers (timer coalescing can shift fire times).
- After an automatic wake the display stays off and a ≥2 min unattended idle timer
  starts: immediately call
  `SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_CONTINUOUS)` (optionally
  `ES_AWAYMODE_REQUIRED`) or the machine goes back to sleep mid-action. Clear when done.
- Same rules cover hibernate (S4) if `HiberFilePresent`.

**Modern Standby (S0ix) — treat as not guaranteed.** Microsoft documents `fResume`
wake only for S3/S4 ("For wakes from Modern Standby, refer to transitioning between
idle and active states"). On S0ix the always-on timer wakes the SoC but can't turn on
the display, desktop apps are throttled by the Desktop Activity Moderator, and Win11
24H2+ disables most wake sources when battery drain is detected. Community reports
conflict; per-machine empirical reality. Detection: `GetPwrCapabilities().AoAc == TRUE`
⇒ S0 machine ⇒ report "best-effort".

### Pre-suspend hook

`RegisterSuspendResumeNotification` (Win8+) with `DEVICE_NOTIFY_CALLBACK` — direct
callback, no window/message loop needed (message-only `HWND_MESSAGE` window is the
fallback). `PBT_APMSUSPEND` gives **~2 seconds** (still the documented budget on
Win10/11) — re-arm the wake timer and persist state inside the handler. Veto is
impossible: `PBT_APMQUERYSUSPEND` was removed in Vista.

### No-admin verdict per scenario

| Scenario | Wakes? |
|---|---|
| S3 desktop/laptop, AC, RTCWAKE=1 | Yes |
| Any machine, battery (DC default = Disable) | **No** (default configs) |
| RTCWAKE = "Important only" | **No** |
| S0ix laptop (most new laptops) | Not reliable / undocumented |
| Hibernate (S4), hiberfile enabled, RTCWAKE=1 | Yes |

---

## Q3 — macOS: pmset vs IOPMSchedulePowerEvent vs caffeinate

### Mechanism survey with explicit picks

| Option | Unprivileged? | Verdict |
|---|---|---|
| `pmset schedule wakeorpoweron "MM/dd/yy HH:mm:ss"` | **No — "pmset must be run as root in order to modify any settings"** (pmset(1)). Reads (`-g sched`, `-g cap`, `-g ps`) are unprivileged | Right primitive, wrong privilege: only usable through a root helper |
| `IOPMSchedulePowerEvent` (IOKit) | **No — Apple docs: "Must be called as root"**; returns `kIOReturnNotPrivileged` (0xE00002C1) otherwise. Not deprecated; no entitlement path | Same verdict. FFI only worth it inside the root helper (structured `IOPMCopyScheduledPowerEvents` enumeration without parsing pmset output) |
| `caffeinate` / `IOPMAssertionCreateWithName` | Yes | Keep-awake only — **cannot schedule a future wake**. Optional "keep Mac awake until next timer" mode; cannot override lid-close sleep |
| launchd `StartCalendarInterval` | Yes | Fires *at next natural wake* if slept through — i.e. it IS the misfire pass, not a wake mechanism |
| Root helper: `SMAppService` daemon (macOS 13+) | One-time user approval in System Settings → Login Items | **PICK for enabling wake.** Apple's current endorsed pattern for dmg apps (SMJobBless deprecated 13.0, AuthorizationExecuteWithPrivileges deprecated 10.7). Helper runs as root, owns `pmset schedule`/cancel with owner tag `com.bellman.*` |
| `osascript … with administrator privileges` | Interactive auth dialog per call (or once, to install a scoped sudoers drop-in — the StayAwake pattern) | **PICK as lightweight fallback** for macOS ≤12 or daemon-declined |

**No-elevation verdict for macOS: NO.** There is no public unprivileged API that
programs a future RTC wake. Default Bellman macOS install = degraded mode (misfire
pass catches timers at next natural wake, optionally piggybacking Power Nap dark
wakes — CCC's documented "run when the system next wakes" strategy), with a one-time
opt-in "Enable wake from sleep" button that registers the SMAppService helper.

**Semantics to code:**
- `pmset schedule` = one-time events (multiple allowed, per-owner tagged — always tag
  ours and only cancel ours); `pmset repeat` = a single recurring pair, overwritten
  per call — not our primitive. `pmset schedule cancel type "date"` args must match
  exactly; `cancelall` doesn't remove system-owned `com.apple.alarm.*` events.
- Power-on from **shutdown** requires AC power; FileVault blocks unattended boot.
  Scheduled **wake from sleep** works on battery in principle.
- **Apple Silicon: works but non-deterministic in the field** (verified cases of the
  machine waking then re-sleeping before automations ran; DssW, the leading commercial
  scheduling-app vendor, deliberately treats IOKit wake as an optional "Availability"
  feature). No authoritative source says it's categorically broken — ship the
  empirical self-test (below), don't encode arch-based assumptions.

### Pre-suspend hook

`IORegisterForSystemPower` (plain C FFI, IOKit.framework) + CFRunLoop:
`kIOMessageSystemWillSleep` → do pre-sleep work → `IOAllowPowerChange()`. Hard
deadline **30 s**, take <1 s. `NSWorkspace.willSleepNotification` (objc2) is the
simpler alternative but with no guaranteed grace — use IOKit as authoritative,
NSWorkspace as backup.

---

## Q4 — Capability detection design (probe + decision tree)

### Probe checklists (run at startup; re-run on resume, on config change, on failure)

**Linux**
- [ ] `/sys/class/rtc/rtc0/wakealarm` exists (else try `rtc1`; else → `NoRtcWakeHardware`)
- [ ] Writable by our euid/egid (`access(W_OK)`; else → `NoPermission(reason: "run installer or add udev rule")`)
- [ ] Read current armed-alarm epoch (merge input)
- [ ] `/sys/power/state` contains `mem` and/or `disk`; `/sys/power/mem_sleep` flavor (`[s2idle]` vs `[deep]`)
- [ ] RTC clock basis: `/sys/class/rtc/rtc0/since_epoch` vs `SystemTime::now()` → UTC/localtime offset
- [ ] System bus up, `org.freedesktop.login1` owned; `Inhibit("sleep",…,"delay")` succeeds → pre-suspend window guaranteed (else → `NoPreSuspendHook`, wake becomes arm-at-schedule-time best-effort)
- [ ] Ground truth: optional user-triggered self-test — arm +60 s, suspend, observe

**Windows**
- [ ] `GetPwrCapabilities`: `SystemS3`, `AoAc` (S0ix flag → `BestEffort`), `RtcWake` (lowest Sx the RTC wakes from), `HiberFilePresent`
- [ ] `PowerReadACValue` / `PowerReadDCValue` on RTCWAKE (`238c9fa8-…`/`bd3b718a-…`), active scheme: 1=ok, 0/2=blocked for that power source
- [ ] Live probe: arm far-future `SetWaitableTimer(fResume=TRUE)`, check `GetLastError() != ERROR_NOT_SUPPORTED`, cancel
- [ ] `GetSystemPowerStatus` + `RegisterPowerSettingNotification(GUID_ACDC_POWER_SOURCE)` → re-evaluate on plug/unplug; re-probe on every `PBT_APMRESUMEAUTOMATIC`

**macOS**
- [ ] `uname -m` / `sysctl hw.optional.arm64` → arch (AS ⇒ `BestEffort` caveat)
- [ ] `pmset -g cap` → feature bits; `pmset -g ps` → AC/battery (power-on-from-shutdown impossible on battery)
- [ ] Helper status: `SMAppService.daemon.status` registered? else `geteuid()==0`? else → `NoPermission(reason: "one-time Enable wake from sleep setup")`
- [ ] `pmset -g sched` (readable unprivileged) → existing events, verify our writes landed
- [ ] `IORegisterForSystemPower` registration succeeds → pre-sleep hook
- [ ] Ground truth: helper schedules a wake ~2–3 min out, machine sleeps, check `pmset -g log | grep -i wake` for RTC wake reason

### Decision tree (runtime OS detect → mechanism → probe → state)

```
std::env::consts::OS
├─ "linux"   → LinuxWake (wakealarm via std::fs; zbus logind inhibitor)
├─ "windows" → WindowsWake (windows-rs waitable timer [+ optional WakeToRun task])
├─ "macos"   → MacWake (root-helper pmset; IORegisterForSystemPower hook)
└─ _         → Unsupported — wake disabled, reason surfaced

per-OS probe() → WakeCapability enum:
  Enabled                     — armed path verified (all checks pass)
  BestEffort(reason)          — mechanism works but platform caveat
                                (Windows S0ix / DC-only-allowed, macOS Apple Silicon,
                                 Linux s2idle-with-unknown-sources)
  NoPermission(how_to_fix)    — mechanism exists, we lack rights
                                (Linux udev rule, macOS helper, Win elevated powercfg fix)
  NoHardware / UnsupportedOs  — cannot wake, period
  Disabled by user config     — wake_machine=false on all timers / global off

GUI: one status line — "Wake from sleep: on" | "on (battery excluded)" |
"off — needs one-time setup [Set up]" | "unsupported on this system".
Log: exactly one JSONL event per probe *transition* (not per probe).
```

Never both/neither invariant: if `Enabled|BestEffort`, the pre-suspend hook programs
the single next wake; if anything else, nothing is programmed and the misfire pass
owns catch-up. The states are exhaustive and mutually exclusive by construction.

---

## Q5 — Single-next-wake bridge

**Rule: Bellman programs exactly ONE wake — the earliest due timer among those with
`wake_machine=true` — re-programmed (a) inside the pre-suspend hook and (b) on any
schedule change that moves the next wake-enabled timer.**

Per-OS pre-suspend hook (all unprivileged):

| OS | Hook | Budget |
|---|---|---|
| Linux | logind `PrepareForSleep(true)` + delay inhibitor (zbus) | `InhibitDelayMaxUSec`, default 5 s |
| Windows | `RegisterSuspendResumeNotification` + `DEVICE_NOTIFY_CALLBACK` → `PBT_APMSUSPEND` | ~2 s documented |
| macOS | `IORegisterForSystemPower` → `kIOMessageSystemWillSleep` → `IOAllowPowerChange` | 30 s max |

**Cancel/rearm rules:**
- Timer added/edited/deleted → if next wake-enabled timer changed, rearm
  (Linux: clear+set wakealarm — mind EBUSY two-step and the merge policy vs earlier
  foreign alarms; Windows: `CancelWaitableTimer` + `SetWaitableTimer`; macOS: helper
  cancels only `com.bellman.*`-tagged events, schedules new).
- If the machine is *awake*, the armed wake is harmless — the in-process scheduler
  fires normally; the RTC alarm is consumed/cancelled on next rearm. Only the
  pre-suspend arming really matters; arming at schedule time too is the belt-and-braces
  pick (covers non-logind sleeps on Linux).
- After resume (`PrepareForSleep(false)` / `PBT_APMRESUMEAUTOMATIC` /
  `kIOMessageSystemHasPoweredOn`): cancel any stale armed wake, run the misfire pass,
  rearm for the next wake-enabled timer.

**Never both / never neither (interaction with the misfire pass):** the wake alarm and
the in-process timer target the same instant. Whichever path runs first claims the Run
via the store's at-least-once run-claim (BUILD_PLAN rule 5) — the loser sees the claim
and stands down. The misfire pass runs on *every* detected resume regardless of wake
capability state, so a failed/absent wake degrades to late-fire per the per-timer
misfire policy. The claim-before-act mechanism already in the build plan makes the
double-fire impossible; the always-run misfire pass makes the neither case impossible.

---

## Q6 — Rust crates / FFI picks

| OS | Dependency | Use |
|---|---|---|
| Linux | `zbus` 5.x (async, `zbus::blocking` available) | logind `Inhibit` + `PrepareForSleep`. wakealarm itself: **std::fs only — no crate exists or is needed** (crates.io `rtc` hits are WebRTC/embedded) |
| Windows | `windows` (windows-rs), features `Win32_System_Threading`, `Win32_System_Power`, `Win32_Foundation` (+ `Win32_System_TaskScheduler`, `Win32_System_Com`, `Win32_System_Variant`, `Win32_UI_WindowsAndMessaging` only if the secondary task/message-window paths ship) | All APIs verified present in windows-rs metadata. No mature wake crate exists (`keepawake-rs` only *prevents* sleep; `sleepwake` is a CLI, usable as reference) — thin ~100-line wrapper |
| macOS | `core-foundation` (+`-sys`), `objc2` + `objc2-app-kit` (NSWorkspace), hand-declared `extern "C"` IOKit symbols with `#[link(name="IOKit", kind="framework")]` (`io-kit-sys` exists but doesn't wrap IOPMLib — declare `IORegisterForSystemPower`, `IOAllowPowerChange`, `IOPMSchedulePowerEvent`, `IOPMCopyScheduledPowerEvents` yourself) | Shelling out to `pmset` from the root helper is the pragmatic pick — pmset is a thin wrapper over the same IOKit calls and is version-resilient; FFI only inside the helper for structured event enumeration |

Shelling out vs FFI overall: shell out only where the privileged helper already exists
(macOS `pmset`); direct syscalls/FFI where the mechanism is unprivileged (Linux sysfs,
Windows APIs). Never shell out to `rtcwake` (root-only, wrong shape — it suspends).

### API sketch — `bellman-core::platform::wake`

```rust
/// One per OS, selected at startup by cfg!(target_os).
pub trait WakeBackend: Send + Sync {
    /// Cheap, side-effect-free-ish probe (Windows live-probe arms+cancels a far
    /// timer; Linux does W_OK access checks; macOS checks helper status).
    /// Re-run on: startup, resume, power-source change, schedule-arm failure.
    fn capability(&self) -> WakeCapability;

    /// Program the single next machine wake at `utc`. Implementations must:
    /// merge with/honor earlier foreign alarms (Linux), use absolute UTC
    /// (Windows), tag events com.bellman.* (macOS helper).
    fn program_wake(&self, utc: SystemTime) -> Result<(), WakeError>;

    /// Cancel our armed wake (not foreign ones). No-op-safe.
    fn cancel_wake(&self) -> Result<(), WakeError>;

    /// Register the pre-suspend callback (rearm point) and post-resume
    /// callback (misfire pass + rearm). Returns error if hooks unavailable
    /// (degrades Linux to arm-at-schedule-time).
    fn on_power_events(
        &self,
        will_sleep: Box<dyn Fn() + Send>,   // keep under OS budget: 2–5 s
        did_wake:   Box<dyn Fn() + Send>,
    ) -> Result<(), WakeError>;
}

pub enum WakeCapability {
    Enabled,
    BestEffort(&'static str),     // "Modern Standby laptop", "battery excluded", "Apple Silicon"
    NoPermission(String),         // human-readable one-time fix (udev rule / helper / elevated powercfg)
    NoHardware,
    UnsupportedOs,
}

pub struct WakeBridge<B: WakeBackend> { /* holds backend, next-wake cache */ }
impl<B: WakeBackend> WakeBridge<B> {
    /// Called by scheduler on any timers change: recompute earliest
    /// wake_machine=true timer; rearm if changed. Single source of truth.
    pub fn on_schedule_changed(&mut self, next: Option<SystemTime>);
    /// Called from will_sleep hook: program_wake(cached_next). <1 syscalls-heavy.
    pub fn arm_for_suspend(&mut self);
    /// Called from did_wake hook: cancel stale, hand off to misfire pass.
    pub fn on_resume(&mut self);
}
```

**Per-timer `wake_machine` flag semantics:** default `false` (v1 safe default — waking
a machine is a power/privacy-relevant act). Only timers with `wake_machine=true`
participate in the next-wake computation. If the capability probe is not
`Enabled|BestEffort`, the flag is accepted and stored but surfaced in GUI as
"wake unavailable — will fire on resume instead" and has no system effect. No prompt,
no elevation request, ever, at runtime — the only elevation paths are explicit
user-initiated one-time setup buttons.

---

## Risks (ranked)

1. **Windows battery default (RTCWAKE DC=Disable):** the common laptop-on-battery case
   silently won't wake. Mitigation: per-power-source probing, honest UI, elevated-fix
   button. Do NOT auto-elevate.
2. **S0ix Modern Standby laptops (most new Windows laptops):** wake timers unreliable/
   undocumented; 24H2 battery-drain protection can kill wake sources at runtime.
   Mitigation: `AoAc` detection → `BestEffort`, misfire pass is the real guarantee.
3. **Linux shared-RTC clobbering:** suspend-then-hibernate (default on many battery
   distros) re-arms the RTC at every suspend; BIOS wake-on-RTC and `WakeSystem=true`
   timers collide. Mitigation: merge policy (honor earlier alarm), arm in the delay-lock
   window, re-evaluate every resume.
4. **macOS has no unprivileged path at all:** feature is dark until a one-time root
   helper is approved; Apple Silicon wake is non-deterministic in the field. Mitigation:
   degraded-by-default, empirical self-test, DssW-style "availability" framing.
5. **AC-vs-battery policy everywhere:** macOS power-on-from-shutdown needs AC; Windows
   DC default off; some laptops ignore RTC wake on battery. Probe and surface.
6. **Windows unattended re-sleep:** automatic wakes start a ≥2 min idle timer with the
   display off — without `SetThreadExecutionState` the machine sleeps mid-action.
7. **Pre-suspend budget overruns** (2 s Win / 5 s Linux / 30 s macOS): handlers must be
   a few syscalls; heavy work belongs post-resume.
8. **RTC-in-localtime dual-boot skew** (Linux): probe `since_epoch` offset or wake at
   the wrong hour.
9. **udev rule is sticky and system-wide** (Linux): any process in the group can wake
   the machine; document it in the installer.
10. **Hibernate conversion:** Windows "hibernate after N" and Linux suspend-then-hibernate
    can turn an S3 scenario into S4 mid-sleep — both handled by the same armed alarm
    where firmware supports S4 wake, which is a vendor lottery.

---

## Citations (key primary sources)

**Linux**
- Kernel `drivers/rtc/sysfs.c` (wakealarm 0644, EBUSY, one-shot, UTC comment) — https://raw.githubusercontent.com/torvalds/linux/master/drivers/rtc/sysfs.c
- Kernel ABI sysfs-class-rtc — https://raw.githubusercontent.com/torvalds/linux/master/Documentation/ABI/testing/sysfs-class-rtc
- Kernel `drivers/rtc/dev.c` (CAP_SYS_TIME scope) — https://raw.githubusercontent.com/torvalds/linux/master/drivers/rtc/dev.c
- Kernel sleep-states doc — https://www.kernel.org/doc/html/latest/admin-guide/pm/sleep-states.html
- org.freedesktop.login1(5) (full method + polkit action list) — https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html
- systemd Inhibitor Locks — https://systemd.io/INHIBITOR_LOCKS/
- systemd.timer(5) (WakeSystem= system-manager-only) — https://www.freedesktop.org/software/systemd/man/latest/systemd.timer.html
- systemd-sleep.conf(5) (suspend-then-hibernate programs RTC) — https://www.freedesktop.org/software/systemd/man/latest/systemd-sleep.conf.html
- rtcwake(8) — https://www.mankier.com/8/rtcwake
- UPower D-Bus (no wake API) — https://upower.freedesktop.org/docs/UPower.html
- udev-rule precedent — https://github.com/patrickjquinn/Marathon-Shell ; sudoers precedent — https://help.ubuntu.com/community/MythTV/Install/WhatNext/ACPIWake
- zbus — https://crates.io/crates/zbus

**Windows**
- System Wake-up Events (S3/S4 scope, unattended idle, ERROR_NOT_SUPPORTED probe) — https://learn.microsoft.com/en-us/windows/win32/power/system-wake-up-events
- SetWaitableTimer / SetWaitableTimerEx (no privilege; coalescing; Win8 relative-freeze) — https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-setwaitabletimer , https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-setwaitabletimerex
- PBT_APMSUSPEND (~2 s) — https://learn.microsoft.com/en-us/windows/win32/power/pbt-apmsuspend
- RegisterSuspendResumeNotification — https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-registersuspendresumenotification-registersuspendresumenotification
- Modern Standby Wake Sources (always-on timer class; 24H2 drain protection) — https://learn.microsoft.com/en-us/windows-hardware/design/device-experiences/modern-standby-wake-sources
- Desktop Activity Moderator — https://learn.microsoft.com/en-us/windows/win32/w8cookbook/desktop-activity-moderator
- SYSTEM_POWER_CAPABILITIES (AoAc, RtcWake) — https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-system_power_capabilities
- PowerReadACValue — https://learn.microsoft.com/en-us/windows/win32/api/powersetting/nf-powersetting-powerreadacvalue
- ITaskSettings WakeToRun (keeps machine awake until task completes) — https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-itasksettings-get_waketorun
- TaskFolder.RegisterTaskDefinition (only boot triggers need admin) — https://learn.microsoft.com/en-us/windows/win32/taskschd/taskfolder-registertaskdefinition
- RTCWAKE defaults vary (factory dumps) — https://forums.guru3d.com/threads/windows-power-plan-settings-explorer-utility.416058/post-6372288 , https://github.com/seerge/g-helper/issues/5773
- windows-rs module refs — https://microsoft.github.io/windows-docs-rs/

**macOS**
- pmset(1) ("must be run as root to modify"; schedule/repeat/cancel semantics) — https://keith.github.io/xcode-man-pages/pmset.1.html
- IOPMSchedulePowerEvent ("Must be called as root") — https://developer.apple.com/documentation/iokit/1557076-iopmschedulepowerevent
- IORegisterForSystemPower (30 s deadline) — https://developer.apple.com/documentation/iokit/1557114-ioregisterforsystempower
- IOPMLib.h (no deprecation on schedule APIs) — https://opensource.apple.com/source/IOKitUser/IOKitUser-647.6/pwr_mgt.subproj/IOPMLib.h.auto.html
- Apple Dev Forums: privilege escalation patterns (SMAppService current) — https://developer.apple.com/forums/thread/708765
- CCC v6 scheduling (AC requirement, dark-wake piggyback) — https://bombich.com/en/kb/ccc/6/advanced-scheduling-options
- DssW Power Manager (wake as optional "Availability") — https://www.dssw.co.uk/blog/2023-04-21-power-manager-and-pmset/
- Apple Silicon field flakiness — https://talk.automators.fm/t/how-to-wake-mac-for-automation/16263
- kIOReturnNotPrivileged + structured event enumeration — https://dennisbabkin.com/blog/?t=macos-programming-shutdown-notifications-xcode-build-schemes-diagnose-memory-corruption-crashes
- StayAwake sudoers pattern — https://github.com/TY-teo/StayAwake
- io-kit-sys / core-foundation / objc2-app-kit — https://crates.io/crates/io-kit-sys , https://crates.io/crates/core-foundation , https://crates.io/crates/objc2-app-kit

**Confidence caveats:** (1) "Important wake timers" classification has no formal MS
documentation — rests on community consensus + absence of any marking API;
(2) S0ix wake-timer behavior is undocumented and reports conflict — flagged empirical;
(3) RTCWAKE factory defaults demonstrably vary by OEM/build — runtime probing is the
only safe strategy; (4) Apple-Silicon scheduled-wake reliability is "works but
non-deterministic" — ship the self-test, don't hard-code arch assumptions.
