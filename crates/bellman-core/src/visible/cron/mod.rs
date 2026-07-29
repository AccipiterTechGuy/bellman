//! Cron discovery and safe user-crontab mutation.

pub mod parse;
pub mod write;

use crate::visible::cron::parse::{
    env_tz_before, parse_crontab, CrontabLine, CrontabMode, CronSchedule,
};
use crate::visible::explain::explain_cron;
use crate::visible::id::task_id;
use crate::visible::next_run::{next_from_schedule, system_tz_name};
use crate::visible::types::{DiscoveredTask, LastResult, SourceKind};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Discover jobs from the current user's crontab (`crontab -l`).
pub fn discover_user_crontab(username: &str) -> (Vec<DiscoveredTask>, Vec<String>) {
    let mut warnings = Vec::new();
    let text = match read_user_crontab() {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("user crontab: {e}"));
            return (Vec::new(), warnings);
        }
    };
    let source = format!("crontab:{username}");
    let tasks = tasks_from_crontab_text(
        &text,
        CrontabMode::User,
        SourceKind::CronUser,
        &source,
        username,
        /*writable*/ true,
        None,
    );
    (tasks, warnings)
}

/// Read invoking user's crontab via `crontab -l`. Empty crontab → empty string.
pub fn read_user_crontab() -> Result<String, String> {
    let out = Command::new("crontab")
        .arg("-l")
        .output()
        .map_err(|e| format!("failed to spawn crontab: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Common: "no crontab for user"
    if stderr.to_ascii_lowercase().contains("no crontab") {
        return Ok(String::new());
    }
    Err(format!(
        "crontab -l failed ({}): {}",
        out.status,
        stderr.trim()
    ))
}

/// Discover `/etc/crontab` (read-only).
pub fn discover_system_crontab() -> (Vec<DiscoveredTask>, Vec<String>) {
    discover_file(
        Path::new("/etc/crontab"),
        CrontabMode::System,
        SourceKind::CronSystem,
        "root",
        false,
        Some(
            "v1 is read-only for system files; refusing writes to /etc/crontab. \
             To change it, edit as root outside Bellman (e.g. `sudo visudo`-style: \
             `sudo ${EDITOR:-vi} /etc/crontab`). Bellman will not call sudo."
                .into(),
        ),
    )
}

/// Discover `/etc/cron.d/*` drop-ins.
pub fn discover_cron_d() -> (Vec<DiscoveredTask>, Vec<String>) {
    let mut tasks = Vec::new();
    let mut warnings = Vec::new();
    let dir = Path::new("/etc/cron.d");
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warnings.push(format!("cron.d: {e}"));
            return (tasks, warnings);
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        // Skip backup/disabled style names commonly ignored by cron
        if name.ends_with('~') || name.starts_with('.') {
            continue;
        }
        let (t, w) = discover_file(
            &path,
            CrontabMode::System,
            SourceKind::CronD,
            "root",
            false,
            Some(format!(
                "v1 is read-only for /etc/cron.d; refusing writes to {}. \
                 Run as root outside Bellman if you must change it.",
                path.display()
            )),
        );
        tasks.extend(t);
        warnings.extend(w);
    }
    (tasks, warnings)
}

/// Discover executable scripts in run-parts directories.
pub fn discover_run_parts() -> (Vec<DiscoveredTask>, Vec<String>) {
    let mut tasks = Vec::new();
    let mut warnings = Vec::new();
    let dirs = [
        ("/etc/cron.hourly", "hourly", "0 * * * *"),
        ("/etc/cron.daily", "daily", "0 0 * * *"),
        ("/etc/cron.weekly", "weekly", "0 0 * * 0"),
        ("/etc/cron.monthly", "monthly", "0 0 1 * *"),
    ];
    let tz = system_tz_name();
    for (dir, label, expr) in dirs {
        let path = Path::new(dir);
        let entries = match fs::read_dir(path) {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("{dir}: {e}"));
                continue;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        for file in files {
            let _name = file
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            // run-parts skips names with dots (classic) — still list them with a note
            let source = file.display().to_string();
            let id = task_id(SourceKind::CronRunParts, &[&source]);
            let fields: Vec<&str> = expr.split_whitespace().collect();
            let schedule = CronSchedule::Fields {
                minute: fields.first().unwrap_or(&"0").to_string(),
                hour: fields.get(1).unwrap_or(&"0").to_string(),
                dom: fields.get(2).unwrap_or(&"*").to_string(),
                month: fields.get(3).unwrap_or(&"*").to_string(),
                dow: fields.get(4).unwrap_or(&"*").to_string(),
            };
            let next = next_from_schedule(&schedule, &tz, Utc::now())
                .ok()
                .flatten();
            tasks.push(DiscoveredTask {
                id,
                source_kind: SourceKind::CronRunParts,
                source: source.clone(),
                owner: "root".into(),
                command: source.clone(),
                stdin_payload: None,
                schedule_expr: format!("run-parts {label}"),
                human_explanation: format!(
                    "run-parts script in {dir} (typically {label} via /etc/crontab)"
                ),
                next_run: next,
                last_run: None,
                last_result: LastResult::Unknown,
                enabled: true,
                writable: false,
                write_block_reason: Some(
                    "v1 is read-only for run-parts dirs; display only.".into(),
                ),
                timezone: Some(tz.clone()),
                line_no: None,
                raw_line: None,
                disabled_original: None,
                platform_note: None,
            });
        }
    }
    (tasks, warnings)
}

/// Discover `/etc/anacrontab`.
pub fn discover_anacron() -> (Vec<DiscoveredTask>, Vec<String>) {
    let mut tasks = Vec::new();
    let mut warnings = Vec::new();
    let path = Path::new("/etc/anacrontab");
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("anacrontab: {e}"));
            return (tasks, warnings);
        }
    };
    for (i, raw) in text.lines().enumerate() {
        let line_no = (i + 1) as u32;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.contains('=') {
            // env
            continue;
        }
        // period delay job-id command
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let period = parts[0];
        let delay = parts[1];
        let job_id = parts[2];
        let command = parts[3..].join(" ");
        let source = path.display().to_string();
        let id = task_id(
            SourceKind::Anacron,
            &[&source, &line_no.to_string(), job_id, &command],
        );
        tasks.push(DiscoveredTask {
            id,
            source_kind: SourceKind::Anacron,
            source: source.clone(),
            owner: "root".into(),
            command,
            stdin_payload: None,
            schedule_expr: format!("anacron period={period} delay={delay} id={job_id}"),
            human_explanation: format!(
                "anacron job {job_id}: every {period} day(s), delay {delay} min"
            ),
            next_run: None, // anacron next-run needs spool stamps; leave unknown honestly
            last_run: None,
            last_result: LastResult::Unknown,
            enabled: true,
            writable: false,
            write_block_reason: Some(
                "v1 is read-only for /etc/anacrontab; display only.".into(),
            ),
            timezone: Some(system_tz_name()),
            line_no: Some(line_no),
            raw_line: Some(raw.to_string()),
            disabled_original: None,
            platform_note: None,
        });
    }
    (tasks, warnings)
}

fn discover_file(
    path: &Path,
    mode: CrontabMode,
    kind: SourceKind,
    default_owner: &str,
    writable: bool,
    write_block: Option<String>,
) -> (Vec<DiscoveredTask>, Vec<String>) {
    let mut warnings = Vec::new();
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("{}: {e}", path.display()));
            return (Vec::new(), warnings);
        }
    };
    let source = path.display().to_string();
    let tasks = tasks_from_crontab_text(
        &text,
        mode,
        kind,
        &source,
        default_owner,
        writable,
        write_block,
    );
    (tasks, warnings)
}

/// Convert crontab text into discovered tasks.
pub fn tasks_from_crontab_text(
    text: &str,
    mode: CrontabMode,
    kind: SourceKind,
    source: &str,
    default_owner: &str,
    writable: bool,
    write_block: Option<String>,
) -> Vec<DiscoveredTask> {
    let lines = parse_crontab(text, mode);
    let default_tz = system_tz_name();
    let mut tasks = Vec::new();
    for line in &lines {
        let CrontabLine::Job {
            raw,
            line_no,
            schedule,
            user,
            command,
            stdin_payload,
            disabled_by_bellman,
            original_line,
        } = line
        else {
            continue;
        };
        let owner = user
            .clone()
            .unwrap_or_else(|| default_owner.to_string());
        let tz = env_tz_before(&lines, *line_no, &default_tz);
        let expr = schedule.expression();
        let human = explain_cron(schedule, &tz);
        let next = next_from_schedule(schedule, &tz, Utc::now())
            .ok()
            .flatten();
        // Keep command as shell-executable form (literal %). Stdin stays separate
        // so rewrites can re-escape via join_percent and run_task is not confused.
        let id_key_cmd = original_line.as_deref().unwrap_or(raw.as_str());
        let id = task_id(
            kind,
            &[source, &line_no.to_string(), &owner, id_key_cmd],
        );
        // Cron never records exit status — always Unknown (or Never if never seen).
        // We do not consult the journal to invent ok.
        tasks.push(DiscoveredTask {
            id,
            source_kind: kind,
            source: source.to_string(),
            owner,
            command: command.clone(),
            stdin_payload: stdin_payload.clone(),
            schedule_expr: expr,
            human_explanation: human,
            next_run: next,
            last_run: None,
            last_result: LastResult::Unknown,
            enabled: !disabled_by_bellman,
            writable: writable && matches!(kind, SourceKind::CronUser),
            write_block_reason: if writable {
                None
            } else {
                write_block.clone()
            },
            timezone: Some(tz),
            line_no: Some(*line_no),
            raw_line: Some(raw.clone()),
            disabled_original: original_line.clone(),
            platform_note: None,
        });
    }
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_written_line_appears() {
        let text = "# note\n30 4 * * 1-5 /home/me/job.sh\n";
        let tasks = tasks_from_crontab_text(
            text,
            CrontabMode::User,
            SourceKind::CronUser,
            "crontab:me",
            "me",
            true,
            None,
        );
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, "/home/me/job.sh");
        assert_eq!(tasks[0].schedule_expr, "30 4 * * 1-5");
        assert!(matches!(tasks[0].last_result, LastResult::Unknown));
    }
}
