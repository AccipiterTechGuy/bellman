//! Process launch: arg array, no shell, timeout kill, output cap, BELLMAN_RUN_ID.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Default combined stdout+stderr capture cap (64 KiB).
pub const DEFAULT_OUTPUT_CAP_BYTES: usize = 64 * 1024;

/// Default launch timeout (60 s).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Launch configuration.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// Absolute or PATH-resolved binary (never passed to a shell).
    pub command: String,
    pub args: Vec<String>,
    pub workdir: Option<String>,
    /// Kill the process after this duration.
    pub timeout: Duration,
    /// Max bytes retained from combined stdout+stderr.
    pub output_cap: usize,
    /// Value for the `BELLMAN_RUN_ID` environment variable.
    pub run_id: Uuid,
}

/// Outcome of a launch attempt.
#[derive(Debug, Clone)]
pub struct LaunchOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub killed: bool,
    /// Captured output (capped).
    pub output: String,
    pub duration: Duration,
}

/// Launch failures (spawn / IO), distinct from non-zero exit.
#[derive(Debug)]
pub enum LaunchError {
    Spawn(String),
    Io(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(s) | Self::Io(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Spawn `command` with `args` (no shell), enforce timeout, cap output.
///
/// On timeout the child is killed (SIGKILL on Unix after a brief SIGTERM
/// attempt is overkill for v1 — we use `kill()` which is SIGKILL on force).
pub fn run_launch(cfg: &LaunchConfig) -> Result<LaunchOutcome, LaunchError> {
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args)
        .env("BELLMAN_RUN_ID", cfg.run_id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(wd) = &cfg.workdir {
        cmd.current_dir(Path::new(wd));
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| LaunchError::Spawn(format!("spawn '{}': {e}", cfg.command)))?;

    let start = Instant::now();
    let timeout = cfg.timeout;
    let cap = cfg.output_cap;

    // Drain pipes on background threads so a full pipe cannot deadlock us.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_handle = thread::spawn(move || read_capped_pipe(stdout, cap));
    let err_handle = thread::spawn(move || read_capped_pipe(stderr, cap / 2));

    let mut timed_out = false;
    let mut killed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    // Best-effort kill. On Unix this is SIGKILL via kill().
                    let _ = child.kill();
                    killed = true;
                    let status = child
                        .wait()
                        .map_err(|e| LaunchError::Io(format!("wait after kill: {e}")))?;
                    break status;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                return Err(LaunchError::Io(format!("try_wait: {e}")));
            }
        }
    };

    let stdout_bytes = out_handle
        .join()
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default();
    let stderr_bytes = err_handle
        .join()
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default();

    let mut combined = String::new();
    push_utf8_capped(&mut combined, &stdout_bytes, cap);
    if !stderr_bytes.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        let room = cap.saturating_sub(combined.len());
        push_utf8_capped(&mut combined, &stderr_bytes, room);
    }

    Ok(LaunchOutcome {
        exit_code: status.code(),
        timed_out,
        killed,
        output: combined,
        duration: start.elapsed(),
    })
}

fn read_capped_pipe(
    pipe: Option<impl Read>,
    cap: usize,
) -> Result<Vec<u8>, std::io::Error> {
    let Some(mut pipe) = pipe else {
        return Ok(Vec::new());
    };
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let room = cap.saturating_sub(buf.len());
                if room == 0 {
                    // Drain the rest so the child is not blocked, but discard.
                    continue;
                }
                let take = n.min(room);
                buf.extend_from_slice(&chunk[..take]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(buf)
}

fn push_utf8_capped(dst: &mut String, bytes: &[u8], cap: usize) {
    if cap == 0 || bytes.is_empty() {
        return;
    }
    let lossy = String::from_utf8_lossy(bytes);
    let room = cap.saturating_sub(dst.len());
    if lossy.len() <= room {
        dst.push_str(&lossy);
    } else {
        // Avoid splitting mid-char: take a byte-safe prefix via char boundaries.
        let mut end = room.min(lossy.len());
        while end > 0 && !lossy.is_char_boundary(end) {
            end -= 1;
        }
        dst.push_str(&lossy[..end]);
    }
}
