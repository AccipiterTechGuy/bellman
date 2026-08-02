//! Aggregate discovery across providers.

use crate::visible::cron;
use crate::visible::providers::{at, bellman, systemd};
use crate::visible::types::{DiscoveredTask, ScanResult, SourceFilter, SourceKind};
use chrono::Utc;
use std::path::Path;

/// Platform label for JSON output.
pub fn platform_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// Scan all (or filtered) schedule sources on this machine.
///
/// On non-Linux, returns a single honest "not implemented" placeholder when
/// the filter would otherwise yield an empty list that looks like "nothing
/// scheduled".
pub fn scan(
    filter: SourceFilter,
    user_filter: Option<&str>,
    bellman_db: Option<&Path>,
) -> ScanResult {
    let mut tasks = Vec::new();
    let mut warnings = Vec::new();

    if cfg!(target_os = "linux") {
        scan_linux(filter, user_filter, bellman_db, &mut tasks, &mut warnings);
    } else if cfg!(target_os = "macos") {
        scan_stub(
            "macos",
            "launchd",
            "Visible Scheduler v1: launchd discovery not implemented on this platform yet",
            filter,
            &mut tasks,
            &mut warnings,
        );
        // Bellman store still works everywhere
        if matches!(filter, SourceFilter::All | SourceFilter::Bellman) {
            if let Some(db) = bellman_db {
                let (t, w) = bellman::discover_bellman(db);
                tasks.extend(t);
                warnings.extend(w);
            }
        }
    } else if cfg!(target_os = "windows") {
        scan_stub(
            "windows",
            "Task Scheduler",
            "Visible Scheduler v1: Windows Task Scheduler discovery not implemented on this platform yet",
            filter,
            &mut tasks,
            &mut warnings,
        );
        if matches!(filter, SourceFilter::All | SourceFilter::Bellman) {
            if let Some(db) = bellman_db {
                let (t, w) = bellman::discover_bellman(db);
                tasks.extend(t);
                warnings.extend(w);
            }
        }
    } else {
        warnings.push(format!(
            "unsupported platform {}; only Bellman store can be listed",
            platform_name()
        ));
        if let Some(db) = bellman_db {
            let (t, w) = bellman::discover_bellman(db);
            tasks.extend(t);
            warnings.extend(w);
        }
    }

    // Stable order: source_kind, source, schedule, command, id
    tasks.sort_by(|a, b| {
        (
            a.source_kind.as_str(),
            a.source.as_str(),
            a.schedule_expr.as_str(),
            a.command.as_str(),
            a.id.as_str(),
        )
            .cmp(&(
                b.source_kind.as_str(),
                b.source.as_str(),
                b.schedule_expr.as_str(),
                b.command.as_str(),
                b.id.as_str(),
            ))
    });

    let filter_label = match filter {
        SourceFilter::All => "all",
        SourceFilter::Cron => "cron",
        SourceFilter::CronD => "cron.d",
        SourceFilter::Systemd => "systemd",
        SourceFilter::At => "at",
        SourceFilter::Bellman => "bellman",
        SourceFilter::Anacron => "anacron",
        SourceFilter::RunParts => "run-parts",
    };

    ScanResult {
        scanned_at: Utc::now(),
        platform: platform_name().into(),
        filter: filter_label.into(),
        count: tasks.len(),
        tasks,
        warnings,
    }
}

fn scan_linux(
    filter: SourceFilter,
    user_filter: Option<&str>,
    bellman_db: Option<&Path>,
    tasks: &mut Vec<DiscoveredTask>,
    warnings: &mut Vec<String>,
) {
    let want = |k: SourceKind| k.matches_filter(filter);

    if want(SourceKind::CronUser) {
        // Only the invoking user's crontab is readable via `crontab -l` without
        // root. Never re-label those rows as a different --user.
        let invoking = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "user".into());
        match user_filter {
            None => {
                let (t, w) = cron::discover_user_crontab(&invoking);
                tasks.extend(t);
                warnings.extend(w);
            }
            Some(u) if u == invoking.as_str() => {
                let (t, w) = cron::discover_user_crontab(&invoking);
                tasks.extend(t);
                warnings.extend(w);
            }
            Some(u) => {
                warnings.push(format!(
                    "cannot read crontab for other user '{u}' without root; skipped"
                ));
            }
        }
    }

    if want(SourceKind::CronSystem) {
        let (t, w) = cron::discover_system_crontab();
        tasks.extend(filter_user(t, user_filter));
        warnings.extend(w);
    }

    if want(SourceKind::CronD) || matches!(filter, SourceFilter::Cron) {
        // Cron filter includes cron.d; CronD-only also
        if matches!(
            filter,
            SourceFilter::All | SourceFilter::Cron | SourceFilter::CronD
        ) {
            let (t, w) = cron::discover_cron_d();
            tasks.extend(filter_user(t, user_filter));
            warnings.extend(w);
        }
    }

    if (want(SourceKind::CronRunParts)
        || matches!(filter, SourceFilter::Cron | SourceFilter::RunParts))
        && matches!(
            filter,
            SourceFilter::All | SourceFilter::Cron | SourceFilter::RunParts
        )
    {
        let (t, w) = cron::discover_run_parts();
        tasks.extend(filter_user(t, user_filter));
        warnings.extend(w);
    }

    if want(SourceKind::Anacron) {
        let (t, w) = cron::discover_anacron();
        tasks.extend(filter_user(t, user_filter));
        warnings.extend(w);
    }

    if want(SourceKind::SystemdSystem) || want(SourceKind::SystemdUser) {
        let (t, w) = systemd::discover_systemd();
        tasks.extend(filter_user(t, user_filter));
        warnings.extend(w);
    }

    if want(SourceKind::At) {
        let (t, w) = at::discover_at();
        tasks.extend(filter_user(t, user_filter));
        warnings.extend(w);
    }

    if want(SourceKind::Bellman) {
        if let Some(db) = bellman_db {
            let (t, w) = bellman::discover_bellman(db);
            tasks.extend(filter_user(t, user_filter));
            warnings.extend(w);
        } else {
            warnings.push("bellman db path not provided; skipped bellman timers".into());
        }
    }
}

fn filter_user(tasks: Vec<DiscoveredTask>, user: Option<&str>) -> Vec<DiscoveredTask> {
    match user {
        None => tasks,
        Some(u) => tasks.into_iter().filter(|t| t.owner == u).collect(),
    }
}

fn scan_stub(
    platform: &str,
    native: &str,
    message: &str,
    filter: SourceFilter,
    tasks: &mut Vec<DiscoveredTask>,
    warnings: &mut Vec<String>,
) {
    warnings.push(message.to_string());
    // Only inject the placeholder when not asking solely for bellman
    if matches!(filter, SourceFilter::Bellman) {
        return;
    }
    tasks.push(DiscoveredTask {
        id: format!("unsupported:{platform}"),
        source_kind: SourceKind::Unsupported,
        source: native.into(),
        owner: String::new(),
        command: String::new(),
        stdin_payload: None,
        schedule_expr: String::new(),
        human_explanation: message.into(),
        next_run: None,
        last_run: None,
        last_result: crate::visible::types::LastResult::Unknown,
        enabled: false,
        writable: false,
        write_block_reason: Some(message.into()),
        timezone: None,
        line_no: None,
        raw_line: None,
        disabled_original: None,
        platform_note: Some(message.into()),
    });
}

/// Find a task by id in a scan result (or re-scan).
pub fn find_task<'a>(result: &'a ScanResult, id: &str) -> Option<&'a DiscoveredTask> {
    result.tasks.iter().find(|t| t.id == id)
}
