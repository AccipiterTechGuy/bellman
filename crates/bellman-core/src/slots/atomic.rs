//! Atomic JSON publish: temp file in the target directory + same-dir rename.
//!
//! On Windows, `NamedTempFile::persist` uses ReplaceFile-style semantics so a
//! concurrent reader never sees a partial file. Never edit slot JSON in place.

use super::error::{SlotError, SlotResult};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

/// Default max bytes for a single slot input read (256 KiB).
pub const DEFAULT_MAX_READ_BYTES: u64 = 256 * 1024;

/// Write `value` as pretty JSON into `dir/file_name` via temp + atomic rename.
///
/// The temp file is created **in the same directory** as the final path so the
/// rename stays on one filesystem (required for atomicity).
///
/// `file_name` must be a single path component (no `/`, `\`, `..`, or absolute
/// prefixes). The final path is always a direct child of `dir`.
pub fn atomic_write_json(
    dir: &Path,
    file_name: &str,
    value: &impl Serialize,
) -> SlotResult<PathBuf> {
    fs::create_dir_all(dir)
        .map_err(|e| SlotError::Io(format!("create_dir_all {}: {e}", dir.display())))?;
    let final_path = safe_child_path(dir, file_name)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_write_bytes(dir, &final_path, &bytes)?;
    Ok(final_path)
}

/// Write raw bytes to `final_path` (which must live under `dir`) via temp+persist.
pub fn atomic_write_bytes(dir: &Path, final_path: &Path, bytes: &[u8]) -> SlotResult<()> {
    // Enforce final_path is a direct child of dir (no attacker-selected parents).
    let file_name = final_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| SlotError::Invalid("atomic write: missing file name".into()))?;
    let expected = safe_child_path(dir, file_name)?;
    if final_path != expected {
        return Err(SlotError::Invalid(format!(
            "atomic write: final path {} is not a direct child of {}",
            final_path.display(),
            dir.display()
        )));
    }
    // Parent is `dir` only — never create_dir_all on an attacker path.
    fs::create_dir_all(dir)
        .map_err(|e| SlotError::Io(format!("create_dir_all {}: {e}", dir.display())))?;
    let mut tmp = NamedTempFile::new_in(dir)
        .map_err(|e| SlotError::Io(format!("NamedTempFile::new_in {}: {e}", dir.display())))?;
    tmp.write_all(bytes)
        .map_err(|e| SlotError::Io(format!("write temp: {e}")))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| SlotError::Io(format!("sync temp: {e}")))?;
    // Windows-safe: persist uses atomic replace when the target exists.
    tmp.persist(&expected).map_err(|e| {
        SlotError::Io(format!(
            "persist {} → {}: {e}",
            e.file.path().display(),
            expected.display()
        ))
    })?;
    Ok(())
}

/// Resolve `dir/file_name` only when `file_name` is a single safe component.
pub fn safe_child_path(dir: &Path, file_name: &str) -> SlotResult<PathBuf> {
    if file_name.is_empty()
        || file_name.contains('\0')
        || file_name.contains('/')
        || file_name.contains('\\')
    {
        return Err(SlotError::Invalid(format!(
            "unsafe file name (path separators forbidden): {file_name:?}"
        )));
    }
    if file_name == "." || file_name == ".." || file_name.contains("..") {
        return Err(SlotError::Invalid(format!(
            "unsafe file name (dot segments forbidden): {file_name:?}"
        )));
    }
    // Exactly one Normal component equal to the raw name (rejects absolute /
    // multi-component / Windows prefixes).
    use std::path::Component;
    let as_path = Path::new(file_name);
    let mut comps = as_path.components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(os)), None) if os.to_str() == Some(file_name) => {}
        _ => {
            return Err(SlotError::Invalid(format!(
                "unsafe file name (not a single path component): {file_name:?}"
            )));
        }
    }
    let final_path = dir.join(file_name);
    if final_path.file_name().and_then(|n| n.to_str()) != Some(file_name) {
        return Err(SlotError::Invalid(format!(
            "unsafe file name join escaped dir: {file_name:?}"
        )));
    }
    if final_path.parent() != Some(dir) {
        return Err(SlotError::Invalid(format!(
            "final path parent is not the target dir for {file_name:?}"
        )));
    }
    Ok(final_path)
}

/// Read a file with a hard size cap; refuse symlinks.
pub fn read_capped(path: &Path, max_bytes: u64) -> SlotResult<Vec<u8>> {
    refuse_symlink(path)?;
    let meta = fs::symlink_metadata(path)
        .map_err(|e| SlotError::Io(format!("metadata {}: {e}", path.display())))?;
    if meta.file_type().is_symlink() {
        return Err(SlotError::Symlink(path.to_path_buf()));
    }
    let size = meta.len();
    if size > max_bytes {
        return Err(SlotError::Oversized {
            path: path.to_path_buf(),
            size,
            max: max_bytes,
        });
    }
    let f = File::open(path).map_err(|e| SlotError::Io(format!("open {}: {e}", path.display())))?;
    let mut buf = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    // Cap the read even if the file grows under us.
    let mut limited = std::io::Read::take(f, max_bytes.saturating_add(1));
    limited
        .read_to_end(&mut buf)
        .map_err(|e| SlotError::Io(format!("read {}: {e}", path.display())))?;
    if buf.len() as u64 > max_bytes {
        return Err(SlotError::Oversized {
            path: path.to_path_buf(),
            size: buf.len() as u64,
            max: max_bytes,
        });
    }
    Ok(buf)
}

/// Refuse if `path` is a symlink (or any non-regular file type used as reparse).
pub fn refuse_symlink(path: &Path) -> SlotResult<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(SlotError::Io(format!(
                "symlink_metadata {}: {e}",
                path.display()
            )));
        }
    };
    if meta.file_type().is_symlink() {
        return Err(SlotError::Symlink(path.to_path_buf()));
    }
    Ok(())
}

/// True when `path` is a regular file (not a symlink).
pub fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Open options used when creating free stubs (exclusive create when possible).
#[allow(dead_code)]
pub fn exclusive_create(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}
