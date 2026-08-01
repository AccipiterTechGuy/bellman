//! IK3 — quarantine of rejected replies (`docs/todo/json_normalization.md`
//! R12, `docs/todo/cards/IK3_reply_channel.md`).
//!
//! Rejected reply bytes are quarantined to a `bad/` directory under the
//! timers tree root by **copying the already-read condemned bytes** — never
//! by renaming, moving, or deleting the live app-owned file. Between
//! Bellman's read of the bad content and any rename, the app can atomically
//! replace the file with a valid reply, so the rename would ship the valid
//! reply to quarantine. Copying the bytes Bellman already holds has no such
//! race.
//!
//! Quarantine is **idempotent**: the artifact name derives from the source
//! path plus a digest of the rejected bytes, so an unchanged condemned file
//! produces no new artifact (and no duplicate `reply_rejected` event at the
//! call site). An oversize file (>64 KB) is never read; only a small
//! metadata sidecar is written with `content_copied: false`, which is a
//! **complete single artifact**, not an orphan half-pair.
//!
//! Each artifact is written to temp files in `bad/`, fsynced, and installed
//! at its deterministic final name with create-new/no-replace semantics
//! (hard link; `AlreadyExists` is the idempotent no-op case). Creation and
//! pruning share the interprocess `bad/` lock (`gate::acquire_quarantine`);
//! reply ingest holds the R10 timer shard first, never the reverse.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Quarantine sidecar wire schema (R1).
pub const QUARANTINE_SCHEMA_V1: &str = "bellman-quarantine/1";

/// `bad/` directory under the timers tree root.
pub fn quarantine_dir(timers_root: &Path) -> PathBuf {
    timers_root.join("bad")
}

/// FNV-1a 64-bit, hex-encoded — stable digest for artifact naming (no new deps).
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Outcome of one quarantine call.
pub struct QuarantineOutcome {
    /// True when this call installed a new artifact; false when the final
    /// name already existed (idempotent no-op — nothing was rewritten).
    pub created: bool,
    /// Installed payload path, or `None` for a metadata-only
    /// (`content_copied: false`) artifact.
    pub payload_path: Option<PathBuf>,
    /// Installed sidecar path (always present).
    pub sidecar_path: PathBuf,
}

/// Copy condemned bytes into `bad/` as one payload/sidecar pair.
///
/// `bytes` are the bytes already read and condemned by the caller — the
/// live source file is never opened, renamed, or deleted here. The caller
/// holds the quarantine lock (`gate::acquire_quarantine`) and has already
/// acquired the timer shard (lock order: timer shard THEN `bad/` lock).
pub fn quarantine_bytes(
    bad_dir: &Path,
    source_path: &Path,
    bytes: &[u8],
    reason: &str,
    detail: Value,
) -> io::Result<QuarantineOutcome> {
    fs::create_dir_all(bad_dir)?;
    let digest = fnv1a64_hex(bytes);
    let base = format!(
        "{}-{digest}",
        fnv1a64_hex(path_string(source_path).as_bytes())
    );
    let payload_final = bad_dir.join(format!("{base}.payload"));
    let sidecar_final = bad_dir.join(format!("{base}.sidecar.json"));
    let payload_tmp = bad_dir.join(format!("{base}.payload.tmp"));
    let sidecar_tmp = bad_dir.join(format!("{base}.sidecar.json.tmp"));

    write_tmp(&payload_tmp, bytes)?;
    let sidecar = sidecar_json(
        source_path,
        reason,
        true,
        bytes.len() as u64,
        Some(digest),
        detail,
    );
    write_tmp(&sidecar_tmp, &serde_json::to_vec_pretty(&sidecar)?)?;

    if install_no_replace(&payload_tmp, &payload_final)? {
        // Payload installed; the sidecar install only fails to install when
        // a previous run crashed between the two — linking it completes the
        // pair, `AlreadyExists` leaves the existing one untouched.
        install_no_replace(&sidecar_tmp, &sidecar_final)?;
        sync_dir(bad_dir);
        Ok(QuarantineOutcome {
            created: true,
            payload_path: Some(payload_final),
            sidecar_path: sidecar_final,
        })
    } else {
        // Same source path + same bytes were quarantined before: no-op.
        let _ = fs::remove_file(&sidecar_tmp);
        Ok(QuarantineOutcome {
            created: false,
            payload_path: Some(payload_final),
            sidecar_path: sidecar_final,
        })
    }
}

/// Oversize/special-file case: no payload copy, metadata-only sidecar with
/// `content_copied: false`. The unread file is never opened here; the name
/// deduplicates on (source path, observed length) — a same-length
/// replacement is intentionally the same rejection because Bellman refuses
/// to read it.
pub fn quarantine_unread(
    bad_dir: &Path,
    source_path: &Path,
    observed_len: u64,
    reason: &str,
    detail: Value,
) -> io::Result<QuarantineOutcome> {
    fs::create_dir_all(bad_dir)?;
    let base = format!(
        "{}-unread-{}",
        fnv1a64_hex(path_string(source_path).as_bytes()),
        fnv1a64_hex(observed_len.to_string().as_bytes())
    );
    let sidecar_final = bad_dir.join(format!("{base}.sidecar.json"));
    let sidecar_tmp = bad_dir.join(format!("{base}.sidecar.json.tmp"));

    let sidecar = sidecar_json(source_path, reason, false, observed_len, None, detail);
    write_tmp(&sidecar_tmp, &serde_json::to_vec_pretty(&sidecar)?)?;

    let created = install_no_replace(&sidecar_tmp, &sidecar_final)?;
    if created {
        sync_dir(bad_dir);
    }
    Ok(QuarantineOutcome {
        created,
        payload_path: None,
        sidecar_path: sidecar_final,
    })
}

/// Retention: remove artifact pairs older than `retention`, then oldest
/// pairs until the directory aggregate fits `budget_bytes`. Payload/sidecar
/// pairs are removed together; a `content_copied: false` sidecar counts as
/// a complete single artifact. Returns the number of artifacts removed.
pub fn prune(
    bad_dir: &Path,
    retention: Duration,
    budget_bytes: u64,
    now: DateTime<Utc>,
) -> io::Result<usize> {
    let mut artifacts = list_artifacts(bad_dir)?;
    // Oldest first; stable so equal mtimes keep a deterministic order.
    artifacts.sort_by_key(|a| a.mtime);
    let mut removed = 0usize;
    let mut remaining: Vec<Artifact> = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let age = now.signed_duration_since(DateTime::<Utc>::from(artifact.mtime));
        let too_old = age.to_std().is_ok_and(|d| d > retention);
        if too_old {
            remove_artifact(&artifact)?;
            removed += 1;
        } else {
            remaining.push(artifact);
        }
    }
    let mut total: u64 = remaining.iter().map(|a| a.bytes).sum();
    for artifact in &remaining {
        if total <= budget_bytes {
            break;
        }
        remove_artifact(artifact)?;
        total = total.saturating_sub(artifact.bytes);
        removed += 1;
    }
    Ok(removed)
}

/// Startup: remove stale temp files and orphan half-pairs. A sidecar marked
/// `content_copied: false` is a COMPLETE single artifact, not an orphan.
/// Returns the number of files removed.
pub fn startup_sweep(bad_dir: &Path) -> io::Result<u64> {
    let rd = match fs::read_dir(bad_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in rd {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    let mut removed = 0u64;
    // Stale temporaries from an interrupted install.
    for name in &names {
        if name.ends_with(".tmp") && fs::remove_file(bad_dir.join(name)).is_ok() {
            removed += 1;
        }
    }
    let has = |candidate: &str| names.iter().any(|n| n == candidate);
    // Orphan payload: a crash between payload and sidecar install.
    for name in &names {
        if let Some(base) = name.strip_suffix(".payload") {
            if !has(&format!("{base}.sidecar.json")) && fs::remove_file(bad_dir.join(name)).is_ok()
            {
                removed += 1;
            }
        }
    }
    // Orphan sidecar — unless it is a complete metadata-only artifact.
    for name in &names {
        if let Some(base) = name.strip_suffix(".sidecar.json") {
            if !has(&format!("{base}.payload"))
                && !is_metadata_only(bad_dir.join(name).as_path())
                && fs::remove_file(bad_dir.join(name)).is_ok()
            {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// A quarantine artifact: every file sharing one `<base>` name.
struct Artifact {
    files: Vec<PathBuf>,
    bytes: u64,
    /// Oldest mtime among the pair's files — the artifact's age basis.
    mtime: SystemTime,
}

/// Group `bad/` entries into artifacts by `<base>` (strip `.payload` /
/// `.sidecar.json`). Unknown names and `*.tmp` files are ignored.
fn list_artifacts(bad_dir: &Path) -> io::Result<Vec<Artifact>> {
    let rd = match fs::read_dir(bad_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut bases: BTreeMap<String, Artifact> = BTreeMap::new();
    for entry in rd {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let base = name
            .strip_suffix(".payload")
            .or_else(|| name.strip_suffix(".sidecar.json"));
        let Some(base) = base else { continue };
        let meta = entry.metadata()?;
        let mtime = meta.modified()?;
        let artifact = bases.entry(base.to_owned()).or_insert_with(|| Artifact {
            files: Vec::new(),
            bytes: 0,
            mtime,
        });
        artifact.files.push(entry.path());
        artifact.bytes = artifact.bytes.saturating_add(meta.len());
        if mtime < artifact.mtime {
            artifact.mtime = mtime;
        }
    }
    Ok(bases.into_values().collect())
}

/// Delete every file of one artifact; a missing file is already gone.
fn remove_artifact(artifact: &Artifact) -> io::Result<()> {
    for path in &artifact.files {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// True when `path` is a parseable sidecar with `content_copied: false`.
fn is_metadata_only(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| v.get("content_copied").and_then(Value::as_bool))
        == Some(false)
}

/// Source path as the string the artifact name and sidecar record.
fn path_string(source_path: &Path) -> String {
    source_path.to_string_lossy().into_owned()
}

/// Write `bytes` to a fresh temp file in `bad/` and fsync it.
fn write_tmp(tmp_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Install `tmp` at `final_path` with create-new/no-replace semantics: the
/// hard link fails with `AlreadyExists` when the final name exists — the
/// idempotent no-op case — and the temp is removed either way.
fn install_no_replace(tmp: &Path, final_path: &Path) -> io::Result<bool> {
    match fs::hard_link(tmp, final_path) {
        Ok(()) => {
            let _ = fs::remove_file(tmp);
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(tmp);
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Best-effort directory fsync so the installed names survive a crash.
fn sync_dir(dir: &Path) {
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Sidecar JSON: reserved fields are authoritative, `detail` object keys
/// fill in the rest (`...detail` from the spec).
fn sidecar_json(
    source_path: &Path,
    reason: &str,
    content_copied: bool,
    observed_len: u64,
    content_digest: Option<String>,
    detail: Value,
) -> Value {
    let mut map = match detail {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    map.insert("schema".into(), QUARANTINE_SCHEMA_V1.into());
    map.insert("reason".into(), reason.into());
    map.insert("source_path".into(), path_string(source_path).into());
    map.insert("quarantined_at".into(), Utc::now().to_rfc3339().into());
    map.insert("content_copied".into(), content_copied.into());
    map.insert("observed_len".into(), observed_len.into());
    map.insert(
        "content_digest".into(),
        content_digest.map(Value::from).unwrap_or(Value::Null),
    );
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_file_count(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn fnv1a64_hex_known_vectors() {
        assert_eq!(fnv1a64_hex(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64_hex(b"a"), "af63dc4c8601ec8c");
    }

    #[test]
    fn quarantine_dir_is_bad_under_root() {
        assert_eq!(
            quarantine_dir(Path::new("/data/timers")),
            PathBuf::from("/data/timers/bad")
        );
    }

    #[test]
    fn double_quarantine_is_idempotent_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = quarantine_dir(tmp.path());
        let source = tmp.path().join("timer-x/reply-run1.json");
        let bytes = b"{not json";
        let first = quarantine_bytes(&bad, &source, bytes, "invalid_json", Value::Null).unwrap();
        assert!(first.created);
        let second = quarantine_bytes(&bad, &source, bytes, "invalid_json", Value::Null).unwrap();
        assert!(!second.created);
        assert_eq!(first.payload_path, second.payload_path);
        assert_eq!(first.sidecar_path, second.sidecar_path);
        // Exactly one payload + one sidecar, payload holds the copied bytes.
        assert_eq!(dir_file_count(&bad), 2);
        assert_eq!(
            fs::read(first.payload_path.unwrap()).unwrap(),
            bytes.as_slice()
        );
        let sidecar: Value =
            serde_json::from_slice(&fs::read(first.sidecar_path).unwrap()).unwrap();
        assert_eq!(sidecar["schema"], QUARANTINE_SCHEMA_V1);
        assert_eq!(sidecar["reason"], "invalid_json");
        assert_eq!(sidecar["content_copied"], true);
        assert_eq!(sidecar["observed_len"], bytes.len() as u64);
        assert_eq!(sidecar["content_digest"], fnv1a64_hex(bytes));
    }

    #[test]
    fn quarantine_never_touches_the_source_file() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = quarantine_dir(tmp.path());
        let source = tmp.path().join("reply-live.json");
        fs::write(&source, b"{still invalid").unwrap();
        let condemned = fs::read(&source).unwrap();
        let outcome =
            quarantine_bytes(&bad, &source, &condemned, "invalid_json", Value::Null).unwrap();
        assert!(outcome.created);
        // The live app-owned file survives untouched.
        assert_eq!(fs::read(&source).unwrap(), condemned);
    }

    #[test]
    fn unread_variant_writes_metadata_only_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = quarantine_dir(tmp.path());
        let source = tmp.path().join("timer-x/reply-huge.json");
        let outcome = quarantine_unread(&bad, &source, 70 * 1024, "oversize", Value::Null).unwrap();
        assert!(outcome.created);
        assert!(outcome.payload_path.is_none());
        // No payload anywhere in the directory.
        assert_eq!(dir_file_count(&bad), 1);
        let sidecar: Value =
            serde_json::from_slice(&fs::read(&outcome.sidecar_path).unwrap()).unwrap();
        assert_eq!(sidecar["content_copied"], false);
        assert_eq!(sidecar["reason"], "oversize");
        assert_eq!(sidecar["observed_len"], 70 * 1024);
        assert!(sidecar["content_digest"].is_null());
        // Same path + same observed length deduplicates.
        let again = quarantine_unread(&bad, &source, 70 * 1024, "oversize", Value::Null).unwrap();
        assert!(!again.created);
        assert_eq!(dir_file_count(&bad), 1);
    }

    #[test]
    fn changed_bytes_produce_a_second_distinct_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = quarantine_dir(tmp.path());
        let source = tmp.path().join("timer-x/reply-run1.json");
        let first =
            quarantine_bytes(&bad, &source, b"{bad v1", "invalid_json", Value::Null).unwrap();
        let second =
            quarantine_bytes(&bad, &source, b"{bad v2", "invalid_json", Value::Null).unwrap();
        assert!(first.created && second.created);
        assert_ne!(first.payload_path, second.payload_path);
        assert_eq!(dir_file_count(&bad), 4);
    }

    #[test]
    fn prune_removes_old_pairs_together() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = quarantine_dir(tmp.path());
        let source = tmp.path().join("timer-x/reply-run1.json");
        let one = quarantine_bytes(&bad, &source, b"{bad", "invalid_json", Value::Null).unwrap();
        // Age zero is still "older than zero retention" once any time passes.
        std::thread::sleep(Duration::from_millis(5));
        let removed = prune(&bad, Duration::ZERO, u64::MAX, Utc::now()).unwrap();
        assert_eq!(removed, 1);
        // Both halves of the pair are gone.
        assert!(!one.payload_path.unwrap().exists());
        assert!(!one.sidecar_path.exists());
        assert_eq!(dir_file_count(&bad), 0);
    }

    #[test]
    fn prune_respects_budget_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = quarantine_dir(tmp.path());
        let source = tmp.path().join("timer-x/reply-run1.json");
        quarantine_bytes(&bad, &source, b"{bad one", "invalid_json", Value::Null).unwrap();
        quarantine_bytes(&bad, &source, b"{bad two", "invalid_json", Value::Null).unwrap();
        let total: u64 = fs::read_dir(&bad)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        // Budget fits exactly one pair; one artifact must go.
        let one_pair = total / 2;
        let removed = prune(&bad, Duration::from_secs(86_400), one_pair, Utc::now()).unwrap();
        assert_eq!(removed, 1);
        let remaining_total: u64 = fs::read_dir(&bad)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(remaining_total <= one_pair);
        // The survivor is a complete pair, not a half.
        assert_eq!(dir_file_count(&bad), 2);
    }

    #[test]
    fn startup_sweep_removes_orphans_keeps_metadata_only_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = quarantine_dir(tmp.path());
        fs::create_dir_all(&bad).unwrap();
        // Stale temp from an interrupted install.
        fs::write(bad.join("aaa.payload.tmp"), b"partial").unwrap();
        // Orphan payload: crash between payload and sidecar install.
        fs::write(bad.join("bbb.payload"), b"{bad").unwrap();
        // Orphan sidecar whose payload never landed (content_copied: true).
        fs::write(
            bad.join("ccc.sidecar.json"),
            br#"{"schema":"bellman-quarantine/1","content_copied":true}"#,
        )
        .unwrap();
        // Complete metadata-only artifact — NOT an orphan.
        let keep = quarantine_unread(
            &bad,
            &tmp.path().join("timer-x/reply-huge.json"),
            100 * 1024,
            "oversize",
            Value::Null,
        )
        .unwrap();
        let removed = startup_sweep(&bad).unwrap();
        assert_eq!(removed, 3);
        assert_eq!(dir_file_count(&bad), 1);
        assert!(keep.sidecar_path.exists());
    }

    #[test]
    fn startup_sweep_on_missing_dir_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(startup_sweep(&tmp.path().join("nope")).unwrap(), 0);
    }
}
