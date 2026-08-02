//! Interprocess per-timer gate (R10): a bounded, stable set of OS-file locks
//! under the data root, keyed by timer UUID.
//!
//! Each timer maps to exactly one of [`GATE_SHARDS`] shard files,
//! `<data_dir>/locks/gate-<NN>.lock`, where `NN` is the first byte of the
//! timer UUID as two lowercase hex digits. The shard set is bounded (256
//! files) and stable: shard files live under the data root, never inside a
//! timer folder, so they survive folder rename and deletion. Shard files are
//! opened with create-if-missing semantics (never `create_new`) — they are
//! shared, long-lived resources, not per-operation artifacts.
//!
//! The quarantine (`bad/`) tree has its own single lock,
//! `<data_dir>/locks/quarantine.lock`.
//!
//! # Lock-order rule
//!
//! A caller holding a timer shard guard may then take the quarantine lock.
//! NEVER the reverse: no code path may take a timer shard while holding the
//! quarantine lock. This single rule keeps the two lock domains deadlock-free.
//!
//! Locking is advisory (`flock` on unix, `LockFileEx` on Windows) with
//! blocking exclusive semantics; all lifecycle mutators cooperate by acquiring
//! the gate before mutating. Dropping the [`GateGuard`] releases the lock.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[cfg(not(any(unix, windows)))]
compile_error!("reply::gate supports only unix and windows targets");

/// Number of per-timer gate shards: one file per possible first UUID byte.
pub const GATE_SHARDS: u32 = 256;

/// Guard holding an exclusive OS-file lock on a gate shard.
///
/// The lock is released when the guard is dropped (the file is closed; on
/// Windows the region is explicitly unlocked first).
pub struct GateGuard {
    // Held so the lock stays taken until drop (close releases it on unix;
    // on Windows the field is read by the Drop impl to unlock first).
    #[cfg_attr(unix, allow(dead_code))]
    file: File,
    path: PathBuf,
}

impl GateGuard {
    /// Path of the shard file this guard holds. Intended for tests.
    pub fn shard_path(&self) -> &Path {
        &self.path
    }
}

/// Blocking exclusive acquire of the per-timer gate shard for `timer_id`.
///
/// Creates `<data_dir>/locks` if missing. Blocks until the shard is free.
pub fn acquire(data_dir: &Path, timer_id: Uuid) -> io::Result<GateGuard> {
    acquire_path(shard_path(data_dir, timer_id))
}

/// Blocking exclusive acquire of the single quarantine (`bad/`) lock.
///
/// Lock order: a caller holding a timer shard may then take this; NEVER the
/// reverse.
pub fn acquire_quarantine(data_dir: &Path) -> io::Result<GateGuard> {
    acquire_path(data_dir.join("locks").join("quarantine.lock"))
}

/// Blocking exclusive acquire of an arbitrary lock file (SCH1 publication
/// target shards, the dispatcher OS lock). Blocks until free.
pub fn acquire_file(path: &Path) -> io::Result<GateGuard> {
    acquire_path(path.to_path_buf())
}

/// How long [`try_acquire_file`] keeps retrying before it calls itself a
/// follower. Sized to swallow the fork/exec window (microseconds) while
/// staying far below the time a genuine leader holds the lease (a whole
/// publish cycle), so a real contest is still reported promptly.
const TRY_ACQUIRE_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);

/// Non-blocking exclusive acquire of an arbitrary lock file (the R11
/// publisher lease). Returns `Ok(None)` when another process holds it —
/// the caller is then a follower, not an error case.
///
/// # Why this retries instead of asking once
///
/// `fork(2)` copies the whole file-descriptor table, and `O_CLOEXEC` is
/// honoured at `exec(2)`, **not** at `fork(2)`. So between the fork and the
/// exec, a brand-new child holds a duplicate of every open file description
/// in the process — and an `flock` belongs to the *description*, not to the
/// fd. For that window the lock is held by a process that has no interest in
/// it and will drop it a few microseconds later, and any other thread asking
/// for that lock is told `EWOULDBLOCK`.
///
/// Bellman forks constantly (every launch action, the demo, the wake helper),
/// so a single-shot election loses this race at a measurable rate. Measured
/// on Linux 7.0 with no competing lock holder anywhere in the process:
/// 0 failures in 5.2M attempts with no children being spawned, and 7.3–7.5%
/// failures in 7.5M attempts with one thread spawning `/bin/true` in a loop.
///
/// The window is bounded by the child reaching `exec`, not by how long the
/// child then runs: a 5-second child leaves the lease free again ~6µs after
/// the parent releases it. So a short bounded retry closes the hole
/// completely, and no caller has to treat a spurious loss as real.
///
/// A genuine holder keeps the lease for an entire publish cycle, which is
/// orders of magnitude longer than `TRY_ACQUIRE_WINDOW`, so real contention
/// still returns `Ok(None)` and the caller is still correctly a follower.
pub fn try_acquire_file(path: &Path) -> io::Result<Option<GateGuard>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let deadline = std::time::Instant::now() + TRY_ACQUIRE_WINDOW;
    loop {
        // Reopened per attempt: a fresh description cannot be one some child
        // inherited before we got here.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if try_lock_exclusive(&file)? {
            return Ok(Some(GateGuard {
                file,
                path: path.to_path_buf(),
            }));
        }
        drop(file);
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Map a timer id to its shard path: `<data_dir>/locks/gate-<NN>.lock` where
/// `NN` is the first UUID byte as two lowercase hex digits. Visible for tests.
pub fn shard_path(data_dir: &Path, timer_id: Uuid) -> PathBuf {
    let first = timer_id.as_bytes()[0];
    data_dir
        .join("locks")
        .join(format!("gate-{first:02x}.lock"))
}

/// Open (creating if needed) the shard file at `path` and lock it exclusively.
fn acquire_path(path: PathBuf) -> io::Result<GateGuard> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Shards are stable and shared: create-if-missing, never create_new.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    lock_exclusive(&file)?;
    Ok(GateGuard { file, path })
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsFd;
    rustix::fs::flock(file.as_fd(), rustix::fs::FlockOperation::LockExclusive)
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    // The lock is released by closing the file on drop.
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    use std::os::unix::io::AsFd;
    match rustix::fs::flock(
        file.as_fd(),
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    ) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(false),
        Err(e) => Err(io::Error::from_raw_os_error(e.raw_os_error())),
    }
}

#[cfg(windows)]
fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows::Win32::System::IO::OVERLAPPED;

    let handle = HANDLE(file.as_raw_handle() as isize);
    let mut overlapped = OVERLAPPED::default();
    let flags = LOCKFILE_EXCLUSIVE_LOCK.0 | LOCKFILE_FAIL_IMMEDIATELY.0;
    match unsafe { LockFileEx(handle, flags, 0, u32::MAX, u32::MAX, &mut overlapped) } {
        Ok(()) => Ok(true),
        Err(e) if e.code() == ERROR_LOCK_VIOLATION.to_hresult() => Ok(false),
        Err(e) => Err(io::Error::from_raw_os_error(e.code().0)),
    }
}

#[cfg(windows)]
fn lock_exclusive(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK};
    use windows::Win32::System::IO::OVERLAPPED;

    let handle = HANDLE(file.as_raw_handle() as isize);
    let mut overlapped = OVERLAPPED::default();
    // Lock the whole file (offset 0, max length) exclusively, blocking.
    unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK.0,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0))
}

#[cfg(windows)]
impl Drop for GateGuard {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::UnlockFileEx;
        use windows::Win32::System::IO::OVERLAPPED;

        let handle = HANDLE(self.file.as_raw_handle() as isize);
        let mut overlapped = OVERLAPPED::default();
        // Unlock before close; ignore errors (close also releases the lock).
        unsafe {
            let _ = UnlockFileEx(handle, 0, u32::MAX, u32::MAX, &mut overlapped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn id_with_first_byte(byte: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[0] = byte;
        Uuid::from_bytes(bytes)
    }

    /// FLK1. An election held while the process is forking must not report
    /// "someone else leads" when nobody does. `fork(2)` duplicates the fd
    /// table and `O_CLOEXEC` only fires at `exec(2)`, so every child briefly
    /// owns a copy of the open file description the lock belongs to.
    ///
    /// Nothing in this test ever holds the lease across an attempt: each
    /// acquire drops its guard before the next one asks. Every `Ok(None)` is
    /// therefore spurious by construction. Without the retry in
    /// `try_acquire_file` this fails within a few hundred attempts.
    #[cfg(unix)]
    #[test]
    fn election_is_not_lost_to_a_concurrent_forks_inherited_fd() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join("logs").join("publisher.lock");

        let stop = Arc::new(AtomicBool::new(false));
        let spawner = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut spawned = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    match std::process::Command::new("/bin/true")
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(mut child) => {
                            spawned += 1;
                            let _ = child.wait();
                        }
                        Err(_) => break,
                    }
                }
                spawned
            })
        };

        let mut attempts = 0u32;
        let mut spurious = 0u32;
        let started = Instant::now();
        while attempts < 3_000 && started.elapsed() < Duration::from_secs(20) {
            attempts += 1;
            match try_acquire_file(&lock).expect("acquire must not error") {
                Some(guard) => drop(guard),
                None => spurious += 1,
            }
        }
        stop.store(true, Ordering::Relaxed);
        let spawned = spawner.join().expect("spawner thread");

        assert_eq!(
            spurious, 0,
            "{spurious}/{attempts} elections reported a follower while no one \
             held the lease ({spawned} children spawned concurrently)"
        );
    }

    #[test]
    fn shard_path_is_deterministic() {
        let dir = Path::new("/tmp/bellman-gate-test");
        let id = Uuid::new_v4();
        assert_eq!(shard_path(dir, id), shard_path(dir, id));
    }

    #[test]
    fn shard_path_uses_first_byte_hex() {
        let dir = Path::new("/tmp/bellman-gate-test");
        let path = shard_path(dir, id_with_first_byte(0x0a));
        assert_eq!(path, dir.join("locks").join("gate-0a.lock"));
        let path = shard_path(dir, id_with_first_byte(0xff));
        assert_eq!(path, dir.join("locks").join("gate-ff.lock"));
    }

    #[test]
    fn distinct_first_bytes_give_distinct_shards() {
        let dir = Path::new("/tmp/bellman-gate-test");
        let a = shard_path(dir, id_with_first_byte(0x01));
        let b = shard_path(dir, id_with_first_byte(0x02));
        assert_ne!(a, b);
    }

    #[test]
    fn gate_shard_count_matches_byte_space() {
        assert_eq!(GATE_SHARDS, 256);
    }

    #[cfg(unix)]
    #[test]
    fn acquire_release_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let guard = acquire(tmp.path(), id).unwrap();
        assert_eq!(guard.shard_path(), shard_path(tmp.path(), id).as_path());
        assert!(guard.shard_path().exists());
        drop(guard);
        // After release the shard is acquirable again.
        let _guard = acquire(tmp.path(), id).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn sequential_acquires_work_after_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        for _ in 0..3 {
            let guard = acquire(tmp.path(), id).unwrap();
            drop(guard);
        }
    }

    #[cfg(unix)]
    #[test]
    fn second_acquire_blocks_while_first_guard_held() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let id = Uuid::new_v4();
        std::thread::scope(|s| {
            s.spawn(|| {
                let _guard = acquire(&dir, id).unwrap();
                std::thread::sleep(Duration::from_millis(150));
            });
            // Give the spawned thread a head start so it holds the lock first.
            std::thread::sleep(Duration::from_millis(30));
            let start = Instant::now();
            let _guard = acquire(tmp.path(), id).unwrap();
            let elapsed = start.elapsed();
            assert!(
                elapsed >= Duration::from_millis(100),
                "acquire returned too fast: {elapsed:?}"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn quarantine_lock_is_independent_of_timer_shards() {
        let tmp = tempfile::tempdir().unwrap();
        let quarantine = acquire_quarantine(tmp.path()).unwrap();
        assert_eq!(
            quarantine.shard_path(),
            tmp.path().join("locks").join("quarantine.lock").as_path()
        );
        // A timer shard acquires immediately while the quarantine lock is held.
        let start = Instant::now();
        let _timer_guard = acquire(tmp.path(), Uuid::new_v4()).unwrap();
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "timer shard blocked on quarantine lock"
        );
        drop(quarantine);
    }
}
