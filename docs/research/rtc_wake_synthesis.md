# Synthesis — RTC wake-machine-from-sleep for Bellman (R1 pair)

Synthesizer: ① (kimi, slot 1_kimi) · Date: 2026-07-27
Inputs: `1_kimi/research.md` (R1), `2_claude/research.md` (R2 — includes live probes
on a real Linux desktop, systemd 255).

## 0. Final verdicts (merged)

| OS | No-elevation wake? | Final pick | Fallback / degraded |
|---|---|---|---|
| Linux | **YES on systemd ≥ 254 local desktop sessions** (ambient CAP_WAKE_ALARM); partial elsewhere | `timerfd_create(CLOCK_REALTIME_ALARM)` — kernel-multiplexed, clobber-immune | cooperative `wakealarm` sysfs write if admin made it writable (udev rule); else Disabled |
| Windows | **YES — policy-gated, not privilege-gated** | `CreateWaitableTimerExW` + `SetWaitableTimer(fResume=TRUE)`, absolute UTC | Task Scheduler `WakeToRun` task — **deferred to v1.x** |
| macOS | **NO — hard** (powerd enforces euid 0 server-side) | Optional SMAppService root daemon (one-time Login-Items approval) owning `IOPMSchedulePowerEvent` | No daemon ⇒ Disabled + misfire pass; optional "hold sleep" assertion mode |

Both researchers independently agree on the core architecture: the RTC/wake event
only *resumes the machine*; Bellman's normal scheduler loop + misfire-on-resume pass
does the actual firing. Capability is probed at runtime in-process, never inferred
from OS/distro versions (R2 proved the necessity live: a systemd 255 local-session
box still returned EPERM from a daemon-descended launch path).

## 1. Disagreement table

| # | Topic | R1 (kimi) | R2 (claude) | Resolution |
|---|---|---|---|---|
| 1 | **Linux primary mechanism** | `/sys/class/rtc/rtc0/wakealarm` + install-time udev rule; verdict "no unprivileged path by default" | `timerfd_create(CLOCK_REALTIME_ALARM)`; ambient CAP_WAKE_ALARM from pam_systemd since **systemd v254** (PR #26548) makes it work unprivileged on Ubuntu 24.04+/Fedora 39+/Arch | **Adopt R2.** R2's finding is newer and decisive: the kernel alarmtimer subsystem multiplexes all outstanding alarm timers onto the hardware RTC at suspend entry, so the single-alarm clobbering problem *disappears* on the primary path. sysfs+udev demoted to fallback for pre-254 distros. R1 simply missed the v254 change |
| 2 | **Capability inference** | Probe (files, W_OK, logind) | Probe, plus live evidence that ambient caps are **launch-lineage dependent** (XDG autostart keeps them; `systemd --user` services and daemon-descended shells lose them) | **Adopt R2, hard rule:** the probe is a real `timerfd_create` call in the actual Bellman process at startup. Never conclude from systemd version. If autostart ever moves to a user unit, it needs `AmbientCapabilities=CAP_WAKE_ALARM` |
| 3 | **sysfs clobbering mitigation** (fallback path) | Non-destructive merge: keep any earlier foreign alarm; take over only if ours is earlier; never restore-on-exit | Same overwrite-if-earlier rule, **plus restore the displaced foreign alarm after resume** | **R1's base rule + R2's restore as refinement.** Both read-before-write and only displace later alarms; restoring the displaced alarm post-resume is cheap and polite. Note: on the timerfd primary path this is moot (kernel multiplexes) |
| 4 | **suspend-then-hibernate clobbering** | Flagged: `systemd-suspend-then-hibernate` re-arms the RTC at every suspend (default on many battery distros) | Not mentioned | **Keep R1's risk**, scoped: it only threatens the sysfs fallback path; the timerfd path is immune (alarmtimer is the thing systemd-sleep programs through) |
| 5 | **Windows Task Scheduler fallback** | Ship as optional secondary in v1 (survives app death, keeps machine awake until task completes) | Defer to v1.x — duplicates what the misfire pass already guarantees; zero external state in v1 | **Adopt R2 (v1.x).** The misfire pass covers app-dead functionally; hibernate coverage isn't guaranteed anyway (§3 risk 6) |
| 6 | **S0ix Modern Standby classification** | `BestEffort` — "not guaranteed", treat as unreliable | `Enabled{modern_standby: true}` — a throttled **screen-off execution window is enough to run wake actions**; `ERROR_NOT_SUPPORTED` probe catches hard-fail machines | **Adopt R2's framing with R1's honesty:** Enabled-with-caveat; GUI line says "wake runs screen-off (Modern Standby)". R2 is right that Bellman's actions (launch cmd, slot JSON, notification) don't need a display |
| 7 | **Wake instant vs fire instant** | Alarm and in-process timer target the same instant; store run-claim dedups | Program `wake_utc − 45 s`; the wake event **never fires actions** — after resume the normal loop fires at true due time (late ⇒ misfire pass) | **Adopt R2.** The −45 s slack (covers macOS 30-s round-down, resume latency, clock settle) + "RTC only resumes" removes the double-fire class *by construction*, cleaner than claim-dedup at the same instant |
| 8 | **macOS pre-suspend hook location** | `IORegisterForSystemPower` in the app | In the **daemon**; GUI app uses `NSWorkspace.willSleep/didWake` for the misfire pass | **Adopt R2.** The daemon owns power management; also R2's point that scheduled events persist on disk ⇒ program eagerly at schedule-change, the sleep hook is only a refresh |
| 9 | **macOS helper fallback** | osascript-with-admin / scoped sudoers drop-in as fallbacks for macOS ≤12 | SMAppService only (13+); SMJobBless deprecated | **Merge:** SMAppService primary; R1's osascript/sudoers route documented as optional pre-13 fallback, v1.x at earliest |
| 10 | **Linux crates** | zbus + std::fs only | **rustix** (`timerfd_create(TimerfdClockId::RealtimeAlarm)`) + zbus + std::fs | **Adopt R2** (rustix added) |
| 11 | **Enum shape** | `Enabled / BestEffort(reason) / NoPermission / NoHardware / UnsupportedOs` | `Enabled{mechanism, modern_standby} / Disabled{reason}` with rich `DisabledReason` | **Merge:** R2's two-variant shape + a `caveats` list on Enabled carrying R1's BestEffort cases (Modern Standby screen-off, Apple Silicon non-determinism, battery-rail exclusion) |

Facts each report uniquely contributed (all carried into the final design):
- **R1 only:** RTC-kept-in-localtime dual-boot skew (probe `since_epoch` vs wall clock);
  `systemd.timer WakeSystem=` is system-manager-only; suspend-then-hibernate RTC
  re-arming; Windows WakeToRun keeps machine awake until task completes; unattended
  re-sleep needs `SetThreadExecutionState(ES_SYSTEM_REQUIRED)`; macOS FileVault/AC
  constraints on power-on-from-shutdown; osascript/sudoers fallback routes.
- **R2 only:** CAP_WAKE_ALARM/timerfd mechanism + live EPERM evidence; macOS wake
  times **round down to 30 s**; DarkWake guidance (take a power assertion at the due
  moment or the box re-sleeps mid-action; never treat "process running" as "due time
  arrived"); Windows RTC drift over multi-day sleeps (10–50 min late) ⇒ re-arm every
  resume; `RtcWake >= PowerSystemHibernate` as S4-wake predictor; XPC client
  code-signature validation for the daemon; RESUMEAUTOMATIC-only on auto-wakes;
  `powercfg /waketimers` needs elevation (don't build probes on it).

## 2. Final per-OS design

### Linux
1. **Probe (ordered):** `/sys/class/rtc/rtc0` exists → `device/power/wakeup == enabled`
   → **`timerfd_create(CLOCK_REALTIME_ALARM, TFD_CLOEXEC)` succeeds?** (keep the fd) ⇒
   `Enabled(LinuxAlarmTimerfd)`; EPERM → `access(wakealarm, W_OK)` ⇒
   `Enabled(LinuxWakealarmSysfs)` (cooperative protocol: read current alarm, displace
   only later ones, restore displaced on resume, two-step clear+set vs EBUSY, one-shot)
   ; else `Disabled(NoPermission{hint: systemd ≥254 local session /
   AmbientCapabilities=CAP_WAKE_ALARM / setcap / udev rule})`.
2. Informational: `/sys/power/state`, `/sys/power/mem_sleep`, RTC-localtime offset via
   `since_epoch`, login1 `CanSuspend`.
3. **Pre-suspend:** login1 delay inhibitor + `PrepareForSleep` (zbus), ≤5 s budget
   (`InhibitDelayMaxUSec` default, R2 verified live). On the timerfd path the kernel
   programs the RTC at suspend entry itself — the inhibitor mainly serves the sysfs
   fallback and provides the clean resume signal.
4. Suspend (s2idle/S3): works. Hibernate (S4): firmware lottery — **not guaranteed**,
   misfire pass covers. rtcwake: rejected (root, wrong shape, prompt risk).
   `WakeSystem=` user timers: rejected (officially "requires privileges", adds a
   moving part outside our process).

### Windows
1. **Probe:** `CallNtPowerInformation(SystemPowerCapabilities)` → `AoAc` (S0ix),
   `SystemS3/S4`, `RtcWake`; `PowerGetActiveScheme` + `PowerReadACValue/DCValue` on
   `GUID_SLEEP_SUBGROUP/GUID_ALLOW_RTC_WAKE` (bd3b718a-…) + live
   `GetSystemPowerStatus().ACLineStatus`: value 0 or 2 ⇒
   `Disabled(WakeTimersDisabledByPolicy{rail})` (app timers aren't "important");
   1 ⇒ live arm test: far-future `SetWaitableTimer(fResume=TRUE)`,
   `GetLastError()==ERROR_NOT_SUPPORTED` ⇒ `Disabled(ResumeTimersUnsupported)`, clean ⇒
   `Enabled(WindowsWaitableTimer, caveats: modern_standby if AoAc)`.
2. Re-probe on `PBT_APMPOWERSTATUSCHANGE` (AC↔DC flips the gate) and every resume.
3. **Arm:** absolute UTC FILETIME only (relative freezes during sleep, Win8+); no
   HIGH_RESOLUTION flag; process must stay alive holding the timer. After auto-wake
   (RESUMEAUTOMATIC, screen off, ~2 min unattended budget): immediately
   `SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_CONTINUOUS)`, clear when done.
4. RegisterSuspendResumeNotification(DEVICE_NOTIFY_CALLBACK) → PBT_APMSUSPEND (~2 s,
   bookkeeping) / PBT_APMRESUMEAUTOMATIC (misfire + rearm).
5. Elevated-fix button when policy-blocked: `powercfg /setacvalueindex SCHEME_CURRENT
   SUB_SLEEP RTCWAKE 1` — user-initiated only.

### macOS
1. **Probe:** `SMAppService.daemon.status` — `.enabled` ⇒ through-daemon sentinel
   schedule/cancel round-trip ⇒ `Enabled(MacPmDaemon)` (+ Apple Silicon caveat);
   `.requiresApproval` ⇒ `Disabled(HelperAwaitingApproval)` + GUI button deep-linking
   to System Settings → Login Items; `.notRegistered` ⇒ `Disabled(HelperNotInstalled)`
   + one-click enroll.
2. **Daemon:** tiny XPC surface (`schedule_wake(date)`, `cancel_my_wakes`), validates
   client code signature, calls `IOPMSchedulePowerEvent(kIOPMAutoWake,
   "com.bellman.wake")`. One-shot events only, own-tag only, cancel by exact arg match.
   **Never `pmset repeat`** (single global pair) and **never `cancelall`** (wipes every
   app's events). `IORegisterForSystemPower` lives in the daemon; GUI observes
   NSWorkspace for the misfire pass.
3. Facts honored in the bridge: wake times round down to 30 s (slack absorbs);
   DarkWake ⇒ take `IOPMAssertionCreateWithName` at the due moment; verify arms via
   unprivileged `IOPMCopyScheduledPowerEvents`; power-on-from-shutdown needs AC and is
   blocked by FileVault (wake-from-sleep is unaffected — and wake is all Bellman needs).
4. Apple Silicon: works but non-deterministic in the field (DssW treats it as an
   "Availability" nicety) ⇒ caveat + optional empirical self-test.

## 3. Final capability model + decision tree

```
os = compile-time cfg ──► probe(os) ──► WakeCapability
   ├─ Enabled { mechanism, caveats: Vec<Caveat> }
   │    mechanism: LinuxAlarmTimerfd | LinuxWakealarmSysfs
   │              | WindowsWaitableTimer | MacPmDaemon
   │    caveats:  ModernStandbyScreenOff | AppleSiliconBestEffort
   │              | BatteryRailExcluded | HibernateNotGuaranteed
   └─ Disabled { reason }   // R2's DisabledReason set, every variant
                            // carries a user sentence + fix hint

program_wake(t): Enabled ? arm(t − 45 s) : silent skip
arm failure:     re-probe once → flip state on change, log transition
GUI status:      "Wake from sleep: ON via <mech> (<caveat note>)" |
                 "OFF — <reason sentence>" (+ fix-it button where actionable)
JSONL:           one wake_capability event at startup + one per transition
```

Probe triggers (both reports agree): startup, every resume, schedule-mutation/arm
failure, and power-source change (Windows). All probes are prompt-free, root-free,
side-effect-free or self-canceling, < a few ms.

## 4. Final single-next-wake bridge

- Election: `next = min(next_due(t) for t where t.wake_machine && t.enabled)`.
- One `rearm()` (cancel-then-program, idempotent) called from: store mutations that
  move the winner, pre-suspend hook (refresh/last chance), resume + app start (also
  absorbs Windows RTC drift).
- **Arm at `wake_utc − 45 s`; the wake event never fires actions.** Post-resume the
  normal loop fires at true due time; resumed-late ⇒ misfire pass. Never-both and
  never-neither hold by construction (R2's formulation; R1's run-claim remains as the
  store-level backstop).
- `cancel_wake()` no-op-safe; macOS cancel replays exact (time, id, type) — stored
  alongside `armed`.
- Per-timer `wake_machine: bool`, default **false**; greyed in GUI with the status-line
  reason when capability is Disabled. No runtime elevation, no unexpected prompts —
  elevation only via explicit user-initiated setup buttons.

## 5. Final crate list

| OS | Crates | Notes |
|---|---|---|
| Linux | `rustix` (timerfd), `zbus` 5.x (login1 Inhibit/PrepareForSleep), `std::fs` (sysfs fallback) | zbus already in Tauri's Linux dep tree; hand-write the 3-method proxy |
| Windows | `windows` — `Win32_System_Threading`, `Win32_System_Power`, `Win32_Foundation`, `Win32_UI_WindowsAndMessaging`; `Win32_System_TaskScheduler` only if the v1.x fallback ships | ~100-line wrapper; no existing wake crate |
| macOS | `objc2` + `objc2-app-kit` (NSWorkspace), `core-foundation`, ~6 hand `extern "C"` IOKit decls (io-kit-sys/apple-sys don't bind the power subset); SMAppService via objc2-service-management or a small Swift shim | Daemon calls the C API directly, not pmset |

Shell-out verdict (unanimous): never rtcwake/pmset from the app. `pmset -g sched`
parsing is a debug aid only; `IOPMCopyScheduledPowerEvents` is the API answer.

## 6. Consolidated risks (merged, ranked)

1. **Windows battery default** (RTCWAKE DC=Disable) + AC/DC rail flips — per-rail
   probe, re-probe on power-source change, honest UI, elevated-fix button.
2. **S0ix laptops** — screen-off execution window (fine for actions, say so in GUI);
   some hard-fail (probe catches). macOS DarkWake analogous ⇒ power assertion at due
   time.
3. **Linux capability is per-process** (ambient cap lineage; live-verified EPERM on
   systemd 255) — in-process probe only; XDG autostart keeps the cap; document the
   user-unit AmbientCapabilities requirement.
4. **sysfs fallback clobbering** (shared single alarm; suspend-then-hibernate re-arms
   at every suspend; BIOS wake-on-RTC) — cooperative protocol; primary timerfd path
   immune.
5. **macOS helper refusal** — permanent `HelperAwaitingApproval`; feature must read as
   optional enhancement, not broken state. Plus macOS shared-event-list discipline
   (own-tag one-shots only).
6. **Hibernate not guaranteed anywhere** (firmware-dependent; waitable timers die
   under S4/fast-startup) — contract: wake covers suspend/sleep; hibernate relies on
   the misfire pass. Document it.
7. **Process-lifetime coupling** (timerfd + waitable timer die with Bellman) —
   acceptable by design; tray-quit logs "wake disarmed".
8. **Windows RTC drift** over multi-day sleeps (10–50 min late) — re-arm every resume,
   slack + misfire pass absorb.
9. **RTC-in-localtime skew** (Linux dual-boot) — probe `since_epoch` offset (sysfs
   path).
10. **Pre-suspend budget overruns** (2 s Win / 5 s Linux / 30 s macOS) — handlers are
    a few syscalls; heavy work post-resume.
11. **Apple Silicon non-determinism + near-empty-battery deferral** — caveat +
    optional self-test; document.

## 7. Confidence notes

- R2's CAP_WAKE_ALARM claim is backed by the systemd v254 NEWS/PR and a live EPERM
  probe; high confidence. The synthesis's Linux verdict ("mostly YES unprivileged on
  current distros") rests on it and on runtime probing rather than version checks.
- Windows "important wake timers" classification and S0ix behavior remain
  under-documented by Microsoft — both reports flag this; the design's answer is the
  live arm probe + misfire safety net.
- Apple Silicon wake reliability: field reports conflict; the empirical self-test is
  the designed answer, not an arch-based assumption.
