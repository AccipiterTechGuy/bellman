//! IK6 — the IPC transport: ONE local socket for all of Bellman.
//!
//! The socket is a faster folder, not a second protocol: newline-delimited
//! JSON carrying the same shapes as the files (`bellman-slot/1` out,
//! `bellman-reply/1` in), the same validation, the same records. Everything
//! downstream of the adapter seam — validation, transitions, watchdog,
//! `status.json`, the event log — neither knows nor cares which transport
//! produced a reply: both adapters call the one ingest function,
//! [`crate::reply::ReplyEngine::ingest`].
//!
//! Trust boundary: identical to the file protocol's — same-user processes.
//! The socket lives in a private **0700** directory and is **0600** itself;
//! the OS is the gate, no credentials, no secrets. On Linux the directory is
//! `$XDG_RUNTIME_DIR/bellman`; on macOS a private directory below `$TMPDIR`
//! (XDG_RUNTIME_DIR is normally absent); otherwise a user-owned fallback
//! below the OS temp dir — never a world-visible socket directly in `/tmp`.
//!
//! Framing enforces R12 before parsing: at most 64 KB is buffered for one
//! frame ([`crate::reply::MAX_REPLY_FILE_BYTES`], the file transport's
//! number). A peer that sends more without a newline is disconnected without
//! Bellman retaining the unbounded prefix; every other client stays
//! serviceable (each connection lives on its own threads).
//!
//! Claiming a timer: the client's first frame names
//! `{ "app_name": ..., "timer_id": ... }`. The claim is validated like a
//! file reply — `app_name` must match the timer's explicit integration
//! owner; there is no first-acker ownership over IPC either. Replies from
//! the connection then flow through the shared ingest under the R10
//! per-timer gate, exactly like a reply file the watcher picked up.
//!
//! Windows uses a named pipe (`\\.\pipe\bellman-<user>`) whose DACL —
//! built from the current user's own token SID — allows exactly that user;
//! the same same-user trust boundary, no credentials, no secrets.

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

mod server;

pub use server::{IpcConfig, IpcHandle, IpcServer, SendOutcome};

/// Schema tag of the claim frame (the first frame a client sends).
pub const CLAIM_SCHEMA_V1: &str = "bellman-claim/1";
/// Directory name below the runtime dir holding the socket.
pub const SOCKET_DIR_NAME: &str = "bellman";
/// Socket file name inside that directory.
pub const SOCKET_FILE_NAME: &str = "bellman.sock";
/// Server-instance lock file beside the socket.
pub const LOCK_FILE_NAME: &str = "bellman.sock.lock";

/// Outgoing frames buffered per client before the client counts as wedged
/// (a bounded queue — a slow socket never queues fire records behind work).
pub const OUT_QUEUE_DEPTH: usize = 16;

static ADVERTISED: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn advertised_cell() -> &'static RwLock<Option<PathBuf>> {
    ADVERTISED.get_or_init(|| RwLock::new(None))
}

/// The socket path `timer.json` and fire notifications advertise when the
/// IPC server is up (`"ipc": { "socket": "<path>" }` — data, not code).
pub fn advertised_socket() -> Option<PathBuf> {
    advertised_cell()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

pub(crate) fn set_advertised(path: PathBuf) {
    *advertised_cell().write().unwrap_or_else(|p| p.into_inner()) = Some(path);
}

/// Where the socket directory lives, per OS — a pure function (no fs side
/// effects) so platform rules are testable anywhere:
///
/// - Linux/other unix with `XDG_RUNTIME_DIR`: `$XDG_RUNTIME_DIR/bellman`.
/// - Linux without it: a user-owned fallback below the temp dir.
/// - macOS: `$TMPDIR/bellman` (already per-user there; XDG_RUNTIME_DIR is
///   normally absent), else the same fallback.
fn resolve_socket_dir(
    os: &str,
    xdg_runtime: Option<&Path>,
    tmpdir: Option<&Path>,
    uid: u32,
) -> PathBuf {
    match os {
        "macos" | "ios" => match tmpdir {
            Some(t) => t.join(SOCKET_DIR_NAME),
            None => temp_fallback(uid),
        },
        _ => match xdg_runtime {
            Some(x) => x.join(SOCKET_DIR_NAME),
            None => temp_fallback(uid),
        },
    }
}

/// User-owned fallback below the OS temp dir — never a world-visible socket
/// directly in `/tmp` (the directory itself is created 0700).
fn temp_fallback(uid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("bellman-{uid}"))
}

/// The resolved socket directory for this process/platform.
pub fn default_socket_dir() -> PathBuf {
    resolve_socket_dir(
        std::env::consts::OS,
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).as_deref(),
        std::env::var_os("TMPDIR").map(PathBuf::from).as_deref(),
        current_uid(),
    )
}

/// The resolved socket path for this process/platform. Windows: the named
/// pipe `\\.\pipe\bellman-<username>` (a pipe name, not a filesystem path;
/// its ACL does the gating a 0700 directory does on Unix).
pub fn default_socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string());
        return PathBuf::from(format!(r"\\.\pipe\bellman-{user}"));
    }
    #[cfg(not(windows))]
    default_socket_dir().join(SOCKET_FILE_NAME)
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_prefers_xdg_runtime_dir() {
        let dir = resolve_socket_dir(
            "linux",
            Some(Path::new("/run/user/1000")),
            Some(Path::new("/tmp")),
            1000,
        );
        assert_eq!(dir, PathBuf::from("/run/user/1000/bellman"));
    }

    #[test]
    fn linux_without_xdg_falls_back_to_user_owned_temp_dir() {
        let dir = resolve_socket_dir("linux", None, Some(Path::new("/tmp")), 1000);
        assert_eq!(dir, std::env::temp_dir().join("bellman-1000"));
        // Never a world-visible socket directly in /tmp.
        assert_ne!(dir, std::env::temp_dir().join("bellman"));
    }

    #[test]
    fn macos_uses_tmpdir_without_xdg_runtime_dir() {
        // macOS normally has no XDG_RUNTIME_DIR; TMPDIR is already per-user.
        let dir = resolve_socket_dir(
            "macos",
            None,
            Some(Path::new("/var/folders/xx/yyy/T")),
            501,
        );
        assert_eq!(dir, PathBuf::from("/var/folders/xx/yyy/T/bellman"));
    }

    #[test]
    fn macos_without_tmpdir_falls_back_to_user_owned_temp_dir() {
        let dir = resolve_socket_dir("macos", None, None, 501);
        assert_eq!(dir, std::env::temp_dir().join("bellman-501"));
    }
}
