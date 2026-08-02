> ARCHIVED 2026-08-02 — shipped; card bellman-c11-validation-full-system-originality, run 2026-08-01_0005, merge 9916dc1.

# C11 — Full-system validation + originality sweep

Repo: `~/bellman`. **The last gate before Bellman is offered to strangers.**
Everything else has shipped: the scheduler core, the integration kit (IK1–IK6),
the fire dispatcher (SCH1/SCH2), both demo apps, the first-run demo offer, and
the ship-readiness fixes (SHIP1).

Read first: `docs/PLAN.md` and `docs/BUILD_PLAN.md` (they override any
assumption), then `docs/INTEGRATION.md` — the integration surface is the
product's differentiator and the bulk of what this card must exercise.

**This card changes code only to FIX, TEST and POLISH.** No new features, no
architecture changes, and nothing that touches the frozen output protocol
(`CARD_INDEX.md` → *Standing decision*).

## The rule that outranks everything else here

**Play the human. Do not play the developer.**

Every previous round of testing on this project passed while the product was
broken for real people, and always for the same reason: the tester had repo
knowledge and used it. They called `run_now` instead of waiting for a clock.
They ran `apt-get update` before following the README because of course you do.
They pointed a client at the data directory they already knew was live.

So for the user-facing parts of this card, **behave like someone who found
Bellman an hour ago**:

- Install from the README **verbatim**, on a clean machine, with no bootstrap
  step you invented. If a command fails, that is a finding — not something to
  work around and continue.
- Use the GUI by clicking it, not by calling the Tauri commands behind it.
- Read the docs as **instructions to follow**, not as a specification to
  check against the code. When you have to open a source file to understand
  what a doc means, write that down: it is a documentation defect.
- Do not use knowledge you only have because you can see the repository.
- **Write down every moment of confusion**, even when you resolve it. "I could
  not tell which data directory was live" is worth more than a green test.

Record these as a first-person narrative in `docs/VALIDATION.md` under
*Walkthrough*. Where the experience was poor but nothing is broken, say so and
mark it a polish item rather than a failure.

## The other rule: a clock must do the firing

**No evidence in this card may come from `run-now`.** SCH2 exists because a
whole tested integration kit shipped with its headline feature broken — every
test drove the fire path directly instead of letting a timer fire. If a
scenario needs a fire, schedule it and wait.

Where a scenario is only practical with `run-now`, mark it explicitly as
`fired: manual` in the evidence so a reader can weigh it correctly.

## 1 — Code health

- `cargo test --workspace` green; `cargo clippy --all-targets` zero warnings;
  `cargo fmt --check` clean.
- The frontend suites (`ui/`, vitest) green.
- Any test marked ignored/skipped: list it in `VALIDATION.md` with why. A
  silently skipped test is a lie in a green run.

## 2 — Scheduling, end to end

- All **7 occurrence kinds** (once / interval / daily / weekly / monthly /
  yearly / cron) created and observed firing on their own schedule.
- **Misfire behaviour**: stop the app across a due time, restart, verify the
  configured coalesce/skip policy — and that the event log says what happened.
- **DST and clock jumps against a real clock**, not just the unit-tested
  maths. A disposable VM or `libfaketime` is the way; do not manipulate the
  host clock.
- **Wake-from-sleep on real hardware**: `rtcwake -m mem -s 30`, verify the
  machine wakes and the timer fires or misfires per policy. If no such
  hardware is available, say so plainly in `VALIDATION.md` — this is the one
  gap where an honest "not tested" is acceptable and a guess is not.
- **Pruner / retention**: exercise a rotation and a retention sweep; confirm
  archives gzip and the byte budget holds.

## 3 — The integration surface (the actual product)

None of this existed when this card was first written, and it is now the
majority of the value.

- **A real app woken and replying**: `testing_apps/lightbulb/` completes a run
  from a scheduled fire — `fired → acknowledged → completed` under one
  `run_id`, mirrored in `status.json`.
- **The GUI demo** (`testing_apps/lightbulb_gui/`) does the same from a
  scheduled fire, driven by clicking, on both data directories.
- **Pickup grace and `no_ack`**: an owned timer with nothing listening reaches
  `no_ack` after the configured grace, and a late reply still revises it.
- **The opt-in watchdog**: `error_detection` + `expected_secs` produces
  `failed`/`timed_out` at `× factor`, the reply file is left byte-identical,
  and a heartbeat extends the deadline.
- **Rejection and quarantine under a live watcher**: malformed bytes, wrong
  `app_name`, unknown `run_id`, oversize payload, a stale reply from a
  superseded run. Each logs `reply_rejected`, copies to `timers/bad/`, and
  leaves the live file in place.
- **Slot channel CRUD** from an external script — add, modify, delete — with
  the timer live in the running scheduler **without a restart** (this is
  SCH2's fix; prove it still holds).
- **Both transports**: the same reply over the file path and over the IPC
  socket produce identical state, log lines and `status.json`.
- **A client in a language the docs do not cover**, written from
  `docs/INTEGRATION.md` alone by someone not reading the Rust. It works, or
  the docs are wrong.

## 4 — Install and packaging, as a stranger

- README §Install **verbatim** in a clean `ubuntu:24.04` container, no
  bootstrap. Then the same for the documented **Fedora** and **Arch** recipes.
- The built `.deb` installs; `bellman` on `PATH`, `bellman-app` present,
  launcher entry appears, and the demos ship inside the package (SHIP1) at
  the path the wizard names.
- **First-run wizard on a genuinely clean desktop session** — ephemeral
  `XDG_DATA_HOME` + Xvfb, never the host's real profile. Tick the demo offer
  and confirm the bulb lights from a scheduled fire.
- Both data directories behave as documented, and a user can find the live one
  from the GUI (Settings → Data) and `bellman --help`.
- Windows and macOS: whatever can be validated without the hardware, plus an
  explicit list of what still cannot.

## 5 — Originality sweep

The repo is public, so this now matters more than when it was first written.

Compare against the inspiration clones in `/home/sami/reference_repos/bellman/`
(croner-rust, Cronicle, kalarm, notify, pomodorolm, tokio-cron-scheduler,
zeit): **no copied code, no copied identifiers, comments or strings**, idioms
rewritten in our own structure. Document per crate/module in
`docs/ORIGINALITY.md` with a verdict and what was rewritten. Rewrite anything
too close rather than arguing it is fine.

## 6 — Polish

Consistent naming, module docs on every public item, dead code removed, and no
personal names, attributions or absolute personal paths anywhere in tracked
files (SHIP1 added a CI gate for the last one — confirm it still bites).

## Deliverables

- **`docs/VALIDATION.md`** — every scenario with PASS/FAIL, the exact command
  or click path, and the log lines that prove it. Plus the first-person
  *Walkthrough* section, and an explicit **"not tested, and why"** list. An
  unstated gap is worse than a stated one.
- **`docs/ORIGINALITY.md`** — per-module verdicts.
- Fixes for everything found that is genuinely broken. If a finding is larger
  than this card, write it up as a follow-up card rather than half-fixing it.

## Exit gate

- Workspace tests, clippy and fmt all green, with every skipped test explained.
- All 7 occurrence kinds observed firing **on their own schedule**; no
  `run-now` anywhere in the evidence except where explicitly marked `manual`.
- A real application woken by a scheduled fire replies and reaches
  `completed`, over **both** transports.
- Malformed, stale and oversize replies each behave exactly as
  `INTEGRATION.md` documents, verified against a live watcher.
- A slot-created timer fires without a restart.
- README §Install succeeds verbatim on clean Ubuntu, Fedora and Arch
  containers, with no bootstrap step.
- The `.deb` installs and its shipped demo runs from the path the wizard names.
- `VALIDATION.md` contains the human walkthrough, including the confusions —
  a walkthrough with no friction recorded means the role-play was not done.
- `ORIGINALITY.md` covers every module.
- Nothing in the diff changes a wire shape or adds a required client library.
