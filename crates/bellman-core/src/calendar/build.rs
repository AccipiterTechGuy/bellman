//! Build a [`CalendarSnapshot`] from a source-agnostic list of expandable tasks.
//!
//! Day-one data comes from Bellman store timers; cron/systemd/at rows can be
//! fed through the same [`ExpandableTask`] shape once their schedules are known
//! (Visible Scheduler already discovers them).

use super::types::{
    CalendarBuildOptions, CalendarCaps, CalendarDay, CalendarEntry, CalendarSnapshot,
    CalendarStatus, WeekStart,
};
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::store::Timer;
use crate::visible::types::{DiscoveredTask, LastResult};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use std::collections::BTreeMap;
use std::str::FromStr;

/// One task that can expand fires inside a window — source-agnostic.
#[derive(Debug, Clone)]
pub struct ExpandableTask {
    /// Stable id of the task this expands.
    pub id: String,
    /// Its display name.
    pub name: String,
    /// Whether it is currently scheduled.
    pub enabled: bool,
    /// The recurrence to expand across the window.
    pub occurrence: Occurrence,
    /// Past-fire status hint when known (ok/failed/unknown).
    pub past_status: CalendarStatus,
    /// The command line, carried only when `--show-commands` was given.
    pub command: Option<String>,
    /// Which scheduler it came from.
    pub source_kind: String,
}

impl ExpandableTask {
    /// Adapt a Bellman store timer into the source-agnostic shape the
    /// calendar expands, so cron and systemd rows can join the same grid.
    pub fn from_timer(t: &Timer) -> Self {
        let command = match &t.action {
            crate::store::Action::Launch { command, args, .. } => {
                if args.is_empty() {
                    Some(command.clone())
                } else {
                    Some(format!("{command} {}", args.join(" ")))
                }
            }
            crate::store::Action::Notify { title, body } => {
                if body.is_empty() {
                    Some(format!("notify: {title}"))
                } else {
                    Some(format!("notify: {title} — {body}"))
                }
            }
            crate::store::Action::None => None,
        };
        Self {
            id: t.id.to_string(),
            name: t.name.clone(),
            enabled: t.enabled,
            occurrence: t.occurrence.clone(),
            past_status: CalendarStatus::Unknown,
            command,
            source_kind: "bellman".into(),
        }
    }

    /// Best-effort conversion from a Visible Scheduler discovery.
    ///
    /// Bellman rows need the store timer for a full [`Occurrence`]; this path
    /// only rebuilds cron / once-style expressions when possible.
    pub fn try_from_discovered(task: &DiscoveredTask) -> Option<Self> {
        let tz = task.timezone.clone().unwrap_or_else(|| "UTC".into());
        let occ = occurrence_from_discovered(task, &tz)?;
        let past_status = match &task.last_result {
            LastResult::Ok { .. } => CalendarStatus::Ok,
            LastResult::Failed { .. } => CalendarStatus::Failed,
            LastResult::Never | LastResult::Unknown => CalendarStatus::Unknown,
        };
        Some(Self {
            id: task.id.clone(),
            name: short_name(task),
            enabled: task.enabled,
            occurrence: occ,
            past_status,
            command: Some(task.command.clone()),
            source_kind: task.source_kind.as_str().to_string(),
        })
    }

    fn repeats(&self) -> bool {
        !matches!(self.occurrence.kind(), OccurrenceKind::Once { .. })
    }
}

fn short_name(task: &DiscoveredTask) -> String {
    // Prefer a compact label: basename of command, else schedule source.
    let cmd = task.command.trim();
    if cmd.is_empty() || cmd == "(no action)" {
        return task.source.clone();
    }
    // First token, basename.
    let first = cmd.split_whitespace().next().unwrap_or(cmd);
    let base = first.rsplit('/').next().unwrap_or(first);
    if base.len() > 28 {
        format!("{}…", &base[..27])
    } else {
        base.to_string()
    }
}

fn occurrence_from_discovered(task: &DiscoveredTask, tz: &str) -> Option<Occurrence> {
    // Try classic 5-field (or 6-field) cron in schedule_expr.
    let expr = task.schedule_expr.trim();
    if expr.split_whitespace().count() >= 5 && !expr.starts_with("once ") {
        // Reject known non-cron prefixes from bellman provider.
        if expr.starts_with("daily ")
            || expr.starts_with("weekly ")
            || expr.starts_with("monthly ")
            || expr.starts_with("yearly ")
            || expr.starts_with("every ")
        {
            return None;
        }
        // Special @reboot etc. — no expandable fires.
        if expr.starts_with('@') && expr.to_ascii_lowercase().contains("reboot") {
            return None;
        }
        if let Ok(occ) = Occurrence::new(
            OccurrenceKind::Cron {
                expr: expr.to_string(),
            },
            tz,
        ) {
            return Some(occ);
        }
    }
    None
}

/// Build expandable tasks from a Bellman store (primary day-one source).
pub fn tasks_from_store(store: &crate::store::Store) -> Result<Vec<ExpandableTask>, String> {
    let timers = store
        .list_timers()
        .map_err(|e| format!("list_timers: {e}"))?;
    Ok(timers.iter().map(ExpandableTask::from_timer).collect())
}

/// Build a full snapshot for the requested window.
pub fn build_snapshot(
    tasks: &[ExpandableTask],
    opts: &CalendarBuildOptions,
) -> Result<CalendarSnapshot, String> {
    if opts.to < opts.from {
        return Err(format!(
            "range end {} is before start {}",
            opts.to, opts.from
        ));
    }
    let tz: Tz = Tz::from_str(&opts.timezone)
        .map_err(|e| format!("unknown timezone '{}': {e}", opts.timezone))?;

    let mut warnings = Vec::new();
    let caps = opts.caps;

    // Expansion window: [start_of_from, start_of_day_after_to) in display tz,
    // converted to each task's local for preview, then fires mapped back.
    let range_start_local = opts
        .from
        .and_hms_opt(0, 0, 0)
        .ok_or("invalid from date")?
        .and_local_timezone(tz)
        .earliest()
        .or_else(|| {
            opts.from
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(tz)
                .latest()
        })
        .ok_or_else(|| format!("cannot resolve local midnight for {}", opts.from))?;
    // Cursor for "strictly after" must be just before range start.
    let after_cursor_utc = range_start_local.with_timezone(&Utc) - Duration::nanoseconds(1);

    let end_exclusive = opts.to + Duration::days(1);
    let range_end_local = end_exclusive
        .and_hms_opt(0, 0, 0)
        .ok_or("invalid to date")?
        .and_local_timezone(tz)
        .earliest()
        .or_else(|| {
            end_exclusive
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(tz)
                .latest()
        })
        .ok_or_else(|| format!("cannot resolve local end for {}", opts.to))?;
    let range_end_utc = range_end_local.with_timezone(&Utc);

    let mut all_entries: Vec<CalendarEntry> = Vec::new();
    let task_limit = tasks.len().min(caps.max_tasks);
    if tasks.len() > caps.max_tasks {
        warnings.push(format!(
            "capped tasks at {} (source had {})",
            caps.max_tasks,
            tasks.len()
        ));
    }

    for task in tasks.iter().take(task_limit) {
        if all_entries.len() >= caps.max_total_entries {
            warnings.push(format!(
                "capped total entries at {}",
                caps.max_total_entries
            ));
            break;
        }
        let remaining = caps.max_total_entries - all_entries.len();
        let fire_cap = caps.max_fires_per_task.min(remaining);
        let task_tz = task.occurrence.timezone();
        let after_local = after_cursor_utc.with_timezone(&task_tz);
        // Bound preview: for very short intervals, hard-cap.
        let fires = task.occurrence.preview(after_local, fire_cap);
        let mut added = 0usize;
        for fire in fires {
            let fire_utc = fire.with_timezone(&Utc);
            if fire_utc >= range_end_utc {
                break;
            }
            if fire_utc < range_start_local.with_timezone(&Utc) {
                continue;
            }
            let local = fire_utc.with_timezone(&tz);
            let date = local.date_naive();
            if date < opts.from || date > opts.to {
                continue;
            }
            let status = classify_status(task, fire_utc, opts.now_utc);
            let time = format!("{:02}:{:02}", local.hour(), local.minute());
            let time_secs = local.hour() * 3600 + local.minute() * 60 + local.second();
            all_entries.push(CalendarEntry {
                task_id: task.id.clone(),
                name: truncate_name(&task.name, 40),
                time,
                time_secs: Some(time_secs),
                date: date.format("%Y-%m-%d").to_string(),
                repeats: task.repeats(),
                status,
                command: if opts.show_commands {
                    task.command.clone()
                } else {
                    None
                },
                source_kind: task.source_kind.clone(),
            });
            added += 1;
            if added >= fire_cap {
                if fire_cap == caps.max_fires_per_task {
                    warnings.push(format!(
                        "task {} truncated at {} fires",
                        task.id, caps.max_fires_per_task
                    ));
                }
                break;
            }
        }
    }

    // Stable sort: date, time, name, task_id.
    all_entries.sort_by(|a, b| {
        (
            a.date.as_str(),
            a.time.as_str(),
            a.time_secs.unwrap_or(0),
            a.name.as_str(),
            a.task_id.as_str(),
        )
            .cmp(&(
                b.date.as_str(),
                b.time.as_str(),
                b.time_secs.unwrap_or(0),
                b.name.as_str(),
                b.task_id.as_str(),
            ))
    });

    let days = build_grid(
        opts.from,
        opts.to,
        opts.month,
        opts.week_start,
        &all_entries,
        caps.max_per_cell,
    );

    Ok(CalendarSnapshot {
        from: opts.from.format("%Y-%m-%d").to_string(),
        to: opts.to.format("%Y-%m-%d").to_string(),
        year: opts.month.map(|(y, _)| y),
        month: opts.month.map(|(_, m)| m),
        timezone: opts.timezone.clone(),
        week_start: opts.week_start,
        show_commands: opts.show_commands,
        days,
        entries: all_entries,
        caps,
        warnings,
    })
}

fn classify_status(
    task: &ExpandableTask,
    fire_utc: DateTime<Utc>,
    now_utc: DateTime<Utc>,
) -> CalendarStatus {
    if !task.enabled {
        return CalendarStatus::Disabled;
    }
    if fire_utc > now_utc {
        return CalendarStatus::Upcoming;
    }
    // Past fire — use last-result hint when available.
    task.past_status
}

fn truncate_name(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_string();
    }
    let mut s: String = name.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// Build the week-aligned grid of days for the visible range.
fn build_grid(
    from: NaiveDate,
    to: NaiveDate,
    month: Option<(i32, u32)>,
    week_start: WeekStart,
    entries: &[CalendarEntry],
    max_per_cell: usize,
) -> Vec<CalendarDay> {
    // For month view, pad to full weeks covering the month; for arbitrary
    // ranges, pad weeks that cover [from, to].
    let (grid_from, grid_to, in_month_check): (
        NaiveDate,
        NaiveDate,
        Box<dyn Fn(NaiveDate) -> bool>,
    ) = if let Some((y, m)) = month {
        let start = NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(from);
        let end = super::period::month_bounds(y, m)
            .map(|(_, e)| e)
            .unwrap_or(to);
        let gf = start_of_week(start, week_start);
        let gt = end_of_week(end, week_start);
        (gf, gt, Box::new(move |d| d.year() == y && d.month() == m))
    } else {
        let gf = start_of_week(from, week_start);
        let gt = end_of_week(to, week_start);
        let f = from;
        let t = to;
        (gf, gt, Box::new(move |d| d >= f && d <= t))
    };

    let mut by_date: BTreeMap<String, Vec<&CalendarEntry>> = BTreeMap::new();
    for e in entries {
        by_date.entry(e.date.clone()).or_default().push(e);
    }

    let mut days = Vec::new();
    let mut d = grid_from;
    while d <= grid_to {
        let key = d.format("%Y-%m-%d").to_string();
        let list = by_date.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
        let total = list.len();
        let (shown, overflow) = if total > max_per_cell {
            (&list[..max_per_cell], total - max_per_cell)
        } else {
            (list, 0)
        };
        days.push(CalendarDay {
            date: key,
            day: d.day(),
            in_month: in_month_check(d),
            entries: shown.iter().map(|e| (*e).clone()).collect(),
            overflow,
            total,
        });
        d += Duration::days(1);
    }
    days
}

fn start_of_week(date: NaiveDate, week_start: WeekStart) -> NaiveDate {
    let mon_based = date.weekday().num_days_from_monday();
    let col = week_start.column(mon_based);
    date - Duration::days(col as i64)
}

fn end_of_week(date: NaiveDate, week_start: WeekStart) -> NaiveDate {
    let mon_based = date.weekday().num_days_from_monday();
    let col = week_start.column(mon_based);
    date + Duration::days((6 - col) as i64)
}

/// Convenience: snapshot a month from store timers.
#[allow(clippy::too_many_arguments)] // thin wrapper over CalendarBuildOptions fields
pub fn snapshot_month_from_store(
    store: &crate::store::Store,
    year: i32,
    month: u32,
    timezone: &str,
    week_start: WeekStart,
    show_commands: bool,
    now_utc: DateTime<Utc>,
    caps: CalendarCaps,
) -> Result<CalendarSnapshot, String> {
    let (from, to) = super::period::month_bounds(year, month)?;
    let tasks = tasks_from_store(store)?;
    build_snapshot(
        &tasks,
        &CalendarBuildOptions {
            from,
            to,
            month: Some((year, month)),
            timezone: timezone.to_string(),
            week_start,
            show_commands,
            now_utc,
            caps,
        },
    )
}

#[cfg(test)]
mod grid_tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn august_2026_starts_saturday_mon_week() {
        // 2026-08-01 is Saturday. Mon-start week → grid starts 2026-07-27.
        let from = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        let days = build_grid(from, to, Some((2026, 8)), WeekStart::Mon, &[], 5);
        assert_eq!(days.first().unwrap().date, "2026-07-27");
        assert!(!days.first().unwrap().in_month);
        // Find Aug 1
        let aug1 = days.iter().find(|d| d.date == "2026-08-01").unwrap();
        assert!(aug1.in_month);
        assert_eq!(aug1.day, 1);
        // Six weeks: 31-day month starting Saturday with Mon week-start spans 6 weeks.
        assert_eq!(days.len(), 42);
        assert_eq!(days.last().unwrap().date, "2026-09-06");
    }

    #[test]
    fn month_starting_monday() {
        // 2026-06-01 is Monday.
        let from = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        let days = build_grid(from, to, Some((2026, 6)), WeekStart::Mon, &[], 5);
        assert_eq!(days.first().unwrap().date, "2026-06-01");
        assert!(days.first().unwrap().in_month);
    }

    #[test]
    fn month_starting_sunday_sun_week() {
        // 2026-02-01 is Sunday. Sun-start → grid starts on Feb 1.
        let from = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
        let days = build_grid(from, to, Some((2026, 2)), WeekStart::Sun, &[], 5);
        assert_eq!(days.first().unwrap().date, "2026-02-01");
        assert!(days.first().unwrap().in_month);
    }

    #[test]
    fn leap_february_2028() {
        let from = NaiveDate::from_ymd_opt(2028, 2, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2028, 2, 29).unwrap();
        let days = build_grid(from, to, Some((2028, 2)), WeekStart::Mon, &[], 5);
        assert!(days.iter().any(|d| d.date == "2028-02-29" && d.in_month));
        assert!(!days.iter().any(|d| d.date == "2028-02-30"));
    }
}
