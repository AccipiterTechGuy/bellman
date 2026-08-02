//! Explicit `task run --confirm` for discovered tasks.

use crate::visible::types::{DiscoveredTask, LastResult, RunOutcome, SourceKind};
use chrono::Utc;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Run a task's command immediately. Requires `confirm == true`.
///
/// Never called implicitly from scan/explain/show.
///
/// `task.command` is the shell-executable command with **literal** `%`
/// characters (already unescaped from crontab `\%`). Optional cron stdin is
/// taken from `task.stdin_payload` — we never split `command` on `%`.
pub fn run_task(
    task: &DiscoveredTask,
    confirm: bool,
    timeout_secs: u64,
) -> Result<RunOutcome, String> {
    if !confirm {
        return Err("refusing to run without --confirm (run-now must never be implicit)".into());
    }
    if matches!(task.source_kind, SourceKind::Unsupported) {
        return Err("cannot run an unsupported-platform placeholder".into());
    }

    let shell_cmd = task.command.trim();
    if shell_cmd.is_empty() || shell_cmd == "(no action)" {
        return Err("task has no runnable command".into());
    }

    let timeout = Duration::from_secs(timeout_secs.max(1));
    let started_at = Utc::now();
    let output = run_with_timeout(shell_cmd, task.stdin_payload.as_deref(), timeout)?;
    let finished_at = Utc::now();
    let exit_code = output.status.code().unwrap_or(-1);
    Ok(RunOutcome {
        task_id: task.id.clone(),
        command: shell_cmd.to_string(),
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

fn run_with_timeout(
    shell_cmd: &str,
    stdin_payload: Option<&str>,
    timeout: Duration,
) -> Result<CmdOutput, String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(shell_cmd)
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;

    if let Some(payload) = stdin_payload {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(payload.as_bytes())
                .map_err(|e| format!("write stdin: {e}"))?;
        }
    }

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;
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
                                format!("\nkilled after {}s timeout", timeout.as_secs()).as_bytes(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visible::types::{LastResult, SourceKind};

    fn task_with_cmd(cmd: &str) -> DiscoveredTask {
        DiscoveredTask {
            id: "t1".into(),
            source_kind: SourceKind::CronUser,
            source: "crontab:t".into(),
            owner: "t".into(),
            command: cmd.into(),
            stdin_payload: None,
            schedule_expr: "0 * * * *".into(),
            human_explanation: String::new(),
            next_run: None,
            last_run: None,
            last_result: LastResult::Unknown,
            enabled: true,
            writable: true,
            write_block_reason: None,
            timezone: None,
            line_no: Some(1),
            raw_line: None,
            disabled_original: None,
            platform_note: None,
        }
    }

    #[test]
    fn run_keeps_literal_percent_in_command() {
        // Auditor REPRO: original crontab `echo 100\% done` → command "echo 100% done".
        // Must not truncate at %.
        let t = task_with_cmd("echo 100% done");
        let out = run_task(&t, true, 5).expect("run");
        assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);
        assert!(
            out.stdout.contains("100% done"),
            "stdout was {:?}",
            out.stdout
        );
        assert_eq!(out.command, "echo 100% done");
    }

    #[test]
    fn run_refuses_without_confirm() {
        let t = task_with_cmd("true");
        let err = run_task(&t, false, 5).unwrap_err();
        assert!(err.contains("--confirm"), "{err}");
    }
}
