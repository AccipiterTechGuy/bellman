//! Refuse store open when the database path sits on a network filesystem.
//!
//! Detection is platform-specific and **fail-closed**: if the filesystem type
//! cannot be determined, the open is refused. Callers that intentionally open
//! on unusual local FS types can set [`super::OpenOptions::refuse_network_fs`]
//! to `false` (tests only).

use super::error::{StoreError, StoreResult};
use std::path::{Path, PathBuf};

/// Known network / remote filesystem type names (case-insensitive match).
const NETWORK_FS_TYPES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb",
    "smb3",
    "smbfs",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.davfs2",
    "fuse.davfs",
    "afs",
    "ncpfs",
    "glusterfs",
    "ceph",
    "lustre",
    "9p",
    // macOS / BSD names
    "nfs",
    "smbfs",
    "afpfs",
    "webdav",
    // Windows GetDriveType reports DRIVE_REMOTE; we map that to this token.
    "remote",
    "network",
];

/// True when `fs_type` names a network/remote filesystem.
pub fn is_network_fs_type(fs_type: &str) -> bool {
    let lower = fs_type.to_ascii_lowercase();
    if NETWORK_FS_TYPES
        .iter()
        .any(|n| lower == *n || lower.starts_with(&format!("{n}.")))
    {
        return true;
    }
    // Remote FUSE helpers (sshfs/rclone/davfs), not local fuse.ntfs etc.
    if lower.starts_with("fuse.sshfs")
        || lower.starts_with("fuse.rclone")
        || lower.starts_with("fuse.davfs")
    {
        return true;
    }
    matches!(
        lower.as_str(),
        "nfs"
            | "nfs4"
            | "cifs"
            | "smb"
            | "smb3"
            | "smbfs"
            | "afs"
            | "ceph"
            | "lustre"
            | "9p"
            | "afpfs"
            | "webdav"
            | "remote"
            | "network"
    )
}

/// Ensure `path` (or its nearest existing ancestor) is not on a network FS.
pub fn refuse_network_fs(path: &Path) -> StoreResult<()> {
    let probe = nearest_existing_ancestor(path);

    // UNC / share path shapes are network regardless of probe success.
    if is_unc_or_share_path(path) || is_unc_or_share_path(&probe) {
        return Err(StoreError::NetworkFilesystem(format!(
            "{} looks like a UNC/network share path (network/remote stores are not supported)",
            path.display()
        )));
    }

    let fs_type = detect_fs_type(&probe).ok_or_else(|| {
        StoreError::NetworkFilesystem(format!(
            "cannot determine filesystem type for {} — refusing open (fail-closed; \
             network/remote stores are not supported)",
            probe.display()
        ))
    })?;

    if is_network_fs_type(&fs_type) {
        return Err(StoreError::NetworkFilesystem(format!(
            "{} is on filesystem type '{fs_type}' (network/remote stores are not supported)",
            probe.display()
        )));
    }
    Ok(())
}

fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    loop {
        if cur.exists() {
            return cur;
        }
        if !cur.pop() {
            return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        }
    }
}

/// UNC (`\\server\share`) or URL-like share paths.
fn is_unc_or_share_path(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    // Windows UNC and mixed-separator variants.
    if s.starts_with(r"\\") || s.starts_with("//") {
        return true;
    }
    // `smb://`, `nfs://`, `afp://` style.
    let lower = s.to_ascii_lowercase();
    lower.starts_with("smb://")
        || lower.starts_with("nfs://")
        || lower.starts_with("afp://")
        || lower.starts_with("cifs://")
}

/// Detect filesystem type name for `path`. Returns `None` when detection fails
/// (caller must fail closed).
fn detect_fs_type(path: &Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        detect_fs_type_linux(path)
    }
    #[cfg(target_os = "macos")]
    {
        detect_fs_type_macos(path)
    }
    #[cfg(windows)]
    {
        detect_fs_type_windows(path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = path;
        // Unsupported platform: no detector → fail closed at refuse_network_fs.
        None
    }
}

#[cfg(target_os = "linux")]
fn detect_fs_type_linux(path: &Path) -> Option<String> {
    let abs = std::fs::canonicalize(path).ok().or_else(|| {
        path.parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .or_else(|| std::env::current_dir().ok())
    })?;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;

    let mut best_len = 0usize;
    let mut best_fstype: Option<String> = None;

    for line in mountinfo.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mut left_parts = left.split_whitespace();
        let _ = left_parts.next()?; // id
        let _ = left_parts.next()?; // parent
        let _ = left_parts.next()?; // major:minor
        let _ = left_parts.next()?; // root
        let mount_point = left_parts.next()?;

        let mut right_parts = right.split_whitespace();
        let fstype = right_parts.next()?;

        let mp = Path::new(mount_point);
        if path_is_under(&abs, mp) {
            let len = mount_point.len();
            if len >= best_len {
                best_len = len;
                best_fstype = Some(fstype.to_string());
            }
        }
    }
    best_fstype
}

#[cfg(target_os = "linux")]
fn path_is_under(path: &Path, mount: &Path) -> bool {
    if mount == Path::new("/") {
        return true;
    }
    path.starts_with(mount)
}

/// macOS: `statfs` via libc (always linked on Darwin).
#[cfg(target_os = "macos")]
fn detect_fs_type_macos(path: &Path) -> Option<String> {
    use std::ffi::{CStr, CString};
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct Statfs {
        f_bsize: u32,
        f_iosize: i32,
        f_blocks: u64,
        f_bfree: u64,
        f_bavail: u64,
        f_files: u64,
        f_ffree: u64,
        f_fsid: [i32; 2],
        f_owner: u32,
        f_type: u32,
        f_flags: u32,
        f_fssubtype: u32,
        f_fstypename: [u8; 16],
        f_mntonname: [u8; 1024],
        f_mntfromname: [u8; 1024],
        f_flags_ext: u32,
        f_reserved: [u32; 7],
    }

    extern "C" {
        fn statfs(path: *const libc::c_char, buf: *mut Statfs) -> i32;
    }

    // libc is always present on macOS; use c_char from std via i8.
    mod libc {
        pub type c_char = i8;
    }

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf = MaybeUninit::<Statfs>::uninit();
    let rc = unsafe { statfs(c_path.as_ptr(), buf.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let buf = unsafe { buf.assume_init() };
    let name = unsafe { CStr::from_ptr(buf.f_fstypename.as_ptr() as *const i8) };
    let s = name.to_string_lossy().into_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Windows: UNC already caught; drive root via `GetDriveTypeW`.
#[cfg(windows)]
fn detect_fs_type_windows(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;

    const DRIVE_UNKNOWN: u32 = 0;
    const DRIVE_NO_ROOT_DIR: u32 = 1;
    const DRIVE_REMOTE: u32 = 4;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(lp_root_path_name: *const u16) -> u32;
    }

    // Build a root like `C:\` from the path.
    let root = windows_drive_root(path)?;
    let wide: Vec<u16> = std::ffi::OsString::from(root)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dtype = unsafe { GetDriveTypeW(wide.as_ptr()) };
    match dtype {
        DRIVE_REMOTE => Some("remote".into()),
        DRIVE_UNKNOWN | DRIVE_NO_ROOT_DIR => None,
        _ => Some("local".into()),
    }
}

#[cfg(windows)]
fn windows_drive_root(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    // `C:\...` or `C:/...`
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let letter = s.chars().next()?;
        if letter.is_ascii_alphabetic() {
            return Some(format!("{letter}:\\"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_tmp_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("timers.db");
        refuse_network_fs(&db).expect("local tmp must be allowed");
    }

    #[test]
    fn classifies_known_network_types() {
        for t in [
            "nfs",
            "nfs4",
            "cifs",
            "smb",
            "smb3",
            "smbfs",
            "fuse.sshfs",
            "fuse.rclone",
            "fuse.davfs2",
            "afs",
            "ceph",
            "lustre",
            "9p",
            "afpfs",
            "webdav",
            "remote",
            "network",
            "NFS",
            "CIFS",
        ] {
            assert!(is_network_fs_type(t), "{t} should be network");
        }
    }

    #[test]
    fn classifies_local_types() {
        for t in [
            "ext4", "xfs", "btrfs", "zfs", "apfs", "hfs", "ntfs", "exfat", "tmpfs", "overlay",
            "local",
        ] {
            assert!(!is_network_fs_type(t), "{t} should be local");
        }
    }

    #[test]
    fn unc_paths_are_refused() {
        let err = refuse_network_fs(Path::new(r"\\fileserver\share\timers.db")).unwrap_err();
        assert!(matches!(err, StoreError::NetworkFilesystem(_)), "got {err}");
        let err2 = refuse_network_fs(Path::new("//fileserver/share/timers.db")).unwrap_err();
        assert!(matches!(err2, StoreError::NetworkFilesystem(_)));
    }

    #[test]
    fn smb_url_paths_are_refused() {
        let err = refuse_network_fs(Path::new("smb://server/share/timers.db")).unwrap_err();
        assert!(matches!(err, StoreError::NetworkFilesystem(_)));
    }

    #[test]
    fn network_type_token_is_refused_by_classifier() {
        // The open path uses detect_fs_type → is_network_fs_type; prove the
        // decision table without requiring a real NFS mount.
        assert!(is_network_fs_type("nfs"));
        assert!(is_network_fs_type("cifs"));
        assert!(!is_network_fs_type("ext4"));
    }
}
