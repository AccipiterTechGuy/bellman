//! Refuse store open when the database path sits on a network filesystem.

use super::error::{StoreError, StoreResult};
use std::path::{Path, PathBuf};

/// Known network / remote filesystem types (Linux `statfs` / mountinfo).
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
    "afs",
    "ncpfs",
    "glusterfs",
    "ceph",
    "lustre",
    "9p",
];

/// Ensure `path` (or its nearest existing ancestor) is not on a network FS.
pub fn refuse_network_fs(path: &Path) -> StoreResult<()> {
    let probe = nearest_existing_ancestor(path);
    if let Some(fs_type) = detect_fs_type(&probe) {
        let lower = fs_type.to_ascii_lowercase();
        if NETWORK_FS_TYPES.iter().any(|n| lower == *n || lower.starts_with(&format!("{n}.")))
            || lower.starts_with("fuse.")
                && NETWORK_FS_TYPES
                    .iter()
                    .any(|n| n.starts_with("fuse.") && lower.starts_with(n))
        {
            return Err(StoreError::NetworkFilesystem(format!(
                "{} is on filesystem type '{fs_type}' (network/remote stores are not supported)",
                probe.display()
            )));
        }
        // Broad fuse.* remote-ish catch for sshfs/rclone variants already listed;
        // plain fuse. is local-ish (e.g. fuse.ntfs) — only reject known remotes.
        if matches!(
            lower.as_str(),
            "nfs" | "nfs4" | "cifs" | "smb" | "smb3" | "smbfs" | "afs" | "ceph" | "lustre" | "9p"
        ) || lower.starts_with("fuse.sshfs")
            || lower.starts_with("fuse.rclone")
            || lower.starts_with("fuse.davfs")
        {
            return Err(StoreError::NetworkFilesystem(format!(
                "{} is on filesystem type '{fs_type}' (network/remote stores are not supported)",
                probe.display()
            )));
        }
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
            // Fall back to cwd.
            return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        }
    }
}

/// Best-effort FS type detection via `/proc/self/mountinfo` (Linux).
/// Returns `None` when detection is unavailable (non-Linux / unreadable).
fn detect_fs_type(path: &Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        detect_fs_type_linux(path)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        None
    }
}

#[cfg(target_os = "linux")]
fn detect_fs_type_linux(path: &Path) -> Option<String> {
    let abs = std::fs::canonicalize(path).ok().or_else(|| {
        // Path may not exist yet — canonicalize parent.
        path.parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .or_else(|| std::env::current_dir().ok())
    })?;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;

    // Pick the longest mount point that is a prefix of `abs`.
    let mut best_len = 0usize;
    let mut best_fstype: Option<String> = None;

    for line in mountinfo.lines() {
        // mountinfo fields: … mount_point mount_source … - fstype source superopts
        // See https://www.kernel.org/doc/Documentation/filesystems/proc.txt
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mut left_parts = left.split_whitespace();
        // skip id, parent, major:minor, root
        let _ = left_parts.next()?;
        let _ = left_parts.next()?;
        let _ = left_parts.next()?;
        let _ = left_parts.next()?;
        let mount_point = left_parts.next()?;
        // Optional fields until '-', already split off.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_tmp_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("timers.db");
        refuse_network_fs(&db).expect("local tmp must be allowed");
    }
}
