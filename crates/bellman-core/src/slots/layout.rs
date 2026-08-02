//! Slot directory layout: `slots/{free,work,done,bad}/`.

use super::atomic::{atomic_write_json, is_regular_file, refuse_symlink};
use super::envelope::SlotRequest;
use super::error::{SlotError, SlotResult};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Minimum number of empty free-slot stubs kept ready.
pub const MIN_FREE_SLOTS: usize = 5;

/// Default age after which a `work/` claim is considered orphaned.
pub const DEFAULT_ORPHAN_AGE: Duration = Duration::from_secs(5 * 60);

/// Default retention for answered `done/` files before GC.
pub const DEFAULT_DONE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// Subdirectory names under the slots root.
pub const DIR_FREE: &str = "free";
pub const DIR_WORK: &str = "work";
pub const DIR_DONE: &str = "done";
pub const DIR_BAD: &str = "bad";

/// Filesystem layout + replenish / GC helpers for one slots root.
#[derive(Debug, Clone)]
pub struct SlotLayout {
    root: PathBuf,
    min_free: usize,
}

impl SlotLayout {
    /// Create (if needed) the four state directories under `root`.
    pub fn open(root: impl AsRef<Path>) -> SlotResult<Self> {
        Self::open_with(root, MIN_FREE_SLOTS)
    }

    /// Open with an explicit free-slot floor (tests may use a different N).
    pub fn open_with(root: impl AsRef<Path>, min_free: usize) -> SlotResult<Self> {
        let root = root.as_ref().to_path_buf();
        for sub in [DIR_FREE, DIR_WORK, DIR_DONE, DIR_BAD] {
            fs::create_dir_all(root.join(sub))
                .map_err(|e| SlotError::Io(format!("create {}/{}: {e}", root.display(), sub)))?;
        }
        let layout = Self { root, min_free };
        layout.replenish()?;
        Ok(layout)
    }

    /// The `slots/` root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Pre-generated empty stubs a producer claims by rename.
    pub fn free_dir(&self) -> PathBuf {
        self.root.join(DIR_FREE)
    }
    /// Requests Bellman has claimed and is applying.
    pub fn work_dir(&self) -> PathBuf {
        self.root.join(DIR_WORK)
    }
    /// Responses. Never fire notifications — those live in `fires/`.
    pub fn done_dir(&self) -> PathBuf {
        self.root.join(DIR_DONE)
    }
    /// Quarantined garbage, each with an `.err.json` sidecar saying why.
    pub fn bad_dir(&self) -> PathBuf {
        self.root.join(DIR_BAD)
    }

    /// How many free stubs are kept ready, so one app can never block
    /// another by taking the last one.
    pub fn min_free(&self) -> usize {
        self.min_free
    }

    /// Count regular files in `free/` (stubs + filled pending requests).
    pub fn free_count(&self) -> SlotResult<usize> {
        count_regular_json(&self.free_dir())
    }

    /// Ensure `free/` holds at least `min_free` empty stubs.
    ///
    /// Only counts *empty stubs* toward the floor; filled pending requests
    /// are extra. After every claim the service calls this so the invariant
    /// `free stubs >= MIN` always holds for new producers.
    pub fn replenish(&self) -> SlotResult<usize> {
        let stubs = list_free_stubs(self)?;
        let mut created = 0usize;
        let mut next_id = next_slot_id(self)?;
        while stubs.len() + created < self.min_free {
            let id = format!("{next_id:04}");
            let name = format!("slot-{id}.json");
            let path = self.free_dir().join(&name);
            if path.exists() {
                next_id = next_id.saturating_add(1);
                continue;
            }
            let stub = SlotRequest::free_stub(&id);
            atomic_write_json(&self.free_dir(), &name, &stub)?;
            created += 1;
            next_id = next_id.saturating_add(1);
        }
        Ok(created)
    }

    /// List regular `.json` files in `free/` (sorted by name).
    pub fn list_free_files(&self) -> SlotResult<Vec<PathBuf>> {
        list_json_files(&self.free_dir())
    }

    /// List regular `.json` files in `work/` (sorted by name).
    pub fn list_work_files(&self) -> SlotResult<Vec<PathBuf>> {
        list_json_files(&self.work_dir())
    }

    /// List regular `.json` files in `done/` (sorted by name).
    pub fn list_done_files(&self) -> SlotResult<Vec<PathBuf>> {
        list_json_files(&self.done_dir())
    }

    /// Atomically claim `free/name` → `work/name`. Returns `Ok(None)` if lost race.
    pub fn claim_file(&self, free_path: &Path) -> SlotResult<Option<PathBuf>> {
        refuse_symlink(free_path)?;
        let name = free_path
            .file_name()
            .ok_or_else(|| SlotError::Internal("claim path has no file_name".into()))?;
        let work_path = self.work_dir().join(name);
        match fs::rename(free_path, &work_path) {
            Ok(()) => Ok(Some(work_path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            // Cross-device / already-exists races on some platforms.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(SlotError::Io(format!(
                "claim rename {} → {}: {e}",
                free_path.display(),
                work_path.display()
            ))),
        }
    }

    /// Move a work (or free) file into `bad/` and write a sibling `.err.json`.
    ///
    /// Uses `symlink_metadata` (not `Path::exists`) so **dangling symlinks**
    /// are still moved: `exists()` follows the target and returns false when
    /// it is missing, which previously left hostile links in `free/`.
    pub fn quarantine(
        &self,
        path: &Path,
        reason: &str,
        slot_id: Option<String>,
    ) -> SlotResult<PathBuf> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown.json")
            .to_string();
        let dest = unique_dest(&self.bad_dir(), &name)?;
        // Lexical presence: regular file, live symlink, or dangling symlink.
        let source_present = fs::symlink_metadata(path).is_ok();
        if source_present {
            // Prefer rename (moves the directory entry, including dangling links).
            // Fall back to copy+remove only for regular files on cross-fs.
            if fs::rename(path, &dest).is_err() {
                let is_symlink =
                    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
                if is_symlink {
                    // Cannot copy a dangling link usefully; drop the entry.
                    let _ = fs::remove_file(path);
                } else {
                    let _ = fs::copy(path, &dest);
                    let _ = fs::remove_file(path);
                }
            }
        }
        let sidecar_name = format!("{}.err.json", dest.file_name().unwrap().to_string_lossy());
        let sidecar = super::envelope::SlotErrSidecar::new(reason, slot_id, Some(name));
        atomic_write_json(&self.bad_dir(), &sidecar_name, &sidecar)?;
        Ok(dest)
    }

    /// Remove `done/` files older than `retention`.
    pub fn gc_done(&self, retention: Duration) -> SlotResult<usize> {
        gc_dir_older_than(&self.done_dir(), retention)
    }

    /// Files in `work/` whose mtime is older than `max_age` (orphan candidates).
    pub fn list_orphan_work(&self, max_age: Duration) -> SlotResult<Vec<PathBuf>> {
        let cutoff = SystemTime::now()
            .checked_sub(max_age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut out = Vec::new();
        for path in list_json_files(&self.work_dir())? {
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if modified <= cutoff {
                out.push(path);
            }
        }
        Ok(out)
    }
}

fn count_regular_json(dir: &Path) -> SlotResult<usize> {
    Ok(list_json_files(dir)?.len())
}

fn list_json_files(dir: &Path) -> SlotResult<Vec<PathBuf>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => {
            return Err(SlotError::Io(format!("read_dir {}: {e}", dir.display())));
        }
    };
    for ent in entries {
        let ent = ent.map_err(|e| SlotError::Io(format!("read_dir entry: {e}")))?;
        let path = ent.path();
        let name = ent.file_name();
        let name_s = name.to_string_lossy();
        if name_s.starts_with('.') {
            continue;
        }
        if !name_s.ends_with(".json") {
            continue;
        }
        // Skip quarantine sidecars that land in the same dir.
        if name_s.ends_with(".err.json") {
            continue;
        }
        // Include regular files *and* symlinks. Symlinks must surface so poll
        // can quarantine them; filtering them out left hostile links unhandled.
        if is_regular_file(&path) || is_symlink_path(&path) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

fn is_symlink_path(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn list_free_stubs(layout: &SlotLayout) -> SlotResult<Vec<PathBuf>> {
    let mut stubs = Vec::new();
    for path in layout.list_free_files()? {
        // Parse JSON when possible; malformed free files are left for the scanner.
        let Ok(bytes) = super::atomic::read_capped(&path, super::atomic::DEFAULT_MAX_READ_BYTES)
        else {
            continue;
        };
        if let Ok(req) = serde_json::from_slice::<SlotRequest>(&bytes) {
            if req.is_free_stub() {
                stubs.push(path);
            }
        }
    }
    Ok(stubs)
}

/// Next numeric slot id: 1 + max of ids found in free/work/done/bad filenames.
fn next_slot_id(layout: &SlotLayout) -> SlotResult<u32> {
    let mut max = 0u32;
    for dir in [
        layout.free_dir(),
        layout.work_dir(),
        layout.done_dir(),
        layout.bad_dir(),
    ] {
        if !dir.exists() {
            continue;
        }
        for ent in fs::read_dir(&dir)
            .map_err(|e| SlotError::Io(format!("read_dir {}: {e}", dir.display())))?
        {
            let ent = ent.map_err(|e| SlotError::Io(format!("read_dir entry: {e}")))?;
            let name = ent.file_name();
            let s = name.to_string_lossy();
            if let Some(id) = parse_slot_id_from_name(&s) {
                max = max.max(id);
            }
        }
    }
    Ok(max.saturating_add(1).max(1))
}

/// Parse `slot-0007.json` → `7`.
pub fn parse_slot_id_from_name(name: &str) -> Option<u32> {
    let stem = name
        .strip_suffix(".err.json")
        .or_else(|| name.strip_suffix(".json"))?;
    let id = stem.strip_prefix("slot-")?;
    id.parse().ok()
}

fn unique_dest(dir: &Path, name: &str) -> SlotResult<PathBuf> {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return Ok(candidate);
    }
    // Disambiguate collisions in bad/.
    for i in 1..10_000 {
        let alt = dir.join(format!("{name}.{i}"));
        if !alt.exists() {
            return Ok(alt);
        }
    }
    Err(SlotError::Internal(format!(
        "could not find unique name for {name} in {}",
        dir.display()
    )))
}

fn gc_dir_older_than(dir: &Path, retention: Duration) -> SlotResult<usize> {
    let cutoff = SystemTime::now()
        .checked_sub(retention)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = 0usize;
    for path in list_json_files(dir)? {
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if modified <= cutoff {
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
            // Drop matching sidecar if present.
            let side = path.with_extension("json.err.json");
            // Also try name.err.json form.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                let side2 = dir.join(format!("{name}.err.json"));
                let _ = fs::remove_file(side2);
            }
            let _ = fs::remove_file(side);
        }
    }
    Ok(removed)
}
