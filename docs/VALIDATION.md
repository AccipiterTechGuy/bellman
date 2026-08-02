# Bellman — full-system validation (C11)

Run: 2026-08-01/02, branch `train/2026-08-01_0005`, on a Linux Mint 22.3
x86_64 desktop (kernel 7.0.0-28, systemd 255, rustc 1.97.1, Node 24.13.0,
tauri-cli 2.11.4). Container work used Docker 29.6.2 with `ubuntu:24.04`,
`fedora:latest` and `archlinux:latest`.

**Two rules governed this card and were kept:**

1. **A clock did the firing.** No evidence below comes from `bellman run-now`.
   Every scheduled fire was produced by the running desktop app's own
   scheduler on the real wall clock. There is no `fired: manual` line in this
   document, because no scenario needed one.
2. **The user-facing parts were done as a stranger would**, and the friction
   is written down in *[Walkthrough](#walkthrough--what-this-was-like-to-use)*,
   including the parts that resolved fine.

Raw evidence (JSON captured by each run) is under
[`docs/qa-c11/`](qa-c11/) — one file per scenario:
`kinds`, `misfire`, `prune`, `reply`, `apps`, `crud`, `datadirs`,
`dst_gap`, `dst_fold`, `deb_packaged_demo`, plus the originality sweep and
its script, and the Perl client. Originality is a separate document:
[`docs/ORIGINALITY.md`](ORIGINALITY.md).

## Summary

| # | Area | Result |
|---|---|---|
| 1 | Code health — fmt / clippy / tests / vitest | **PASS** — fmt, clippy, rustdoc and 113 JS tests clean; 487 Rust tests, **0 ignored, 0 skipped**, green on 25 consecutive full-workspace runs. Getting there cost a third product bug: a lease lost to a forking sibling thread ([FLK1](todo/cards/FLK1_publisher_lease_test_parallelism.md)) |
| 2 | All 7 occurrence kinds firing on their own schedule | **PASS** — every one within 8 ms of `scheduled_for` |
| 2 | Misfire across a real stop/start | **PASS after a fix** — `skip` was silent in the log |
| 2 | DST gap (spring forward), real clock | **PASS** |
| 2 | DST fold (fall back), real clock | **PASS** |
| 2 | Wake from sleep on real hardware | **NOT TESTED** — see [Not tested, and why](#not-tested-and-why) |
| 2 | Pruner: rotation, gzip archives, retention, byte budget | **PASS** |
| 3 | A real app woken by a scheduled fire, over both transports | **PASS after a fix** — the IPC transport could never be selected |
| 3 | GUI demo, driven by clicking, from a scheduled fire | **PASS** |
| 3 | Pickup grace / `no_ack` / late-reply revision | **PASS** |
| 3 | Opt-in watchdog + heartbeat extension | **PASS** |
| 3 | Rejection + quarantine under a live watcher (5 cases) | **PASS** |
| 3 | Slot CRUD live, no restart (SCH2) | **PASS** |
| 3 | A client in a language the docs do not cover | **PASS** (Perl) |
| 4 | README §Install verbatim on Ubuntu, Fedora and Arch | **FAILED, then fixed** — three defects (`sudo`/identity, `libxdo`, `linuxdeploy`); all four runs then literal in clean containers — three distros × root, plus Ubuntu as an ordinary user — **PASS** |
| 4 | `.deb` installs; demos ship at the wizard's path; demo runs | **PASS** |
| 4 | First-run wizard on a clean session, demo offer | **PASS** |
| 4 | Both data directories | **PASS**, with a documentation gap |
| 4 | Windows / macOS | **PARTIAL** — see [Not tested, and why](#not-tested-and-why) |
| 5 | Originality sweep | **PASS** — [ORIGINALITY.md](ORIGINALITY.md) |
| 6 | Polish: personal-path gate, naming, dead code, **doc coverage** | **PASS** — 756 undocumented public items cleared, `#![warn(missing_docs)]` landed. The sweep also broke a frozen wire shape and had to be repaired: see D12 |
| 7 | The diff changes no wire shape | **FAILED, then fixed** — the doc sweep silently dropped a `serde` attribute on `bellman-slot/1`. Caught by the supervisor, not by me. See D12 |

Twelve defects were found and fixed on this card; each functional one has a
regression test that was verified to fail without its fix. **D12 was caused
by this card**, not merely found by it:

| # | Defect | Fix |
|---|---|---|
| D1 | A timer with `transport.mode: "ipc"` / `"auto"` could **never** be delivered over the socket from a scheduled fire — the scheduler's own reply engine was built with `ipc: None` | `70349af` |
| D2 | A misfire that policy **skipped** was logged as nothing at all, contradicting PLAN.md and INTEGRATION.md | `a8cd9c9` |
| D3 | README's Arch recipe names a package (`libxdo`) that does not exist; the build command aborts on Fedora and Arch (`linuxdeploy` vs `.relr.dyn`) | `e151f11` |
| D4 | Three documentation gaps: undocumented `config.json` clamps, and three slot payload fields missing from the INTEGRATION.md table | `0e65c98` |
| D5 | The **one background watcher** (reply ingest + slot channel + event publisher) exits at startup if `<data dir>/timers/` does not exist yet, and the app carries on looking healthy | see below |
| D6 | An unparseable system timezone name aborts startup maintenance, so `system.prune` is never created and nothing ever prunes | see below |
| D8 | **Two concurrent producers could be handed the same slot name**, so one app's request was silently destroyed — both told `Ok`, nothing quarantined, the timer simply never appeared | see §3 |
| D9 | The replenisher wrote free stubs with a *replacing* write after an `exists()` check, which could land on top of a request published onto a just-claimed name | see §3 |
| D10 | The `EventPublisher` lease could be lost to a **sibling thread's `fork`** — `fork(2)` copies the fd table and `O_CLOEXEC` fires only at `exec(2)`, so a child briefly holds a duplicate of the description the `flock` belongs to, and an election refused by nobody skipped a maintenance round | see below |
| D11 | Reply files were opened through `rustix::fs::open` **without `O_CLOEXEC`** (unlike `std`, which sets it for you), leaking that descriptor into every command Bellman launches — and those commands come from user timer configuration | see below |
| D12 | **Self-inflicted.** The doc sweep dropped `#[serde(default, skip_serializing_if = "Option::is_none")]` from `SlotResponse::timer_id`, so every rejection written to `slots/done/` emitted `"timer_id": null` where the key had been absent — a change to a **frozen** wire shape | see below |
| D7 | README §Install said nothing about who runs what: its first command assumes `sudo`, which `ubuntu:24.04` and `archlinux:latest` do not ship, so the recipe stopped before installing anything; and it had no guidance on `sudo` for steps 2–5 or on running `pacman` unattended | see §4 |

The walkthrough in §4 and *[Walkthrough](#walkthrough--what-this-was-like-to-use)*
records how each was found, including the one this card's first pass papered
over.

---

## 1 — Code health

### `cargo fmt`

**Was failing.** `cargo fmt --check` reported **538 diffs across 100 files** —
essentially the whole workspace. CI never noticed because `linux.yml` did not
run the check, and carried a comment saying so ("Pre-existing sources are not
rustfmt-clean"). C11's exit gate requires it, so the tree was formatted
(`cargo fmt --all`, no behavioural change) and the gate added to CI so it
cannot drift back. Commit `831ad3e`.

```
$ cargo fmt --all --check ; echo $?
0
```

### The suite has to be repeatably green, not green once

`cargo test --workspace --all-targets` was **not** stable. Run in a loop it
failed roughly three times in twenty, in four different places. Two of the
four turned out to be the product, not the tests:

| symptom | what it actually was |
|---|---|
| `slots::tests::concurrent_producers_all_get_unique_slots` — *expected 8 timers, got 7* | a **real product race**: two producers handed the same slot name, one app's request silently destroyed. Written up as D8 in [§3](#two-apps-publishing-at-once--found-a-silent-request-loss) |
| `pruner::tests::prune_rotates_jsonl_and_respects_retention_edges` — *non-empty current should rotate* | a test defect: it ignored the return of the publish cycle that seeds the log, so a publish that did not land failed later with a message pointing at the pruner. The one-second retention it configured also raced the archive it had just created |
| six `sch1_dispatcher` tests failing at once, five of them at the same `.lock().unwrap()` | **one** failure, amplified. Those tests serialise on a mutex; a genuine timeout poisoned it and every later test in the binary then panicked on the unwrap. Six red tests reporting one problem, with the real one buried. The guard only orders the tests, so it now recovers from poisoning — one failure reports as one failure |
| `pruner::tests::prune_with_lease_elsewhere_reports_skipped_and_never_stamps_last_prune` — *assertion failed: !report.skipped_not_leader* | **the product again**, D10: the election was refused by a lock nothing held. Chased in full below — it took the longest of the four and was the only one that first looked like the test being unreasonable |

The first and the last were fixed in the product. The second was fixed in the
test: the seeding cycle's result is now **asserted**, with the failure message
naming the real cause (election lost, or an I/O error) instead of letting an
empty log surface later as "non-empty current should rotate" and blame the
pruner; the retention window matches the policy under test, so age retention
is exercised by the 90-day-old file it plants rather than by a stopwatch.

Both of these tests briefly grew bounded retry loops while the cause of the
fourth symptom was unknown. **Both were removed once D10 was fixed** — they
were losing to D10 and nothing else, and the one on the post-release prune
was actively hiding it. Every assertion in the suite is now at least as
strict as it was when this card opened, and two are stricter.

Measured, not asserted:

| `cargo test --workspace --all-targets` | failures |
|---|---|
| before this card's fixes | 3 in 20 runs, three different tests |
| after the slot and test fixes, before the lease fix | 1 in 20 runs, always the same one |
| `-- --test-threads=1` (before the lease fix) | 0 in 3 runs (366/366 each) |
| **after the lease fix** | **0 in 25 consecutive runs** |

**The publisher-lease flake — the mechanism, and a third product bug.** This
one took real instrumenting. The test has a "live watcher" publisher hold the
election lease, checks that a prune correctly reports `skipped_not_leader`
and does not stamp `last_prune`, then releases the lease and asserts the
*next* prune wins it. Roughly one full-suite run in twenty, it did not — at
`pruner/tests.rs:129`, `test setup: prune must hold the publisher lease`:

```
DIAG2 leader=true published=1 error=None pending=Ok(0) cur_len=Ok(166)
DIAG3 lock_free=Ok(false) watcher_is_leader=false     ← released, still locked
assertion failed: !report.skipped_not_leader
```

The seed published cleanly and the holder had released — yet the `flock` on
`publisher.lock` was still not acquirable. Every publisher in the suite uses
its own temp directory, so no other test shares that path, and 80 isolated
runs of this test never reproduced it; it needs the full suite.

Instrumenting `gate::try_acquire_file` to scan **every** process's
`/proc/*/fd` on `WOULDBLOCK` — not just our own, since an inherited fd in a
child would be invisible in `/proc/self/fd` — produced the decisive clue:

```
DIAGLOCK path=/tmp/.tmpfeHr3X/logs/publisher.lock dev=66309 ino=7623784 nlink=1 me=2572637
  pid=2572637 (bellman_core-d2) fd="31" -> /tmp/.tmpfeHr3X/logs/publisher.lock same_inode=true
  pid=2572637 (bellman_core-d2) fd="34" -> /tmp/.tmpyZjov3/logs/publisher.lock same_inode=false
```

`nlink=1` rules out hard links. Across the entire process table exactly one
fd points at the contended inode — fd 31, the one `try_acquire_file` had just
opened itself. **Nothing on the machine held that lock, and the kernel still
said `EWOULDBLOCK`.** That is only possible if the holder had gone away
between the failed `flock` and the scan.

It had. The cause is a POSIX behaviour rather than anything in Bellman:

> **`fork(2)` copies the file-descriptor table, and `O_CLOEXEC` is honoured
> at `exec(2)`, not at `fork(2)`. An `flock` belongs to the open file
> description, not to the descriptor.** So between fork and exec a brand-new
> child holds a duplicate of every lock its parent holds — including one
> another thread is about to release — and anyone asking for that lock in
> that window is refused by a "holder" that has no interest in it.

Bellman forks constantly (every launch action, the demo, the wake helper) and
the test binary runs 366 of those at once. Isolated outside the codebase,
with **no competing lock holder anywhere in the program** — the only
lock/unlock is the measuring loop, which always releases before it asks
again, so every failure is spurious by construction:

| another thread spawning children | attempts | spurious `EWOULDBLOCK` |
|---|---|---|
| no | 5,234,658 | **0** (0.000%) |
| yes | 7,564,851 | **499,234** (7.25 / 7.49 / 7.46 %) |

**Can this happen in the product?** `FLK1` asked that first, and it is really
two questions. They have different answers, so they are answered separately —
an earlier revision of this paragraph gave a flat "no" and then described a
real production effect two sentences later, which is a contradiction and is
corrected here.

> **Does the race occur outside the test suite? YES.** Nothing about it is
> test-specific. A Bellman that fires a launch action, opens the demo or runs
> the wake helper is forking, and any election in that window can be refused
> by a lock nobody holds. The effect is bounded: one skipped maintenance
> round. The pruner already handles that correctly — it reports
> `skipped_not_leader`, does **not** stamp `last_prune`, and the next tick
> retries — so it is self-healing rather than damaging. Bounded and
> self-healing is still a product defect, which is why D10 is filed as one and
> fixed in `reply::gate` rather than in the tests.
>
> **Can a child retain the lease for as long as it runs? NO** — and this is
> the part `FLK1` feared and got wrong. The exposure ends at `exec`, which is
> where `O_CLOEXEC` fires, not when the child exits. Hold the lease, spawn a
> child that sleeps five seconds, release the lease: the lease is free again
> after **~6µs** (7.397 / 5.584 / 6.167µs over three runs), not after five
> seconds. A Bellman spawning a wake action does not starve every other
> publisher for the life of that child.

Every spawn path was audited to be sure nothing widens the window:
`actions/launch.rs` uses `pre_exec` only to `setsid()`, `demo.rs` only sets a
process group, and nothing anywhere uses `dup2`, forks without exec, or hands
a descriptor to a child deliberately.

**The fix is in the layer that owns the problem.** `try_acquire_file` now
retries for a bounded 100 ms rather than asking once — orders of magnitude
longer than a fork→exec window, orders of magnitude shorter than a real
leader's hold (an entire publish cycle), so genuine contention is still
reported promptly and correctly as a follower. Each attempt reopens the file,
so a description inherited before we arrived cannot be the one under test.
`reply::gate::tests::election_is_not_lost_to_a_concurrent_forks_inherited_fd`
spawns children on one thread while electing on another and asserts zero
spurious follower reports. A test like that can pass for the wrong reason —
spawn nothing and there is no race to lose — so it refuses to: elections do
not begin until children are demonstrably being produced, the loop runs until
at least 20 have overlapped it, and the spawn count and any spawn error are
asserted **before** the zero is believed. Verified three ways:

| the test, run against | result |
|---|---|
| the fix as written | green |
| the retry removed | **294 of 3000** elections spurious |
| `Command::new` pointed at a path that cannot exist | *"cannot spawn children, so the fork race cannot be exercised"* — fails loudly rather than passing vacuously |

**No assertion was weakened.** `pruner/tests.rs:129` still demands the prune
win its election outright, and it now does. An earlier revision of this
document said the test had been changed to retry the prune until it won —
that change was reverted once the cause was known, because retrying in the
test would have hidden exactly the bug below.

**The audit turned up a second defect (D11).** `reply/watcher.rs` opened
reply files with `rustix::fs::open`, which passes its flags through verbatim
— unlike `std::fs::File::open`, which sets `O_CLOEXEC` for you. That
descriptor was therefore inherited by every process Bellman launches, and
those are **user-supplied commands from timer configuration**. Fixed by
adding `OFlags::CLOEXEC`. It is the only `rustix::fs::open` in the tree.

Its regression test asserts the behaviour rather than the flag: the open goes
through `open_reply_file` — the exact call `read_reply_file` makes, split out
so the test cannot drift from the product — the handle is held open across a
spawn, and the child is asked which descriptors it actually ended up with
after `exec`. With `OFlags::CLOEXEC` removed the child hands the path back:

```
$ cargo test -p bellman-core --lib -- reply::watcher::cloexec   # CLOEXEC removed
a launched command inherited the reply-file descriptor
/tmp/.tmpwi9P0h/reply-cloexec-probe.json; it saw:
/dev/null
pipe:[18602305]
/dev/null
/tmp/.tmpwi9P0h/reply-cloexec-probe.json
```

The dispatcher timeout that started the poison cascade was not itself
reproducible on an idle machine: it happened while four container builds were saturating
all sixteen cores, against assertions that already allow 10–30 s. Timeouts
that generous were left alone — loosening a test to survive a load spike I
created myself would be weakening the gate, not fixing it.

An earlier revision of this document claimed twenty consecutive green runs
before the lease fix existed; that sentence was left behind by a later
measurement that contradicted it, and it was removed rather than reconciled.
The table at the top of this section is the measurement, and it now ends
green for a reason that is named.

### The doc sweep broke a frozen wire shape (D12)

The worst finding on this card is one this card created. `cc32b0a`, the
"document every public item" sweep, inserted a doc comment above
`SlotResponse::timer_id` and in doing so dropped the attribute that was
sitting there:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub timer_id: Option<Uuid>,
```

Every rejection Bellman writes into `slots/done/` has since carried a key
that used to be absent:

```
expected (base f5b56de) {"schema":"bellman-slot/1",…,"status":"error","error":"bad","events":[]}
observed (before fix)   {"schema":"bellman-slot/1",…,"status":"error","timer_id":null,"error":"bad","events":[]}
```

`bellman-slot/1` is frozen by CARD_INDEX's standing decision, and this card's
own exit gate says nothing in the diff may change a wire shape. Reading was
never affected — serde still defaults the field on parse — but emitting is
exactly the surface the freeze is about. **My handoff and this document both
stated no wire shape had changed. That was wrong, and the supervisor caught
it, not me.**

Nothing caught it because `json_shapes.rs` only ever built the accepted case
through `sample_slot_response()`; the branch where every `Option` is `None`
had no coverage at all. There is now a rejection sample in the shared shape
list and a golden test that asserts the emitted object **key for key**, so an
absent key and a `null` one are not the same answer. Verified red with the
attribute removed:

```
assertion `left == right` failed: the bellman-slot/1 rejection shape changed
  left: Object {…, "status": String("error"), "timer_id": Null}
```

Because the cause was a mechanical sweep rather than a considered edit, the
same class of error was audited across the whole card rather than at the one
site: **every `#[…]` attribute present at `f5b56de` is still present at HEAD,
across all 117 changed `.rs` files.** `SlotResponse::timer_id` was the only
one lost. (A first pass flagged `web.rs`'s `WebActionDto` attribute too; that
was rustfmt wrapping it across lines, not a loss.)

### clippy, tests, frontend

```
$ cargo clippy --workspace --all-targets -- -D warnings ; echo $?
0                                   # 0 warnings

$ cargo test --workspace --all-targets
… 487 passed; 0 failed; 0 ignored   # summed across 15 test binaries.
                                    # 478 before this card; +9 regression
                                    # tests. 25 consecutive runs, all green
                                    # — see the measurement above.

$ cargo test --workspace --doc
0 passed; 0 failed; 0 ignored       # there are no doctests

$ npm --prefix ui test
Test Files  7 passed (7)
     Tests  113 passed (113)
```

### Skipped / ignored tests

**There are none in the suite.** `grep -rn '#\[ignore' crates src-tauri tests`
returns nothing; `grep -rn '\.skip\|\.todo\|xit(' ui/src` returns nothing.
Every test that exists runs on every invocation.

There is exactly **one place where tests are deliberately not run**, and it is
in CI rather than in the suite:

| where | what is skipped | why | consequence |
|---|---|---|---|
| `.github/workflows/windows.yml` | `cargo test --workspace --all-targets --exclude bellman-app` — the Tauri shell crate's **unit tests** | they exit abnormally on a headless Windows runner | those tests still **compile** on Windows (the next CI step builds all targets) and they **run** on Linux and macOS. So the code is covered; only the Windows-specific execution of it is not. |

This is stated in the README's status table too. Nothing else is excluded:
Linux and macOS CI both run the full `cargo test --workspace --all-targets`.

---

## 2 — Scheduling, end to end

Harness: the release `bellman-app` on a private `Xvfb` display with an
ephemeral `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR` and `HOME`,
under `dbus-run-session` so it never touches the operator's session or data.
Timers are created from **outside** the app with `bellman slot-submit`, which
is also the SCH2 path.

**Every script that produced the evidence below is committed** under
[`docs/qa-c11/harness/`](qa-c11/harness/), with its own
[README](qa-c11/harness/README.md) covering prerequisites, the container
image for the DST and packaged-demo runs, and why the private D-Bus session
is not optional. Each scenario names its exact command. After
`cargo tauri build --no-bundle --ci`, the whole set re-runs with:

```sh
cd docs/qa-c11/harness
for s in e2e_*.py; do python3 "$s" || echo "FAILED: $s"; done
```

Each script asserts its own outcome and exits non-zero on failure.

### All seven occurrence kinds fired on their own schedule

```sh
python3 docs/qa-c11/harness/e2e_kinds.py     # ~5 min, mostly waiting
```

The app was started first and stayed up for the whole run; all seven timers
were submitted afterwards by a separate process, and nothing was restarted.

| kind | occurrence submitted | slot response `next_fire_at` | `fired` event `scheduled_for` | `logged_at` | late by |
|---|---|---|---|---|---|
| `once` | `{once, 2026-08-02T00:00:56}` | 2026-08-01T21:00:56Z | 2026-08-01T21:00:56Z | …21:00:56.008225699Z | 8 ms |
| `interval` | `{interval, every_secs 120}` | …21:01:06.616435180Z | …21:01:06.616435180Z | …21:01:06.622222212Z | 6 ms |
| `daily` | `{daily, 00:01:16}` | 2026-08-01T21:01:16Z | 2026-08-01T21:01:16Z | …21:01:16.006862491Z | 7 ms |
| `weekly` | `{weekly, 00:01:26, [sun]}` | 2026-08-01T21:01:26Z | 2026-08-01T21:01:26Z | …21:01:26.005262298Z | 5 ms |
| `monthly` | `{monthly, 00:01:36, day 2}` | 2026-08-01T21:01:36Z | 2026-08-01T21:01:36Z | …21:01:36.007986384Z | 8 ms |
| `yearly` | `{yearly, 00:01:46, month 8, day 2}` | 2026-08-01T21:01:46Z | 2026-08-01T21:01:46Z | …21:01:46.007703555Z | 8 ms |
| `cron` | `{cron, "56 1 0 * * *"}` | 2026-08-01T21:01:56Z | 2026-08-01T21:01:56Z | …21:01:56.007665027Z | 8 ms |

**PASS.** Timezone `Europe/Helsinki`; the run crossed local midnight, so
`weekly` correctly resolved to Sunday and `monthly`/`yearly` to day 2 of
August. Evidence: [`qa-c11/kinds_evidence.json`](qa-c11/kinds_evidence.json)
(includes the `fires/` notifications and the full `bellman list` afterwards).

This same run is the SCH2 proof from the outside: seven timers created by a
foreign process all fired in a scheduler that was never restarted.

### Misfire across a real stop and start — **found a defect**

```sh
python3 docs/qa-c11/harness/e2e_misfire.py   # ~6 min
```

Two daily timers were scheduled for the same instant with opposite policies,
the app was stopped (SIGTERM, as closing it would) **before** that instant,
and started again **after** it. No clock was touched.

First run, on the pre-fix build:

| timer | policy | what the log said |
|---|---|---|
| `mf-coalesce` | `coalesce` | one `fired_late` for the missed slot — correct |
| `mf-skip` | `skip` | **nothing at all** |

`docs/PLAN.md` promises "every miss/outcome is logged to JSONL (`fired_late`,
`skipped_misfire`, `coalesced`)" and INTEGRATION.md lists `skipped_misfire`
among the kinds Bellman writes — but the only producer of that kind was the
*overlap* skip inside the fire transaction. Both misfire skip branches
(`Skip` past grace, and `Coalesce` when the whole backlog is out of grace)
advanced the timer silently. A user whose machine was off across a fire had
**no way to tell a skipped fire from a scheduler that forgot**.

Fixed in `a8cd9c9` (regression test
`scheduler::tests::misfire_skip_past_grace_is_logged_not_silent`). Re-run on
the fixed build:

```
2026-08-01T21:51:08.684Z skipped_misfire mf-skip      scheduled_for=21:49:58Z
    message="misfire_skip_past_grace"
    detail={"policy":"skip","missed_count":1,"oldest_missed":"2026-08-01T21:49:58+00:00","lateness_secs":70}
2026-08-01T21:51:08.685Z fired_late      mf-coalesce  scheduled_for=21:49:58Z
2026-08-01T21:51:08.709Z wake_delivered  mf-coalesce  action=none
```

**PASS** (after the fix). `coalesce` recovers the missed slot exactly once;
`skip` drops it and now says so, with the count and the lateness. Evidence:
[`qa-c11/misfire_evidence.json`](qa-c11/misfire_evidence.json).

### DST and clock jumps against a real clock

```sh
# image build in docs/qa-c11/harness/README.md
docker run --rm -v $PWD/docs/qa-c11/harness/dst_inner.py:/dst_inner.py:ro \
  -v $PWD/target/release/bellman-app:/usr/bin/bellman-app:ro \
  -v $PWD/target/release/bellman:/usr/bin/bellman:ro \
  c11-dst python3 /dst_inner.py gap 1200      # and: fold 5100
```

Run in disposable containers with `libfaketime`, which shifts the *wall*
clock of one process tree; the host clock was never touched and no clock was
stepped or accelerated. The container's own seconds carry the app through the
transition, so this is a real elapsed-time test, just parked next to a
transition instead of waiting for October.

> **Harness note, recorded because it cost an hour.** libfaketime fakes
> `CLOCK_MONOTONIC` as well by default, and that **stalls the scheduler
> outright** — its chunked sleeps never wake and nothing fires. Proved with a
> control run in the same container with the same binary and a 60 s interval
> timer: no faketime → fires at t=65 s; faketime with faked monotonic → never
> fires in 200 s; faketime with `FAKETIME_DONT_FAKE_MONOTONIC=1` → fires at
> t=60 s. This is libfaketime being pathological (a real monotonic clock does
> not jump), not a product defect — but anyone repeating this must set that
> variable or they will misread a stalled harness as a broken scheduler.

**Gap (spring forward).** `Europe/Helsinki`, 2027-03-28: local 03:00:00 EET
becomes 04:00:00 EEST, so local 03:30 does not exist that day. Policy
`DstGapPolicy::FirstValidAfterGap`.

```
fake start   2027-03-28 02:55:00 EET+0200
submitted    dst-target {daily 03:30:00}  next_fire_at=2027-03-28T01:00:00Z
EVENT dst-control-daily    fired scheduled_for=2027-03-28T00:58:00Z logged_at=…00:58:00.002Z
EVENT dst-target           fired scheduled_for=2027-03-28T01:00:00Z logged_at=…01:00:00.001Z
EVENT dst-control-interval fired scheduled_for=2027-03-28T01:00:00.519Z logged_at=…01:00:00.521Z
```

`01:00:00Z` is `04:00:00 EEST` — the first valid local instant after the gap.
**PASS**: the non-existent 03:30 fired once, at the documented instant, on
the clock.

**Fold (fall back).** `Europe/Helsinki`, 2026-10-25: local 04:00:00 EEST
becomes 03:00:00 EET, so local 03:02 happens twice — at `00:02:00Z` (EEST)
and again at `01:02:00Z` (EET). Policy `DstFoldPolicy::FirstOccurrence` says
fire once, at the earlier.

```
fake start   2026-10-25 02:58:00 EEST+0300
submitted    dst-target {daily 03:02:00}  next_fire_at=2026-10-25T00:02:00Z
EVENT dst-target           fired scheduled_for=2026-10-25T00:02:00Z logged_at=…00:02:00.002Z
EVENT dst-control-daily    fired scheduled_for=2026-10-25T00:03:00Z logged_at=…00:03:00.001Z
EVENT dst-control-interval fired scheduled_for=2026-10-25T00:03:00.523Z logged_at=…00:03:00.524Z
```

The run then kept going for another 80 fake minutes — through the transition,
past `01:02:00Z` (the instant local 03:02 comes round the second time), and on
to `01:23:00Z`:

```
EVENT dst-control-interval fired scheduled_for=2026-10-25T00:58:00.523Z
EVENT dst-control-interval fired scheduled_for=2026-10-25T01:03:00.523Z   ← past the repeat
EVENT dst-control-interval fired scheduled_for=2026-10-25T01:18:00.523Z
EVENT dst-control-interval fired scheduled_for=2026-10-25T01:23:00.523Z
final tally: dst-target 1 · dst-control-daily 1 · dst-control-interval 17
```

**PASS**: the ambiguous local time fired **exactly once**, at the earlier of
its two instants, and did not fire again in the repeated hour — with 21 fake
minutes of margin past the second window.

The 5-minute interval timer riding along fired 17 times at exactly 300 s
spacing straight through the transition (…00:48, 00:53, 00:58, 01:03…) with
no gap and no doubling: elapsed-time schedules are anchored in UTC and the
offset change does not touch them, as designed. The gap run's interval timer
did the same across its transition.

**Clock jumps.** The backward-jump and suspend-oversleep paths are covered by
the simulated-clock acceptance tests (`scheduler::tests`, mock clock pair),
which run in the suite above. A *forward* wall jump of seven months relative
to the boot instant is what every faketime run here does, and neither
container showed a spurious re-fire or a lost timer.

### Pruner, rotation and retention

```sh
python3 docs/qa-c11/harness/e2e_prune.py     # ~2 min
```

Thresholds came from the documented `config.json` keys, set to the smallest
values the product accepts (see the friction note below). Seeded on disk
before the app started: `events.current.jsonl` just over the 1 MiB rotation
threshold, and six valid gzipped archives of ~1 MiB each with mtimes staggered
15–45 days old. One 1 s interval timer then supplied live traffic.

```
before: current 1 101 390 B + 6 archives ≈ 6 308 580 B  = 7 410 570 B   (budget 4 194 304 B)
after : current       1 051 B + 4 archives              = 3 432 771 B   budget holds ✓

new archive from rotation: events-2026-W31.jsonl.gz  (277 105 B, gzip magic ✓, 3 500 lines)
surviving archives       : W13 (27 d), W14 (21 d), W15 (15 d) — all gzip ✓, 13 500 lines each
removed by retention     : W10 (45 d), W11 (39 d), W12 (33 d) — every one older than 30 days
log event                : {"kind":"pruned","message":"log_retention",
                            "detail":{"aged_out":3,"budget_pruned":0,"bytes_removed":3154565}}
```

**PASS**: rotation fires at the threshold, archives are real gzip and
decompress to the expected line counts, the 30-day age rule removes exactly
the three archives older than 30 days, the retained total lands under budget,
and the prune is itself logged. Evidence:
[`qa-c11/prune_evidence.json`](qa-c11/prune_evidence.json).

The `system.prune` timer is visible to a user as promised —
`bellman list` shows it alongside their own timers (weekly, next fire two days
out in that run).

---

## 3 — The integration surface

All of §3 ran against a **live** desktop app with its reply watcher running,
and every scenario started from a scheduled fire.

```sh
python3 docs/qa-c11/harness/e2e_apps.py      # the two demo apps + both transports
python3 docs/qa-c11/harness/e2e_reply.py     # no_ack, watchdog, rejections, superseded
python3 docs/qa-c11/harness/e2e_crud.py      # slot add / modify / delete, live
```

### A real app woken by a scheduled fire

*(`e2e_apps.py`, above.)*

`testing_apps/lightbulb/lightbulb.py`, unmodified, started before its timer
was due and left watching `fires/`:

```
00:35:10 FIRE lightbulb-demo run_id=d8c305bc-2292-4817-bb2f-5592b1fd332b
00:35:14 TERMINAL lightbulb-demo: completed transport=json
log kinds: registered → fired → wake_delivered → acknowledged → completed   (one run_id)
status.json: {"state":"completed", "acknowledged_at":…, "completed_at":…,
              "result":{"on_duration_secs":4.01}, "transport":"json"}
```

**PASS.** Evidence: [`qa-c11/apps_evidence.json`](qa-c11/apps_evidence.json).

### Both transports — **found a defect**

*(`e2e_apps.py`, above.)*

Two timers identical except for `transport.mode` (`json` and `ipc`), owned by
the same `app_name`, answered with **byte-identical** `bellman-reply/1`
documents — one written into the reply file, one sent as one line over the
socket.

On the pre-fix build the IPC side simply did not work:

```
twin-ipc: fired → recorded transport "json" → a reply stub was written on disk
          (an IPC firing must not have one) → the connected, claimed client
          received no frame at all → no_ack after the 60 s pickup grace
```

Root cause: `SchedulerConfig::reply_engine()` — the constructor the running
scheduler builds its reply engine from — hardcoded `ipc: None`, so
`select_transport` degraded **every clock-driven firing** to files whatever
the timer said. Every existing IPC test built its own engine with the handle
wired in and called `project_fire` directly, so the whole suite passed while
the feature could not work in the product. That is the same blind spot SCH2
was written about, one layer up.

Fixed in `70349af`. Regression test
`scheduler_config_engine_selects_the_socket_for_an_ipc_timer` drives the same
constructor the app does, and also asserts that the pre-fix construction
degrades — verified to fail without the fix:

```
$ # with `ipc: None` restored
test scheduler_config_engine_selects_the_socket_for_an_ipc_timer ... FAILED
   panicked: the scheduler's own reply engine must hold the live IPC handle
```

Re-run on the fixed build, from scheduled fires:

| | `twin-json` | `twin-ipc` |
|---|---|---|
| log kinds | registered, fired, wake_delivered, acknowledged, completed | registered, fired, wake_delivered, acknowledged, completed |
| `status.json` `state` | `completed` | `completed` |
| `status.json` `result` | `{"note":"identical payload on both transports","ok":true}` | *identical* |
| `status.json` `transport` | `json` | `ipc` |
| reply stub on disk | present | **absent** (correct for IPC) |
| fire message | carries `reply_path` | carries `ipc.socket`, no `reply_path` |
| socket permissions | — | dir `0700`, socket `0600` |

**PASS** (after the fix). The two transports produce the same state, the same
log lines and the same `status.json` apart from identity, timing and the
`transport` field itself.

### Pickup grace and `no_ack`, and a late reply revising it

*(`e2e_reply.py`, above.)*

An owned timer with nothing listening:

```
21:12:13.020Z  fired    rc-noack
21:13:13.021Z  no_ack   rc-noack   "no acknowledgement was received"   (exactly 60 s, the shipped grace)
   → a reply written after that point:
status.json: {"state":"completed", "completed_at":"21:13:13Z",
              "result":{"late":true}, "no_ack_at":"21:13:13.021Z"}
```

**PASS** — and note `no_ack_at` is *retained* beside the later `completed`,
so the whole story stays visible, exactly as INTEGRATION.md says.

### The opt-in watchdog, and heartbeats extending it

*(`e2e_reply.py`, above.)*

Watchdog armed with `error_detection: true, expected_secs: 5` (× factor 2.0 →
a 10 s deadline), then silence:

```
21:12:16  reply: state=running, expected_secs=5, error_detection=true
21:12:26.197Z  status.json → {"state":"failed","failure_kind":"timed_out"}   (10.2 s later)
reply file sha256 before = after = d1b3a536c49218f5…   → byte-identical
```

Same arming, but kept alive by six heartbeats 3 s apart (18 s total, on a
10 s deadline), then completed:

```
mid-run status.json: {"state":"running","heartbeat_at":"21:12:31Z","progress":"step 5/6",
                      "expected_secs":5,"error_detection":true}
final status.json:   {"state":"completed", …}
```

**PASS**: expiry marks but never touches the app's file, and every new
heartbeat extends the deadline.

### Rejection and quarantine under a live watcher

*(`e2e_reply.py`, above.)*

Five bad writes onto a live reply path, each while the watcher was running:

| case | bytes | logged | rejection message | quarantined | live file left in place |
|---|---|---|---|---|---|
| malformed bytes | 29 | `reply_rejected` | `invalid JSON` | payload + `.sidecar.json` | ✓ |
| wrong `app_name` | 198 | `reply_rejected` | `app_name does not match the run's owner` | payload + sidecar | ✓ |
| unknown `run_id` | 192 | `reply_rejected` | `run_id does not match the reply filename` | payload + sidecar | ✓ |
| oversize (> 64 KB) | 70 216 | `reply_rejected` | `oversize` | sidecar only, named `…-unread-…` | ✓ |
| reserved state (`no_ack`) | 189 | `reply_rejected` | `state is reserved to Bellman` | payload + sidecar | ✓ |

**PASS** — and the oversize case is nicer than documented: the body is
rejected **unread**, so the quarantine keeps only the sidecar, and the
filename says `unread`.

**Stale reply from a superseded run.** An interval timer fired twice; the
first run's reply was answered after the second had started:

```
21:12:41.966Z  superseded  run_id=29614e3f…  "reply arrived for a run that is no longer current"
```

**PASS**, with one thing worth knowing that INTEGRATION.md does not say:
when the next firing supersedes an unresolved run, Bellman **removes that
run's reply stub** at the same time. A slow app returning to its old
`reply_path` will find the file gone. That is fine — the documented minimal
reply (`schema` + `run_id` + `app_name` + `state`) can simply be composed and
written to the same path, and it is then logged `superseded` and deleted
again — but the doc reads as though the stub is still sitting there. Recorded
as a polish item, not a failure.

### Two apps publishing at once — **found a silent request loss**

This one arrived the hard way. The full workspace suite is supposed to be
repeatably green, and it was not: roughly one run in twenty,
`concurrent_producers_all_get_unique_slots` failed with *expected 8 timers,
got 7*. A flaky test is easy to dismiss as a flaky test. It was not — it was
the product telling the truth.

Instrumenting the publish path caught it:

```
DIAGPUB thread=3 rid=21c74226-… path=Ok(".../slots/free/slot-0100.json")
DIAGPUB thread=0 rid=792d35ec-… path=Ok(".../slots/free/slot-0100.json")   ← same name
… expected 8 timers, got 7      free/ clean · work/ empty · bad/ EMPTY
```

**Two producers were handed the same reserved slot name.** `publish` reserves
a slot by reading a free stub and then renaming that name away — but `rename`
moves whatever is at the name; it does not check that the name still holds
the stub that was read. Between one producer's read and its rename, another
can claim the same stub, publish its request back onto the same name, and
finish. The first then renames away the *second's request*, writes its own
over the name, and deletes the second's along with the claim temp.

Both producers get `Ok`. Nothing lands in `bad/`. No event is logged. The
app's timer simply never exists — which is the worst shape a bug can have on
an integration surface whose promise is "several different systems can each
claim their own slot independently, one system never blocks another".

**Fixed**: the claim now *verifies what it claimed*. After the rename, the
file is re-read; if it is not still the free stub with the slot id that was
read, it is renamed straight back under its own name and the producer moves
to the next candidate. Nothing is destroyed; the other producer's request is
briefly absent from a directory listing, which a rescan handles by design.

A second, narrower instance of the same class was fixed with it: the
replenisher chose a stub id, checked `exists()`, and then wrote with a
*replacing* write. A claimed stub is renamed to a dot-file, which
`parse_slot_id_from_name` does not count, so the id of a just-claimed
highest-numbered stub could be chosen and the stub written over the request
about to land there. Stub creation is now **exclusive** — a same-directory
hard link that fails with `AlreadyExists` rather than clobbering.

Regression tests, both verified to fail without their fix:

```
slots::tests::concurrent_publishers_never_share_a_reserved_slot_name
    8 producers × 25 rounds; asserts the reserved names are distinct AND that
    every request still reads back as its own producer's.
    Without the fix: fails at round 1 —
      "two producers were handed the same slot name:
       [… slot-0004.json, slot-0004.json …]"

slots::tests::replenish_never_overwrites_a_published_request
    60 publishes while a second thread hammers replenish(); asserts every
    published request_id is still readable at its own path.
```

### Slot channel CRUD against a running scheduler (SCH2)

*(`e2e_crud.py`, above.)*

One app session, never restarted; all three operations issued by an external
`bellman slot-submit`:

```
add     crud-add     once @21:38:44Z                 → FIRED 21:38:44.001Z
modify  crud-modify  once @21:46:54Z → 21:39:44Z     → FIRED 21:39:44.001Z at the NEW time
delete  crud-delete  once @21:39:14Z, deleted first  → never fired; gone from `bellman list`
```

**PASS.** A moved fire time takes effect and a deleted timer stops firing, on
a live scheduler, with no restart.

### A client in a language the docs do not cover

*(`e2e_apps.py`, above — it starts the Perl client alongside the lightbulb.)*

`docs/INTEGRATION.md` ships copy-paste clients in Python, bash, PowerShell and
Node. [`qa-c11/clock_in.pl`](qa-c11/clock_in.pl) is a **Perl** client written
from that document alone (steps 1–3: scan `fires/`, filter by `app_name`,
dedupe by `run_id`, open `reply_path` verbatim, temp-write + rename). Core
Perl only — `JSON::PP` and `POSIX` have shipped with perl since 5.14.

```
00:35:20 FIRE clockin-demo run_id=1abe0ccf-ee67-4ced-8518-f7278a1c0523
00:35:23 TERMINAL clockin-demo: completed transport=json
status.json: {"state":"completed","acknowledged_at":"2026-08-01T21:35:20Z",
              "expected_secs":2,"completed_at":"2026-08-01T21:35:22Z",
              "result":{"language":"perl","worked_secs":2},"transport":"json"}
```

**PASS.** The document was sufficient: nothing had to be looked up in the
Rust. The two things that needed care were both stated in it — that
`reply_path` is absolute and must be opened verbatim, and that the stub
already carries the identity fields.

### The GUI demo, driven by clicking

```sh
scripts/run_gui_qa.sh wiz1
```

On an isolated `Xvfb` + private D-Bus, against
the release `bellman-app` from this tree. The Bellman window is driven through
its own webview via WebDriver; the demo's tk window is driven with XTEST
clicks on that display only.

```
wizard opens on a fresh profile; the demo tick is present and UNTICKED by default
finish without ticking → no demo panel, timer count unchanged
re-run, tick, finish   → panel with a copyable command and a Run button;
                         timer count STILL unchanged (Bellman never creates the demo's timer)
Run the demo           → python3 …/lightbulb_gui.py --slots <this install's slots>
click inside the demo window → the demo created its OWN timer through the slot protocol
                               (16c5d2b4-44c5-4d81-8876-b378bc9b8ad9)
run completed: run_id=800eba16-…  kinds=['fired','wake_delivered','acknowledged','completed']
choice persisted (config.json demo_opt_in); Settings shows the same panel
closing Bellman leaves the demo process running
phase 2: python3 present but tkinter missing → no Run button, python3-tk note shown
```

**PASS**, no `run-now` anywhere in that path. Screenshots refreshed in
[`docs/qa4-screenshots/`](qa4-screenshots/) (`wiz1-*`).

---

## 4 — Install and packaging, as a stranger

Each recipe was run **literally** in a clean container, as the only commands
executed, with `bash` as the shell (the README's steps 2 and 3 use `source`).
Nothing is shimmed, substituted or inferred: where the runs differ from each
other it is because §Install itself distinguishes the cases, and each
difference is named where it is used.

**The container gets a `git bundle`, not a bind-mounted checkout.** A bundle
is a complete repository in one file, so nothing the run needs lives outside
the mount. That is not cosmetic: mounting a git **worktree** does not work,
because its `.git` is a pointer to a gitdir stored elsewhere, and the clone
then fails inside the container with `fatal: not a git repository`. An
earlier revision of these transcripts mounted the parent checkout and so
carried a hidden dependency on it — they now take

```sh
git bundle create /tmp/bellman.bundle train/2026-08-01_0005   # from this worktree
```

and the run needs that single file and nothing else.

`git clone https://github.com/AccipiterTechGuy/bellman` was run for real on
the first pass; at the time of the run the public `main` was `f5b56de`, the
exact commit this branch started from, so what those containers built is what
a stranger gets. The re-runs after the fixes clone this branch instead, since
the fixes are not on public `main` yet — that substitution is named where it
is used.

### The first thing that failed was `sudo` — and the first fix was wrong too

The first pass wrapped every recipe in a two-line `sudo` shim. That was
wrong: the card says install verbatim **with no bootstrap step you invented**,
and a shim is exactly that. Removing it turns up a genuine defect:

```
$ docker run --rm ubuntu:24.04     sh -lc 'sudo true'   → exit 127  sudo: not found
$ docker run --rm archlinux:latest sh -lc 'sudo true'   → exit 127  sudo: command not found
$ docker run --rm fedora:latest    sh -lc 'sudo true'   → exit 0
$ docker run --rm {ubuntu:24.04,fedora:latest,archlinux:latest} id -u  → 0, 0, 0
```

`ubuntu:24.04` and `archlinux:latest` ship no `sudo` at all, and all three
images run as uid 0 — so on two of the three the README stopped on its very
first command, before installing anything. A desktop has `sudo`; a container,
a chroot or a minimal image very often does not, and those are exactly where
people build.

The **second** attempt at the fix was also wrong, in a way worth recording.
It said "everything from step 2 on runs as your normal user and must not be
run as root" — a sentence the transcripts then contradicted, because in a
container they stay uid 0 all the way through. Either the README or the
evidence was lying, and it was the README: running steps 2–5 as root is
perfectly fine. rustup and nvm install into the **invoking user's** `$HOME`,
so as root the toolchain lands in root's home, which is consistent and works.
What is actually wrong is putting `sudo` in *front* of them — that installs
the toolchain into root's home while you are someone else, and then `cargo`
is not on your `PATH`.

**Fixed** in the README, which now says the true thing:

- step 1 is the only part needing root privileges; it is written with `sudo`
  because that is how a desktop user gets them, and **you drop the `sudo` if
  you already are root** — naming the two images where the prefix fails;
- steps 2 onward need no privileges and must **not** be prefixed with `sudo`,
  with the reason (`$HOME`, `PATH`) rather than a bare prohibition;
- `pacman -Syu` deliberately keeps its confirmation prompt because it is a
  full system upgrade, and **`--noconfirm` is what you add when running
  unattended** — which is what the apt and dnf `-y` flags already are.

That last point removes the other private substitution: the earlier Arch
transcript piped `yes '' |` into `pacman`, a command the README did not
contain. It now runs `--noconfirm`, because the README says to.

Every run below follows the README including those sentences. No shim, no
invented step, and the identity each run uses is stated.

### Ubuntu 24.04 — **PASS, verbatim, no bootstrap**

Every step succeeded, including both bundles and `sudo apt install ./…deb`.
Afterwards, in the same container:

```
/usr/bin/bellman   /usr/bin/bellman-app   /usr/share/applications/Bellman.desktop
/usr/share/bellman/testing_apps/{README.md,lightbulb/,lightbulb_gui/}
/usr/share/bellman/docs/INTEGRATION.md
rustc 1.97.1 · node v24.13.0 · tauri-cli 2.11.4
bellman --help → "Data directory: ~/.bellman/ …"
```

### Fedora — **FAILED as written**

Two failures, one after the other.

1. Prerequisites installed fine.
2. `cargo tauri build --bundles deb,appimage --ci --no-sign` built the app and
   the **deb**, then died bundling the AppImage:
   `Error failed to bundle project: 'failed to run linuxdeploy'`.

Root cause (reproduced with `--verbose`): linuxdeploy's bundled `strip` cannot
read modern Fedora shared objects —

```
ERROR: Strip call failed: … /Bellman.AppDir/usr/lib/libwebkit2gtk-4.1.so.0:
       unknown type [0x13] section `.relr.dyn'
       Unable to recognise the format of the input file
```

`SHT_RELR` (0x13) is what current binutils emits and linuxdeploy's copy
predates it. Upstream tooling, not Bellman — but the README told people to run
that command on Fedora.

**Fixed** (`e151f11`) by giving Fedora the bundle it can actually build:
`cargo tauri build --bundles rpm --ci --no-sign`, with the reason stated in
the README instead of leaving a mystery failure. Verified end to end in the
container:

```
Bundling Bellman-0.1.0-1.x86_64.rpm     → dnf install ./…rpm  → Complete!
/usr/sbin/bellman  /usr/sbin/bellman-app  /usr/share/applications/Bellman.desktop
/usr/share/bellman/testing_apps/{lightbulb,lightbulb_gui}   ← after the packaging fix below
bellman --version → bellman 0.1.0
```

The first rpm had **no** `/usr/share/bellman/testing_apps/`: the shipped demos
and INTEGRATION.md were declared only under `bundle.linux.deb.files`. Since
the README now points Fedora users at the rpm, `tauri.conf.json` gained a
`bundle.linux.rpm` block mirroring the deb one (same `desktopTemplate`, same
files), so the path the first-run wizard names exists there too.

### Arch — **FAILED as written**

The very first command stopped:

```
$ sudo pacman -Syu --needed git base-devel webkit2gtk-4.1 curl wget file \
    openssl libxdo libayatana-appindicator librsvg
error: target not found: libxdo
```

Arch ships that library in **`xdotool`** (`pacman -F libxdo.so` →
`extra/xdotool … usr/lib/libxdo.so`); every other name on the line is
correct. With `xdotool` substituted, prerequisites install and the build runs
— and then hits the same `linuxdeploy` failure as Fedora.

**Fixed** (`e151f11`): the package name is corrected, and Arch's build step is
now `cargo tauri build --no-bundle --ci` (Tauri has no pacman target).

### All four runs, literal, in clean containers

Every step of the page, in order, in a fresh container each time, with
nothing added. The identity each run uses is stated, because §Install now
names two of them and both are covered. Transcripts:
[`qa-c11/harness/install/`](qa-c11/harness/install/).

> "Nothing added" was very nearly not true. Each script also ran
> `git config --global --add safe.directory '*'`, which §Install does not ask
> for. The supervisor spotted it and proved it inert — the clone from
> `/srcbundle` succeeds without it. It is removed rather than documented as a
> second exception, and the Ubuntu run below was **re-run from scratch on the
> commit that removed it** (`536d64c`), reaching `OK-UBUNTU-LITERAL` with
> `EXIT=0`: `apt`, rustup, nvm, `cargo install tauri-cli`, `git clone`, `npm
> ci`, `cargo tauri build --bundles deb,appimage`, and `apt install` of the
> resulting `Bellman_0.1.0_amd64.deb`. The other three scripts differ from it
> only in package manager and identity; the deleted line was byte-identical in
> all four, and it is only `git` that could ever have read it.

```
### ubuntu:24.04 — root, image ships no sudo                        exit 0
id -u → 0
apt update; apt install -y …                     (no sudo: root, per §Install)
curl … sh.rustup.rs | sh -s -- -y ; source ~/.cargo/env      (no sudo prefix)
nvm install 24 ; cargo install tauri-cli --locked
npm ci ; cargo tauri build --bundles deb,appimage --ci --no-sign
apt install -y ./target/release/bundle/deb/Bellman_*.deb
  /usr/bin/bellman   /usr/bin/bellman-app   Bellman.desktop
  /usr/share/bellman/testing_apps/{README.md,lightbulb/,lightbulb_gui/}
  Bellman_0.1.0_amd64.deb        7 118 832 B
  Bellman_0.1.0_amd64.AppImage  81 353 208 B
  bellman --version → bellman 0.1.0
OK-UBUNTU-LITERAL

### fedora:latest — root, but this image HAS sudo                   exit 0
id -u → 0 ; command -v sudo → /usr/sbin/sudo
sudo dnf install -y …                       (prefix kept, exactly as written)
rustup / nvm / tauri-cli, no sudo
npm ci ; cargo tauri build --bundles rpm --ci --no-sign
sudo dnf install -y ./target/release/bundle/rpm/Bellman-*.rpm
  /usr/sbin/bellman   /usr/sbin/bellman-app   Bellman.desktop
  /usr/share/bellman/testing_apps/{README.md,lightbulb/,lightbulb_gui/}
  bellman --version → bellman 0.1.0
OK-FEDORA-LITERAL

### archlinux:latest — root, no sudo, unattended                    exit 0
id -u → 0
pacman -Syu --needed --noconfirm … xdotool …
      (no sudo: root · --noconfirm: §Install says to add it when unattended)
rustup / nvm / tauri-cli, no sudo
npm ci ; cargo tauri build --no-bundle --ci
  target/release/bellman       7 488 776 B
  target/release/bellman-app  10 677 288 B
  ./target/release/bellman --version → bellman 0.1.0
OK-ARCH-LITERAL

### ubuntu:24.04 — an ORDINARY USER with sudo (the desktop case)    exit 0
id -un → builder ; id -u → 1001
sudo apt update ; sudo apt install -y …                 (sudo, as a user does)
curl … | sh -s -- -y                        (NO sudo — §Install says not to)
  assert: command -v cargo == $HOME/.cargo/bin/cargo
npm ci ; cargo tauri build --bundles deb,appimage --ci --no-sign
sudo apt install -y ./target/release/bundle/deb/Bellman_*.deb
  /usr/bin/bellman   /usr/bin/bellman-app   Bellman.desktop  + both demos
  drwxrwxr-x builder builder ~builder/.cargo
  drwxrwxr-x builder builder ~builder/.nvm
  assert: /root/.cargo and /root/.nvm do not exist
OK-UBUNTU-NONROOT
```

**PASS on all three distributions and on both identities.** The last run is
the one that proves the `$HOME` sentence rather than asserting it: the
toolchain belongs to `builder`, and root has none.

That fourth run needs the container made to resemble a desktop first —
install `sudo`, create a user, add them to sudoers. That scaffolding is **not
part of §Install**, it is in the outer half of a two-part script with the
README's own commands in the inner half, and it is written out rather than
folded in so it cannot be mistaken for something the page asks of anyone. The
other three need no scaffolding at all.

### The `.deb`'s own shipped demo — **found two more defects**

```sh
docker run --rm -v $PWD/docs/qa-c11/harness/deb_demo_inner.py:/d.py:ro \
  -v $PWD/target/release/bundle/deb/Bellman_*.deb:/new.deb:ro \
  c11-dst bash -c 'apt-get install -y /new.deb >/dev/null && python3 /d.py'
```

Run inside the container that `apt install`ed the deb, using **only** packaged
files: `/usr/bin/bellman-app`, `/usr/bin/bellman`, and
`/usr/share/bellman/testing_apps/lightbulb/lightbulb.py`.

The package itself was fine:

```
/usr/bin/bellman   /usr/bin/bellman-app   /usr/share/applications/Bellman.desktop
/usr/share/bellman/testing_apps/lightbulb/lightbulb.py   ← the path the wizard names
```

But **the run did not complete**. The timer fired, the demo picked it up, the
bulb lit for its four seconds and it wrote a perfectly good reply —

```json
{"schema":"bellman-reply/1","run_id":"30df812e-…","app_name":"lightbulb",
 "state":"completed","acknowledged_at":"2026-08-01T22:05:55Z","expected_secs":4,
 "completed_at":"2026-08-01T22:05:59Z","result":{"on_duration_secs":4.01}}
```

— and Bellman recorded `no_ack`. The reply file was still sitting there
untouched, `timers/bad/` was empty, and the event log contained only two
lines: `wake_capability` and `registered`. No `fired`, no `no_ack`, nothing
after the moment the timer was created.

Two lines of the app's own stderr explained it:

```
bellman: startup maintenance error: prune: unknown timezone '/UTC': failed to parse timezone
bellman: watcher exited: io: watch timers/: No path was found.
```

**D6.** `iana_system_tz()` guesses the zone from `$TZ` or the `/etc/localtime`
symlink. That container's link is `/usr/share/zoneinfo//UTC` — a double slash,
which the guess turns into the name `/UTC`. chrono-tz cannot parse it, so
`Occurrence::new` fails, `ensure_system_prune_timer` fails, and
`startup_maintenance` aborts. On such a machine **nothing ever prunes.**

**D5, the serious one.** Because startup maintenance aborted, nothing had
created `<data dir>/timers/` — it is made lazily by the first timer folder,
and on a normal machine `system.prune` happens to make it first. The single
background watcher then does
`debouncer.watch(engine.tree.root(), Recursive)`, `notify` returns *No path
was found*, and **the whole thread ends**. That thread is reply ingest, the
slot channel and the event-publisher tick. The app keeps running, the window
works, timers still fire — and every reply an integrating app writes is
ignored for the rest of the session. Nothing warns the user; the only symptom
is that every run goes `no_ack`.

Both are fixed:

- the watcher creates the tree root before watching it, and the desktop app
  creates `timers/` alongside `logs/` and `slots/` at startup;
- a guessed zone name is normalised and **verified against chrono-tz**, and
  falls back to `UTC` when it does not parse — a guess that does not parse is
  worse than no guess.

Regression tests, both verified to fail without their fix:
`slots::tests::watcher_starts_on_a_data_dir_with_no_timers_root_yet` and
`pruner::tests::unparseable_system_timezone_falls_back_to_utc`.

Re-run of the same container scenario — same image, same broken
`/etc/localtime -> /usr/share/zoneinfo//UTC` — with the fixed `.deb`
installed over it:

```
readlink /etc/localtime → /usr/share/zoneinfo//UTC        (the same broken link)
bellman: system.prune ready id=00000000-0000-4000-8000-000000000001
         next=Some(2026-08-03T03:17:00Z)                  (D6: falls back to UTC)
bellman: prune catch-up: timers=0 archives_removed=0      (maintenance completes)
(no "watcher exited" line)                                 (D5: the watcher lives)

event kinds: registered → fired → wake_delivered → acknowledged → completed
status.json: {"state":"completed","acknowledged_at":"2026-08-01T22:18:39Z",
              "expected_secs":4,"completed_at":"2026-08-01T22:18:43Z",
              "result":{"on_duration_secs":4.01},"transport":"json"}
```

**PASS.** The package installs, puts both binaries and the launcher entry in
place, ships the demos at `/usr/share/bellman/testing_apps/` — the path the
wizard names — and the demo it ships completes a run from a scheduled fire
using nothing but packaged files.

### Both data directories

```sh
python3 docs/qa-c11/harness/e2e_datadirs.py  # ~4 min
```

With the desktop app running on its own store, a timer was created the plain
way the CLI's help suggests (`bellman add`, no flags):

```
GUI store : <XDG_DATA_HOME>/io.bellman.desktop   → timers: ["system.prune"]
CLI store : $HOME/.bellman                       → timers: ["cli-default-timer"]
bellman --help: "Data directory: ~/.bellman/ … The desktop app uses its own
                 per-OS app-data dir instead — see docs/LOCAL.md."
GUI Settings → Data shows its own directory, database, logs and slots paths.

150 s later, well past the timer's due time:
  cli-default-timer  last_fired = null,  0 `fired` events in either log
```

**PASS on the documented behaviour** — the two stores are separate exactly as
`docs/LOCAL.md` says, each interface names its own, and neither can see the
other's timers. See the walkthrough for the friction this causes.

### Windows and macOS

**Not validated on real hardware** — see the next section for exactly what is
and is not covered.

---

## Not tested, and why

An unstated gap is worse than a stated one. This is the complete list.

### Wake from sleep on real hardware — **not tested**

The card asks for `rtcwake -m mem -s 30` on real hardware. Hardware is
available (this is a bare-metal desktop, `systemd-detect-virt` → `none`, with
`/sys/class/rtc/rtc0`). **The test was not run**, because it suspends the
whole machine: the operator's own Bellman, three validation containers, and
this session were all live on it, and suspending someone's workstation is not
an unattended agent's call. It also needs root.

What *was* observed, on that hardware:

```
$ cargo run -p bellman-core --example wake_probe
Wake from sleep: OFF — no permission to arm a wake timer
    (systemd ≥254 local session / AmbientCapabilities=CAP_WAKE_ALARM / setcap / udev rule)
reason=NoPermission { hint: … }
```

That is the documented P7 outcome for a **daemon-descended shell** (this
session is not a local desktop login), and it matches the exit gate P7 signed
off. The Enabled-from-a-desktop-session half of that gate was observed live
during P7; it was not re-observed here.

To close this gap, one command on an idle machine with a local desktop
session:

```sh
# The desktop app must be the thing running, so create the timer in ITS store
# (see "Both data directories" above — plain `bellman add` would land in the
# CLI store, which nothing is driving).
APP=~/.local/share/io.bellman.desktop
bellman add --db "$APP/timers.db" --name wake-probe --occurrence once \
            --time "$(date -d '+90 seconds' +%Y-%m-%dT%H:%M:%S)"
sudo rtcwake -m mem -s 60          # suspends for 60 s, then the RTC wakes it
grep wake-probe "$APP/logs/events.current.jsonl"
# expect: one `fired` (or `fired_late` / `skipped_misfire` per the timer's
#         policy) and a `wake_capability` line saying which mechanism was used
```

### Windows and macOS — **partially covered**

| covered | how |
|---|---|
| the whole workspace compiles on both | `macos.yml`, `windows.yml` |
| the full test suite runs on macOS | `macos.yml` — `cargo test --workspace --all-targets` |
| the test suite runs on Windows except the `bellman-app` unit tests | `windows.yml` (see §1) |
| NSIS, MSI and dmg bundles build unsigned | both workflows assert the artifacts exist |
| the per-OS wake decision trees | unit-tested against mocked API answers (P7) |

| **not** covered | why |
|---|---|
| installing and launching on real Windows or macOS | no such hardware in this environment |
| tray, autostart, single-instance behaviour on those OSes | same |
| signing / notarisation, SmartScreen and Gatekeeper behaviour | needs signing material and real machines |
| the macOS wake helper daemon's SMAppService enrolment | needs a real macOS login |
| Windows named-pipe IPC end to end | the Unix-socket half was validated here; the pipe half is unit-tested only |

The README already says these packages are unfinished; nothing in this card
changes that.

### Other gaps

- **The 1 GiB retained-log budget was not exercised at its shipped value.**
  The sanitizer's 4 MiB floor combined with gzip means proving the default
  needs well over 100 MiB of raw events. The budget rule itself *was* proved
  end to end at the 4 MiB floor (above), and the shipped constants are
  covered by `pruner::tests` / `events::tests`.
- **Long-run soak.** The longest continuous run here was ~85 minutes
  (the DST fold container). No multi-day soak was attempted.
- **KDE / Wayland tray behaviour.** Only the isolated X11 `Xvfb` display was
  used, with `metacity`. GNOME/KDE tray behaviour is P6's gate, not re-run.
- **`bellman scan` against a machine with real crontabs.** Out of this card's
  scope and it would print the operator's own schedules.

---

## Walkthrough — what this was like to use

First person, as someone who found Bellman an hour ago. Everything in this
section was written down as it happened, including the parts that resolved.

**Installing.** The README's Ubuntu path is genuinely good. It told me to run
`apt update` first *and said why*, it told me `-y` was needed on rustup and
why, and it warned me that `source "$HOME/.cargo/env"` cannot be skipped. All
three warnings were about things that would otherwise have stopped me. It
worked start to finish with nothing invented.

**I nearly papered over the first failure, and that is worth admitting.**
`ubuntu:24.04` has no `sudo`, so the README's very first command died with
`sudo: not found`. My first instinct was to drop in a two-line shim and carry
on — which is precisely the move this card exists to forbid: *if a command
fails, that is a finding, not something to work around*. With the shim gone
it is a real, ordinary defect (the page never says step 1 needs root, or what
to do when you already are), and the fix is one useful sentence rather than a
private workaround. The lesson generalises: a shim is a way of not hearing
what the machine just told you.

**And then I got the fix wrong, which is the more interesting mistake.**
Having removed the shim, I wrote "steps 2 onward must not be run as root"
into the README — and left the container transcripts running as root all the
way through. That is worse than the shim: the shim was a private workaround,
this was a published claim my own evidence contradicted. Nobody would have
noticed from reading either half alone. The truth is narrower and more
useful: root is fine, `sudo` in front of steps 2–5 is not, and the reason is
`$HOME`. A prohibition without a reason is how you end up with a rule that
does not survive contact with a container.

**Then I tried Fedora and Arch, and both stopped again.** On Arch the very first
command died on `libxdo`; I had to go and find out that Arch calls it
`xdotool`. On both, the build command in the README aborted at the AppImage
step with `failed to run linuxdeploy` and no explanation — I had to re-run
with `--verbose` and read a `strip` error about `.relr.dyn` to understand
that it was upstream tooling and not my machine. A stranger would very
reasonably conclude at that point that the project does not build.
Both are fixed now, and I wrote the *reason* into the README, because the
next person hitting a linuxdeploy failure deserves better than silence.

**I could not tell which data directory was live — the docs told me, but
only if I read the right one.** `README.md` and `docs/LOCAL.md` both explain
the two directories clearly, and `bellman --help` names the CLI's. But the
thing I actually wanted to know is not stated anywhere: **a timer created
with plain `bellman add` will never fire while only the desktop app is
running.** The CLI writes into `~/.bellman`; the scheduler lives in the
desktop app and reads its own directory. I created a timer, waited past its
time, and nothing happened — `last_fired: null`, no events. Nothing is broken
and nothing lied to me, but "the two stores are separate" and "one of them has
no clock in it" are different facts, and only the first is written down.
**Polish item**, recorded below.

**The lightbulb README teaches `run-now`.** `testing_apps/lightbulb/README.md`
step 1 creates the timer with `every_secs: 3600`, and step 3 says
`bellman run-now lightbulb-demo` "(or wait for the interval)". Following it
literally, a newcomer's *only* practical way to see the demo work is the one
path that does not exercise the scheduler. Making step 1 a short interval
would let the clock do it. **Polish item.**

**Two data directories, two demo invocations.** The wizard's demo panel builds
the right `--slots` path for you, which is exactly right. But if you follow
the terminal lightbulb's README instead, it hands you `~/.bellman/slots`, and
if your Bellman is the desktop app that is the empty store. The README does
say "for the desktop app use its app-data dir instead"; I still got it wrong
once before I read carefully. **Polish item.**

**`config.json` looked like it was being ignored.** I set
`log_rotation_max_bytes` to 6000 to watch a rotation, generated 280 KB of
events, and nothing rotated. I read the pruner, then the publisher, then the
config loader, and finally found `AppConfig::sanitized()` silently raising it
to a 1 MiB floor. Nothing in `docs/CONFIG.md` mentioned a floor — the table
lists defaults only. **This is a documentation defect and I fixed it**
(`0e65c98`): the floors and clamps are now in CONFIG.md, along with the fact
that `prune_interval_secs` is the startup catch-up threshold rather than the
`system.prune` timer's cadence, which I had also assumed wrongly.

**I had to open a source file to answer a protocol question — twice.**
INTEGRATION.md's payload table is the reference a stranger uses. It does not
list `misfire_policy` (which `docs/PLAN.md` promises as part of the v1 slot
schema), `workdir`, or `transport` — all three of which the wire format
accepts. I only learned they existed by reading `slots/envelope.rs`. By the
card's own rule ("when you have to open a source file to understand what a
doc means, write that down"), that is a documentation defect. **Fixed**
(`0e65c98`).

**The reply protocol is genuinely as small as it claims.** Writing the Perl
client took one read of *Connect your own application* and no source diving.
The two things that could have bitten me — that `reply_path` is absolute and
must be opened verbatim, and that the stub already carries the identity
fields — are both stated, in bold, next to the code. This part of the product
is in good shape.

**One doc sentence is optimistic about superseded runs.** INTEGRATION.md says
a late reply to a superseded run "is logged `superseded` and not applied",
which reads as though the file is still there to write to. It is not: Bellman
deletes the stub when the next firing supersedes the run, so a slow app going
back to its `reply_path` gets a missing file. The documented "compose the
minimal reply yourself" escape hatch works and the outcome is correct, but the
text sets the wrong expectation. **Polish item.**

**The GUI is pleasant and the wizard does the right thing.** The demo tick is
off by default, ticking it does *not* create a timer behind your back (the
demo claims its own), the panel gives you a copyable command as well as a
button, and the Run button correctly disappears when `tkinter` is missing with
a note telling you which package to install. Closing Bellman leaves the demo
running rather than killing it. Nothing here surprised me in a bad way.

**Running two Bellmans on one login session is not possible** — the
single-instance plugin is keyed on the D-Bus session, so a second launch with
a completely different `XDG_DATA_HOME` exits immediately and silently. That is
the documented single-instance behaviour ("second launch focuses the existing
window") and it is the right default; it is worth knowing that isolating by
data directory alone is not enough, which is why every run in this document
uses `dbus-run-session`. Not a defect; recorded because it cost me twenty
minutes of thinking the binary was broken.

### Polish items (nothing broken, worth doing)

All four were small enough to just do, in `a9a9eaf`:

| item | where | done |
|---|---|---|
| Say plainly that a timer in the CLI store does not fire unless something drives that store | `README.md` §Your data stays yours, `docs/LOCAL.md` | ✓ |
| Make the lightbulb README's demo timer a short interval so the clock, not `run-now`, shows the loop | `testing_apps/lightbulb/README.md` (3600 s → 120 s; step 3 now says wait, with `run-now` demoted to "proves the app answers, not that the scheduler works") | ✓ |
| Say that a superseded run's reply stub is deleted, and that composing the minimal reply is the way back | `docs/INTEGRATION.md` | ✓ |
| Warn that the lightbulb README's `~/.bellman/slots` is the CLI store and the desktop app's is elsewhere | `testing_apps/lightbulb/README.md` | ✓ |

---

## 5 — Originality

Separate document: **[docs/ORIGINALITY.md](ORIGINALITY.md)** — per-module
verdicts, the mechanical sweep that produced them
([`qa-c11/originality_sweep.py`](qa-c11/originality_sweep.py),
[`qa-c11/originality.json`](qa-c11/originality.json)), and the one shared line
that was reviewed and kept. Headline: **zero logic-bearing code shared** with
5.5 M characters of the seven reference projects; nothing needed rewriting.

## 6 — Polish

- **Personal-path CI gate still bites.** `scripts/check_no_personal_paths.sh`
  reports `personal-path gate: clean` on the tree including everything added
  by this card, and returns 1 with a precise message when a `/home/<user>`
  path is planted in a tracked file. Verified both directions.
- **Personal *names*, and an allowlist that outlived its reason.** The gate
  has a second scan, `PERSONAL_TOKENS`, which does cover the operator's
  username — six fixture strings carrying it (`visible/id.rs`,
  `visible/providers/at.rs`) passed only because both files were named in
  `TOKEN_ALLOWED_FILES`. They are pre-existing at this card's base commit,
  not introduced by it, and are now `alice`: `task_id()` hashes whatever it
  is handed and the `at -l` sample parses identically, so the change is
  inert. **The exemptions are deleted too** — left in place they would have
  permitted the token to return to exactly the two files just cleaned, while
  the gate reported clean. There is now no per-file exemption for the token
  scan at all, and the failure text says so, so the next person cannot
  "fix" a hit by re-adding one.

  Verified in both directions, per file:

  ```
  $ bash scripts/check_no_personal_paths.sh                    # baseline
  personal-path gate: clean                                    exit 0

  # the token planted in each formerly-exempt file, one at a time
  personal-token leak (…): crates/bellman-core/src/visible/id.rs:34: …
                                                               exit 1
  personal-token leak (…): crates/bellman-core/src/visible/providers/at.rs:44: …
                                                               exit 1
  ```

  An earlier revision of this bullet said the gate "does not cover" names and
  matched only paths. That was wrong — it was the allowlist, not a missing
  check. Both the fixtures and the description were caught by review rather
  than by me.
- **Naming and dead code.** `cargo clippy --workspace --all-targets -D
  warnings` is clean, which covers unused code, unused imports and the naming
  lints. No `#[allow(dead_code)]` was added by this card.
- **Formatting.** The workspace is rustfmt-clean and CI now enforces it.
- **Module docs: complete.** All 135 source files carry a `//!` header — two
  did not (`src-tauri/src/main.rs`, `src-tauri/src/dto_serde_tests.rs`) and
  now do.
- **Item docs: complete.** Every public function, method, struct field,
  variant and constant in `bellman-core` and the desktop shell now carries a
  sentence saying **why it exists**, not a restatement of its name.

  ```
  $ RUSTFLAGS="-W missing_docs" cargo check -p bellman-core --message-format=short \
      2>&1 | grep -c 'missing documentation'
  0            # was 593
  $ RUSTFLAGS="-W missing_docs" cargo check -p bellman-app  --message-format=short \
      2>&1 | grep -c 'missing documentation'
  0            # was 756 including bellman-core's
  ```

  `#![warn(missing_docs)]` now sits in `crates/bellman-core/src/lib.rs` and
  `src-tauri/src/lib.rs`, so the gap cannot come back quietly — and since CI
  builds with `RUSTFLAGS: -Dwarnings`, an undocumented `pub` fails the build
  rather than scrolling past.

  `cargo doc --no-deps` is also clean on both crates now (it was not: eight
  intra-doc links pointed at private items or at names not in scope, so the
  rendered pages had dead references).

  This was worth doing rather than deferring: the wire-shape types were
  already documented, but the *reasons* were not written down anywhere a
  reader would find them — that `scheduled_for` is an intent rather than an
  occurrence, that `no_ack_at` is retained after a late reply revises the
  state, that an oversize reply is rejected unread, that `TimerPatch::
  last_fired` is doubly wrapped because the outer `Some` means "change it"
  and the inner `None` means "clear it". Those sentences are the ones an
  integrator needs.

## Reproducing this

Everything is committed. [`docs/qa-c11/harness/`](qa-c11/harness/) holds the
script behind every scenario above, and
[its README](qa-c11/harness/README.md) has the prerequisites, the container
image recipe for the DST and packaged-demo runs, and the one environment
variable (`FAKETIME_DONT_FAKE_MONOTONIC`) without which the DST runs look
broken.

```sh
cd <repo>
(cd ui && npm ci)
cargo tauri build --no-bundle --ci        # target/release/{bellman,bellman-app}

cd docs/qa-c11/harness
for s in e2e_*.py; do python3 "$s" || echo "FAILED: $s"; done   # ~35 min total
```

Each script asserts its own outcome, exits non-zero on failure, and writes the
JSON it is quoted from. They keep out of your way by construction: a private
`Xvfb`, a private `XDG_DATA_HOME`/`HOME`, and a private D-Bus session, so your
own Bellman and your own data directory are untouched. Paths are resolved from
the script's location; `BELLMAN_ROOT` and `BELLMAN_QA_RUN_ROOT` override them.

Two artefacts are useful beyond this run:
[`qa-c11/clock_in.pl`](qa-c11/clock_in.pl) (the Perl client, written from
INTEGRATION.md alone) and
[`qa-c11/originality_sweep.py`](qa-c11/originality_sweep.py) (the originality
sweep, which takes `BELLMAN_REFERENCE_REPOS` and re-runs anywhere).

The three README §Install runs are committed too, under
[`qa-c11/harness/install/`](qa-c11/harness/install/) — the README's own
commands in order, with the two stated substitutions (`sudo` dropped where
the image has none, and a local clone) named in the harness README.
