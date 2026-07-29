//! Explicit `task run --confirm` for discovered tasks.

use crate::visible::types::{DiscoveredTask, LastResult, RunOutcome, SourceKind};
use chrono::Utc;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Run a task's command immediately. Requires `confirm == true`.
///
/// Never called implicitly from scan/explain/show.
pub fn run_task(task: &DiscoveredTask, confirm: bool, timeout_secs: u64) -> Result<RunOutcome, String> {
    if !confirm {
        return Err(
            "refusing to run without --confirm (run-now must never be implicit)".into(),
        );
    }
    if matches!(task.source_kind, SourceKind::Unsupported) {
        return Err("cannot run an unsupported-platform placeholder".into());
    }

    let command = task.command.trim();
    if command.is_empty() || command == "(no action)" {
        return Err("task has no runnable command".into());
    }

    // Cron always uses a shell; match that for run-now of cron-sourced tasks.
    // Take command before first unescaped `%` (stdin payload, not argv).
    let shell_cmd = command
        .split_once('%')
        .map(|(c, _)| c)
        .unwrap_or(command)
        .to_string();

    let timeout = Duration::from_secs(timeout_secs.max(1));
    let started_at = Utc::now();
    let output = run_with_timeout(&shell_cmd, timeout)?;
    let finished_at = Utc::now();
    let exit_code = output.status.code().unwrap_or(-1);
    Ok(RunOutcome {
        task_id: task.id.clone(),
        command: shell_cmd,
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        started_at,
        finished_at,
    })
}

struct CmdOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_with_timeout(shell_cmd: &str, timeout: Duration) -> Result<CmdOutput, String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(shell_cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child
                    .wait_with_output()
                    .map_err(|e| format!("wait: {e}"))?;
                return Ok(CmdOutput {
                    status: out.status,
                    stdout: out.stdout,
                    stderr: out.stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let out = child
                        .wait_with_output()
                        .map_err(|e| format!("wait after kill: {e}"))?;
                    return Ok(CmdOutput {
                        status: out.status,
                        stdout: out.stdout,
                        stderr: {
                            let mut e = out.stderr;
                            e.extend_from_slice(
                                format!("\nkilled after {}s timeout", timeout.as_secs())
                                    .as_bytes(),
                            );
                            e
                        },
                    });
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("try_wait: {e}")),
        }
    }
}

/// Map run outcome into last-result (for callers that want to display it).
pub fn outcome_to_last_result(outcome: &RunOutcome) -> LastResult {
    if outcome.exit_code == 0 {
        LastResult::Ok {
            exit_code: outcome.exit_code,
        }
    } else {
        LastResult::Failed {
            exit_code: outcome.exit_code,
        }
    }
}
