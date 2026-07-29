Macro recorder GUI + the security model that gates it — design research

# What we want to build

Two things that only make sense together.

**A macro recorder page in Bellman's GUI.** A table of saved macros. A record button: the
user presses record, moves the mouse, clicks, types — Bellman captures the sequence into a
queue of steps. Stop, name it, save it. Each macro can then be attached to a timer, so it
runs on a schedule.

**A security gate in front of RUNNING them.** The gate sits on execution, not on the page.

- **Recording needs no password.** Recording only writes a file. A user can open the macro
  page, record, name, edit and save freely. Nothing has been done to the machine.
- **Running needs the password.** Replaying a macro, and letting a timer trigger one, is the
  dangerous act — that is what the Settings page gates, and turning it on requires a
  password that is protected rather than stored in the clear.

This split is a design decision from the operator, and it is a good one: treat it as fixed,
not as something to re-litigate. Note what it buys — the whole recorder UI can be built,
demoed and tested with no password anywhere, so the dev-bypass hole (A6) shrinks to the
execution path alone instead of wrapping the entire feature.

Bellman is a **cross-platform** app (Linux, macOS, Windows) built in **Tauri v2 + Rust +
Svelte 5**. Answer against that stack.

# Why this needs real design work

A macro types into whatever is focused and clicks wherever it is told. That is arbitrary
input injection — effectively code execution with the user's own privileges, on a schedule,
possibly while nobody is watching. Getting the gate wrong is worse than having no gate,
because a gate that looks secure and is not changes how much people trust the feature.

**Verify online and date your sources.** Crate APIs, OS permission models and KDF parameter
recommendations all move.

# Part A — the security model

## A1. Threat model FIRST — and be honest

Before proposing any mechanism, state what this actually defends against. Consider the
uncomfortable case: if an attacker can already write files as this user, they can run
anything as this user — so what does a password on a macro feature genuinely buy?

Plausible honest answers include: it stops another local process or agent from *silently*
arming a scheduled macro; it makes the dangerous feature a deliberate act rather than a
default; it protects against casual access to an unlocked machine. Those are real, limited
benefits. Say which ones apply and which do not. **Do not oversell it.** A design document
that claims more protection than it delivers is the failure mode here.

## A2. "Encrypt the password" — resolve the primitive

The request says encrypt the password. That phrase covers two different operations and the
design needs to be explicit about which it means:

- **Verifying** the password → you *hash* it with a password KDF and store the hash. You do
  not encrypt it, because encryption implies it can be turned back into the password, which
  is exactly what must not be possible.
- **Protecting the macros** → you *derive a key* from the password and encrypt the macro
  store with it. This is the one that actually protects something: without the password the
  macro definitions are unreadable, not merely un-runnable.

Recommend which of these Bellman needs — plausibly both. Name concrete algorithms and
parameters: Argon2id (state memory/time/parallelism for a 2026 desktop), and an AEAD for
the store. Evaluate the `argon2`, `chacha20poly1305`, `age`, and `zeroize` crates.

A gate that only refuses to *run* macros, while the macro file sits in plain JSON next to
it, is a UI lock and should be described as one.

## A3. The unattended paradox — the hard question

Bellman is a scheduler. A timer fires at 03:00 with nobody at the keyboard. If macros
require a password, **who enters it?**

This is the central tension and the design lives or dies on it. Work through the options:

- Unlock once per session at login, hold the derived key in memory (`zeroize` on lock/exit)
  — what happens after a reboot at 02:00?
- Store the key in the OS keyring so the operating system guards it — Secret Service
  (Linux), Credential Manager (Windows), Keychain (macOS), via the `keyring` crate. What
  are the real unlock semantics on each, headless and at boot?
- Require Bellman to be running and unlocked, and simply refuse (and log) otherwise.
- A per-macro trust level, where only explicitly trusted macros run unattended.

Give a recommendation and state plainly what it costs the user.

## A4. Key handling and recovery

- Key lifetime in memory, re-lock policy, `zeroize`.
- Rate limiting / lockout on repeated wrong passwords.
- **Forgotten password**: is there recovery, or is it data loss? Whichever it is, it must be
  stated to the user *before* they set the password. No silent surprises.
- Where does the salt/verifier live relative to the macro store, and what does a backup or
  a machine migration do to it?

## A5. Never record a secret

If the user types a password while recording, it lands in the macro file. Can the recorder
detect a focused password field on each OS and pause? If not on some platform, what is the
honest mitigation — a warning, a manual pause key, redaction on review?

## A6. The development bypass — required, and it must not ship

Because of the record/run split above, the recorder page itself needs NO bypass — it is
unlocked by design. The hole is only needed for the **execution** path, so that crews and
agents can test replay and timer-triggering without a human typing a password. There, the
gate defaults to ENABLED during development.

Keep the hole as small as the split allows: it covers replay and timer-trigger only, and it
must not also unlock anything else.

That is a deliberate hole, and holes like it are famous for reaching production. Design it
so it *cannot*:

- A **compile-time** cargo feature, not a runtime env var or config flag.
- Release builds refuse to build with it enabled (a `build.rs` or CI guard).
- A loud, permanent, impossible-to-miss banner in the UI whenever it is active.
- A CI check that shipped artifacts do not contain it.

Name the mechanism precisely. "We'll remember to turn it off" is not an answer.

## A7. Audit

Every macro run — armed, started, finished, failed, refused — into Bellman's existing JSONL
event log. What must be recorded for a user to reconstruct what their machine did overnight?

# Part B — the recorder and its GUI

## B1. What gets captured

Mouse movement, clicks (left/right/middle, double), drags as single gestures, scroll,
keystrokes with modifiers, and the natural pauses between them. Which Rust crates do global
input **capture** cross-platform — evaluate `rdev`, `inputbot`, `device_query` — and which
do input **injection** for replay — evaluate `enigo`. State each crate's maintenance status,
platform gaps, and whether Wayland is supported at all (expect not; `ydotool` and uinput are
the usual escape hatch, and they need permissions).

## B2. Two recording modes

We want both, and they should be named clearly:

- **Full replay** — every movement with timing. Faithful, large, fragile, hard to edit.
- **Steps** — the meaningful clicks and keys in order, with waits between. Smaller, editable,
  agent-readable.

Recommend which is the default and why.

## B3. Recorder UX — study the prior art, do not invent it

This problem is solved. Read how, then implement independently:

| Project | Read it for |
|---|---|
| https://github.com/SeleniumHQ/selenium-ide | The canonical record → editable step list → replay loop |
| https://playwright.dev/docs/codegen | Modern recorder producing readable, editable steps |
| https://github.com/RMPR/atbswp | Small cross-platform desktop recorder — the closest shape to this |
| https://github.com/Pulover/PuloversMacroCreator | Mature GUI macro recorder: table, step editing, hotkeys |
| https://www.autohotkey.com/ | Windows macro scripting — the vocabulary users already know |
| https://github.com/RaiMan/SikuliX1 | Image-based targeting instead of coordinates |
| https://github.com/aisingapore/TagUI | RPA flows mixing coordinates, images and OCR |
| https://github.com/enigo-rs/enigo · https://github.com/Narsil/rdev | The likely Rust dependencies (injection / capture) |
| https://github.com/hwchen/keyring-rs | OS keyring access for A3 |
| Keyboard Maestro (macOS, commercial) | UX reference only — how a polished macro table reads |

For each: what did it get right, and where did it give up? Prior art we read, not code we
copy.

## B4. The page itself

Table columns, how a macro's linked timer is shown, how steps are edited (delete a step,
insert a wait, re-record just one step), and how the user is *always* aware recording is
live — a persistent indicator, not a subtle one.

## B5. Safety rails the recorder and player both need

- A global panic key that stops a running macro instantly.
- Maximum run time and maximum step count.
- Never run two macros at once.
- A dry-run / step-through before a macro is ever attached to a timer.
- Recorded coordinates break when resolution or window position changes — say how this
  design copes, and note that a parallel study is running on overlay grids versus image
  anchoring for exactly this problem.

# Deliverable — `research.md` in YOUR OWN folder

- The threat model, stated plainly, including what this does NOT protect against.
- An explicit statement of where the record/run boundary is enforced in the code — one
  chokepoint that every execution path goes through, not a check repeated in five places.
- A concrete security design: algorithms, parameters, crates, where each secret lives.
- Your answer to the unattended paradox, with its cost to the user.
- The dev-bypass mechanism, and the guard that stops it shipping.
- Crate recommendations for capture and injection, with per-OS gaps called out.
- A recorder UX design grounded in the prior art above.
- A card breakdown: what is card 1, card 2, card 3, in build order.
- Risks and open questions.

# Acceptance

- Every crate and API claim carries a dated citation; every KDF parameter is justified.
- The unattended-execution question is answered concretely, not deferred.
- The honest limits of the password gate are stated, not glossed.
- Wayland is addressed explicitly rather than assumed to work.
- The synthesiser also writes `synthesis.md`: a disagreement table across all four reports
  and a single recommended design with the card breakdown.
