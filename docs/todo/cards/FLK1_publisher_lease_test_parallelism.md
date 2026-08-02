# FLK1 — RESOLVED: the publisher lease was lost to a forking sibling thread

Raised by **C11** (full-system validation, 2026-08-02) and **closed by C11**
on the same card once the mechanism was found. Kept as the write-up, because
the cause is a POSIX behaviour that will bite anyone who adds another
non-blocking lock to this codebase.

Three genuine product bugs came out of chasing this: the slot-name collision,
the replenisher clobber, and the one below.

## The symptom

Run in a loop, the workspace suite failed intermittently — never the same
assertion twice in a row, always in the family of tests that drive
`EventPublisher` leadership:

- `events::publisher::tests::recovery_keeps_the_journal_when_cleanup_fails`
- `pruner::tests::prune_with_lease_elsewhere_reports_skipped_and_never_stamps_last_prune`
- `pruner::tests::prune_rotates_jsonl_and_respects_retention_edges`

Each ended up looking like "the publisher was not the leader when it should
have been", which then surfaced as a misleading downstream assertion
(nothing rotated, no error cleared, no archive).

## The mechanism

**`fork(2)` copies the file-descriptor table; `O_CLOEXEC` is honoured at
`exec(2)`, not at `fork(2)`. An `flock` belongs to the open file description,
not to the descriptor. So between fork and exec, a brand-new child holds a
duplicate of every lock the parent holds** — including one another thread is
about to release. Any thread asking for that lock in that window is told
`EWOULDBLOCK` by a "holder" that has no interest in it and will drop it
microseconds later.

Bellman forks constantly — every launch action, the demo, the wake helper —
and the test binary runs 366 of those concurrently, so a single-shot election
lost the race at a measurable rate.

Isolated, outside the codebase, with **no competing lock holder anywhere in
the program** (the only lock/unlock is the measuring loop itself, which always
releases before it asks again, so every failure is spurious by construction):

| another thread spawning children | attempts | spurious `EWOULDBLOCK` |
|---|---|---|
| no | 5,234,658 | **0** (0.000%) |
| yes | 7,564,851 | **499,234** (7.25 / 7.49 / 7.46 %) |

The earlier `/proc`-wide diagnostic had found no holder at all — every process
on the machine was scanned and only our own just-opened fd pointed at the
contended inode. That is the fingerprint of exactly this: by the time the scan
ran, the child had exec'd and the inherited copy was gone.

## Question (3) — does this hurt the product? Answered: no, and here is why

The card asked this first, and it deserved the fear it was written with. It
does not reproduce, for a specific reason:

**The exposure is the fork→exec window, not the child's lifetime.** `exec` is
where `O_CLOEXEC` fires, so the inherited copy dies there. Measured: hold the
lease, spawn a child that sleeps 5 seconds, release the lease — the lease is
free again after **~6µs** (7.397µs / 5.584µs / 6.167µs over three runs), not
after 5 seconds. A Bellman that spawns a wake action while holding the
publisher lease does **not** starve other publishers for the life of that
child.

Every spawn path was audited for anything that would widen this:

| site | what it does | verdict |
|---|---|---|
| `actions/launch.rs:95` | `pre_exec` → `setsid()` only | execs immediately; window unchanged |
| `src-tauri/src/demo.rs:198` | `process_group(0)` | no fd work at all |
| `reply/watcher.rs:72` | `rustix::fs::open` | **defect, fixed — see below** |

Nothing anywhere uses `dup2`, keeps a `fork` without `exec`, or hands a
descriptor to a child on purpose.

Worst case in the product before the fix was therefore *one skipped
maintenance round*, which `prune` already handles correctly: it reports
`skipped_not_leader`, does **not** stamp `last_prune`, and the next tick
retries. Real, but bounded and self-healing.

## The fix

`reply::gate::try_acquire_file` now retries for a bounded 100 ms instead of
asking once. That is orders of magnitude longer than a fork→exec window and
orders of magnitude shorter than a real leader's hold (a whole publish cycle),
so genuine contention is still reported promptly and correctly as
`Ok(None)` — the caller is still a follower when someone really leads. Each
attempt reopens the file, so a description some child inherited before we
arrived cannot be the one we test.

Regression test:
`reply::gate::tests::election_is_not_lost_to_a_concurrent_forks_inherited_fd`
— spawns children from one thread while electing on another, never holding a
guard across an attempt, and asserts zero spurious follower reports. Without
the retry it fails at **120 of 3000** attempts.

**No assertion anywhere was weakened.** In particular
`pruner/tests.rs:129` (`test setup: prune must hold the publisher lease`)
still demands that the prune wins its election outright, and now it does.

## The second defect this audit turned up

`reply/watcher.rs` opened reply files through `rustix::fs::open`, which passes
`OFlags` through verbatim — unlike `std::fs::File::open`, which sets
`O_CLOEXEC` for you. The reply-file descriptor was therefore inherited by
every process Bellman launches, and those are **user-supplied commands from
timer configuration**. Fixed by adding `OFlags::CLOEXEC`. It is the only
`rustix::fs::open` in the tree; the rest go through `std`.

## Exit gate — met

- [x] The mechanism is named, with evidence (table above, both directions).
- [x] Question (3) is answered explicitly: no, and measured.
- [x] `cargo test --workspace --all-targets`, 25 consecutive runs, all green,
      with no assertion weakened relative to before.
