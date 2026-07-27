# Research topic: RTC wake-machine-from-sleep for Bellman — 3-OS support + capability detection

Bellman (see docs/PLAN.md + docs/BUILD_PLAN.md in this repo) is a cross-platform
task scheduler. Operator decision: **wake-from-sleep ships in v1**, but strictly
capability-gated — at runtime Bellman detects which OS it is on and whether the
wake mechanism is actually available/permitted there; if not, it silently skips
and the misfire-on-resume pass covers the gap. Not-Linux → don't use the Linux
path; not-Windows → don't use the Windows path; not-macOS → don't use the macOS
path. Never crash, never prompt unexpectedly, never require elevation to merely
run the app.

# Research questions (each researcher answers ALL, independently)
1. **Linux**: rtcwake vs direct /sys/class/rtc/rtc0/wakealarm writes vs systemd
   facilities — which works WITHOUT root (udev rules? polkit? group membership?),
   distro differences, interplay with an already-set wakealarm (only one RTC
   alarm exists — how do we not clobber other apps'?), suspend vs hibernate.
2. **Windows**: waitable timers with CreateWaitableTimerEx + SetWaitableTimer
   (fResume=TRUE) from a user process — when does it actually wake the machine
   (Allow-wake-timers power setting!), vs scheduling a Task Scheduler task with
   WakeToRun; modern-standby (S0ix) vs S3 behavior; no-admin paths.
3. **macOS**: pmset schedule wake (needs root?) vs IOPMSchedulePowerEvent from
   a user process (entitlement/privilege reality in current macOS), caffeinate,
   Login-Items implications; App-Store-safety irrelevant (we distribute dmg).
4. **Capability detection design**: for each OS, a cheap PROBE at startup (and
   on failure) that answers "can this build+user+power-config actually wake the
   machine?" — exact checks, and the decision tree runtime OS detection →
   mechanism → probe → enabled/disabled with reason (surfaced in GUI as a
   status line, logged as one event).
5. **Single-next-wake bridge**: Bellman programs ONE wake for the next
   wake-enabled timer due while asleep (re-programmed on schedule changes and
   before suspend — how to hook pre-suspend on each OS: login1 PrepareForSleep,
   NSWorkspace willSleepNotification, WM_POWERBROADCAST). Cancel/rearm rules;
   interaction with the misfire pass on resume (wake fires timer OR resume
   misfire catches it — never both/never neither).
6. **Rust crates / FFI**: existing crates covering any of this (zbus for
   login1, windows-rs APIs, IOKit bindings) vs shelling out to rtcwake/pmset —
   recommend concrete dependencies per OS.

# Deliverable (research.md in YOUR OWN folder)
- Per-OS recommendation with the no-elevation verdict (possible: yes/no/partial
  + exact mechanism), citations.
- The capability probe checklist per OS + the enable/disable decision tree.
- API sketch: bellman-core platform::wake module trait (program_wake(utc),
  cancel_wake(), capability() -> enum with reason) + per-timer wake_machine
  flag semantics.
- Risks list (clobbering shared RTC alarm, S0ix laptops, AC-vs-battery policy).

# Acceptance
- All 6 questions answered with explicit picks, not surveys.
- Synthesis-ready: agent ① also writes synthesis.md comparing both reports
  (R1 pair: ① doubles as synthesizer) with a disagreement table + final design.
