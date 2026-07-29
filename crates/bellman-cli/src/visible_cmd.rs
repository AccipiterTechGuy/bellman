//! Visible Scheduler CLI: `scan` and `task` subcommands.

use crate::commands::CliError;
use bellman_core::visible::{
    default_backup_dir, default_snapshot_path, diff_scans, disable_task, edit_task, enable_task,
    find_task, load_snapshot, new_cron_task, run_task, save_snapshot, scan, timer_logs,
    DiscoveredTask, ScanResult, SourceFilter, SourceKind, WritePlan,
};
use serde_json::{json, Value};
use std::path::Path;

// ── Payload extensions (JSON-first) ─────────────────────────────────────

pub enum VisiblePayload {
    Scan(ScanResult),
    ScanDiff {
        current: ScanResult,
        diff: bellman_core::ScanDiff,
    },
    Task(DiscoveredTask),
    Explain {
        id: String,
        explanation: String,
        task: DiscoveredTask,
    },
    Logs {
        id: String,
        lines: usize,
        text: String,
    },
    Write(WritePlan),
    WriteNew {
        plan: WritePlan,
        task: Option<DiscoveredTask>,
    },
    Run(bellman_core::RunOutcome),
}

impl VisiblePayload {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Scan(r) => {
                let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("command".into(), json!("scan"));
                }
                v
            }
            Self::ScanDiff { current, diff } => json!({
                "command": "scan",
                "mode": "diff",
                "current": current,
                "diff": diff,
            }),
            Self::Task(t) => json!({
                "command": "task_show",
                "task": t,
            }),
            Self::Explain {
                id,
                explanation,
                task,
            } => json!({
                "command": "task_explain",
                "id": id,
                "explanation": explanation,
                "task": task,
            }),
            Self::Logs { id, lines, text } => json!({
                "command": "task_logs",
                "id": id,
                "lines": lines,
                "text": text,
            }),
            Self::Write(plan) => json!({
                "command": format!("task_{}", plan.action),
                "plan": plan,
            }),
            Self::WriteNew { plan, task } => json!({
                "command": "task_new",
                "plan": plan,
                "task": task,
            }),
            Self::Run(o) => json!({
                "command": "task_run",
                "run": o,
            }),
        }
    }

    pub fn to_human(&self) -> String {
        match self {
            Self::Scan(r) => {
                let mut lines = vec![format!(
                    "scan platform={} filter={} count={} warnings={}",
                    r.platform,
                    r.filter,
                    r.count,
                    r.warnings.len()
                )];
                for t in &r.tasks {
                    lines.push(format!(
                        "{}\t{}\t{}\tenabled={}\tnext={}\t{}\t{}",
                        t.id,
                        t.source_kind.as_str(),
                        t.owner,
                        t.enabled,
                        t.next_run
                            .map(|x| x.to_rfc3339())
                            .unwrap_or_else(|| "-".into()),
                        t.schedule_expr,
                        truncate(&t.command, 60)
                    ));
                }
                for w in &r.warnings {
                    lines.push(format!("warning: {w}"));
                }
                lines.join("\n")
            }
            Self::ScanDiff { diff, current } => {
                let mut lines = vec![format!(
                    "scan --diff current_count={} added={} removed={} changed={}",
                    current.count,
                    diff.added.len(),
                    diff.removed.len(),
                    diff.changed.len()
                )];
                for t in &diff.added {
                    lines.push(format!("+ {} {}", t.id, t.command));
                }
                for t in &diff.removed {
                    lines.push(format!("- {} {}", t.id, t.command));
                }
                for c in &diff.changed {
                    lines.push(format!(
                        "~ {} {}: {} → {}",
                        c.id, c.field, c.before, c.after
                    ));
                }
                lines.join("\n")
            }
            Self::Task(t) => format_task(t),
            Self::Explain { explanation, .. } => explanation.clone(),
            Self::Logs { text, .. } => text.clone(),
            Self::Write(plan) | Self::WriteNew { plan, .. } => format_plan(plan),
            Self::Run(o) => format!(
                "run task={} exit={} stdout_bytes={} stderr_bytes={}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                o.task_id,
                o.exit_code,
                o.stdout.len(),
                o.stderr.len(),
                o.stdout,
                o.stderr
            ),
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn format_task(t: &DiscoveredTask) -> String {
    format!(
        "id: {}\nsource: {} ({})\nowner: {}\nenabled: {}\nwritable: {}\nschedule: {}\nexplain: {}\nnext: {}\nlast_run: {}\nlast_result: {:?}\ncommand: {}\n{}",
        t.id,
        t.source,
        t.source_kind.as_str(),
        t.owner,
        t.enabled,
        t.writable,
        t.schedule_expr,
        t.human_explanation,
        t.next_run
            .map(|x| x.to_rfc3339())
            .unwrap_or_else(|| "-".into()),
        t.last_run
            .map(|x| x.to_rfc3339())
            .unwrap_or_else(|| "-".into()),
        t.last_result,
        t.command,
        t.write_block_reason
            .as_deref()
            .map(|r| format!("write_block: {r}"))
            .unwrap_or_default()
    )
}

fn format_plan(plan: &WritePlan) -> String {
    let mut s = format!(
        "action={} applied={} dry_run={} target={}\n",
        plan.action, plan.applied, plan.dry_run, plan.target
    );
    if let Some(r) = &plan.refused {
        s.push_str(&format!("REFUSED: {r}\n"));
    }
    if let Some(n) = &plan.note {
        s.push_str(&format!("note: {n}\n"));
    }
    if let Some(b) = &plan.backup_path {
        s.push_str(&format!("backup: {b}\n"));
    }
    if plan.before != plan.after {
        s.push_str("--- before ---\n");
        s.push_str(&plan.before);
        if !plan.before.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("--- after ---\n");
        s.push_str(&plan.after);
        if !plan.after.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

// ── Commands ────────────────────────────────────────────────────────────

pub fn cmd_scan(
    db: &Path,
    source: Option<&str>,
    user: Option<&str>,
    do_diff: bool,
) -> Result<VisiblePayload, CliError> {
    const CMD: &str = "scan";
    let filter = match source {
        Some(s) => SourceFilter::parse(s).map_err(|m| CliError::new(CMD, "invalid_args", m))?,
        None => SourceFilter::All,
    };
    let result = scan(filter, user, Some(db));
    // Always refresh snapshot after a successful full-ish scan so --diff works.
    let snap = default_snapshot_path();
    if do_diff {
        let prev = load_snapshot(&snap).map_err(|e| CliError::new(CMD, "snapshot_error", e))?;
        let diff = diff_scans(prev.as_ref(), &result);
        let _ = save_snapshot(&snap, &result);
        Ok(VisiblePayload::ScanDiff {
            current: result,
            diff,
        })
    } else {
        let _ = save_snapshot(&snap, &result);
        Ok(VisiblePayload::Scan(result))
    }
}

fn rescan(db: &Path) -> ScanResult {
    scan(SourceFilter::All, None, Some(db))
}

fn require_task(db: &Path, id: &str) -> Result<DiscoveredTask, CliError> {
    const CMD: &str = "task";
    let result = rescan(db);
    find_task(&result, id)
        .cloned()
        .ok_or_else(|| {
            CliError::new(
                CMD,
                "not_found",
                format!("task id not found: {id} (run `bellman scan` to list ids)"),
            )
        })
}

pub fn cmd_task_show(db: &Path, id: &str) -> Result<VisiblePayload, CliError> {
    let task = require_task(db, id)?;
    Ok(VisiblePayload::Task(task))
}

pub fn cmd_task_explain(db: &Path, id: &str) -> Result<VisiblePayload, CliError> {
    let task = require_task(db, id)?;
    Ok(VisiblePayload::Explain {
        id: id.into(),
        explanation: task.human_explanation.clone(),
        task,
    })
}

pub fn cmd_task_logs(db: &Path, id: &str, lines: usize) -> Result<VisiblePayload, CliError> {
    let task = require_task(db, id)?;
    let text = match task.source_kind {
        SourceKind::SystemdSystem | SourceKind::SystemdUser => {
            timer_logs(&task, lines).map_err(|e| CliError::new("task_logs", "logs_error", e))?
        }
        SourceKind::CronUser
        | SourceKind::CronSystem
        | SourceKind::CronD
        | SourceKind::CronRunParts
        | SourceKind::Anacron => {
            format!(
                "cron does not record exit status or per-job logs reliably.\n\
                 last_result for this task is {:?}.\n\
                 Check system journal / /var/log/syslog for CRON invocations only \
                 (invocation ≠ success).",
                task.last_result
            )
        }
        SourceKind::Bellman => {
            // Best-effort: point at event log path
            format!(
                "Bellman timer logs live in the event JSONL (see `~/.bellman/logs`).\n\
                 last_result={:?} last_run={:?}",
                task.last_result, task.last_run
            )
        }
        SourceKind::At => "at jobs have no persistent log after execution.".into(),
        SourceKind::Unsupported => task
            .platform_note
            .clone()
            .unwrap_or_else(|| "not implemented".into()),
    };
    Ok(VisiblePayload::Logs {
        id: id.into(),
        lines,
        text,
    })
}

pub fn cmd_task_enable(db: &Path, id: &str, apply: bool) -> Result<VisiblePayload, CliError> {
    let task = require_task(db, id)?;
    let plan = match task.source_kind {
        SourceKind::CronUser => enable_task(&task, apply, &default_backup_dir())
            .map_err(|e| CliError::new("task_enable", "write_error", e))?,
        SourceKind::Bellman => {
            // Delegate to store pause/resume semantics via enable flag
            enable_bellman(db, &task, true, apply)?
        }
        _ => bellman_core::refuse_system_write(&task, "enable"),
    };
    Ok(VisiblePayload::Write(plan))
}

pub fn cmd_task_disable(db: &Path, id: &str, apply: bool) -> Result<VisiblePayload, CliError> {
    let task = require_task(db, id)?;
    let plan = match task.source_kind {
        SourceKind::CronUser => disable_task(&task, apply, &default_backup_dir())
            .map_err(|e| CliError::new("task_disable", "write_error", e))?,
        SourceKind::Bellman => enable_bellman(db, &task, false, apply)?,
        _ => bellman_core::refuse_system_write(&task, "disable"),
    };
    Ok(VisiblePayload::Write(plan))
}

fn enable_bellman(
    db: &Path,
    task: &DiscoveredTask,
    enabled: bool,
    apply: bool,
) -> Result<WritePlan, CliError> {
    use bellman_core::store::{OpenOptions, Store, TimerPatch, TimerUpdate};
    use uuid::Uuid;

    let timer_id = task
        .source
        .strip_prefix("bellman:")
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            CliError::new("task", "invalid_args", "bellman task id missing uuid")
        })?;

    let before = format!("enabled={}", task.enabled);
    let after = format!("enabled={enabled}");
    if !apply {
        return Ok(WritePlan {
            task_id: Some(task.id.clone()),
            action: if enabled { "enable" } else { "disable" }.into(),
            target: task.source.clone(),
            applied: false,
            dry_run: true,
            before,
            after,
            backup_path: None,
            refused: None,
            note: Some("dry-run: pass --apply to write".into()),
        });
    }
    let mut store = Store::open_with(
        db,
        OpenOptions {
            refuse_network_fs: true,
            ..OpenOptions::default()
        },
    )
    .map_err(|e| CliError::new("task", "store_error", e.to_string()))?;
    let timer = store
        .get_timer(timer_id)
        .map_err(|e| CliError::new("task", "store_error", e.to_string()))?
        .ok_or_else(|| CliError::new("task", "not_found", "bellman timer missing"))?;
    store
        .update_timer(TimerUpdate {
            id: timer_id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                enabled: Some(enabled),
                ..TimerPatch::default()
            },
        })
        .map_err(|e| CliError::new("task", "store_error", e.to_string()))?;
    Ok(WritePlan {
        task_id: Some(task.id.clone()),
        action: if enabled { "enable" } else { "disable" }.into(),
        target: task.source.clone(),
        applied: true,
        dry_run: false,
        before,
        after,
        backup_path: None,
        refused: None,
        note: None,
    })
}

pub fn cmd_task_run(db: &Path, id: &str, confirm: bool) -> Result<VisiblePayload, CliError> {
    let task = require_task(db, id)?;
    let outcome = run_task(&task, confirm, 300)
        .map_err(|e| CliError::new("task_run", "run_error", e))?;
    Ok(VisiblePayload::Run(outcome))
}

pub fn cmd_task_new(
    db: &Path,
    command: &str,
    cron: &str,
    source: Option<&str>,
    apply: bool,
) -> Result<VisiblePayload, CliError> {
    const CMD: &str = "task_new";
    let src = source.unwrap_or("cron");
    match src {
        "cron" => {
            let (plan, task) = new_cron_task(command, cron, apply, &default_backup_dir())
                .map_err(|e| CliError::new(CMD, "write_error", e))?;
            Ok(VisiblePayload::WriteNew { plan, task })
        }
        "bellman" => {
            // Create a Bellman cron timer in the store
            use bellman_core::occurrence::{Occurrence, OccurrenceKind};
            use bellman_core::store::{NewTimer, OpenOptions, Store};
            use bellman_core::visible::next_run::system_tz_name;

            let before = "(no timer)".to_string();
            let after = format!("cron={cron} command={command}");
            if !apply {
                return Ok(VisiblePayload::WriteNew {
                    plan: WritePlan {
                        task_id: None,
                        action: "new".into(),
                        target: "bellman".into(),
                        applied: false,
                        dry_run: true,
                        before,
                        after,
                        backup_path: None,
                        refused: None,
                        note: Some("dry-run: pass --apply to write".into()),
                    },
                    task: None,
                });
            }
            let tz = system_tz_name();
            let occ = Occurrence::new(
                OccurrenceKind::Cron {
                    expr: cron.to_string(),
                },
                &tz,
            )
            .map_err(|e| CliError::new(CMD, "invalid_args", e))?;
            let mut new = NewTimer::new(format!("task-{}", &cron.replace(' ', "-")), occ);
            new.action = bellman_core::Action::Launch {
                command: command.to_string(),
                args: vec![],
                workdir: None,
            };
            let mut store = Store::open_with(
                db,
                OpenOptions {
                    refuse_network_fs: true,
                    ..OpenOptions::default()
                },
            )
            .map_err(|e| CliError::new(CMD, "store_error", e.to_string()))?;
            let timer = store
                .create_timer(new)
                .map_err(|e| CliError::new(CMD, "store_error", e.to_string()))?;
            // Re-scan to get DiscoveredTask shape
            let result = rescan(db);
            let task = result
                .tasks
                .into_iter()
                .find(|t| t.source == format!("bellman:{}", timer.id));
            Ok(VisiblePayload::WriteNew {
                plan: WritePlan {
                    task_id: task.as_ref().map(|t| t.id.clone()),
                    action: "new".into(),
                    target: format!("bellman:{}", timer.id),
                    applied: true,
                    dry_run: false,
                    before,
                    after,
                    backup_path: None,
                    refused: None,
                    note: None,
                },
                task,
            })
        }
        other => Err(CliError::new(
            CMD,
            "invalid_args",
            format!("unknown --source {other} (expected cron|bellman)"),
        )),
    }
}

pub fn cmd_task_edit(
    db: &Path,
    id: &str,
    command: Option<&str>,
    cron: Option<&str>,
    apply: bool,
) -> Result<VisiblePayload, CliError> {
    let task = require_task(db, id)?;
    let plan = match task.source_kind {
        SourceKind::CronUser => edit_task(
            &task,
            command,
            cron,
            apply,
            &default_backup_dir(),
        )
        .map_err(|e| CliError::new("task_edit", "write_error", e))?,
        _ => bellman_core::refuse_system_write(&task, "edit"),
    };
    Ok(VisiblePayload::Write(plan))
}

