# SHIP1 — Ship-readiness fixes before strangers arrive

Repo: `~/bellman`. Source: the R9 ship-readiness swarm (crew `2026-08-01_0001`,
grok + antigravity researching, claude synthesising) — report at
`Research_from_Crew/bellman-ship-readiness-critique-is-this_research_2026-08-01_015728/`.

Verdict was **ship with fixes**, reached independently by both researchers.
Every item below was **re-verified by hand before this card was written**; the
one claim that did not survive verification is recorded at the end so it does
not come back a third time.

**This card does not touch the frozen output protocol.** No wire shape, no
required SDK, no change to one-writer-per-file. Every item is an
implementation defect, a documentation defect, or missing repository
furniture. See `CARD_INDEX.md` → *Standing decision*.

## A — The fire notification documentation is wrong in two ways (P0)

The integration guide is the product's whole value proposition, and this is
the part an implementer branches on.

**A1. `bellman-fire/1` does not exist.** `docs/INTEGRATION.md:340-348`
documents a fire notification with `"schema":"bellman-fire/1"`. The only
occurrence of that string anywhere in the tree is a test comment recording
its removal: *"the legacy bellman-fire/1 WriteSlotPayload duplicate was
removed by SCH1"* (`crates/bellman-core/tests/json_shapes.rs:127`). The docs
describe a schema that was deleted and never updated.

**A2. `occurrence_kind` carries the opposite vocabulary to the one
documented.** `INTEGRATION.md:347` says it is `on_time | late | coalesced |
catch_up_<n>`, and the example at `:459` shows `"occurrence_kind":"on_time"`.
The producer passes `timer.occurrence.kind().kind_label()`
(`crates/bellman-core/src/reply/publication.rs:143`), which yields the
**recurrence type** — `once | interval | daily | weekly | monthly | yearly |
cron`. A real fire says `"occurrence_kind":"interval"`. An app that branches
on the documented values matches nothing, forever, silently.

**A3. The warning box added on 2026-07-31 states the collision backwards.**
`INTEGRATION.md:424-429` currently claims that in the fire notification
`occurrence_kind` means "the timing of this particular firing". It does not —
that is the value `status.json` was documented as carrying, and the two were
described the wrong way round. Fix the box, do not just fix the example
around it; a half-fix here leaves the more confusing artefact in place.

**Scope**: correct all three, and add a **golden-JSON fixture test** that
serialises a real `FireNotification` and asserts the documented example
matches it field for field, so the doc cannot drift from the wire again.

## B — The first-run wizard makes a false promise about wake-from-sleep (P0)

`ui/src/Wizard.svelte:109-110` tells the user:

> "On Linux, XDG autostart is also what preserves the ambient CAP_WAKE_ALARM
> lineage used for unprivileged RTC wake (systemd ≥ 254 desktop sessions)."

The platform code's own remediation hint says the opposite
(`crates/bellman-core/src/platform/wake/mod.rs:166`):

> "Plain XDG desktop autostart **does not** grant this capability on many
> desktops."

A user ticks autostart, believes wake-from-sleep is handled, and their machine
silently fails to wake for a timer — on a feature the product markets by name.
This is the most damaging finding in the set because the failure is invisible
until a missed wake matters.

**Scope**: rewrite the hint to state what actually grants the capability —
`setcap 'cap_wake_alarm+eip'`, a systemd user unit with
`AmbientCapabilities=CAP_WAKE_ALARM`, or the udev rule for the sysfs fallback —
and make the wizard's wake step reflect the real probe result rather than an
assumption. Keep it short; the wizard is not the place for a capabilities
essay, but it must not assert something false.

## C — §Install still does not survive a literal copy-paste (P0)

Verified on the current README:

- **No `apt update` anywhere.** `sudo apt install -y git …` runs against a
  possibly-empty package list; on a fresh container it fails outright with
  `Unable to locate package git`.
- **`curl` is used before it is installed.** Step 1 pipes `curl … | sh` for
  rustup, but `curl` only arrives in step 2's apt line.
- **Fedora and Arch get no recipe.** "install the equivalents" is not
  guidance, and the names genuinely diverge — Fedora needs
  `webkit2gtk4.1-devel`, `libayatana-appindicator-gtk3-devel`, `libxdo-devel`.

**Scope**: add `sudo apt update` as its own first step; order the prerequisites
so nothing is used before it exists; add a real `dnf` / `pacman` package table.
Do **not** add `patchelf` or `libgtk-3-dev` — see *Refuted* below.

## D — Two data directories, one documented (P1)

The CLI defaults to `~/.bellman/`; the desktop app uses
`~/.local/share/io.bellman.desktop/`. `docs/LOCAL.md:11-15` documents only the
first, and the README's "your data stays yours" section implies a single Linux
path. Someone who follows the CLI docs while running the GUI integrates
against a store that is silently empty — this cost real time during the
2026-07-31 session, repeatedly.

**Scope**: document both paths side by side, per OS, in `LOCAL.md` and the
README; surface the active data directory somewhere in the GUI so it can be
found without reading documentation. `bellman --help` should name it too.

## E — The CLI cannot attach a launch action (P1)

`bellman add --help` exposes no `--command`, `--args`, `--action` or
`--launch`. Every CLI-created timer is `Action::None`, so it fires and does
nothing, while the GUI has the equivalent fields
(`ui/src/TimerDialog.svelte:691-708`). For a product whose stated purpose is
waking other applications, and whose CLI is advertised as AI-skill friendly,
this is a hole in the primary interface.

**Scope**: add `--action {none,launch,notify}` with `--command` / `--args` to
`add` and `edit`, matching the slot payload's action shape; document in
`docs/CLI.md`.

## F — CI compiles but never runs tests on macOS and Windows (P1)

`macos.yml` and `windows.yml` both run `cargo build --workspace --all-targets`
and stop there. The workflows document the reasons honestly in comments — the
retention test shells out to GNU `touch -d @<epoch>`, which BSD/macOS `touch`
rejects, and `bellman-app` unit tests exit abnormally on a headless Windows
runner — but the consequence is not disclosed to users: **two of the three
advertised platforms have zero test execution**, which is a stronger statement
than "unsigned".

**Scope**: replace the `touch -d` dependency with a cross-platform mechanism
(`filetime`, or set times through `std::fs`), make the `bellman-app` unit tests
runnable headless, and re-enable `cargo test` on both workflows. If either
proves genuinely out of reach in this card, say so and instead state the gap
plainly in the README status table — an undisclosed gap is the actual defect.

## G — Public-repository furniture and leaked personal paths (P1)

The repository is public. Verified missing: `SECURITY.md`, `CONTRIBUTING.md`,
`CODE_OF_CONDUCT.md`, and any issue or PR templates under `.github/`.

Verified present and worse: **11 tracked files contain `/home/sami/…`**, all
under `docs/qa4-evidence/` and `docs/qa4-screenshots/` — a username and machine
layout published to strangers.

**Scope**: add the community files; redact the personal paths in the QA
evidence (rewrite to a placeholder root, keeping the evidence meaningful); add
a CI gate that fails on `/home/<user>` or `/Users/<user>` outside an allowlist,
so this cannot silently return. Note `bellman scan` also prints full crontab
command lines including local paths — add a note in `CLI.md` warning against
pasting raw scan output into public issues.

## H — README says Node 24, CI pins Node 20 (P1)

`README.md:59-72` instructs Node 24; `linux.yml:50`, `macos.yml:49` and
`windows.yml:33` all set `node-version: "20"`. Anyone trying to reproduce CI
gets contradictory instructions. Pick one, or state explicitly why they
differ.

## Refuted — do not "fix" this

**`patchelf` and `libgtk-3-dev` are NOT missing dependencies.** Both the
2026-07-31 review and this swarm reported them as required because they appear
in `linux.yml`'s install list. They are not required: a clean `ubuntu:24.04`
container installing **only** the nine packages the README lists built **both**
the `.deb` and the `.AppImage` successfully on 2026-07-31. Tauri's AppImage
bundler fetches its own tooling, and `libgtk-3-dev` arrives transitively with
`libwebkit2gtk-4.1-dev`. CI's extra two are belt-and-braces.

This is the second independent report of this non-issue. Adding them would
tell every user to install packages they do not need. If a future reviewer
raises it a third time, the answer is the same, and the clean-container build
is the evidence.

## Exit gate

Every item proven from a **user's** side, not only in tests — this project has
already shipped one fully-tested integration kit whose headline feature did not
work for a person (SCH2).

- A fire notification captured from a **real scheduled fire** matches the
  example in `INTEGRATION.md` field for field, including `schema` and
  `occurrence_kind`. Asserted by a golden-fixture test that fails if either
  drifts.
- `grep -r "bellman-fire" docs/` returns nothing outside a changelog note.
- A reader following `INTEGRATION.md`'s fire section can branch on
  `occurrence_kind` correctly on the first try — the warning box describes the
  two documents the right way round.
- The wizard's autostart hint no longer claims XDG autostart grants
  `CAP_WAKE_ALARM`, and what it does say matches
  `platform::wake`'s own `fix_hint`. Asserted by a test that fails if the two
  strings contradict, or by making the wizard render the hint **from** the
  platform module rather than restating it.
- **§Install runs verbatim, start to finish, in a clean `ubuntu:24.04`
  container** with nothing preinstalled beyond the base image — no bootstrap
  step of any kind. This is the test that would have caught it: the
  2026-07-31 verification passed only because the harness ran `apt-get update`
  and installed `curl` before starting.
- A Fedora container reaches a successful build following the new `dnf` table.
- `bellman add --action launch --command /usr/bin/true` produces a timer whose
  `status.json` shows the launch ran — no GUI involved.
- `LOCAL.md` and the README name both data directories, and a user can
  discover the active one from the GUI without reading either.
- `cargo test` runs on macOS and Windows in CI — or the README status table
  states plainly that it does not.
- `SECURITY.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` and issue/PR
  templates exist; `grep -rl "/home/sami" .` returns nothing; the CI gate
  fails a deliberately reintroduced personal path.
- Node guidance in the README and in all three workflows agrees.
- **Nothing in `crates/` changes the wire shapes**, and no required client
  library, schema layer or handshake is introduced — asserted by review
  against the standing decision in `CARD_INDEX.md`.
