# RTC wake-machine-from-sleep for Bellman — research report (rs#2 / claude)

Date: 2026-07-27 · Stack context: Tauri v2 shell + Rust core (locked per
`docs/BELLMAN_BUILD_PLAN.md`), Bellman is a resident tray app.
Method: three parallel citation-backed web sweeps (one per OS) + live probes on a
real Linux desktop (Linux Mint 22.3, systemd 255, kernel 7.0, util-linux 2.39.3).

## 0. TL;DR — no-elevation verdicts

| OS | No-elevation wake possible? | Pick (primary) | Fallback |
|---|---|---|---|
| Linux | **PARTIAL → mostly YES** on systemd ≥ 254 local sessions (ambient CAP_WAKE_ALARM) | `timerfd_create(CLOCK_REALTIME_ALARM)` + absolute `timerfd_settime` — kernel-multiplexed, clobber-free | cooperative `/sys/class/rtc/rtc0/wakealarm` write **iff** admin made it writable; else disabled |
| Windows | **YES** (policy-gated, not privilege-gated) | in-process `CreateWaitableTimerExW` + `SetWaitableTimer(fResume=TRUE)`, absolute UTC due time | per-user Task Scheduler task with `WakeToRun` (survives app death + hibernate) |
| macOS | **NO** (powerd enforces euid 0 / Apple-private entitlement server-side) | **optional** SMAppService root daemon (one-time Login-Items approval) owning `IOPMSchedulePowerEvent` | no daemon → disabled + misfire pass; optional "hold sleep until fire" assertion mode |

On every OS the wake event only *resumes the machine*; Bellman's normal scheduler
loop + misfire-on-resume pass does the actual firing. That single rule removes the
double-fire class of bugs by construction (§5).

---

## 1. Linux (Q1)

### Mechanism comparison — explicit pick: alarm-clock timerfd, NOT rtcwake, NOT raw sysfs

**`timerfd_create(CLOCK_REALTIME_ALARM)` (pick).** Requires CAP_WAKE_ALARM, else
EPERM ([timerfd_create(2)](https://www.man7.org/linux/man-pages/man2/timerfd_create.2.html)).
Since **systemd v254** (PR [#26548](https://github.com/systemd/systemd/pull/26548),
[v254 NEWS](https://raw.githubusercontent.com/systemd/systemd/v254-rc1/NEWS)),
`pam_systemd` grants CAP_WAKE_ALARM as an **ambient capability to regular users'
session processes on local seats** — added explicitly so desktop alarm apps can
wake the machine without root. Ubuntu 24.04+ (systemd 255), Fedora 39+, Arch,
openSUSE TW have it; Debian 12 (252), Ubuntu 22.04 (249), RHEL 9 do not.
Decisive advantage: the kernel **alarmtimer** subsystem owns the hardware RTC and
programs it to the earliest of *all* outstanding alarm timers at suspend entry
([LWN 429925](https://lwn.net/Articles/429925/)) — many apps can hold alarms
simultaneously, so **the clobbering problem disappears entirely**. Caveat: the
process must stay alive holding the timerfd across suspend (fine — Bellman is
resident; if Bellman is dead the misfire pass covers by design).

**Live-probe evidence (this report's own machine — important nuance).** Mint 22.3
/ systemd 255, active local seat0 x11 session: `timerfd_create(CLOCK_REALTIME_ALARM)`
still returned **EPERM** — `/proc/self/status` shows CAP_WAKE_ALARM (bit 35) in
CapInh but **CapAmb = 0** in this process (a shell descended from a daemon, not
straight from the PAM session leader). Ambient caps don't survive every launch
path: `systemd --user` services do **not** forward the manager's ambient set
(needs `AmbientCapabilities=CAP_WAKE_ALARM` in the unit, per PR #26548 notes;
v255 regression report: [systemd #33167](https://github.com/systemd/systemd/issues/33167)),
sudo/setuid strips ambient caps, SSH sessions get nothing (local seats only).
**Consequence: never infer capability from distro/systemd version — probe with a
real `timerfd_create` call in the actual Bellman process at startup.** Also note
XDG-autostart (what `tauri-plugin-autostart` uses on Linux) runs inside the
session scope → ambient cap present; if we ever switch autostart to a user
service, the unit needs the AmbientCapabilities line.

**Raw `/sys/class/rtc/rtc0/wakealarm` (fallback only).** Default perms **0644
root:root** — not user-writable on any major distro (verified live; no distro
ships a loosening rule — systemd's `50-udev-default.rules` only adds the `rtc`
symlink, [source](https://github.com/systemd/systemd/blob/main/rules.d/50-udev-default.rules.in);
udev GROUP/MODE keys affect `/dev/rtc0`, not sysfs attributes — making sysfs
writable needs an admin `RUN+=chmod` udev rule or systemd-tmpfiles `z` line).
Semantics: write epoch seconds; write a past value/`0` to clear; **an armed alarm
must be cleared before a new set** (EBUSY otherwise — the `echo 0 > wakealarm`
dance) ([kernel ABI doc](https://www.kernel.org/doc/Documentation/ABI/testing/sysfs-class-rtc),
[LKML doc patch](https://lkml.indiana.edu/hypermail/linux/kernel/0811.3/01224.html)).
One-shot, one register → cooperative protocol required (§5 risks): read current
alarm (world-readable; also `/proc/driver/rtc`), only overwrite if ours is
earlier, restore the displaced alarm after resume.

**`rtcwake` (rejected for v1).** Needs root in practice (opens `/dev/rtc0`,
root-owned); `-m no -t <epoch>` sets without suspending, `-m disable` clears
([rtcwake(8)](https://man.archlinux.org/man/rtcwake.8.en)). Delegation routes all
require privileged setup: KAlarm ships a KAuth/polkit root helper invoking
rtcwake ([KDE bug 486187](https://bugs.kde.org/show_bug.cgi?id=486187)); DIY
sudoers NOPASSWD is forum folklore. A pkexec prompt violates Bellman's
"never prompt unexpectedly" rule → rejected.

**systemd facilities (rejected).** `WakeSystem=` timers: man page —
"requires privileges and is thus generally only available in the system service
manager" (verified in local man page; underlying issue
[systemd #17564](https://github.com/systemd/systemd/issues/17564)). On v254+ a
`--user` timer *may* work since the user manager holds the cap, but it's
officially hedged and adds a moving part outside our process — the direct timerfd
is the same mechanism without the middleman. login1 `ScheduleShutdown` schedules
shutdowns, not wakes.

**Suspend vs hibernate.** RTC alarm wakes from s2idle ("freeze"), S3 ("mem") —
s2idle wakes on any in-band interrupt incl. RTC
([kernel sleep-states.rst](https://github.com/torvalds/linux/blob/master/Documentation/admin-guide/pm/sleep-states.rst)).
Hibernate (`disk`) wake is BIOS-dependent (rtcwake offers it; results vary).
CLOCK_REALTIME_ALARM is documented for suspend; treat hibernate wake as
**not guaranteed** and let the misfire pass cover it. This machine exposes
`freeze mem disk` with `s2idle [deep]` — both suspend flavors present in the wild.

**Distro delta summary**: perms are identical everywhere; the only meaningful
delta is systemd ≥ 254 ambient CAP_WAKE_ALARM (Ubuntu 24.04+/Fedora 39+/Arch yes;
Debian 12/Ubuntu 22.04/RHEL 9 no). AppImage installs can't run a privileged
postinst → on old distros AppImage users simply get capability=Disabled.

## 2. Windows (Q2)

### Pick: in-process waitable timer; Task Scheduler task as belt-and-braces fallback

**`CreateWaitableTimerExW` + `SetWaitableTimer(fResume=TRUE)` — no privilege at
all** ([System Wake-up Events](https://learn.microsoft.com/en-us/windows/win32/power/system-wake-up-events),
[SetWaitableTimer](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-setwaitabletimer)).
Gated by *policy*, not privilege (below). Critical implementation facts:
- **Absolute UTC due time, not relative** — since Windows 8, relative due times do
  not advance during sleep (SetWaitableTimer docs).
- **Do NOT pass CREATE_WAITABLE_TIMER_HIGH_RESOLUTION** — orthogonal to wake,
  fails with ERROR_INVALID_PARAMETER pre-1803.
- **Unsupported-platform probe is built in**: on a machine that can't do resume
  timers the call **succeeds** but `GetLastError() == ERROR_NOT_SUPPORTED` (1) —
  check it right after every arm. Community-confirmed this is what Modern-Standby
  devices return ([MSDN thread](https://social.msdn.microsoft.com/forums/windowsdesktop/en-US/8dadb123-a52e-495d-9d70-5f3e6389c1f2/)).
- **Process must stay alive** holding the timer through sleep — waitable timers
  are kernel objects that die with the process ([WakeupOnStandBy FAQ](https://www.dennisbabkin.com/php/faq.php?what=wosb)).
  No fire-and-exit; that's Task Scheduler's job.
- RTC drift: multi-day S3 sleeps can wake 10–50 min late on some hardware;
  mitigate by re-arming at each wake ([MS Q&A](https://learn.microsoft.com/en-ie/answers/questions/5843621/issue-in-waking-up-of-device-at-accurate-time-afte)).
  Bellman's bridge re-arms on every resume anyway (§5).

**"Allow wake timers" policy** — `GUID_SLEEP_SUBGROUP` /
`GUID_ALLOW_RTC_WAKE` (bd3b718a-0680-4d9d-8ab2-e1d2b4ac806d, alias `RTCWAKE`):
**0 = disabled, 1 = enabled, 2 = "Important wake timers only"** (Windows-internal
timers only — value 2 excludes app timers!)
([Microsoft OEM doc](https://learn.microsoft.com/en-us/windows-hardware/customize/power-settings/sleep-settings-automatically-wake-for-tasks)).
Defaults are SKU-messy — desktop ≈ 1; laptop on battery ≈ 0; laptop on AC often 2
([elevenforum](https://www.elevenforum.com/t/enable-or-disable-to-allow-wake-timers-in-windows-11.7010/)) —
so **probe, never assume**. Readable **without admin** via `PowerGetActiveScheme`
+ `PowerReadACValue`/`PowerReadDCValue`
([PowerReadACValue](https://learn.microsoft.com/en-us/windows/win32/api/powersetting/nf-powersetting-powerreadacvalue));
cross with `GetSystemPowerStatus().ACLineStatus` for the live rail. It gates BOTH
waitable timers and Task Scheduler WakeToRun. Veeam Agent does exactly this
probe-and-tell-the-user ([Veeam docs](https://helpcenter.veeam.com/docs/agentforwindows/userguide/backup_job_schedule_free_desktop.html)).

**Modern Standby (S0ix) — the honest-degradation case.** Microsoft scopes wake
timers to S3/S4 explicitly; under Modern Standby a firing timer gets a throttled,
**screen-off** execution window, then DRIPS again — not a user-visible wake
([Transitioning between idle and active states](https://learn.microsoft.com/en-us/windows-hardware/design/device-experiences/transitioning-between-idle-and-active-states),
[MS Q&A](https://learn.microsoft.com/en-us/answers/questions/1660115/regarding-waking-up-from-sleep-on-a-modern-standby)).
Most post-2020 thin-and-lights are S0ix-only (S3 fused off). **For Bellman this
is actually acceptable**: a screen-off execution window is enough to run a wake
action (launch command / write slot JSON / notification) — but classify it
honestly as `Enabled { mechanism: …, modern_standby: true }` and note the screen
stays off (add `SetThreadExecutionState(ES_DISPLAY_REQUIRED)` if the action wants
the display; must be called within the ~2-min unattended window). Detection:
`CallNtPowerInformation(SystemPowerCapabilities)` → **`AoAc == TRUE` ⇒ Modern
Standby**; same struct yields `SystemS3`, `SystemS4`, `RtcWake`
([MS Q&A 826026](https://learn.microsoft.com/en-us/answers/questions/826026/is-there-windows-api-to-check-whether-the-computer)).

**Task Scheduler fallback.** A **non-admin user can register** a time-trigger
task in their own folder with `WakeToRun=true` + `TASK_LOGON_INTERACTIVE_TOKEN` +
"run ASAP after missed start"
([ITaskSettings::WakeToRun](https://learn.microsoft.com/en-us/windows/desktop/api/taskschd/nf-taskschd-itasksettings-get_waketorun),
[non-admin task creation](https://whenrebootingisnottheanswer.wordpress.com/2016/07/14/creating-a-scheduled-task-without-being-administrator/)).
Pros: survives Bellman crash/exit; wakes from **hibernate** (firmware permitting) —
waitable timers effectively don't survive S4/fast-startup (process image frozen /
killed). Cons: external mutable state, needs the Schedule service, same RTCWAKE
gate. This is what Macrium/Veeam ship
([Macrium KB](https://knowledgebase.macrium.com/display/KNOW80/PC+wont+wake+from+Sleep+to+run+a+backup)).
**v1 recommendation: waitable timer only** (zero external state); add the
scheduled-task fallback in v1.x if hibernate/app-dead coverage proves needed —
it duplicates what the misfire pass already guarantees functionally.

**Hibernate/S5**: nothing wakes from full shutdown; fast-startup "shutdown" kills
the process (timer dies). Predict S4 wake with
`SYSTEM_POWER_CAPABILITIES.RtcWake >= PowerSystemHibernate`. `powercfg /waketimers`
needs an elevated console — do not build the probe on it; `powercfg /a` works
unelevated (diagnostics only).

## 3. macOS (Q3)

### Pick: optional SMAppService root daemon; without it, wake = Disabled (honest)

**No-elevation verdict: NO — hard, current, verified in shipping source.** Every
scheduling path (`pmset schedule`, public `IOPMSchedulePowerEvent`, private
`IOPMRequestSysWake`) funnels into powerd's `_io_pm_schedule_power_event`, which
requires **euid 0** or the Apple-internal entitlement
`com.apple.iokit.wakerequest` (third parties can't get it) — enforced
server-side in powerd, so no client-side trick helps
(PowerManagement-1846.120.8.0.1 `pmconfigd/AutoWakeScheduler.c`,
[apple-oss-distributions](https://github.com/apple-oss-distributions/PowerManagement/blob/main/pmconfigd/AutoWakeScheduler.c);
header contract "Must be called as root":
[IOPMLib.h](https://github.com/opensource-apple/IOKitUser/blob/master/pwr_mgt.subproj/IOPMLib.h)).
Unprivileged call → `kIOReturnNotPrivileged` (-536870207). `pmset schedule`/`repeat`
need sudo (reads like `pmset -g sched` don't) ([ss64 pmset](https://ss64.com/mac/pmset.html));
Apple's own user guide says "sudo pmset repeat wake …" — the Energy Saver
schedule GUI was removed in Ventura.

**Recommended architecture (the accepted modern pattern, dmg-friendly):**
`SMAppService.daemonService` (macOS 13+) registering a root **LaunchDaemon
shipped inside the app bundle** — the user approves **once** in System Settings →
Login Items; no admin-password prompt from our code, no installer
([SMAppService docs](https://developer.apple.com/documentation/servicemanagement/smappservice),
[theevilbit deep-dive](https://theevilbit.github.io/posts/smappservice/)).
SMJobBless is deprecated. Precedent: DssW Power Manager — the canonical Mac wake
scheduler — ships a root LaunchDaemon
([DssW guide](https://www.dssw.co.uk/powermanager/guide/v3/administrator-guide/package-power-manager-daemon/)).
Daemon design: tiny XPC surface (`schedule_wake(date)`, `cancel_my_wakes`),
calls `IOPMSchedulePowerEvent(kIOPMAutoWake, my_id="com.bellman.wake")` /
`IOPMCancelScheduledPowerEvent` (public API), **validates the XPC client's code
signature**. **Never `pmset repeat` and never `cancelall`**: the repeating
schedule is a single system-wide pair (clobbers others), and cancelall wipes
every app's events (§3 risks). One-shot events carry our app id
(`kIOPMPowerEventAppNameKey`), cancel requires exact arg match → we only ever
touch our own entries; queue cap 1000 entries.

**Behavioral facts to design around:**
- Wake times **round down to 30-second granularity** ([macrumors thread](https://forums.macrumors.com/threads/using-iopmschedulepowerevent.472101/)).
- **DarkWake**: a scheduled wake may leave the screen dark; app code runs; no API
  detects DarkWake (Apple DTS: watch IOPMrootDomain power state, or take an
  assertion). **Take `IOPMAssertionCreateWithName` immediately when the due
  moment arrives** so the machine doesn't re-sleep mid-action
  ([Apple forums thread 770517](https://developer.apple.com/forums/thread/770517)).
  Never treat "process running" as "scheduled time arrived" (Power Nap
  maintenance wakes also run app code) — check the clock.
- Clamshell: scheduled system wakes fire lid-closed on Ventura+; near-empty
  battery may defer/skip scheduled wakes.
- FileVault: scheduled *power-on* stalls at pre-boot auth; *wake from sleep*
  returns to the running session (only wake matters to Bellman).
- Events persist on disk across sleep/boot — program eagerly at schedule-change
  time; the pre-sleep hook is only a refresh.
- No-daemon fallbacks that do NOT wake: `caffeinate`/assertions (prevent sleep —
  usable as an explicit opt-in "hold sleep until next timer" mode, AC-aware);
  launchd `StartCalendarInterval` (coalesced catch-up on natural wake — exactly
  our misfire pass, so nothing to add).

## 4. Capability detection design (Q4)

One probe object per OS, run at startup, after resume, on schedule-mutation
failure, and on power-source change (Windows). Result cached; every probe is
prompt-free, root-free, side-effect-free, < a few ms; failures → `Disabled`
with a reason, never a crash.

**Linux probe checklist (order matters):**
1. `/sys/class/rtc/rtc0` exists? no → `Disabled(NoRtc)`.
2. `/sys/class/rtc/rtc0/device/power/wakeup == "enabled"`? no → `Disabled(RtcNotWakeCapable)`.
3. `timerfd_create(CLOCK_REALTIME_ALARM, TFD_CLOEXEC)` → success ⇒
   `Enabled(LinuxAlarmTimerfd)` (keep the fd!); EPERM ⇒ step 4.
4. `access("/sys/class/rtc/rtc0/wakealarm", W_OK)` → yes ⇒
   `Enabled(LinuxWakealarmSysfs)` (cooperative protocol); no ⇒ step 5.
5. `Disabled(NoPermission { hint: "systemd ≥254 grants this to local desktop
   sessions; SSH/service launches need AmbientCapabilities=CAP_WAKE_ALARM, or an
   admin can setcap cap_wake_alarm+ep on bellman" })`.
6. Suspend support (informational, affects wording only): `/sys/power/state`
   contains `mem`/`freeze`; login1 `CanSuspend()` ∈ {yes, challenge, no, na}.

**Windows probe checklist:**
1. `CallNtPowerInformation(SystemPowerCapabilities)` → capture `AoAc`,
   `SystemS3`, `SystemS4`, `RtcWake`.
2. `PowerGetActiveScheme` + `PowerReadACValue`/`PowerReadDCValue`
   (SLEEP_SUBGROUP, ALLOW_RTC_WAKE) + `GetSystemPowerStatus().ACLineStatus` →
   active-rail value: 0 → `Disabled(WakeTimersDisabledByPolicy { rail })`;
   2 → same (app timers aren't "important"); 1 → step 3. Re-run on
   PBT_APMPOWERSTATUSCHANGE (AC↔battery flips the rail).
3. Live arm test: `SetWaitableTimer(fResume=TRUE)` far-future sentinel →
   success + `GetLastError()==ERROR_NOT_SUPPORTED` ⇒
   `Disabled(ResumeTimersUnsupported)`; clean success ⇒
   `Enabled(WindowsWaitableTimer { modern_standby: AoAc })` (cancel sentinel).
   `AoAc=true` annotates the GUI line: "wake runs screen-off (Modern Standby)".

**macOS probe checklist:**
1. `geteuid()==0`? (never for the GUI app, but makes the probe correct inside
   the daemon) → direct `Enabled`.
2. `SMAppService.daemonService(...).status`: `.enabled` → step 3;
   `.requiresApproval` → `Disabled(HelperAwaitingApproval)` + GUI button →
   `SMAppService.openSystemSettingsLoginItems()`; `.notRegistered`/`.notFound` →
   `Disabled(HelperNotInstalled)` + one-click enroll.
3. Through the daemon: schedule far-future sentinel `kIOPMAutoWake`, check
   `kIOReturnSuccess` vs `kIOReturnNotPrivileged`, cancel with identical args ⇒
   `Enabled(MacPmDaemon)` / `Disabled(ProbeFailed)`.
4. Verify after every real arm (unprivileged read): `IOPMCopyScheduledPowerEvents()`
   contains our `my_id` entry.

**Decision tree (runtime):**
```
os = compile-time cfg  ──►  probe(os)  ──►  WakeCapability
                                            ├─ Enabled{mech, notes}
                                            └─ Disabled{reason}
program_wake(t):  capability==Enabled ? arm via mech : silently skip (log once)
on arm failure:   re-probe once → if now Disabled, flip status + log transition
GUI status line:  "Wake from sleep: ON via <mech> (<note>)" |
                  "OFF — <reason sentence>" (+ fix-it hint/button where actionable)
JSONL:            one `wake_capability` event at startup + one per transition,
                  never per-arm spam.
```
The misfire pass runs unconditionally on every app start and every resume — the
wake feature only *improves timeliness*; it never carries correctness.

## 5. Single-next-wake bridge (Q5)

**State**: scheduler core owns `armed: Option<(timer_id, wake_utc)>`. Election:
`next = min(next_due(t) for t in timers if t.wake_machine && t.enabled)`.

**Arm points** (all funnel into one `rearm()` that is cancel-then-program,
idempotent):
1. Any store mutation changing the earliest wake-enabled due time (GUI/CLI/slot).
2. Pre-suspend notification (refresh/last chance).
3. Resume + app start (re-arm the next one; also covers Windows RTC drift).

**Early-wake slack**: program `wake_utc - 45 s` (covers macOS 30-s round-down,
resume latency, clock settle). The RTC event never fires the action — after
resume the normal loop fires the timer at its true due time; if the machine
resumed *after* due time, the misfire pass catches it. Hence **never-both**
(only the in-app loop fires, exactly once, via the store's fired-state) and
**never-neither** (wake missed/disabled ⇒ misfire pass on next natural
start/resume — same code path, no special cases).

**Pre-suspend hooks per OS:**
- **Linux**: login1 — at startup take a *delay* inhibitor
  `Inhibit("sleep", "Bellman", "arming RTC wake", "delay")` (fd-based; allowed
  for active local sessions by default, no prompt), subscribe `PrepareForSleep`.
  On `true`: rearm, close fd (≤ 5 s budget — `InhibitDelayMaxSec` default,
  verified 5 s live); on `false` (resume): misfire pass, rearm, retake fd
  ([systemd inhibitor docs](https://systemd.io/INHIBITOR_LOCKS/)). Note: on the
  timerfd path the kernel programs the RTC at suspend entry itself — the
  inhibitor mainly serves the sysfs fallback + gives us a clean resume signal.
- **Windows**: no pre-suspend re-arm needed (the armed waitable timer is a
  persistent kernel object) — but register a hidden window / 
  `RegisterSuspendResumeNotification(DEVICE_NOTIFY_CALLBACK)` for
  `PBT_APMSUSPEND` (~2 s budget, bookkeeping only) and
  `PBT_APMRESUMEAUTOMATIC` → misfire pass + rearm. Note auto-wakes deliver
  RESUMEAUTOMATIC only (screen off); RESUMESUSPEND arrives only on user-present
  wakes ([WM_POWERBROADCAST](https://learn.microsoft.com/en-us/windows/win32/power/wm-powerbroadcast-messages)).
- **macOS**: in the **daemon**, `IORegisterForSystemPower`; on
  `kIOMessageSystemWillSleep` rearm then `IOAllowPowerChange` promptly (30 s
  allowance, non-abortable — QA1340,
  [ref](https://leopard-adc.pepas.com/qa/qa2004/qa1340.html)). GUI app
  additionally observes `NSWorkspace.willSleep/didWake` for the misfire pass.
  Events persist on disk, so the daemon re-arms eagerly at schedule-change time;
  the sleep hook is just a refresh.

**Cancel/rearm rules**: `cancel_wake()` is a no-op when nothing armed; macOS
cancel must replay the exact original (time, id, type) — store them alongside
`armed`. On timer delete/disable → rearm (elects a new winner or cancels). On
capability flip to Disabled → cancel + log transition.

## 6. Rust crates / FFI (Q6) — concrete picks

No existing crate covers scheduled machine-wake on any OS (crates.io sweep:
`keepawake` = inhibit-only, `circadian` = Linux suspend daemon) — we write the
thin platform layer ourselves. Total new FFI surface is small (~6 C fns on
macOS, 0 elsewhere).

| OS | Crate | Used for |
|---|---|---|
| Linux | [`rustix`](https://crates.io/crates/rustix) (or `nix`) | `timerfd_create(TimerfdClockId::RealtimeAlarm)` + `timerfd_settime(ABSTIME)` — no FFI needed |
| Linux | [`zbus`](https://crates.io/crates/zbus) (already in Tauri's Linux dep tree) | login1 Manager proxy: `Inhibit`, `PrepareForSleep` signal, `CanSuspend` ([zbus_systemd::login1](https://docs.rs/zbus_systemd/latest/zbus_systemd/login1/struct.PrepareForSleep.html) exists but hand-writing the 3-method proxy with zbus's `#[proxy]` macro is smaller than the giant generated crate) |
| Windows | [`windows`](https://crates.io/crates/windows) (windows-rs) | features: `Win32_System_Threading` (CreateWaitableTimerExW/SetWaitableTimer — [binding](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/System/Threading/fn.SetWaitableTimer.html)), `Win32_System_Power` (PowerReadACValue/DCValue, GetSystemPowerStatus, CallNtPowerInformation, RegisterSuspendResumeNotification, SetThreadExecutionState), `Win32_UI_WindowsAndMessaging` (WM_POWERBROADCAST); later `Win32_System_TaskScheduler` if the fallback ships |
| macOS | [`objc2`](https://crates.io/crates/objc2) + [`objc2-app-kit`](https://docs.rs/objc2-app-kit/latest/objc2_app_kit/struct.NSWorkspace.html) (Tauri v2 already uses objc2) | NSWorkspace willSleep/didWake observers; SMAppService via objc2-service-management or a small Swift shim in the bundle |
| macOS | hand-written `extern "C"` against IOKit.framework + [`core-foundation`](https://crates.io/crates/core-foundation) | `IOPMSchedulePowerEvent`, `IOPMCancelScheduledPowerEvent`, `IOPMCopyScheduledPowerEvents`, `IORegisterForSystemPower`, `IOAllowPowerChange`, `IOPMAssertionCreateWithName` — **`io-kit-sys`/`apple-sys` do not bind the power-management subset** ([io-kit-sys](https://lib.rs/crates/io-kit-sys) covers HID/USB), so ~6 manual declarations; link `IOKit` framework |

**Shell-out verdict**: never `rtcwake`/`pmset` from the app (both need root →
prompt). `pmset -g sched` parsing as a *read-only debug aid* is fine but
`IOPMCopyScheduledPowerEvents` is the API answer. The macOS daemon itself calls
the C API, not pmset.

## 7. API sketch — `bellman-core::platform::wake`

```rust
pub enum WakeMechanism {
    LinuxAlarmTimerfd,        // CLOCK_REALTIME_ALARM, kernel-multiplexed
    LinuxWakealarmSysfs,      // cooperative /sys write (admin-enabled boxes)
    WindowsWaitableTimer,     // SetWaitableTimer(fResume)
    MacPmDaemon,              // SMAppService root daemon → IOPMSchedulePowerEvent
}

pub enum DisabledReason {                    // every variant carries a user sentence
    UnsupportedOs,                           // e.g. *BSD build
    NoRtc,                                   // /sys/class/rtc/rtc0 absent
    RtcNotWakeCapable,                       // power/wakeup != enabled
    NoPermission { hint: String },           // Linux EPERM+no W_OK (hint: setcap/session)
    WakeTimersDisabledByPolicy { rail: PowerRail, value: u8 }, // Win RTCWAKE 0|2
    ResumeTimersUnsupported,                 // Win ERROR_NOT_SUPPORTED
    HelperNotInstalled,                      // macOS
    HelperAwaitingApproval,                  // macOS — GUI offers Login-Items deep link
    ProbeFailed { detail: String },
}

pub enum WakeCapability {
    Enabled { mechanism: WakeMechanism, modern_standby: bool }, // ms: screen-off note
    Disabled { reason: DisabledReason },
}

/// One per process. Implementations are internally armed-state-aware;
/// program_wake replaces any previous wake (cancel-then-arm, idempotent).
pub trait MachineWake: Send + Sync {
    fn capability(&self) -> WakeCapability;               // cached; re_probe() on failure/power change
    fn re_probe(&self) -> WakeCapability;
    fn program_wake(&self, at: DateTime<Utc>) -> Result<(), WakeError>;
    fn cancel_wake(&self) -> Result<(), WakeError>;       // Ok(()) when nothing armed
    /// resume/pre-sleep events surfaced to the scheduler (login1 / WM_POWERBROADCAST / IOKit)
    fn power_events(&self) -> Receiver<PowerEvent>;       // Suspending, Resumed
}
```

**Per-timer `wake_machine: bool`** (default **false**; GUI checkbox "Wake the
computer for this timer", CLI `--wake`, slot-JSON optional field `wake_machine`).
Semantics: the timer *participates in the single-next-wake election* — not a
guarantee (capability may be Disabled; GUI shows the timer flag greyed with the
status-line reason). Scheduler calls `program_wake(min(...) - 45 s)` at the arm
points of §5. One JSONL `wake_capability` event at startup + on transition;
per-arm results only at debug level.

## 8. Risks

1. **Shared-RTC clobbering (Linux sysfs path only)**: one alarm register; another
   app's alarm can be overwritten. Mitigation: timerfd path is immune (kernel
   multiplexes — main reason it's the pick); sysfs fallback reads current alarm,
   only writes if ours is earlier, restores displaced alarm on resume; never
   `rtcwake -m disable` blindly.
2. **macOS shared event list**: `pmset repeat` is a single global pair and
   `cancelall` wipes everyone — use only one-shot events tagged
   `com.bellman.wake`, cancel by exact match.
3. **S0ix laptops (the mainstream modern laptop)**: Windows wake = screen-off
   execution window (fine for actions, surprising for humans — say so in the GUI
   line); some S0ix boxes hard-fail resume timers (ERROR_NOT_SUPPORTED probe
   catches). macOS DarkWake similar — take a power assertion at fire time or the
   box re-sleeps mid-action.
4. **AC-vs-battery policy flips (Windows)**: RTCWAKE has separate AC/DC values;
   unplugging can silently disable wakes → re-probe on power-source change
   events and update the status line.
5. **Capability is per-process, not per-machine (Linux)**: ambient
   CAP_WAKE_ALARM depends on launch lineage (verified live: v255 box, local
   session, still EPERM inside a daemon-descended shell). Autostart via XDG
   .desktop keeps it; a future systemd user unit needs
   `AmbientCapabilities=CAP_WAKE_ALARM`.
6. **Hibernate**: not guaranteed anywhere (BIOS-dependent Linux/Windows-task;
   waitable timers die under S4/fast-startup). Contract: wake covers
   suspend/sleep; hibernate relies on the misfire pass. Document it.
7. **Process-lifetime coupling** (Linux timerfd, Windows waitable timer): armed
   wake dies with Bellman. Acceptable by design (misfire pass), but tray-quit
   should log "wake disarmed".
8. **RTC drift over multi-day sleeps (Windows)**: minutes-late wakes observed;
   bridge re-arms on every resume, slack + misfire pass absorb the rest.
9. **macOS helper refusal**: user may never approve the daemon → permanent
   `Disabled(HelperAwaitingApproval)`; the feature must read as an optional
   enhancement in onboarding, not a broken state.
10. **Battery-critical deferral (macOS)**: near-empty battery defers scheduled
    wakes; nothing to do but document.

## Answers-to-questions index

Q1 → §1 (pick: CLOCK_REALTIME_ALARM timerfd; sysfs cooperative fallback; rtcwake
rejected; WakeSystem rejected; suspend yes / hibernate best-effort).
Q2 → §2 (pick: SetWaitableTimer fResume, absolute UTC; RTCWAKE 0/1/2 probe
no-admin; Modern Standby = screen-off enabled-with-note; Task-Scheduler fallback
deferred to v1.x).
Q3 → §3 (pick: no root path exists — optional SMAppService daemon, one-time
approval; never pmset repeat/cancelall).
Q4 → §4 (per-OS checklists + decision tree; probe in-process at runtime, never
version-infer — proven necessary by live EPERM on a v255 box).
Q5 → §5 (single armed wake, −45 s slack, RTC never fires actions ⇒
never-both/never-neither; login1 delay-inhibitor ≤5 s / WM_POWERBROADCAST /
IORegisterForSystemPower 30 s).
Q6 → §6 (rustix + zbus + windows + objc2/objc2-app-kit + ~6 hand FFI decls to
IOKit; no shelling out).
