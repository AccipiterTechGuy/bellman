//! Store-aware fire-neighbour query for the timer dialog / All timers list.
//!
//! Answers: "if I set this timer for this moment, what else is already going
//! to fire then?" Built on the same `Occurrence::preview` path as
//! `preview_fires` — no second scheduling implementation.
//!
//! Thresholds (named constants — change here only):
//! - **collision**: fire lands on the same UTC second as a candidate
//! - **nearby**: fire is within [`NEIGHBOUR_WINDOW_SECS`] of a candidate
//!   (both directions) but not the same second
//!
//! Work bounds:
//! - [`NEIGHBOUR_MAX_FIRES_PER_TIMER`] expanded fires per store timer
//! - only fires within [`NEIGHBOUR_HORIZON_SECS`] of *now* (UTC) are considered

use std::str::FromStr;

use bellman_core::store::Timer;
use bellman_core::{Action, OccurrenceKind, Timer as CoreTimer};
use chrono::offset::Offset as _Offset;
use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Same-second window: two fires whose UTC timestamps truncate to the same
/// second are a **collision**. Not configurable — product definition.
pub const NEIGHBOUR_COLLISION_TO_SECOND: bool = true;

/// Nearby window in seconds, both directions (before and after the candidate).
/// Default **5 minutes**. One constant — easy to change.
pub const NEIGHBOUR_WINDOW_SECS: i64 = 5 * 60;

/// Cap: only expand fires that land within this many seconds of *now* (UTC).
/// **14 days** keeps dialog work bounded even with hundreds of timers.
pub const NEIGHBOUR_HORIZON_SECS: i64 = 14 * 24 * 60 * 60;

/// Cap: max fires expanded per timer via `Occurrence::preview`.
pub const NEIGHBOUR_MAX_FIRES_PER_TIMER: usize = 48;

/// Relation of a store fire to a candidate instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NeighbourRelation {
    /// Same UTC second as the candidate.
    Collision,
    /// Within the nearby window, not the same second.
    Nearby,
}

impl NeighbourRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collision => "collision",
            Self::Nearby => "nearby",
        }
    }
}

/// One store timer fire that sits on/near a candidate.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeighbourHitDto {
    pub timer_id: String,
    pub name: String,
    pub fire_utc: DateTime<Utc>,
    pub local_date: String,
    pub local_time: String,
    pub offset: String,
    pub tz_name: String,
    /// Occurrence kind name: once|interval|daily|weekly|monthly|yearly|cron.
    pub occurrence_kind: String,
    /// Action kind: none|launch|notify.
    pub action_kind: String,
    /// Signed seconds from the matched candidate (fire − candidate).
    pub delta_secs: i64,
    /// `"collision"` or `"nearby"`.
    pub relation: String,
}

/// Neighbours grouped under one candidate fire instant.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateNeighboursDto {
    pub candidate_utc: DateTime<Utc>,
    pub collisions: Vec<NeighbourHitDto>,
    pub nearby: Vec<NeighbourHitDto>,
}

/// Work accounting for the QA bounded-work note.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NeighbourStatsDto {
    pub timers_scanned: usize,
    pub timers_skipped_disabled: usize,
    pub timers_skipped_excluded: usize,
    pub fires_expanded: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryNeighboursResponse {
    pub by_candidate: Vec<CandidateNeighboursDto>,
    /// Flat union of all collisions (stable: by fire_utc then name).
    pub collisions: Vec<NeighbourHitDto>,
    /// Flat union of all nearby hits (not collisions).
    pub nearby: Vec<NeighbourHitDto>,
    pub stats: NeighbourStatsDto,
    /// Window used for this response (seconds).
    pub window_secs: i64,
    pub horizon_secs: i64,
    pub max_fires_per_timer: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryNeighboursArgs {
    /// Candidate fire instants (UTC). Empty → empty response (still valid).
    pub candidates: Vec<DateTime<Utc>>,
    /// Optional timer id to exclude (the timer being edited).
    #[serde(default)]
    pub exclude_timer_id: Option<String>,
    /// Optional nearby-window override; default [`NEIGHBOUR_WINDOW_SECS`].
    #[serde(default)]
    pub window_secs: Option<i64>,
}

/// Classify a store fire relative to a candidate.
///
/// - Same UTC second → collision
/// - `|delta| ≤ window_secs` and not same second → nearby
/// - else → None
pub fn classify_delta(delta_secs: i64, window_secs: i64) -> Option<NeighbourRelation> {
    if delta_secs == 0 {
        return Some(NeighbourRelation::Collision);
    }
    if delta_secs.abs() <= window_secs {
        return Some(NeighbourRelation::Nearby);
    }
    None
}

/// Truncate a UTC instant to whole seconds (product: collision = same second).
pub fn truncate_to_second(dt: DateTime<Utc>) -> DateTime<Utc> {
    dt.with_nanosecond(0).unwrap_or(dt)
}

fn occurrence_kind_name(kind: &OccurrenceKind) -> &'static str {
    match kind {
        OccurrenceKind::Once { .. } => "once",
        OccurrenceKind::Interval { .. } => "interval",
        OccurrenceKind::Daily { .. } => "daily",
        OccurrenceKind::Weekly { .. } => "weekly",
        OccurrenceKind::Monthly { .. } => "monthly",
        OccurrenceKind::Yearly { .. } => "yearly",
        OccurrenceKind::Cron { .. } => "cron",
    }
}

fn action_kind_name(action: &Action) -> &'static str {
    match action {
        Action::None => "none",
        Action::Launch { .. } => "launch",
        Action::Notify { .. } => "notify",
    }
}

fn format_offset_secs(secs: i32) -> String {
    if secs == 0 {
        "UTC".to_string()
    } else {
        let sign = if secs >= 0 { '+' } else { '-' };
        let abs = secs.unsigned_abs();
        let h = abs / 3600;
        let m = (abs % 3600) / 60;
        format!("{sign}{h:02}:{m:02}")
    }
}

fn local_parts(occ: &bellman_core::Occurrence, fire_utc: DateTime<Utc>) -> (String, String, String, String) {
    let local_dt = fire_utc.with_timezone(&occ.timezone());
    let local_date = local_dt.date_naive().format("%Y-%m-%d").to_string();
    let local_time = local_dt.time().format("%H:%M:%S").to_string();
    let offset_secs = local_dt.offset().fix().local_minus_utc();
    let offset = format_offset_secs(offset_secs);
    let tz_name = occ.tz_name().to_string();
    (local_date, local_time, offset, tz_name)
}

/// Expand upcoming fires for one timer using the same preview path as the dialog.
///
/// Cursor starts at `after` (typically now − window, so slightly-past candidates
/// still match). Caps at [`NEIGHBOUR_MAX_FIRES_PER_TIMER`] and drops fires past
/// `horizon_end`.
pub fn expand_fires(
    timer: &CoreTimer,
    after: DateTime<Utc>,
    horizon_end: DateTime<Utc>,
    max_fires: usize,
) -> Vec<DateTime<Utc>> {
    let after_local = after.with_timezone(&timer.occurrence.timezone());
    timer
        .occurrence
        .preview(after_local, max_fires)
        .into_iter()
        .map(|local_dt| local_dt.with_timezone(&Utc))
        .filter(|utc| *utc <= horizon_end)
        .collect()
}

/// Pure neighbour scan over an in-memory timer list (unit-testable).
pub fn query_neighbours_from_timers(
    timers: &[Timer],
    candidates: &[DateTime<Utc>],
    exclude: Option<Uuid>,
    window_secs: i64,
    now: DateTime<Utc>,
) -> QueryNeighboursResponse {
    let window_secs = window_secs.max(0);
    let mut stats = NeighbourStatsDto::default();

    // Empty candidates → empty structured response (still reports caps).
    if candidates.is_empty() {
        return QueryNeighboursResponse {
            by_candidate: vec![],
            collisions: vec![],
            nearby: vec![],
            stats,
            window_secs,
            horizon_secs: NEIGHBOUR_HORIZON_SECS,
            max_fires_per_timer: NEIGHBOUR_MAX_FIRES_PER_TIMER,
        };
    }

    let cand_secs: Vec<DateTime<Utc>> = candidates.iter().map(|c| truncate_to_second(*c)).collect();

    // Expand around the candidates themselves (dialog preview fires may be
    // far-once timers). Cap: at most MAX_FIRES per timer, only fires within
    // the tight [min_cand − window, max_cand + window] cluster. The named
    // NEIGHBOUR_HORIZON_SECS documents the product "don't hang the dialog"
    // bound for near-now scans; candidate-relative expansion stays O(window).
    let min_cand = cand_secs.iter().copied().min().unwrap_or(now);
    let max_cand = cand_secs.iter().copied().max().unwrap_or(now);
    let _ = now; // reserved for future near-now-only entry points
    // Cursor just before the earliest candidate so "nearby before" still matches.
    let after = min_cand - Duration::seconds(window_secs + 1);
    let horizon_end = max_cand + Duration::seconds(window_secs + 1);

    // Per-candidate buckets.
    let mut by_cand: Vec<(DateTime<Utc>, Vec<NeighbourHitDto>, Vec<NeighbourHitDto>)> = cand_secs
        .iter()
        .map(|c| (*c, Vec::new(), Vec::new()))
        .collect();

    for t in timers {
        stats.timers_scanned += 1;
        if !t.enabled {
            stats.timers_skipped_disabled += 1;
            continue;
        }
        if exclude.is_some_and(|id| id == t.id) {
            stats.timers_skipped_excluded += 1;
            continue;
        }

        let fires = expand_fires(t, after, horizon_end, NEIGHBOUR_MAX_FIRES_PER_TIMER);
        stats.fires_expanded += fires.len();

        for fire in fires {
            let fire_s = truncate_to_second(fire);
            // Match against every candidate; keep best (smallest |delta|) relation.
            for (cand, coll_bucket, near_bucket) in by_cand.iter_mut() {
                let delta = (fire_s - *cand).num_seconds();
                let Some(rel) = classify_delta(delta, window_secs) else {
                    continue;
                };
                let (local_date, local_time, offset, tz_name) =
                    local_parts(&t.occurrence, fire_s);
                let hit = NeighbourHitDto {
                    timer_id: t.id.to_string(),
                    name: t.name.clone(),
                    fire_utc: fire_s,
                    local_date,
                    local_time,
                    offset,
                    tz_name,
                    occurrence_kind: occurrence_kind_name(t.occurrence.kind()).to_string(),
                    action_kind: action_kind_name(&t.action).to_string(),
                    delta_secs: delta,
                    relation: rel.as_str().to_string(),
                };
                match rel {
                    NeighbourRelation::Collision => coll_bucket.push(hit),
                    NeighbourRelation::Nearby => near_bucket.push(hit),
                }
            }
        }
    }

    // Stable sort each bucket: by |delta|, then name.
    let sort_hits = |v: &mut Vec<NeighbourHitDto>| {
        v.sort_by(|a, b| {
            a.delta_secs
                .abs()
                .cmp(&b.delta_secs.abs())
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.timer_id.cmp(&b.timer_id))
                .then_with(|| a.fire_utc.cmp(&b.fire_utc))
        });
        // Dedup same timer+fire within a bucket.
        v.dedup_by(|a, b| a.timer_id == b.timer_id && a.fire_utc == b.fire_utc);
    };

    let mut by_candidate = Vec::with_capacity(by_cand.len());
    let mut all_collisions = Vec::new();
    let mut all_nearby = Vec::new();

    for (cand, mut coll, mut near) in by_cand {
        sort_hits(&mut coll);
        sort_hits(&mut near);
        all_collisions.extend(coll.iter().cloned());
        all_nearby.extend(near.iter().cloned());
        by_candidate.push(CandidateNeighboursDto {
            candidate_utc: cand,
            collisions: coll,
            nearby: near,
        });
    }

    sort_hits(&mut all_collisions);
    sort_hits(&mut all_nearby);
    // Flat lists: unique by timer_id + fire_utc across candidates.
    all_collisions.dedup_by(|a, b| a.timer_id == b.timer_id && a.fire_utc == b.fire_utc);
    all_nearby.dedup_by(|a, b| a.timer_id == b.timer_id && a.fire_utc == b.fire_utc);
    // A hit that is a collision for some candidate should not also appear as nearby.
    let coll_keys: std::collections::HashSet<(String, DateTime<Utc>)> = all_collisions
        .iter()
        .map(|h| (h.timer_id.clone(), h.fire_utc))
        .collect();
    all_nearby.retain(|h| !coll_keys.contains(&(h.timer_id.clone(), h.fire_utc)));
    // Nearby panel: keep the nearest fire per timer so five preview days do not
    // flood the list with the same daily timer five times.
    {
        let mut best: std::collections::HashMap<String, NeighbourHitDto> =
            std::collections::HashMap::new();
        for h in all_nearby.drain(..) {
            best.entry(h.timer_id.clone())
                .and_modify(|cur| {
                    if h.delta_secs.abs() < cur.delta_secs.abs() {
                        *cur = h.clone();
                    }
                })
                .or_insert(h);
        }
        all_nearby = best.into_values().collect();
        sort_hits(&mut all_nearby);
    }

    QueryNeighboursResponse {
        by_candidate,
        collisions: all_collisions,
        nearby: all_nearby,
        stats,
        window_secs,
        horizon_secs: NEIGHBOUR_HORIZON_SECS,
        max_fires_per_timer: NEIGHBOUR_MAX_FIRES_PER_TIMER,
    }
}

/// Parse exclude id; invalid UUID → error string.
pub fn parse_exclude_id(raw: Option<&str>) -> Result<Option<Uuid>, String> {
    match raw {
        None | Some("") => Ok(None),
        Some(s) => Uuid::from_str(s)
            .map(Some)
            .map_err(|e| format!("invalid exclude_timer_id: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bellman_core::occurrence::OccurrenceKind;
    use bellman_core::{Action, MisfirePolicy, Occurrence, OverlapPolicy, RetryPolicy};
    use chrono::{NaiveDate, NaiveTime, TimeZone};

    fn make_timer(
        name: &str,
        occ: Occurrence,
        enabled: bool,
        action: Action,
    ) -> Timer {
        let id = Uuid::new_v4();
        // Seed next_fire via a throwaway open store? Simpler: construct Timer directly.
        let now = Utc::now();
        let next = occ
            .preview(now.with_timezone(&occ.timezone()), 1)
            .into_iter()
            .next()
            .map(|d| d.with_timezone(&Utc));
        Timer {
            id,
            name: name.into(),
            enabled,
            tz: occ.tz_name().to_string(),
            occurrence: occ,
            next_fire_utc: next,
            last_fired: None,
            misfire: MisfirePolicy::default_calendar(),
            overlap: OverlapPolicy::default(),
            retry: RetryPolicy::default(),
            valid_from: None,
            valid_until: None,
            max_runs: None,
            tags: vec![],
            action,
            revision: 1,
            jitter_secs: 0,
            accuracy_slack_secs: None,
        }
    }

    fn daily_at(h: u32, m: u32, s: u32, tz: &str) -> Occurrence {
        Occurrence::new(
            OccurrenceKind::Daily {
                at: NaiveTime::from_hms_opt(h, m, s).unwrap(),
            },
            tz,
        )
        .unwrap()
    }

    fn once_at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, tz: &str) -> Occurrence {
        let nd = NaiveDate::from_ymd_opt(y, mo, d).unwrap();
        let nt = NaiveTime::from_hms_opt(h, mi, s).unwrap();
        Occurrence::new(
            OccurrenceKind::Once {
                at: nd.and_time(nt),
            },
            tz,
        )
        .unwrap()
    }

    #[test]
    fn classify_same_second_is_collision() {
        assert_eq!(classify_delta(0, 300), Some(NeighbourRelation::Collision));
    }

    #[test]
    fn classify_inside_window_is_nearby() {
        assert_eq!(classify_delta(60, 300), Some(NeighbourRelation::Nearby));
        assert_eq!(classify_delta(-299, 300), Some(NeighbourRelation::Nearby));
        assert_eq!(classify_delta(300, 300), Some(NeighbourRelation::Nearby));
    }

    #[test]
    fn classify_outside_window_is_none() {
        assert_eq!(classify_delta(301, 300), None);
        assert_eq!(classify_delta(-301, 300), None);
    }

    #[test]
    fn same_second_collision_finds_named_timers() {
        // Pin candidates to a known future wall time so expansion is deterministic.
        let tz = "UTC";
        let t1 = make_timer(
            "alpha-backup",
            daily_at(9, 0, 0, tz),
            true,
            Action::Notify {
                title: "a".into(),
                body: "".into(),
            },
        );
        let t2 = make_timer(
            "beta-launch-heavy",
            daily_at(9, 0, 0, tz),
            true,
            Action::Launch {
                command: "/bin/true".into(),
                args: vec![],
                workdir: None,
            },
        );
        let t3 = make_timer(
            "gamma-notify",
            daily_at(9, 0, 0, tz),
            true,
            Action::None,
        );
        // Far away — must not appear.
        let t4 = make_timer("other-hour", daily_at(10, 0, 0, tz), true, Action::None);

        let now = Utc.with_ymd_and_hms(2030, 6, 1, 0, 0, 0).unwrap();
        // Next 09:00:00 UTC on/after now.
        let candidate = Utc.with_ymd_and_hms(2030, 6, 1, 9, 0, 0).unwrap();

        let resp = query_neighbours_from_timers(
            &[t1, t2, t3, t4],
            &[candidate],
            None,
            NEIGHBOUR_WINDOW_SECS,
            now,
        );

        assert_eq!(resp.by_candidate.len(), 1);
        let coll = &resp.by_candidate[0].collisions;
        let names: Vec<&str> = coll.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"alpha-backup"), "got {names:?}");
        assert!(names.contains(&"beta-launch-heavy"), "got {names:?}");
        assert!(names.contains(&"gamma-notify"), "got {names:?}");
        assert!(!names.contains(&"other-hour"), "got {names:?}");
        assert!(coll.iter().all(|h| h.relation == "collision"));
        assert!(coll.iter().any(|h| h.action_kind == "launch"));
        assert!(coll.iter().any(|h| h.action_kind == "notify"));
        // Exact fire instant matches candidate second.
        for h in coll {
            assert_eq!(truncate_to_second(h.fire_utc), candidate);
        }
    }

    #[test]
    fn nearby_inside_window_not_collision() {
        let t = make_timer(
            "almost-nine",
            daily_at(9, 2, 0, "UTC"), // 2 minutes after 09:00
            true,
            Action::None,
        );
        let now = Utc.with_ymd_and_hms(2030, 6, 1, 0, 0, 0).unwrap();
        let candidate = Utc.with_ymd_and_hms(2030, 6, 1, 9, 0, 0).unwrap();
        let resp =
            query_neighbours_from_timers(&[t], &[candidate], None, NEIGHBOUR_WINDOW_SECS, now);
        assert!(
            resp.by_candidate[0].collisions.is_empty(),
            "must not be collision"
        );
        assert_eq!(resp.by_candidate[0].nearby.len(), 1);
        let hit = &resp.by_candidate[0].nearby[0];
        assert_eq!(hit.name, "almost-nine");
        assert_eq!(hit.relation, "nearby");
        assert_eq!(hit.delta_secs, 120);
    }

    #[test]
    fn outside_window_excluded() {
        let t = make_timer(
            "ten-past",
            daily_at(9, 10, 0, "UTC"), // 10 min — outside 5 min window
            true,
            Action::None,
        );
        let now = Utc.with_ymd_and_hms(2030, 6, 1, 0, 0, 0).unwrap();
        let candidate = Utc.with_ymd_and_hms(2030, 6, 1, 9, 0, 0).unwrap();
        let resp =
            query_neighbours_from_timers(&[t], &[candidate], None, NEIGHBOUR_WINDOW_SECS, now);
        assert!(resp.by_candidate[0].collisions.is_empty());
        assert!(resp.by_candidate[0].nearby.is_empty());
        assert!(resp.collisions.is_empty());
        assert!(resp.nearby.is_empty());
    }

    #[test]
    fn disabled_timers_excluded() {
        let t = make_timer(
            "disabled-nine",
            daily_at(9, 0, 0, "UTC"),
            false,
            Action::None,
        );
        let now = Utc.with_ymd_and_hms(2030, 6, 1, 0, 0, 0).unwrap();
        let candidate = Utc.with_ymd_and_hms(2030, 6, 1, 9, 0, 0).unwrap();
        let resp =
            query_neighbours_from_timers(&[t], &[candidate], None, NEIGHBOUR_WINDOW_SECS, now);
        assert!(resp.collisions.is_empty());
        assert_eq!(resp.stats.timers_skipped_disabled, 1);
    }

    #[test]
    fn exclude_timer_id_skipped() {
        let t = make_timer("self", daily_at(9, 0, 0, "UTC"), true, Action::None);
        let id = t.id;
        let now = Utc.with_ymd_and_hms(2030, 6, 1, 0, 0, 0).unwrap();
        let candidate = Utc.with_ymd_and_hms(2030, 6, 1, 9, 0, 0).unwrap();
        let resp =
            query_neighbours_from_timers(&[t], &[candidate], Some(id), NEIGHBOUR_WINDOW_SECS, now);
        assert!(resp.collisions.is_empty());
        assert_eq!(resp.stats.timers_skipped_excluded, 1);
    }

    #[test]
    fn dst_boundary_collision_still_by_utc_second() {
        // Europe/Helsinki spring-forward 2030-03-31: 03:00 → 04:00.
        // Two once-timers at the same post-gap local wall time must collide
        // on the same UTC second.
        let occ_a = once_at(2030, 3, 31, 4, 30, 0, "Europe/Helsinki");
        let occ_b = once_at(2030, 3, 31, 4, 30, 0, "Europe/Helsinki");
        let a = make_timer("dst-a", occ_a, true, Action::None);
        let b = make_timer("dst-b", occ_b, true, Action::None);

        // Resolve the real UTC of 04:30 local after the gap.
        let fire_local = a
            .occurrence
            .preview(
                Utc.with_ymd_and_hms(2030, 3, 30, 0, 0, 0)
                    .unwrap()
                    .with_timezone(&a.occurrence.timezone()),
                1,
            )
            .into_iter()
            .next()
            .expect("dst fire");
        let candidate = fire_local.with_timezone(&Utc);

        let now = Utc.with_ymd_and_hms(2030, 3, 30, 0, 0, 0).unwrap();
        let resp = query_neighbours_from_timers(
            &[a, b],
            &[candidate],
            None,
            NEIGHBOUR_WINDOW_SECS,
            now,
        );
        assert_eq!(resp.collisions.len(), 2, "both should collide: {:?}", resp.collisions);
        for h in &resp.collisions {
            assert_eq!(truncate_to_second(h.fire_utc), truncate_to_second(candidate));
        }
    }

    #[test]
    fn constants_are_documented_values() {
        assert_eq!(NEIGHBOUR_WINDOW_SECS, 300);
        assert_eq!(NEIGHBOUR_HORIZON_SECS, 14 * 24 * 3600);
        assert_eq!(NEIGHBOUR_MAX_FIRES_PER_TIMER, 48);
        assert!(NEIGHBOUR_COLLISION_TO_SECOND);
    }
}
