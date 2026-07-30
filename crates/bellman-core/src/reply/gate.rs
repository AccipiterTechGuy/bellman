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
