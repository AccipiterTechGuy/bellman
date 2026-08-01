//! Bellman store timers as visible tasks.

use crate::occurrence::OccurrenceKind;
use crate::store::{Action, Store};
use crate::visible::id::task_id;
use crate::visible::types::{DiscoveredTask, LastResult, SourceKind};
use chrono::Utc;
use std::path::Path;

pub fn discover_bellman(db_path: &Path) -> (Vec<DiscoveredTask>, Vec<String>) {
    let mut warnings = Vec::new();
    if !db_path.exists() {
        // Empty store is fine — no timers, not an error.
        return (Vec::new(), warnings);
    }
    let store = match Store::open_with(
        db_path,
        crate::store::OpenOptions {
            refuse_network_fs: true,
            ..Default::default()
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            warnings.push(format!("bellman store {}: {e}", db_path.display()));
            return (Vec::new(), warnings);
        }
    };
    let timers = match store.list_timers() {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("list_timers: {e}"));
            return (Vec::new(), warnings);
        }
    };
    let owner = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let mut tasks = Vec::with_capacity(timers.len());
    for t in timers {
        let expr = occurrence_expr(t.occurrence.kind(), &t.tz);
        let command = action_command(&t.action);
        let source = format!("bellman:{}", t.id);
        let id = task_id(SourceKind::Bellman, &[&source]);
        let human = format!(
            "Bellman timer {:?} ({}) in {}",
            t.name,
            t.occurrence.kind().kind_label(),
            t.tz
        );
        let last_result = if t.last_fired.is_some() {
            // Store does not keep last exit code on the timer row — unknown
            // unless we dig events; stay honest.
            LastResult::Unknown
        } else {
            LastResult::Never
        };
        tasks.push(DiscoveredTask {
            id,
            source_kind: SourceKind::Bellman,
            source,
            owner: owner.clone(),
            command,
            stdin_payload: None,
            schedule_expr: expr,
            human_explanation: human,
            next_run: t.next_fire_utc,
            last_run: t.last_fired,
            last_result,
            enabled: t.enabled,
            writable: true,
            write_block_reason: None,
            timezone: Some(t.tz.clone()),
            line_no: None,
            raw_line: None,
            disabled_original: None,
            platform_note: None,
        });
    }
    // Touch Utc so unused import stays if list empty — actually used via next_fire.
    let _ = Utc::now();
    (tasks, warnings)
}

fn occurrence_expr(kind: &OccurrenceKind, tz: &str) -> String {
    match kind {
        OccurrenceKind::Once { at } => format!("once {} ({tz})", at),
        OccurrenceKind::Interval { every_secs, .. } => format!("every {every_secs}s"),
        OccurrenceKind::Daily { at } => format!("daily {at} ({tz})"),
        OccurrenceKind::Weekly { days, at } => {
            let d: Vec<String> = days.iter().map(|d| format!("{d:?}")).collect();
            format!("weekly {} at {at} ({tz})", d.join(","))
        }
        OccurrenceKind::Monthly { day, at } => format!("monthly day {day} at {at} ({tz})"),
        OccurrenceKind::Yearly { month, day, at } => {
            format!("yearly {month:02}-{day:02} at {at} ({tz})")
        }
        OccurrenceKind::Cron { expr } => expr.clone(),
    }
}

fn action_command(action: &Action) -> String {
    match action {
        Action::Launch { command, args, .. } => {
            if args.is_empty() {
                command.clone()
            } else {
                format!("{command} {}", args.join(" "))
            }
        }
        Action::Notify { title, body } => {
            if body.is_empty() {
                format!("notify: {title}")
            } else {
                format!("notify: {title} — {body}")
            }
        }
        Action::None => "(no action)".into(),
    }
}
