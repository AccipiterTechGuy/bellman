Macro recorder GUI + the security model that gates it

# SETTLED DECISIONS (operator, 2026-07-29) — build to these

The four-way research below has completed; both syntheses are in
`Research_from_Crew/macro-recorder-gui-the-security-model-th_research_2026-07-29_195350/`.
The decisions in this section are the operator's and are **not open for re-litigation** by
an implementing crew. Everything after them is the original research brief, kept for
context.

## D-1. The gate sits on RUNNING, not on the page

A human at the GUI records freely — recording only writes a file, nothing touches the
machine. Replaying a macro, and letting a timer trigger one, is the dangerous act, and that
is what the master password gates.

## D-2. Capability tokens, not passwords

Alongside the master password, Bellman issues **capability tokens**. They are called tokens
deliberately: calling them passwords invites password-shaped implementation mistakes.

- Setting the master password mints **one list of 10 tokens, displayed exactly once**
  (the familiar 2FA backup-code pattern).
- Expiry is configurable in Settings. Each token is **single-use**.
- A token authorises **recording only. Never running.** This must be enforced by the type
  system, not by a comment — a token that can start a replay is a hole straight through the
  gate.
- Minting more tokens, or regenerating the list, requires the master password.
- Revocation: one action burns every outstanding token.

## D-3. The skill documents the procedure; the human supplies the credential

An agent is given a **skill** describing how macro recording works and in what order to do
it. **The skill never contains a token.** The operator pastes a token into the conversation
at the moment of use.

This is the load-bearing property of the whole design: an agent that reads the skill file
learns *how* and gains no *capability*. Do not "improve" this by having the tool fetch a
token for itself — that would collapse documentation and credential back into one thing.

Accepted consequence: a token pasted into a chat is in that transcript on disk forever.
That is exactly why single-use matters. Treat every token as public the moment it is used.

## D-4. Agent-authored macros are UNREVIEWED until a human blesses them

Granting a token authorises a recording *session* — it is not approval of what the session
produced. A macro recorded under a token is stamped **agent-authored, unreviewed**, and in
that state:

- it cannot be run, and
- it cannot be attached to a timer,

until the operator opens it and explicitly approves it. Recording and blessing are two
separate human acts. Without this, a token grant silently becomes consent to whatever the
agent chose to write, which is the exact risk the token system exists to remove.

Each macro carries its provenance: who recorded it, under which token, when, and whether it
has been reviewed.

## D-5. Each token is stored in TWO forms

They serve different jobs and one form cannot do both:

| form | purpose | must work when |
|---|---|---|
| **hash** | verifying a presented token | the store is **locked** — an agent uses a token when nobody is present to unlock anything |
| **encrypted copy** | re-displaying the list to the operator | after master-password unlock, in Settings |

Hash-only would mean the list is shown once and gone forever. Encrypted-only would mean a
token cannot be checked unless the master is already unlocked, which defeats the point.

Stored beside each token, as non-secret metadata: scope, expiry, and a **used-flag that
survives a restart** — otherwise a crash lets a spent token replay. Expiry must not trust
the wall clock alone; reuse the scheduler's existing clock-jump detector.

## D-6. No arbitrary typing primitive for agents in v1

`bellman type "..."` is the most dangerous primitive in the feature — arbitrary command
execution, one focused terminal away. In v1 it is **not exposed to agents**. Humans type;
agents replay macros a human recorded and approved.

This is capability minimisation, and it is the real control the operator was reaching for
when the virtual-keyboard overlay was first proposed. The overlay was dropped (3-1 in
research) because a drawn keyboard is no harder for an attacker to read than a real one —
it adds friction to the honest path and none to the attack path. The gate is what provides
the safety, not the picture.

## D-8. Answers to the research's operator questions (2026-07-29)

**Q2 — may the slot channel trigger macros? YES, but only ALREADY-RECORDED ones.**
A slot may fire an existing, human-approved macro. It may **never create, record or modify**
one. Two controls make this safe enough to hold:
- a **per-macro `allow_slot_trigger` flag**, off by default — a macro is slot-triggerable
  only because the operator said so for that macro;
- the **target-window check** must pass before any input is sent. A local app can otherwise
  fire an approved macro at a moment of its choosing, with the wrong window focused, and
  approved-content-at-the-wrong-time is the residual risk of allowing this at all.

**Q3 — may the CLI unlock? NO. It presents an execution token instead.**
The master password never reaches the CLI, so it can never end up in a shell script.

This creates a token scope the earlier decisions did not have, so it is bounded here:

| scope | what it permits | limits |
|---|---|---|
| `record` | start/stop a recording session | single-use, expiry from Settings |
| `run:<macro-id>` | run **one named macro**, once | single-use, short TTL, names exactly one macro |

**There is no `run:*` token, ever.** A blanket run token is the master password with a
shorter life. If a caller wants to run three macros it presents three tokens.

**Q4 — no recovery code.** Accepted, with one correction the operator's reasoning invites:
modifying the code can remove the *gate*, but it cannot decrypt *data* — no code change
recovers a store whose key is gone.

That is less alarming than it sounds here, because the DEK's home is the OS keyring: losing
the master password does **not** lose the macros while the keyring is intact. It bites on
migration or restore to a new machine, which is exactly where the backup wrap would have
been used. So: no recovery code, and the setup wizard must say plainly that the password is
needed to move macros to another machine.

**Q5 — naming.** `execution password` (the master) and `execution token` (the temporary,
scoped, single-use ones). Reserve the "execution" prefix so a second gated feature later
does not force a rename.

**Q7 — audit Bellman's existing interfaces first. YES.** Confirm the real JSONL schema,
timer ownership, single-instance policy, app-data paths and the Tauri capability set before
any file or module name is fixed in code.

**Q8 — independent pre-ship security review. YES.** File format, atomic replace and
rollback, the feature guards, the private-capability pattern, plus property/fuzz tests.
Before M8 ships.

### Scheduling (operator)

**Finish the current Bellman work before any macro card is trained.** Macro and grid cards
are to be **created only** — minted and specced, never sealed onto the rails — until the
cards already on the board have shipped.

### Still open

- **Q1 — Wayland.** Superseded in part: see the compose/capture split (D-9, pending). Replay
  on Wayland is still unresolved.
- **Q6 — will macros ever have loops and conditionals?** Unanswered. If yes, the security
  model needs revisiting from first principles: review, step caps and audit hashes all stop
  meaning anything once a macro can branch.

## D-9. Loops: a fixed count is allowed, a condition is never (answers Q6)

**Allowed: repeat N times, where N is a plain integer typed by a human.**

A fixed count keeps every security property intact — review still shows exactly what will
happen, the step and time caps are computable before the run, the audit hash still covers
the whole thing, and it cannot run forever.

**Never allowed: conditions.** `repeat until…`, `if…then…`. Once a macro can branch, nobody
can say what it will do, review becomes guesswork, and the step cap is the only thing left
between the operator and a runaway.

**The count is an integer, not an expression.** No arithmetic, no variables, no
`rows.count`. An expression needs evaluation, evaluation needs a parser, and that is the
Turing-complete door — the same door whether it is reached via `while` or via maths.

Requirements:

- **The count belongs to the reviewed macro.** No caller — slot, timer or CLI — may override
  it at trigger time. Otherwise something passes `repeat=100000` and an approved macro
  becomes a weapon. It is content, and the audit hash covers it.
- **Hard ceiling** on N, configurable up to a hard limit, never beyond.
- **Caps multiply**: max steps and max runtime are checked as `steps × N`, not per iteration.
- **Delay between iterations**, with a minimum — applications need time to settle.
- **Stop on failure by default.** If iteration 2 fails, 3/4/5 do not run.
- **The panic key aborts the whole loop**, not the current iteration.
- **Dry-run expands it** — "40 actions, about 12 seconds", not "5 iterations".
- **No nesting.** Flat loops only.

Timers remain the right tool for spread-out repetition (the scheduler works in seconds);
in-macro repeat is for tight sequences.

## D-10. While a macro runs, the operator loses their machine

Input injection means the macro owns the mouse and keyboard, and the human does not. This is
inherent, not a defect — so run duration is a **usability** limit, not only a safety cap.

- **Default total runtime cap measured in seconds, not minutes.** A ten-minute macro locks
  the operator out for ten minutes.
- **The review screen states the duration**: "this will take about 12 seconds, during which
  you cannot use the machine." D-9's repeat count is exactly where this becomes non-obvious.
- **Timer-triggered macros default to idle-only** — the failure case is one firing at 3pm
  in the middle of an email.
- **A visible countdown with the abort key shown on screen** for the whole run, so taking
  the machine back never requires remembering anything.
- **The panic key must keep working during injection.** It must not be swallowed by our own
  synthetic events, and must not be triggered by them. Prove it with a test; the research
  notes `enigo`'s event-marking is documented only on Windows and macOS.

## D-11. Safe caps live in Settings; a macro may ask for less, never more

The limits are **policy**, not per-macro content. They sit in Settings behind the execution
password: max runtime, max steps, max repeat count, minimum delay between actions. Defaults
are deliberately tight — runtime in **seconds**.

- A macro carries its own budget, which must be **≤ the Settings ceiling**. It may ask for
  less. It can never ask for more.
- Raising a ceiling is an authenticated act at the Settings page. Nothing else can raise it:
  not a macro, not a slot, not the CLI, not an agent holding a token.
- At run time the **lower of the two wins**.

The property this buys: an agent-authored macro cannot grant itself more of the operator's
machine. Combined with D-4 (unreviewed until blessed) and D-9 (count is reviewed content),
there is no path by which anything but a human at Settings extends how long the machine can
be taken away.

Enforcement is two-sided, because an estimate is not a guarantee:

- **Pre-flight**: refuse to start if the estimated duration exceeds the budget.
- **Mid-run**: hard-abort the moment actual elapsed time exceeds it — a single step can hang
  and no estimate catches that.

On a cap abort: release every held modifier, stop the loop entirely, and log the run as
**aborted-on-cap**, never as completed. A half-run macro that reads as successful in the
audit log is worse than one that reads as failed.

Cap changes are themselves audited — "the runtime ceiling was raised to 10 minutes" is
exactly the line an operator wants to find when reconstructing what their machine did.

## D-12. Two ways to author a macro: COMPOSE (default) and CAPTURE (where permitted)

The research assumed authoring means capturing live desktop input. It does not have to, and
the alternative is both safer and more portable.

**Compose — the default, available everywhere.**
Screenshot the desktop → the operator clicks *on the screenshot, inside Bellman's own
window* to pick a target → types text into a Bellman field. Clicking inside one's own window
and typing into one's own text box requires **no permission on any operating system**.

What this deletes outright:

- The global input capture requirement — and with it the `input`-group / evdev grant that
  reads all system input. Bellman is not keylogger-shaped in this mode.
- **The entire secret-capture problem (A5).** If Bellman never watches global keystrokes it
  can never accidentally record a password. Password-field detection, Windows UIA
  `IsPassword`, macOS `AXSecureTextField` — all unnecessary here, and with them the
  `IsPassword` factual conflict that blocked M6.
- The dependency on a capture crate on the most security-sensitive path (risk 5), and with
  it the unresolved question of whether `rdev` is maintained.
- The Wayland blocker **for authoring**. Screenshots go through the portal with consent;
  everything else happens inside Bellman's window.

**Capture — faster, only where the OS allows it.**
Record what the operator actually does. Far quicker for a real workflow, and it stays
worth having on X11, Windows and macOS. It carries every cost above, so it is opt-in and
never the default.

**Compose is the universal floor. Capture is the convenience where the platform permits.**
Same shape the grid research landed on, and for the same reason.

**Unchanged: replay.** Running a macro still injects input into other windows, and Wayland
restricts that however the macro was authored. Composing solves authoring, not execution.
The sanctioned consent-prompt route used by remote-desktop tools is unverified — check it
before claiming Wayland support of any kind for replay.

### Effect on the card ladder

- **M2 (capture)** shrinks: compose ships first and needs no per-OS capture layer at all.
  Capture becomes the second, optional half.
- **M6 (secrets / password-field awareness)** mostly disappears for the compose path. It is
  only needed once capture ships, and only on the platforms capture supports.
- Compose depends on the screenshot work in the grid card — build them in that order.

## D-7. Honest scope of what this protects

This is **agent containment**, not anti-malware. An attacker who already has code execution
as this user can act directly and none of the above stops them. What it does buy:

- an agent cannot compose novel input, only trigger approved macros;
- an agent cannot arm a schedule without a human review step;
- every action is attributable to a token and a timestamp.

Those are real and worth building. Do not describe them in the UI as more than they are.

---


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
