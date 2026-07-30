//! Calendar truth model: past cells show only durable recorded outcomes;
//! future cells show schedule projections. Never fabricate past recurrence.
//!
//! Used by the GUI Week / Month views (via Tauri) so historical dates never
//! paint a projected fire as if it actually ran.

use super::build::ExpandableTask;
use super::types::CalendarCaps;
use crate::events::{EventKind, EventRecord};
use crate::store::{ClaimStatus, RunClaim};
use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

/// Whether an entry is durable history or a schedule projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthSource {
    /// Evidence from the run ledger and/or JSONL event history.
    Recorded,
    /// Future schedule projection (strictly after `now`).
    Upcoming,
}

impl TruthSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Upcoming => "upcoming",
        }
    }
}

/// Honest outcome label for a calendar entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeLabel {
    Delivered,
    Failed,
    Skipped,
    Late,
    Coalesced,
    /// Durable evidence a run finished exists (ledger), but no outcome event
    /// remains — never invent success from `ClaimStatus::Completed` alone.
    Unknown,
    Upcoming,
}

impl OutcomeLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Late => "late",
            Self::Coalesced => "coalesced",
            Self::Unknown => "unknown",
            Self::Upcoming => "upcoming",
        }
    }
}

/// One cell entry for Week / Month truth display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruthEntry {
    /// Timer id when known (hex UUID string).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub timer_id: Option<String>,
    /// Run claim id when known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub run_id: Option<String>,
    /// Display name — historical name from the event when present.
    pub name: String,
    /// Scheduled fire instant (UTC). For recorded entries without
    /// `scheduled_for` in the log, falls back to the event timestamp.
    pub scheduled_for: DateTime<Utc>,
    /// Local civil date `YYYY-MM-DD` in the display timezone.
    pub date: String,
    /// Local wall clock `HH:MM:SS` in the display timezone.
    pub time: String,
    /// Seconds past local midnight (stable sort).
    pub time_secs: u32,
    pub source: TruthSource,
    pub outcome: OutcomeLabel,
    /// Occurrence kind when known (`daily`, `weekly`, …).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub kind: Option<String>,
    /// Whether the live timer definition is currently enabled (if still present).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub enabled: Option<bool>,
}

/// Full truth window for a date range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruthWindow {
    pub from: String,
    pub to: String,
    pub timezone: String,
    pub now_utc: DateTime<Utc>,
    pub entries: Vec<TruthEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Options for [`build_truth_window`].
#[derive(Debug, Clone)]
pub struct TruthBuildOptions {
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// IANA timezone for civil date/time placement.
    pub timezone: String,
    /// Instant that splits recorded vs upcoming (injectable for tests).
    pub now_utc: DateTime<Utc>,
    pub caps: CalendarCaps,
}

/// Build the truth model for `[from, to]` inclusive in `timezone`.
///
/// * Instants **strictly before** `now_utc` → only recorded outcomes from
///   `events` (and completed `claims` only when they carry/match outcome
///   evidence). No schedule expansion into the past.
/// * Instants **strictly after** `now_utc` → schedule projections from
///   `tasks`. A projection is suppressed when a recorded entry already
///   covers the same `(timer_id, scheduled_for)` (or run_id).
/// * Names for recorded entries prefer the denormalized event `timer_name`
///   so renames/deletes do not rewrite history.
/// * Reconciliation: match by `run_id` first, then `(timer_id, scheduled
///   second)`. A completed claim never invents `delivered` — claim status
///   is completed on action failure too.
pub fn build_truth_window(
    tasks: &[ExpandableTask],
    events: &[EventRecord],
    claims: &[RunClaim],
    opts: &TruthBuildOptions,
) -> Result<TruthWindow, String> {
    if opts.to < opts.from {
        return Err(format!(
            "range end {} is before start {}",
            opts.to, opts.from
        ));
    }
    let tz: Tz = Tz::from_str(&opts.timezone)
        .map_err(|e| format!("unknown timezone '{}': {e}", opts.timezone))?;

    let mut warnings = Vec::new();
    let (range_start_utc, range_end_utc) = local_range_bounds(opts.from, opts.to, tz)?;

    // ── Recorded: merge outcome events + claims without double-counting ──
    // Primary map keyed by stable bucket id (`run:`… or `ts:`…).
    // Secondary indexes so claims and events find each other regardless of
    // which identity field each side carries.
    let mut buckets: HashMap<String, RecordBucket> = HashMap::new();
    let mut by_run: HashMap<String, String> = HashMap::new(); // run_id → bucket id
    let mut by_timer_sec: HashMap<(String, i64), String> = HashMap::new(); // (timer, sec) → bucket id

    for ev in events {
        if !is_outcome_event(ev.kind) {
            continue;
        }
        let when = ev.scheduled_for.unwrap_or(ev.logged_at);
        // Past-only for recorded display; also require it lands in the window.
        if when >= opts.now_utc {
            continue;
        }
        if when < range_start_utc || when >= range_end_utc {
            continue;
        }
        let bid = resolve_or_create_bucket(
            &mut buckets,
            &mut by_run,
            &mut by_timer_sec,
            ev.timer_id.map(|u| u.to_string()),
            ev.run_id.map(|u| u.to_string()),
            when,
        );
        let b = buckets.get_mut(&bid).expect("bucket just ensured");
        if b.run_id.is_none() {
            b.run_id = ev.run_id.map(|u| u.to_string());
        }
        if b.timer_id.is_none() {
            b.timer_id = ev.timer_id.map(|u| u.to_string());
        }
        if b.name.is_none() {
            if let Some(n) = ev.timer_name.as_ref().filter(|s| !s.is_empty()) {
                b.name = Some(n.clone());
            }
        }
        // Prefer the earliest scheduled_for when multiple events disagree.
        if when < b.scheduled_for {
            b.scheduled_for = when;
        }
        b.kinds.push(ev.kind);
        b.has_outcome_events = true;
    }

    // Claims enrich matching event buckets. A claim alone (no outcome event)
    // is NOT invented as delivered — scheduler/run-now complete claims even
    // when the action fails. Ledger-only rows surface as `unknown` so the UI
    // never rewrites failure as success after log rotation/pruning.
    for claim in claims {
        if claim.status != ClaimStatus::Completed {
            continue;
        }
        let when = claim.scheduled_for;
        if when >= opts.now_utc {
            continue;
        }
        if when < range_start_utc || when >= range_end_utc {
            continue;
        }
        let run_s = claim.run_id.to_string();
        let tid_s = claim.timer_id.to_string();
        let sec = when.timestamp();

        let existing = by_run
            .get(&run_s)
            .cloned()
            .or_else(|| by_timer_sec.get(&(tid_s.clone(), sec)).cloned());

        if let Some(bid) = existing {
            let b = buckets.get_mut(&bid).expect("indexed bucket");
            if b.run_id.is_none() {
                b.run_id = Some(run_s.clone());
                by_run.insert(run_s, bid.clone());
            }
            if b.timer_id.is_none() {
                b.timer_id = Some(tid_s.clone());
            }
            // Prefer claim's scheduled_for when more precise.
            if b.scheduled_for != when {
                // Keep event scheduled_for if set from events; only fill if needed.
            }
            by_timer_sec.insert((tid_s, sec), bid);
            continue;
        }

        // No matching event — durable claim without outcome detail.
        let bid = format!("run:{run_s}");
        buckets.insert(
            bid.clone(),
            RecordBucket {
                timer_id: Some(tid_s.clone()),
                run_id: Some(run_s.clone()),
                scheduled_for: when,
                name: None,
                kinds: Vec::new(),
                has_outcome_events: false,
            },
        );
        by_run.insert(run_s, bid.clone());
        by_timer_sec.insert((tid_s, sec), bid);
    }

    let mut entries: Vec<TruthEntry> = Vec::new();
    let mut recorded_keys: HashSet<DedupeKey> = HashSet::new();

    for b in buckets.into_values() {
        let outcome = outcome_from_bucket(&b);
        let local = b.scheduled_for.with_timezone(&tz);
        let date = local.date_naive();
        if date < opts.from || date > opts.to {
            continue;
        }
        let tid = b.timer_id.as_deref();
        // Recorded history must never rewrite from the live timer definition
        // (rename / edit recurrence / pause). Only immutable event fields are
        // allowed; otherwise stable id and null kind/enabled.
        let name = b.name.clone().unwrap_or_else(|| {
            tid.map(short_id)
                .unwrap_or_else(|| "(unknown)".into())
        });
        let kind = None;
        let enabled = None;
        let time = format!(
            "{:02}:{:02}:{:02}",
            local.hour(),
            local.minute(),
            local.second()
        );
        let time_secs = local.hour() * 3600 + local.minute() * 60 + local.second();
        if let Some(id) = tid {
            recorded_keys.insert(DedupeKey {
                timer_id: id.to_string(),
                scheduled_nanos: b.scheduled_for.timestamp_nanos_opt().unwrap_or(0),
            });
        }
        entries.push(TruthEntry {
            timer_id: b.timer_id,
            run_id: b.run_id,
            name,
            scheduled_for: b.scheduled_for,
            date: date.format("%Y-%m-%d").to_string(),
            time,
            time_secs,
            source: TruthSource::Recorded,
            outcome,
            kind,
            enabled,
        });
    }

    // ── Upcoming: project fires strictly after now within the window ──
    let after_cursor = opts.now_utc; // preview is strictly-after
    let task_limit = tasks.len().min(opts.caps.max_tasks);
    if tasks.len() > opts.caps.max_tasks {
        warnings.push(format!(
            "capped tasks at {} (source had {})",
            opts.caps.max_tasks,
            tasks.len()
        ));
    }

    for task in tasks.iter().take(task_limit) {
        if entries.len() >= opts.caps.max_total_entries {
            warnings.push(format!(
                "capped total entries at {}",
                opts.caps.max_total_entries
            ));
            break;
        }
        let remaining = opts.caps.max_total_entries - entries.len();
        let fire_cap = opts.caps.max_fires_per_task.min(remaining);
        let task_tz = task.occurrence.timezone();
        let after_local = after_cursor.with_timezone(&task_tz);
        let fires = task.occurrence.preview(after_local, fire_cap);
        let mut added = 0usize;
        for fire in fires {
            let fire_utc = fire.with_timezone(&Utc);
            if fire_utc <= opts.now_utc {
                continue;
            }
            if fire_utc >= range_end_utc {
                break;
            }
            if fire_utc < range_start_utc {
                continue;
            }
            let local = fire_utc.with_timezone(&tz);
            let date = local.date_naive();
            if date < opts.from || date > opts.to {
                continue;
            }
            // Suppress duplicate projection when a recorded run already covers it.
            let dk = DedupeKey {
                timer_id: task.id.clone(),
                scheduled_nanos: fire_utc.timestamp_nanos_opt().unwrap_or(0),
            };
            if recorded_keys.contains(&dk) {
                continue;
            }
            // Also suppress same second (ledger/event may truncate nanos).
            let same_second = recorded_keys.iter().any(|r| {
                r.timer_id == task.id
                    && (r.scheduled_nanos / 1_000_000_000) == (dk.scheduled_nanos / 1_000_000_000)
            });
            if same_second {
                continue;
            }

            let time = format!(
                "{:02}:{:02}:{:02}",
                local.hour(),
                local.minute(),
                local.second()
            );
            let time_secs = local.hour() * 3600 + local.minute() * 60 + local.second();
            entries.push(TruthEntry {
                timer_id: Some(task.id.clone()),
                run_id: None,
                name: task.name.clone(),
                scheduled_for: fire_utc,
                date: date.format("%Y-%m-%d").to_string(),
                time,
                time_secs,
                source: TruthSource::Upcoming,
                outcome: OutcomeLabel::Upcoming,
                kind: Some(occurrence_kind_str(&task.occurrence)),
                enabled: Some(task.enabled),
            });
            added += 1;
            if added >= fire_cap {
                if fire_cap == opts.caps.max_fires_per_task {
                    warnings.push(format!(
                        "task {} truncated at {} fires",
                        task.id, opts.caps.max_fires_per_task
                    ));
                }
                break;
            }
        }
    }

    entries.sort_by(|a, b| {
        (
            a.date.as_str(),
            a.time_secs,
            a.name.as_str(),
            a.timer_id.as_deref().unwrap_or(""),
            a.source.as_str(),
        )
            .cmp(&(
                b.date.as_str(),
                b.time_secs,
                b.name.as_str(),
                b.timer_id.as_deref().unwrap_or(""),
                b.source.as_str(),
            ))
    });

    Ok(TruthWindow {
        from: opts.from.format("%Y-%m-%d").to_string(),
        to: opts.to.format("%Y-%m-%d").to_string(),
        timezone: opts.timezone.clone(),
        now_utc: opts.now_utc,
        entries,
        warnings,
    })
}

// ── internals ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct RecordBucket {
    timer_id: Option<String>,
    run_id: Option<String>,
    scheduled_for: DateTime<Utc>,
    name: Option<String>,
    kinds: Vec<EventKind>,
    /// True when at least one JSONL outcome event contributed.
    has_outcome_events: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DedupeKey {
    timer_id: String,
    scheduled_nanos: i64,
}

/// Find an existing bucket by run_id then (timer_id, scheduled second), or create one.
fn resolve_or_create_bucket(
    buckets: &mut HashMap<String, RecordBucket>,
    by_run: &mut HashMap<String, String>,
    by_timer_sec: &mut HashMap<(String, i64), String>,
    timer_id: Option<String>,
    run_id: Option<String>,
    when: DateTime<Utc>,
) -> String {
    let sec = when.timestamp();

    if let Some(ref rid) = run_id {
        if let Some(bid) = by_run.get(rid) {
            // Keep secondary index warm.
            if let Some(ref tid) = timer_id {
                by_timer_sec.insert((tid.clone(), sec), bid.clone());
            }
            return bid.clone();
        }
    }
    if let Some(ref tid) = timer_id {
        if let Some(bid) = by_timer_sec.get(&(tid.clone(), sec)) {
            if let Some(ref rid) = run_id {
                by_run.insert(rid.clone(), bid.clone());
                // Promote: ensure bucket carries run_id.
                if let Some(b) = buckets.get_mut(bid) {
                    if b.run_id.is_none() {
                        b.run_id = Some(rid.clone());
                    }
                }
            }
            return bid.clone();
        }
    }

    let bid = if let Some(ref rid) = run_id {
        format!("run:{rid}")
    } else if let Some(ref tid) = timer_id {
        format!("ts:{tid}:{sec}")
    } else {
        format!(
            "anon:{}",
            when.timestamp_nanos_opt().unwrap_or(when.timestamp())
        )
    };

    buckets.insert(
        bid.clone(),
        RecordBucket {
            timer_id: timer_id.clone(),
            run_id: run_id.clone(),
            scheduled_for: when,
            name: None,
            kinds: Vec::new(),
            has_outcome_events: false,
        },
    );
    if let Some(rid) = run_id {
        by_run.insert(rid, bid.clone());
    }
    if let Some(tid) = timer_id {
        by_timer_sec.insert((tid, sec), bid.clone());
    }
    bid
}

fn is_outcome_event(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Fired
            | EventKind::FiredLate
            | EventKind::SkippedMisfire
            | EventKind::Coalesced
            | EventKind::WakeDelivered
            | EventKind::WakeFailed
            | EventKind::NoAck
    )
}

/// Honest label: event kinds win; claim-only → unknown (never invent delivered).
fn outcome_from_bucket(b: &RecordBucket) -> OutcomeLabel {
    if !b.has_outcome_events || b.kinds.is_empty() {
        // Completed claim is not success — delivery path completes on failure too.
        return OutcomeLabel::Unknown;
    }
    outcome_from_kinds(&b.kinds)
}

fn outcome_from_kinds(kinds: &[EventKind]) -> OutcomeLabel {
    if kinds
        .iter()
        .any(|k| matches!(k, EventKind::WakeFailed | EventKind::NoAck))
    {
        return OutcomeLabel::Failed;
    }
    if kinds.iter().any(|k| matches!(k, EventKind::SkippedMisfire)) {
        return OutcomeLabel::Skipped;
    }
    if kinds.iter().any(|k| matches!(k, EventKind::Coalesced)) {
        return OutcomeLabel::Coalesced;
    }
    if kinds.iter().any(|k| matches!(k, EventKind::FiredLate)) {
        return OutcomeLabel::Late;
    }
    // Fired / WakeDelivered / CatchUp-as-Fired
    OutcomeLabel::Delivered
}

fn local_range_bounds(
    from: NaiveDate,
    to: NaiveDate,
    tz: Tz,
) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let start_local = from
        .and_hms_opt(0, 0, 0)
        .ok_or("invalid from date")?
        .and_local_timezone(tz)
        .earliest()
        .or_else(|| {
            from.and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(tz)
                .latest()
        })
        .ok_or_else(|| format!("cannot resolve local midnight for {from}"))?;
    let end_exclusive = to + Duration::days(1);
    let end_local = end_exclusive
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
        .ok_or_else(|| format!("cannot resolve local end for {to}"))?;
    Ok((
        start_local.with_timezone(&Utc),
        end_local.with_timezone(&Utc),
    ))
}

fn short_id(id: &str) -> String {
    if id.len() > 8 {
        format!("{}…", &id[..8])
    } else {
        id.to_string()
    }
}

fn occurrence_kind_str(occ: &crate::occurrence::Occurrence) -> String {
    use crate::occurrence::OccurrenceKind;
    match occ.kind() {
        OccurrenceKind::Once { .. } => "once".into(),
        OccurrenceKind::Interval { .. } => "interval".into(),
        OccurrenceKind::Daily { .. } => "daily".into(),
        OccurrenceKind::Weekly { .. } => "weekly".into(),
        OccurrenceKind::Monthly { .. } => "monthly".into(),
        OccurrenceKind::Yearly { .. } => "yearly".into(),
        OccurrenceKind::Cron { .. } => "cron".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::types::{CalendarCaps, CalendarStatus};
    use crate::occurrence::{Occurrence, OccurrenceKind};
    use crate::store::{ClaimStatus, RunClaim};
    use chrono::{NaiveTime, TimeZone};
    use uuid::Uuid;

    fn daily(name: &str, hour: u32, min: u32, tz: &str) -> ExpandableTask {
        let at = NaiveTime::from_hms_opt(hour, min, 0).unwrap();
        let occ = Occurrence::new(OccurrenceKind::Daily { at }, tz).unwrap();
        ExpandableTask {
            id: format!("11111111-1111-1111-1111-11111111111{}", name.len() % 10),
            name: name.into(),
            enabled: true,
            occurrence: occ,
            past_status: CalendarStatus::Unknown,
            command: None,
            source_kind: "bellman".into(),
        }
    }

    fn daily_with_id(id: &str, name: &str, hour: u32, min: u32, tz: &str) -> ExpandableTask {
        let mut t = daily(name, hour, min, tz);
        t.id = id.into();
        t
    }

    fn opts(from: &str, to: &str, now: DateTime<Utc>, tz: &str) -> TruthBuildOptions {
        TruthBuildOptions {
            from: NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap(),
            to: NaiveDate::parse_from_str(to, "%Y-%m-%d").unwrap(),
            timezone: tz.into(),
            now_utc: now,
            caps: CalendarCaps::default(),
        }
    }

    #[test]
    fn empty_past_history_shows_nothing() {
        let task = daily_with_id(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "morning",
            9,
            0,
            "UTC",
        );
        // Looking at last week with no events — must not paint daily recurrence.
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[task],
            &[],
            &[],
            &opts("2026-07-20", "2026-07-26", now, "UTC"),
        )
        .unwrap();
        assert!(
            win.entries.is_empty(),
            "past week with no records must be empty, got {:?}",
            win.entries
        );
    }

    #[test]
    fn successful_and_failed_recorded_runs() {
        let tid = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
        let run_ok = Uuid::new_v4();
        let run_fail = Uuid::new_v4();
        let sched_ok = Utc.with_ymd_and_hms(2026, 7, 28, 9, 0, 0).unwrap();
        let sched_fail = Utc.with_ymd_and_hms(2026, 7, 27, 9, 0, 0).unwrap();
        let events = vec![
            EventRecord::new(EventKind::Fired)
                .with_timer(tid, "Old Name")
                .with_run(run_ok)
                .with_scheduled_for(sched_ok)
                .with_logged_at(sched_ok),
            EventRecord::new(EventKind::WakeDelivered)
                .with_timer(tid, "Old Name")
                .with_run(run_ok)
                .with_scheduled_for(sched_ok)
                .with_logged_at(sched_ok + Duration::seconds(1)),
            EventRecord::new(EventKind::Fired)
                .with_timer(tid, "Old Name")
                .with_run(run_fail)
                .with_scheduled_for(sched_fail)
                .with_logged_at(sched_fail),
            EventRecord::new(EventKind::WakeFailed)
                .with_timer(tid, "Old Name")
                .with_run(run_fail)
                .with_scheduled_for(sched_fail)
                .with_logged_at(sched_fail + Duration::seconds(2))
                .with_error("launch failed"),
        ];
        // Live timer was renamed.
        let task = daily_with_id(&tid.to_string(), "New Name", 9, 0, "UTC");
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[task],
            &events,
            &[],
            &opts("2026-07-27", "2026-07-29", now, "UTC"),
        )
        .unwrap();
        let recorded: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.source == TruthSource::Recorded)
            .collect();
        assert_eq!(recorded.len(), 2);
        let fail = recorded
            .iter()
            .find(|e| e.outcome == OutcomeLabel::Failed)
            .unwrap();
        let ok = recorded
            .iter()
            .find(|e| e.outcome == OutcomeLabel::Delivered)
            .unwrap();
        assert_eq!(fail.name, "Old Name");
        assert_eq!(ok.name, "Old Name");
        assert_eq!(fail.date, "2026-07-27");
        assert_eq!(ok.date, "2026-07-28");
    }

    #[test]
    fn pruned_history_does_not_fabricate_recurrence() {
        // Events only for one day; other past days stay empty despite daily timer.
        let tid = Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();
        let task = daily_with_id(&tid.to_string(), "daily", 8, 0, "UTC");
        let sched = Utc.with_ymd_and_hms(2026, 7, 25, 8, 0, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::Fired)
            .with_timer(tid, "daily")
            .with_run(Uuid::new_v4())
            .with_scheduled_for(sched)
            .with_logged_at(sched)];
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[task],
            &events,
            &[],
            &opts("2026-07-20", "2026-07-28", now, "UTC"),
        )
        .unwrap();
        let past: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.source == TruthSource::Recorded)
            .collect();
        assert_eq!(past.len(), 1);
        assert_eq!(past[0].date, "2026-07-25");
        // No projected fills for 20–24, 26–28.
        assert!(win
            .entries
            .iter()
            .all(|e| e.source != TruthSource::Upcoming || e.date.as_str() > "2026-07-28"));
    }

    #[test]
    fn duplicate_suppression_recorded_hides_projection() {
        let tid = Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap();
        // now is just before a scheduled fire that was already recorded as late recovery.
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 8, 30, 0).unwrap();
        // A fire scheduled for 09:00 that somehow was already logged? Use past fire at 08:00.
        let sched = Utc.with_ymd_and_hms(2026, 7, 29, 8, 0, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::FiredLate)
            .with_timer(tid, "late-one")
            .with_run(Uuid::new_v4())
            .with_scheduled_for(sched)
            .with_logged_at(sched + Duration::minutes(20))];
        let task = daily_with_id(&tid.to_string(), "late-one", 8, 0, "UTC");
        let win = build_truth_window(
            &[task],
            &events,
            &[],
            &opts("2026-07-29", "2026-07-29", now, "UTC"),
        )
        .unwrap();
        // Recorded at 08:00 late; no second upcoming chip for same scheduled_for.
        let for_8: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.time.starts_with("08:00"))
            .collect();
        assert_eq!(for_8.len(), 1);
        assert_eq!(for_8[0].source, TruthSource::Recorded);
        assert_eq!(for_8[0].outcome, OutcomeLabel::Late);
        // Future 08:00 tomorrow may appear as upcoming if in range — only today in range.
    }

    #[test]
    fn current_day_past_future_split() {
        let tid = Uuid::parse_str("eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee").unwrap();
        let task = daily_with_id(&tid.to_string(), "split", 9, 0, "UTC");
        // Now is noon; morning fire has a record; afternoon is none for daily 09:00.
        // Daily 09:00: past today 09:00 needs record; future is tomorrow 09:00.
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let morning = Utc.with_ymd_and_hms(2026, 7, 29, 9, 0, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::Fired)
            .with_timer(tid, "split")
            .with_run(Uuid::new_v4())
            .with_scheduled_for(morning)
            .with_logged_at(morning)];
        let win = build_truth_window(
            &[task],
            &events,
            &[],
            &opts("2026-07-29", "2026-07-30", now, "UTC"),
        )
        .unwrap();
        let today_rec: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.date == "2026-07-29" && e.source == TruthSource::Recorded)
            .collect();
        assert_eq!(today_rec.len(), 1);
        let today_up: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.date == "2026-07-29" && e.source == TruthSource::Upcoming)
            .collect();
        // No upcoming on today after noon for a 09:00 daily (next is tomorrow).
        assert!(today_up.is_empty());
        let tom: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.date == "2026-07-30" && e.source == TruthSource::Upcoming)
            .collect();
        assert_eq!(tom.len(), 1);
        assert_eq!(tom[0].outcome, OutcomeLabel::Upcoming);
    }

    #[test]
    fn edited_recurrence_future_uses_new_definition() {
        let tid = Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();
        // Historical fire at 09:00; live timer now fires at 15:00.
        let morning = Utc.with_ymd_and_hms(2026, 7, 28, 9, 0, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::Fired)
            .with_timer(tid, "edited")
            .with_run(Uuid::new_v4())
            .with_scheduled_for(morning)
            .with_logged_at(morning)];
        let task = daily_with_id(&tid.to_string(), "edited", 15, 0, "UTC");
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[task],
            &events,
            &[],
            &opts("2026-07-28", "2026-07-30", now, "UTC"),
        )
        .unwrap();
        let past = win
            .entries
            .iter()
            .find(|e| e.source == TruthSource::Recorded)
            .unwrap();
        assert_eq!(past.time, "09:00:00");
        let future: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.source == TruthSource::Upcoming)
            .collect();
        assert!(future.iter().all(|e| e.time.starts_with("15:00")));
        assert!(future.iter().all(|e| e.date.as_str() >= "2026-07-29"));
    }

    #[test]
    fn deleted_timer_history_keeps_recorded_name() {
        let tid = Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap();
        let sched = Utc.with_ymd_and_hms(2026, 7, 22, 7, 30, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::Coalesced)
            .with_timer(tid, "gone-timer")
            .with_run(Uuid::new_v4())
            .with_scheduled_for(sched)
            .with_logged_at(sched)
            .with_count(3)];
        // No live tasks — timer was deleted.
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[],
            &events,
            &[],
            &opts("2026-07-20", "2026-07-28", now, "UTC"),
        )
        .unwrap();
        assert_eq!(win.entries.len(), 1);
        assert_eq!(win.entries[0].name, "gone-timer");
        assert_eq!(win.entries[0].outcome, OutcomeLabel::Coalesced);
        assert_eq!(win.entries[0].source, TruthSource::Recorded);
    }

    #[test]
    fn claim_ledger_alone_is_unknown_never_delivered() {
        // Scheduler/run-now complete claims even when the action fails — so a
        // completed claim without outcome events must not invent "delivered".
        let tid = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let run_id = Uuid::new_v4();
        let sched = Utc.with_ymd_and_hms(2026, 7, 21, 10, 0, 0).unwrap();
        let claims = vec![RunClaim {
            run_id,
            timer_id: tid,
            scheduled_for: sched,
            status: ClaimStatus::Completed,
            claimed_at: sched,
            completed_at: Some(sched + Duration::seconds(1)),
            event_sequence: 1,
        }];
        let task = daily_with_id(&tid.to_string(), "from-ledger", 10, 0, "UTC");
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[task],
            &[], // events pruned / rotated away
            &claims,
            &opts("2026-07-20", "2026-07-22", now, "UTC"),
        )
        .unwrap();
        assert_eq!(win.entries.len(), 1);
        assert_eq!(win.entries[0].source, TruthSource::Recorded);
        assert_eq!(win.entries[0].outcome, OutcomeLabel::Unknown);
        assert_ne!(win.entries[0].outcome, OutcomeLabel::Delivered);
        // No event name → stable id prefix, never live "from-ledger".
        assert_eq!(win.entries[0].name, short_id(&tid.to_string()));
        assert!(win.entries[0].kind.is_none());
        assert!(win.entries[0].enabled.is_none());
    }

    /// Renamed / edited / paused live timer must not rewrite claim-only history.
    #[test]
    fn renamed_edited_paused_after_prune_does_not_rewrite_history() {
        let tid = Uuid::parse_str("bbbbbbbb-cccc-dddd-eeee-ffffffffffff").unwrap();
        let run_id = Uuid::new_v4();
        let sched = Utc.with_ymd_and_hms(2026, 7, 21, 10, 0, 0).unwrap();
        let claims = vec![RunClaim {
            run_id,
            timer_id: tid,
            scheduled_for: sched,
            status: ClaimStatus::Completed,
            claimed_at: sched,
            completed_at: Some(sched + Duration::seconds(1)),
            event_sequence: 1,
        }];
        // Live definition was renamed, switched to interval, and paused.
        let mut task = daily_with_id(&tid.to_string(), "NEW CURRENT NAME", 10, 0, "UTC");
        task.occurrence = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 60,
                anchor: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            },
            "UTC",
        )
        .unwrap();
        task.enabled = false;
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[task],
            &[], // events pruned
            &claims,
            &opts("2026-07-20", "2026-07-22", now, "UTC"),
        )
        .unwrap();
        assert_eq!(win.entries.len(), 1);
        let e = &win.entries[0];
        assert_eq!(e.outcome, OutcomeLabel::Unknown);
        assert_ne!(e.name, "NEW CURRENT NAME");
        assert_eq!(e.name, short_id(&tid.to_string()));
        assert!(
            e.kind.is_none(),
            "must not copy live kind, got {:?}",
            e.kind
        );
        assert!(
            e.enabled.is_none(),
            "must not copy live enabled/paused, got {:?}",
            e.enabled
        );
    }

    #[test]
    fn event_name_survives_live_rename() {
        let tid = Uuid::parse_str("cccccccc-dddd-eeee-ffff-000000000001").unwrap();
        let run_id = Uuid::new_v4();
        let sched = Utc.with_ymd_and_hms(2026, 7, 22, 8, 0, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::Fired)
            .with_timer(tid, "Historical Name")
            .with_run(run_id)
            .with_scheduled_for(sched)
            .with_logged_at(sched)];
        let mut task = daily_with_id(&tid.to_string(), "NEW CURRENT NAME", 8, 0, "UTC");
        task.enabled = false;
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[task],
            &events,
            &[],
            &opts("2026-07-20", "2026-07-28", now, "UTC"),
        )
        .unwrap();
        let rec = win
            .entries
            .iter()
            .find(|e| e.source == TruthSource::Recorded)
            .unwrap();
        assert_eq!(rec.name, "Historical Name");
        assert!(rec.kind.is_none());
        assert!(rec.enabled.is_none());
    }

    #[test]
    fn browse_past_current_future_month() {
        let tid = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0001").unwrap();
        let task = daily_with_id(&tid.to_string(), "month-browse", 11, 0, "UTC");
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();

        // Past month (June): no records → empty.
        let past = build_truth_window(
            std::slice::from_ref(&task),
            &[],
            &[],
            &opts("2026-06-01", "2026-06-30", now, "UTC"),
        )
        .unwrap();
        assert!(past.entries.is_empty());

        // Current month: one recorded + upcoming after now.
        let rec_sched = Utc.with_ymd_and_hms(2026, 7, 10, 11, 0, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::Fired)
            .with_timer(tid, "month-browse")
            .with_run(Uuid::new_v4())
            .with_scheduled_for(rec_sched)
            .with_logged_at(rec_sched)];
        let cur = build_truth_window(
            std::slice::from_ref(&task),
            &events,
            &[],
            &opts("2026-07-01", "2026-07-31", now, "UTC"),
        )
        .unwrap();
        assert_eq!(
            cur.entries
                .iter()
                .filter(|e| e.source == TruthSource::Recorded)
                .count(),
            1
        );
        assert!(cur
            .entries
            .iter()
            .filter(|e| e.source == TruthSource::Upcoming)
            .all(|e| e.scheduled_for > now));

        // Future month (August): only projections.
        let fut = build_truth_window(
            &[task],
            &[],
            &[],
            &opts("2026-08-01", "2026-08-31", now, "UTC"),
        )
        .unwrap();
        assert!(!fut.entries.is_empty());
        assert!(fut
            .entries
            .iter()
            .all(|e| e.source == TruthSource::Upcoming));
    }

    /// Auditor FINDING 1 repro: matching WakeFailed event + Completed claim
    /// must collapse to a single Recorded/failed entry (not failed+delivered).
    #[test]
    fn event_and_claim_same_run_collapse_to_one_failed() {
        let tid = Uuid::parse_str("aaaaaaaa-aaaa-bbbb-bbbb-cccccccccccc").unwrap();
        let run_id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let sched = Utc.with_ymd_and_hms(2026, 7, 28, 15, 18, 11).unwrap();
        let events = vec![EventRecord::new(EventKind::WakeFailed)
            .with_timer(tid, "audit-duplicate")
            .with_run(run_id)
            .with_scheduled_for(sched)
            .with_logged_at(sched)
            .with_error("launch failed")];
        let claims = vec![RunClaim {
            run_id,
            timer_id: tid,
            scheduled_for: sched,
            status: ClaimStatus::Completed,
            claimed_at: sched,
            completed_at: Some(sched + Duration::seconds(1)),
            event_sequence: 1,
        }];
        let task = daily_with_id(&tid.to_string(), "audit-duplicate", 15, 18, "UTC");
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[task],
            &events,
            &claims,
            &opts("2026-07-27", "2026-07-29", now, "UTC"),
        )
        .unwrap();
        let recorded: Vec<_> = win
            .entries
            .iter()
            .filter(|e| e.source == TruthSource::Recorded)
            .collect();
        assert_eq!(
            recorded.len(),
            1,
            "expected one recorded entry, got {:?}",
            recorded
        );
        assert_eq!(recorded[0].outcome, OutcomeLabel::Failed);
        assert_eq!(recorded[0].name, "audit-duplicate");
        let run_s = run_id.to_string();
        assert_eq!(recorded[0].run_id.as_deref(), Some(run_s.as_str()));
    }

    #[test]
    fn event_and_claim_match_by_timer_and_scheduled_second() {
        // Event lacks run_id; claim has run_id — still one bucket via timer+sec.
        let tid = Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap();
        let run_id = Uuid::new_v4();
        let sched = Utc.with_ymd_and_hms(2026, 7, 28, 9, 0, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::Fired)
            .with_timer(tid, "no-run-id-ev")
            .with_scheduled_for(sched)
            .with_logged_at(sched)];
        // No .with_run on the event.
        let claims = vec![RunClaim {
            run_id,
            timer_id: tid,
            scheduled_for: sched,
            status: ClaimStatus::Completed,
            claimed_at: sched,
            completed_at: Some(sched + Duration::seconds(1)),
            event_sequence: 2,
        }];
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let win = build_truth_window(
            &[],
            &events,
            &claims,
            &opts("2026-07-28", "2026-07-28", now, "UTC"),
        )
        .unwrap();
        assert_eq!(win.entries.len(), 1);
        assert_eq!(win.entries[0].outcome, OutcomeLabel::Delivered);
        let run_s = run_id.to_string();
        assert_eq!(win.entries[0].run_id.as_deref(), Some(run_s.as_str()));
    }

    #[test]
    fn timezone_and_dst_spring_forward_helsinki() {
        // Europe/Helsinki 2026-03-29 spring forward: 03:00 → 04:00.
        // A daily at 02:30 still exists; 03:30 is in the gap (skipped or shifted
        // per policy — we only assert civil date placement for a fire that lands).
        let tid = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa01";
        let at = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        let occ = Occurrence::new(OccurrenceKind::Daily { at }, "Europe/Helsinki").unwrap();
        let task = ExpandableTask {
            id: tid.into(),
            name: "hel".into(),
            enabled: true,
            occurrence: occ,
            past_status: CalendarStatus::Unknown,
            command: None,
            source_kind: "bellman".into(),
        };
        // Now just before noon local on March 30.
        let now = Utc.with_ymd_and_hms(2026, 3, 30, 8, 0, 0).unwrap(); // 10:00 EEST
        let win = build_truth_window(
            &[task],
            &[],
            &[],
            &opts("2026-03-29", "2026-03-31", now, "Europe/Helsinki"),
        )
        .unwrap();
        // All upcoming; none in the past without records.
        assert!(win
            .entries
            .iter()
            .all(|e| e.source == TruthSource::Upcoming));
        // Fires after now should land on local dates 30 or 31.
        for e in &win.entries {
            assert!(e.date == "2026-03-30" || e.date == "2026-03-31");
            assert_eq!(e.time, "12:00:00");
        }
    }

    #[test]
    fn browse_past_current_future_week() {
        let tid = Uuid::parse_str("99999999-9999-9999-9999-999999999999").unwrap();
        let task = daily_with_id(&tid.to_string(), "browse", 11, 0, "UTC");
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap(); // Wed

        // Past week Mon–Sun 20–26: empty without records.
        let past = build_truth_window(
            std::slice::from_ref(&task),
            &[],
            &[],
            &opts("2026-07-20", "2026-07-26", now, "UTC"),
        )
        .unwrap();
        assert!(past.entries.is_empty());

        // Current week 27–02 Aug: only upcoming after now + any records.
        let rec_sched = Utc.with_ymd_and_hms(2026, 7, 28, 11, 0, 0).unwrap();
        let events = vec![EventRecord::new(EventKind::SkippedMisfire)
            .with_timer(tid, "browse")
            .with_run(Uuid::new_v4())
            .with_scheduled_for(rec_sched)
            .with_logged_at(rec_sched)];
        let cur = build_truth_window(
            std::slice::from_ref(&task),
            &events,
            &[],
            &opts("2026-07-27", "2026-08-02", now, "UTC"),
        )
        .unwrap();
        let rec: Vec<_> = cur
            .entries
            .iter()
            .filter(|e| e.source == TruthSource::Recorded)
            .collect();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].outcome, OutcomeLabel::Skipped);
        assert!(cur
            .entries
            .iter()
            .filter(|e| e.source == TruthSource::Upcoming)
            .all(|e| e.scheduled_for > now));

        // Future week: only projections.
        let fut = build_truth_window(
            &[task],
            &[],
            &[],
            &opts("2026-08-03", "2026-08-09", now, "UTC"),
        )
        .unwrap();
        assert!(!fut.entries.is_empty());
        assert!(fut
            .entries
            .iter()
            .all(|e| e.source == TruthSource::Upcoming));
    }
}
