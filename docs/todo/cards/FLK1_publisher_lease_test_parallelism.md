# FLK1 — the publisher lease blocks itself under parallel in-process tests

Raised by **C11** (full-system validation, 2026-08-02), which was asked to
make `cargo test --workspace --all-targets` repeatably green and got most,
but not all, of the way. Two genuine product bugs came out of chasing it
(the slot-name collision and the replenisher clobber, both fixed and
regression-tested on that card). This is the part that is left.

## The symptom

Run in a loop, the workspace suite fails intermittently — never the same
assertion twice in a row, always in the small family of tests that drive
`EventPublisher` leadership:

- `events::publisher::tests::recovery_keeps_the_journal_when_cleanup_fails`
- `pruner::tests::prune_with_lease_elsewhere_reports_skipped_and_never_stamps_last_prune`
- `pruner::tests::prune_rotates_jsonl_and_respects_retention_edges`

Each ends up looking like "the publisher was not the leader when it should
have been", which then surfaces as a misleading downstream assertion
(nothing rotated, no error cleared, no archive).

## What is known

- **Serial runs are clean.** `cargo test -p bellman-core --lib --
  --test-threads=1` passed 366/366 three times in a row. The failures need
  parallel in-process execution.
- **Every test uses its own `tempfile::tempdir()`,** so no two share a
  `publisher.lock` path. Confirmed by inspection of every
  `EventPublisher::{open,with_config}` call site.
- **The lock really is unavailable, not merely reported so.** Instrumenting
  `gate::try_acquire_file` to dump `/proc/self/fd` on `WOULDBLOCK` caught it:

  ```
  DIAGLOCK wouldblock on /tmp/.tmpW1y5D4/logs/publisher.lock
    open fds: ["31"=>/tmp/.tmpW1y5D4/logs/publisher.lock",
               "42"=>/tmp/.tmpWzwpJS/logs/publisher.lock"]
  ```

  Exactly one fd in this process points at the contended path — the one
  `try_acquire_file` had just opened itself. So the `flock` is held by
  something that is **not** a live fd of this process at that moment.
- The owning `EventPublisher` had already released: the same run printed
  `watcher_is_leader=false` immediately before.

## Where to look next

`flock` locks belong to the open file description, so a lock outliving every
visible fd points at a description still alive somewhere — an inherited fd in
a child process is the obvious candidate, and these suites spawn `sleep`
children through the action executor. Rust opens files `O_CLOEXEC` by
default, so if that is the mechanism, something is bypassing it (a `dup`, a
`pre_exec`, or a crate that opens without the flag). Worth checking:

1. whether any launch/executor path spawns while a lease guard is held;
2. whether `rustix`/`tempfile`/`rusqlite` open any of these files without
   `O_CLOEXEC`;
3. whether the same thing can happen in the **product** — a Bellman that
   spawns a wake action while holding the publisher lease would starve every
   other publisher for the life of that child, which would be a real bug and
   not merely a test annoyance. **Answer this one first.**

## Scope

- Find the mechanism, then fix it in whichever layer owns it.
- If it turns out to be test-only, say so with evidence and make the suite
  green without weakening any assertion.
- If (3) reproduces in the product, that is the actual bug and this card
  becomes a lease-lifetime fix.

## Do NOT

- Do not paper over it by retrying leadership inside the tests until they
  pass; C11 already did that in exactly one place where the product's own
  contract says a lost election is legitimate and retried next pass, and that
  is the limit of what is honest without knowing the cause.
- Do not raise the timeouts. They are already 10–30 s.

## Exit gate

- The mechanism is named, with evidence.
- Question (3) is answered explicitly, either way.
- `cargo test --workspace --all-targets` run 25 times in a row on an idle
  machine, all green, with no assertion weakened relative to today.
