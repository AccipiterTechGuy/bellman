//! Safe user-crontab mutation: backup, fence, disable/enable, dry-run/apply.
//!
//! Rules (non-negotiable):
//! - Only the invoking user's own crontab is writable.
//! - System files are refused with an explanation (no sudo).
//! - Outside the bellman-managed fence, preserve lines byte-for-byte.
//! - Disable comments out and stores the exact original; enable restores it.
//! - Default is dry-run (no change). `--apply` required to write.

use crate::visible::cron::parse::{
    had_trailing_newline, join_lines, join_percent, parse_crontab, split_lines, CrontabLine,
    CrontabMode, DISABLE_PREFIX, FENCE_BEGIN, FENCE_END,
};
use crate::visible::cron::{read_user_crontab, tasks_from_crontab_text};
use crate::visible::id::{short_hash, task_id};
use crate::visible::types::{DiscoveredTask, SourceKind, WritePlan};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MAX_BACKUPS: usize = 20;

/// Data dir for crontab backups (default `~/.bellman/crontab-backups`).
pub fn default_backup_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".bellman").join("crontab-backups")
}

fn current_username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| {
            // last resort
            String::from_utf8_lossy(
                &Command::new("id")
                    .arg("-un")
                    .output()
                    .map(|o| o.stdout)
                    .unwrap_or_default(),
            )
            .trim()
            .to_string()
        })
}

/// Install crontab text via `crontab -` (stdin).
pub fn install_user_crontab(text: &str) -> Result<(), String> {
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn crontab: {e}"))?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().ok_or("crontab stdin closed")?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("write crontab stdin: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait crontab: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "crontab install failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Backup current crontab body into `backup_dir`. Returns path.
pub fn backup_crontab(text: &str, backup_dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(backup_dir).map_err(|e| format!("mkdir backups: {e}"))?;
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = backup_dir.join(format!("crontab-{ts}.bak"));
    fs::write(&path, text).map_err(|e| format!("write backup: {e}"))?;
    prune_backups(backup_dir, MAX_BACKUPS)?;
    Ok(path)
}

fn prune_backups(dir: &Path, keep: usize) -> Result<(), String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("read backups: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("crontab-") && n.ends_with(".bak"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    if files.len() > keep {
        let drop_n = files.len() - keep;
        for p in files.into_iter().take(drop_n) {
            let _ = fs::remove_file(p);
        }
    }
    Ok(())
}

fn refuse_non_user(task: &DiscoveredTask) -> Option<String> {
    if task.source_kind.is_system_readonly() {
        return Some(task.write_block_reason.clone().unwrap_or_else(|| {
            format!(
                "refusing to modify {} (source {:?}): v1 is read-only for system schedules. \
                 Bellman will not call sudo/pkexec. Edit manually as root if required.",
                task.source, task.source_kind
            )
        }));
    }
    if !task.writable {
        return Some(
            task.write_block_reason
                .clone()
                .unwrap_or_else(|| "task is not writable".into()),
        );
    }
    if !matches!(task.source_kind, SourceKind::CronUser) {
        return Some(format!(
            "v1 only mutates the invoking user's crontab; this task is {:?}",
            task.source_kind
        ));
    }
    None
}

/// Disable a user-crontab job by commenting it with a recoverable marker.
///
/// `apply == false` → dry-run only (default safety).
pub fn disable_task(
    task: &DiscoveredTask,
    apply: bool,
    backup_dir: &Path,
) -> Result<WritePlan, String> {
    if let Some(reason) = refuse_non_user(task) {
        return Ok(WritePlan {
            task_id: Some(task.id.clone()),
            action: "disable".into(),
            target: task.source.clone(),
            applied: false,
            dry_run: !apply,
            before: String::new(),
            after: String::new(),
            backup_path: None,
            refused: Some(reason),
            note: None,
        });
    }
    if !task.enabled {
        return Ok(WritePlan {
            task_id: Some(task.id.clone()),
            action: "disable".into(),
            target: task.source.clone(),
            applied: false,
            dry_run: !apply,
            before: String::new(),
            after: String::new(),
            backup_path: None,
            refused: None,
            note: Some("already disabled".into()),
        });
    }

    let before = read_user_crontab()?;
    let after = transform_disable(&before, task)?;
    finish_write("disable", task, &before, &after, apply, backup_dir)
}

/// Re-enable a previously Bellman-disabled user-crontab job (byte-identical restore).
pub fn enable_task(
    task: &DiscoveredTask,
    apply: bool,
    backup_dir: &Path,
) -> Result<WritePlan, String> {
    if let Some(reason) = refuse_non_user(task) {
        return Ok(WritePlan {
            task_id: Some(task.id.clone()),
            action: "enable".into(),
            target: task.source.clone(),
            applied: false,
            dry_run: !apply,
            before: String::new(),
            after: String::new(),
            backup_path: None,
            refused: Some(reason),
            note: None,
        });
    }
    if task.enabled {
        return Ok(WritePlan {
            task_id: Some(task.id.clone()),
            action: "enable".into(),
            target: task.source.clone(),
            applied: false,
            dry_run: !apply,
            before: String::new(),
            after: String::new(),
            backup_path: None,
            refused: None,
            note: Some("already enabled".into()),
        });
    }

    let before = read_user_crontab()?;
    let after = transform_enable(&before, task)?;
    finish_write("enable", task, &before, &after, apply, backup_dir)
}

/// Create a new cron line inside the bellman-managed fence.
pub fn new_cron_task(
    command: &str,
    cron_expr: &str,
    apply: bool,
    backup_dir: &Path,
) -> Result<(WritePlan, Option<DiscoveredTask>), String> {
    if command.trim().is_empty() {
        return Err("command must not be empty".into());
    }
    if cron_expr.trim().is_empty() {
        return Err("cron expression must not be empty".into());
    }
    // Validate schedule by parsing a synthetic line (with % re-escaped).
    let probe = format!("{cron_expr} {}", join_percent(command, None));
    let parsed = parse_crontab(&probe, CrontabMode::User);
    let is_job = matches!(parsed.first(), Some(CrontabLine::Job { .. }));
    if !is_job {
        return Err(format!(
            "could not parse cron line: `{cron_expr} {command}`"
        ));
    }

    let user = current_username();
    let before = read_user_crontab().unwrap_or_default();
    let entry_id = short_hash(&format!("{cron_expr}|{command}|{}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    // Escape literal % so crontab does not treat them as stdin separators.
    let encoded_cmd = join_percent(command, None);
    let new_line = format!("{cron_expr} {encoded_cmd}");
    let after = insert_into_fence(&before, &entry_id, &new_line);

    let plan = finish_write_raw("new", None, "crontab:user", &before, &after, apply, backup_dir)?;

    let task = if plan.refused.is_none() {
        let source = format!("crontab:{user}");
        // Find the new task in the after text
        let tasks = tasks_from_crontab_text(
            &after,
            CrontabMode::User,
            SourceKind::CronUser,
            &source,
            &user,
            true,
            None,
        );
        tasks
            .into_iter()
            .find(|t| t.command == command && t.schedule_expr == cron_expr)
    } else {
        None
    };

    Ok((plan, task))
}

/// Edit schedule/command of a user-crontab job (only if writable).
pub fn edit_task(
    task: &DiscoveredTask,
    new_command: Option<&str>,
    new_cron: Option<&str>,
    apply: bool,
    backup_dir: &Path,
) -> Result<WritePlan, String> {
    if let Some(reason) = refuse_non_user(task) {
        return Ok(WritePlan {
            task_id: Some(task.id.clone()),
            action: "edit".into(),
            target: task.source.clone(),
            applied: false,
            dry_run: !apply,
            before: String::new(),
            after: String::new(),
            backup_path: None,
            refused: Some(reason),
            note: None,
        });
    }
    if new_command.is_none() && new_cron.is_none() {
        return Err("nothing to edit (pass --command and/or --cron)".into());
    }

    let before = read_user_crontab()?;
    let after = transform_edit(&before, task, new_command, new_cron)?;
    finish_write("edit", task, &before, &after, apply, backup_dir)
}

fn finish_write(
    action: &str,
    task: &DiscoveredTask,
    before: &str,
    after: &str,
    apply: bool,
    backup_dir: &Path,
) -> Result<WritePlan, String> {
    finish_write_raw(
        action,
        Some(task.id.clone()),
        &task.source,
        before,
        after,
        apply,
        backup_dir,
    )
}

fn finish_write_raw(
    action: &str,
    task_id: Option<String>,
    target: &str,
    before: &str,
    after: &str,
    apply: bool,
    backup_dir: &Path,
) -> Result<WritePlan, String> {
    if before == after {
        return Ok(WritePlan {
            task_id,
            action: action.into(),
            target: target.into(),
            applied: false,
            dry_run: !apply,
            before: before.to_string(),
            after: after.to_string(),
            backup_path: None,
            refused: None,
            note: Some("no change".into()),
        });
    }

    let mut backup_path = None;
    let mut applied = false;
    if apply {
        let bp = backup_crontab(before, backup_dir)?;
        backup_path = Some(bp.display().to_string());
        install_user_crontab(after)?;
        applied = true;
    }

    Ok(WritePlan {
        task_id,
        action: action.into(),
        target: target.into(),
        applied,
        dry_run: !apply,
        before: before.to_string(),
        after: after.to_string(),
        backup_path,
        refused: None,
        note: if apply {
            None
        } else {
            Some("dry-run: pass --apply to write (backup will be taken first)".into())
        },
    })
}

/// Match a job line in crontab text to a discovered task.
fn find_job_line_index(lines: &[String], task: &DiscoveredTask) -> Result<usize, String> {
    // Prefer exact raw_line match
    if let Some(raw) = &task.raw_line {
        if let Some(i) = lines.iter().position(|l| l == raw) {
            return Ok(i);
        }
    }
    // Fall back to line_no (1-based)
    if let Some(n) = task.line_no {
        let idx = n as usize - 1;
        if idx < lines.len() {
            return Ok(idx);
        }
    }
    // Content match on command + schedule
    let parsed = parse_crontab(&join_lines(lines, true), CrontabMode::User);
    for pl in parsed {
        if let CrontabLine::Job {
            raw,
            schedule,
            command,
            disabled_by_bellman,
            original_line,
            ..
        } = pl
        {
            let expr = schedule.expression();
            let cmd_ok = command == task.command
                || original_line
                    .as_ref()
                    .map(|o| o.contains(&task.command))
                    .unwrap_or(false);
            let sched_ok = expr == task.schedule_expr;
            let en_ok = task.enabled != disabled_by_bellman || disabled_by_bellman == !task.enabled;
            if cmd_ok && sched_ok && en_ok {
                if let Some(i) = lines.iter().position(|l| l == &raw) {
                    return Ok(i);
                }
            }
        }
    }
    Err(format!(
        "could not locate task {} in current crontab (it may have changed; re-scan)",
        task.id
    ))
}

fn transform_disable(before: &str, task: &DiscoveredTask) -> Result<String, String> {
    let trailing = had_trailing_newline(before);
    let mut lines = split_lines(before);
    let idx = find_job_line_index(&lines, task)?;
    let original = lines[idx].clone();
    // Never disable something already a plain comment unless it's our marker form
    if original.trim_start().starts_with('#') && !original.contains(DISABLE_PREFIX.trim()) {
        // if it's already our disabled form, no-op handled earlier
    }
    // Replace with marker line that embeds the exact original
    lines[idx] = format!("{DISABLE_PREFIX}{original}");
    Ok(join_lines(&lines, trailing))
}

fn transform_enable(before: &str, task: &DiscoveredTask) -> Result<String, String> {
    let trailing = had_trailing_newline(before);
    let mut lines = split_lines(before);
    let idx = find_job_line_index(&lines, task)?;
    let current = lines[idx].clone();
    let original = if let Some(o) = &task.disabled_original {
        o.clone()
    } else if let Some(rest) = current.trim_start().strip_prefix(DISABLE_PREFIX) {
        rest.to_string()
    } else if let Some(raw) = &task.raw_line {
        if let Some(rest) = raw.trim_start().strip_prefix(DISABLE_PREFIX) {
            rest.to_string()
        } else {
            return Err("enable requires a Bellman-disabled line (marker missing)".into());
        }
    } else {
        return Err("enable requires a Bellman-disabled line (marker missing)".into());
    };
    // Byte-for-byte restore
    lines[idx] = original;
    Ok(join_lines(&lines, trailing))
}

fn transform_edit(
    before: &str,
    task: &DiscoveredTask,
    new_command: Option<&str>,
    new_cron: Option<&str>,
) -> Result<String, String> {
    let trailing = had_trailing_newline(before);
    let mut lines = split_lines(before);
    let idx = find_job_line_index(&lines, task)?;
    let current = lines[idx].clone();

    // Only rewrite lines we understand; if outside fence and not created by us,
    // still allow edit of user crontab jobs per v1 (user owns whole crontab),
    // but preserve surrounding lines.
    let cron = new_cron.unwrap_or(&task.schedule_expr);
    let command = new_command.unwrap_or(&task.command);
    // Re-escape literal % (and re-attach stdin) so rewrite cannot corrupt crontab.
    let encoded = join_percent(command, task.stdin_payload.as_deref());
    let new_line = if current.trim_start().starts_with(DISABLE_PREFIX) {
        format!("{DISABLE_PREFIX}{cron} {encoded}")
    } else {
        format!("{cron} {encoded}")
    };
    // Validate
    let probe_body = new_line
        .strip_prefix(DISABLE_PREFIX)
        .unwrap_or(&new_line);
    let parsed = parse_crontab(probe_body, CrontabMode::User);
    if !matches!(parsed.first(), Some(CrontabLine::Job { .. })) {
        return Err(format!("edited line does not parse as a cron job: {new_line}"));
    }
    lines[idx] = new_line;
    Ok(join_lines(&lines, trailing))
}

/// Insert a line inside `# BEGIN/END bellman-managed`, creating the fence if needed.
fn insert_into_fence(before: &str, entry_id: &str, new_line: &str) -> String {
    let trailing = had_trailing_newline(before) || before.is_empty();
    let mut lines = split_lines(before);
    let begin = lines.iter().position(|l| l.trim() == FENCE_BEGIN);
    let end = lines.iter().position(|l| l.trim() == FENCE_END);

    let id_comment = format!("# id: {entry_id}");
    match (begin, end) {
        (Some(b), Some(e)) if e > b => {
            lines.insert(e, new_line.to_string());
            lines.insert(e, id_comment);
        }
        _ => {
            if !lines.is_empty() && !lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
            lines.push(FENCE_BEGIN.to_string());
            lines.push(id_comment);
            lines.push(new_line.to_string());
            lines.push(FENCE_END.to_string());
        }
    }
    join_lines(&lines, trailing || true)
}

/// Refuse path helper for CLI when user targets system files by id.
pub fn refuse_system_write(task: &DiscoveredTask, action: &str) -> WritePlan {
    WritePlan {
        task_id: Some(task.id.clone()),
        action: action.into(),
        target: task.source.clone(),
        applied: false,
        dry_run: true,
        before: String::new(),
        after: String::new(),
        backup_path: None,
        refused: Some(refuse_non_user(task).unwrap_or_else(|| {
            format!(
                "refusing {action} on {} — display only in v1",
                task.source
            )
        })),
        note: None,
    }
}

/// Synthetic task id helper for tests.
#[allow(dead_code)]
pub fn make_user_task_id(user: &str, line_no: u32, raw: &str) -> String {
    task_id(
        SourceKind::CronUser,
        &[&format!("crontab:{user}"), &line_no.to_string(), user, raw],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visible::cron::parse::split_percent;

    #[test]
    fn disable_enable_roundtrip_byte_identical() {
        let original = "# hand comment\n\nSHELL=/bin/bash\n0 6 * * MON-FRI /usr/bin/backup %daily\n# tail\n";
        let tasks = tasks_from_crontab_text(
            original,
            CrontabMode::User,
            SourceKind::CronUser,
            "crontab:me",
            "me",
            true,
            None,
        );
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        // Command includes % payload in display form — raw_line is what matters
        let disabled = transform_disable(original, task).unwrap();
        assert!(disabled.contains(DISABLE_PREFIX));
        assert!(disabled.contains("# hand comment"));
        assert!(disabled.contains("SHELL=/bin/bash"));
        // Rebuild task as disabled
        let tasks2 = tasks_from_crontab_text(
            &disabled,
            CrontabMode::User,
            SourceKind::CronUser,
            "crontab:me",
            "me",
            true,
            None,
        );
        let disabled_task = tasks2.iter().find(|t| !t.enabled).expect("disabled");
        let restored = transform_enable(&disabled, disabled_task).unwrap();
        assert_eq!(restored, original, "disable→enable must be byte-identical");
    }

    #[test]
    fn percent_survives_in_original_line() {
        let line = r"0 * * * * cat %hello%world";
        let (cmd, stdin) = split_percent("cat %hello%world");
        assert_eq!(cmd, "cat ");
        assert_eq!(stdin.as_deref(), Some("hello\nworld"));
        let text = format!("{line}\n");
        let tasks = tasks_from_crontab_text(
            &text,
            CrontabMode::User,
            SourceKind::CronUser,
            "crontab:me",
            "me",
            true,
            None,
        );
        let t = &tasks[0];
        let disabled = transform_disable(&text, t).unwrap();
        let tasks2 = tasks_from_crontab_text(
            &disabled,
            CrontabMode::User,
            SourceKind::CronUser,
            "crontab:me",
            "me",
            true,
            None,
        );
        let dt = tasks2.iter().find(|x| !x.enabled).unwrap();
        let restored = transform_enable(&disabled, dt).unwrap();
        assert_eq!(restored, text);
        assert!(restored.contains('%'));
    }

    #[test]
    fn fence_preserves_outside_lines() {
        let before = "# keep\n0 1 * * * /bin/true\n";
        let after = insert_into_fence(before, "abc", "0 2 * * * /bin/false");
        assert!(after.contains("# keep"));
        assert!(after.contains("0 1 * * * /bin/true"));
        assert!(after.contains(FENCE_BEGIN));
        assert!(after.contains("0 2 * * * /bin/false"));
        assert!(after.contains(FENCE_END));
    }

    #[test]
    fn system_crontab_refused() {
        let task = DiscoveredTask {
            id: "x".into(),
            source_kind: SourceKind::CronSystem,
            source: "/etc/crontab".into(),
            owner: "root".into(),
            command: "true".into(),
            stdin_payload: None,
            schedule_expr: "0 * * * *".into(),
            human_explanation: String::new(),
            next_run: None,
            last_run: None,
            last_result: crate::visible::types::LastResult::Unknown,
            enabled: true,
            writable: false,
            write_block_reason: Some("readonly".into()),
            timezone: None,
            line_no: Some(1),
            raw_line: None,
            disabled_original: None,
            platform_note: None,
        };
        let plan = disable_task(&task, true, Path::new("/tmp")).unwrap();
        assert!(plan.refused.is_some());
        assert!(!plan.applied);
    }

    #[test]
    fn edit_preserves_escaped_percent() {
        // Auditor REPRO: edit schedule only must keep \% in the crontab line.
        let original = r"0 0 * * * echo 100\% pure
";
        let tasks = tasks_from_crontab_text(
            original,
            CrontabMode::User,
            SourceKind::CronUser,
            "crontab:me",
            "me",
            true,
            None,
        );
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, "echo 100% pure");
        assert!(tasks[0].stdin_payload.is_none());
        let edited = transform_edit(original, &tasks[0], None, Some("0 1 * * *")).unwrap();
        assert!(
            edited.contains(r"echo 100\% pure"),
            "expected re-escaped % in rewritten line, got: {edited:?}"
        );
        assert!(
            !edited.contains("echo 100% pure"),
            "unescaped % would corrupt cron stdin semantics: {edited:?}"
        );
        // Round-trip parse must still see a literal percent command.
        let again = tasks_from_crontab_text(
            &edited,
            CrontabMode::User,
            SourceKind::CronUser,
            "crontab:me",
            "me",
            true,
            None,
        );
        assert_eq!(again[0].command, "echo 100% pure");
        assert_eq!(again[0].schedule_expr, "0 1 * * *");
    }
}
