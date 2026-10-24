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
    /// Arguments, one per element — never shell-split.
    pub args: Vec<String>,
    /// Working directory; the launcher's own when absent.
    pub workdir: Option<String>,
    /// Kill the process after this duration.
    pub timeout: Duration,
    /// Max bytes retained from combined stdout+stderr.
    pub output_cap: usize,
    /// Value for the `BELLMAN_RUN_ID` environment variable.
    pub run_id: Uuid,
    /// SCH1 `Replace`: when signalled, the `try_wait` loop kills and reaps
    /// the child instead of waiting for exit or timeout.
    pub cancel: Option<std::sync::Arc<super::cancel::CancellationToken>>,
}

/// Outcome of a launch attempt.
#[derive(Debug, Clone)]
pub struct LaunchOutcome {
    /// Exit status, when the process exited normally.
    pub exit_code: Option<i32>,
    /// Whether the timeout expired.
    pub timed_out: bool,
    /// Whether Bellman had to kill it.
    pub killed: bool,
    /// The cancellation token stopped the launch (SCH1 `Replace`) — distinct
    /// from a timeout kill: the policy interrupted a healthy run.
    pub cancelled: bool,
    /// Captured output (capped).
    pub output: String,
    /// How long it ran.
    pub duration: Duration,
}

/// Launch failures (spawn / IO), distinct from non-zero exit.
#[derive(Debug)]
pub enum LaunchError {
    /// The program could not be started at all.
    Spawn(String),
    /// It started, but talking to it failed.
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

    // Unix: the child leads its own process group, so a kill (timeout or
    // SCH1 `Replace` cancellation) reaches the whole tree — otherwise an
    // orphaned grandchild keeps the captured pipes open and the reap below
    // blocks until IT exits, stalling the lane for seconds.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            rustix::process::setsid()
                .map(|_| ())
                .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))
        });
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| LaunchError::Spawn(format!("spawn '{}': {e}", cfg.command)))?;
    let tree = ProcessTree::track(&child);

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
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if cfg.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                    // SCH1 `Replace`: the policy cancelled this run — kill
                    // and reap the child, same as the timeout path.
                    cancelled = true;
                    kill_child_tree(&mut child, &tree);
                    killed = true;
                    let status = child
                        .wait()
                        .map_err(|e| LaunchError::Io(format!("wait after cancel kill: {e}")))?;
                    break status;
                }
                if start.elapsed() >= timeout {
                    timed_out = true;
                    // Kill the whole process group (see setsid above).
                    kill_child_tree(&mut child, &tree);
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
        cancelled,
        output: combined,
        duration: start.elapsed(),
    })
}

fn read_capped_pipe(pipe: Option<impl Read>, cap: usize) -> Result<Vec<u8>, std::io::Error> {
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

/// Everything a launched action started, so a kill reaches its descendants.
///
/// Unix needs nothing extra: the child leads its own process group (setsid
/// at spawn). Windows has no process groups, so killing `sh.exe` alone left
/// e.g. its `sleep` running with the captured pipes open, and the reap waited
/// for it — a `Replace` cancellation took the action's full runtime. There the
/// child goes into a Job Object, which its descendants join automatically.
///
/// The job deliberately does NOT kill on close: an action that starts a
/// long-lived program and exits (a launcher script) must leave that program
/// running, exactly as on Unix. Only a cancel or timeout terminates the job.
#[cfg(not(windows))]
struct ProcessTree;

#[cfg(not(windows))]
impl ProcessTree {
    fn track(_child: &std::process::Child) -> Self {
        Self
    }
}

#[cfg(windows)]
struct ProcessTree {
    /// Job HANDLE as an integer; `None` when the job could not be set up, and
    /// the kill falls back to the direct child.
    job: Option<isize>,
}

#[cfg(windows)]
impl ProcessTree {
    fn track(child: &std::process::Child) -> Self {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        unsafe {
            let Ok(job) = CreateJobObjectW(None, None) else {
                return Self { job: None };
            };
            // Processes the child starts from here on join the job. Anything
            // it spawns in the instant before this call is not covered.
            if AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).is_err() {
                let _ = CloseHandle(job);
                return Self { job: None };
            }
            Self {
                job: Some(job.0 as isize),
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(
                    windows::Win32::Foundation::HANDLE(job as *mut _),
                );
            }
        }
    }
}

#[cfg(unix)]
fn kill_child_tree(child: &mut std::process::Child, _tree: &ProcessTree) {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    // Fallback in case setsid failed at spawn.
    let _ = child.kill();
}

#[cfg(windows)]
fn kill_child_tree(child: &mut std::process::Child, tree: &ProcessTree) {
    if let Some(job) = tree.job {
        unsafe {
            let _ = windows::Win32::System::JobObjects::TerminateJobObject(
                windows::Win32::Foundation::HANDLE(job as *mut _),
                1,
            );
        }
    }
    let _ = child.kill();
}

#[cfg(not(any(unix, windows)))]
fn kill_child_tree(child: &mut std::process::Child, _tree: &ProcessTree) {
    let _ = child.kill();
}
