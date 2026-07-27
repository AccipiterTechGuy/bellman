//! `Occurrence` schedule: next_fire, preview, exclusions, skip-next, validity.

use super::civil::{clamp_month_day, resolve_local};
use super::kind::{OccurrenceKind, Weekdays};
use super::policy::{DstFoldPolicy, DstGapPolicy, InvalidMonthDayPolicy};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::str::FromStr;

/// A fully configured occurrence schedule.
///
/// `next_fire(after)` returns the next fire **strictly after** `after`, applying
/// validity window, max_runs, exclusion dates, and any pending skip-next count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    kind: OccurrenceKind,
    /// IANA timezone name (e.g. `"Europe/Helsinki"`). Used for all wall-clock kinds.
    tz: String,
    dst_gap: DstGapPolicy,
    dst_fold: DstFoldPolicy,
    invalid_monthday: InvalidMonthDayPolicy,
    /// Inclusive lower bound (UTC). Fires before this are not returned.
    valid_from: Option<DateTime<Utc>>,
    /// Exclusive upper bound (UTC). Fires at or after this are not returned.
    valid_until: Option<DateTime<Utc>>,
    /// Stop after this many successful fires have been recorded (`runs_done`).
    max_runs: Option<u64>,
    /// How many times this schedule has already fired (for `max_runs`).
    runs_done: u64,
    /// EXDATE-style local calendar dates that must be skipped.
    exclusions: BTreeSet<NaiveDate>,
    /// Number of upcoming occurrences to skip (decremented by `next_fire`).
    pending_skips: u32,
}

impl Occurrence {
    /// Build a schedule with default policies and no limits.
    pub fn new(kind: OccurrenceKind, tz: impl Into<String>) -> Result<Self, String> {
        kind.validate()?;
        let tz = tz.into();
        // Validate tz name early.
        Tz::from_str(&tz).map_err(|e| format!("unknown timezone '{tz}': {e}"))?;
        Ok(Self {
            kind,
            tz,
            dst_gap: DstGapPolicy::default(),
            dst_fold: DstFoldPolicy::default(),
            invalid_monthday: InvalidMonthDayPolicy::default(),
            valid_from: None,
            valid_until: None,
            max_runs: None,
            runs_done: 0,
            exclusions: BTreeSet::new(),
            pending_skips: 0,
        })
    }

    pub fn with_dst_gap(mut self, policy: DstGapPolicy) -> Self {
        self.dst_gap = policy;
        self
    }

    pub fn with_dst_fold(mut self, policy: DstFoldPolicy) -> Self {
        self.dst_fold = policy;
        self
    }

    pub fn with_invalid_monthday(mut self, policy: InvalidMonthDayPolicy) -> Self {
        self.invalid_monthday = policy;
        self
    }

    pub fn with_valid_from(mut self, from: DateTime<Utc>) -> Self {
        self.valid_from = Some(from);
        self
    }

    pub fn with_valid_until(mut self, until: DateTime<Utc>) -> Self {
        self.valid_until = Some(until);
        self
    }

    pub fn with_max_runs(mut self, max: u64) -> Self {
        self.max_runs = Some(max);
        self
    }

    pub fn with_runs_done(mut self, n: u64) -> Self {
        self.runs_done = n;
        self
    }

    pub fn kind(&self) -> &OccurrenceKind {
        &self.kind
    }

    pub fn tz_name(&self) -> &str {
        &self.tz
    }

    pub fn timezone(&self) -> Tz {
        Tz::from_str(&self.tz).expect("tz validated at construction")
    }

    pub fn exclusions(&self) -> &BTreeSet<NaiveDate> {
        &self.exclusions
    }

    pub fn pending_skips(&self) -> u32 {
        self.pending_skips
    }

    pub fn runs_done(&self) -> u64 {
        self.runs_done
    }

    /// Add an EXDATE-style exclusion (local calendar date in the schedule tz).
    pub fn exclude_date(&mut self, date: NaiveDate) {
        self.exclusions.insert(date);
    }

    pub fn clear_exclusion(&mut self, date: NaiveDate) {
        self.exclusions.remove(&date);
    }

    /// Skip the next occurrence exactly once (EXDATE-equivalent one-shot skip).
    pub fn skip_next(&mut self) {
        self.pending_skips = self.pending_skips.saturating_add(1);
    }

    /// Record that a fire has been delivered (increments `runs_done`).
    pub fn record_run(&mut self) {
        self.runs_done = self.runs_done.saturating_add(1);
    }

    /// Next fire strictly after `after`, or `None` if exhausted / out of window.
    ///
    /// Consumes one `pending_skips` entry per skipped candidate. Does **not**
    /// mutate `runs_done` — the caller records runs via `record_run`.
    pub fn next_fire(&mut self, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
        if let Some(max) = self.max_runs {
            if self.runs_done >= max {
                return None;
            }
        }

        let tz = self.timezone();
        // Work in the schedule's zone; accept any input tz via conversion.
        let after = after.with_timezone(&tz);

        // Raise the search floor to the validity start when needed.
        let mut cursor = after;
        if let Some(from) = self.valid_from {
            let from_local = from.with_timezone(&tz);
            // next_fire is strictly-after; if `from` is still ahead of `after`,
            // search from just before `from` so `from` itself can match.
            if from_local > cursor {
                cursor = from_local - Duration::nanoseconds(1);
            }
        }

        // Hard safety cap so a bad cron / empty weekly never spins forever.
        // Exclusion days are jumped in one step (see `jump_past_exclusion_day`),
        // so a full day of 1-second interval ticks does not burn this budget.
        const MAX_CANDIDATES: usize = 10_000;
        let mut skipped = 0u32;
        let need_skip = self.pending_skips;

        for _ in 0..MAX_CANDIDATES {
            let Some(candidate) = self.raw_next_after(cursor) else {
                // Consume skips that already matched a real candidate, even when
                // nothing remains after (e.g. skip_next on a Once).
                self.pending_skips = self.pending_skips.saturating_sub(skipped);
                return None;
            };

            // Validity end (exclusive).
            if let Some(until) = self.valid_until {
                if candidate.with_timezone(&Utc) >= until {
                    self.pending_skips = self.pending_skips.saturating_sub(skipped);
                    return None;
                }
            }
            // Validity start (inclusive) — already floored cursor, but re-check.
            if let Some(from) = self.valid_from {
                if candidate.with_timezone(&Utc) < from {
                    cursor = candidate;
                    continue;
                }
            }

            // Exclusion dates (local calendar date): jump to the end of that
            // local day so high-frequency intervals do not iterate every tick.
            if self.exclusions.contains(&candidate.date_naive()) {
                cursor = self
                    .jump_past_exclusion_day(candidate)
                    .unwrap_or(candidate);
                continue;
            }

            // Pending skip-next.
            if skipped < need_skip {
                skipped += 1;
                cursor = candidate;
                continue;
            }

            // Consume the skips we actually applied.
            self.pending_skips = self.pending_skips.saturating_sub(skipped);
            return Some(candidate);
        }
        self.pending_skips = self.pending_skips.saturating_sub(skipped);
        None
    }

    /// Move the search cursor to just before the next local calendar day after
    /// an excluded date, so the next `raw_next_after` lands on (or after) that day.
    fn jump_past_exclusion_day(&self, candidate: DateTime<Tz>) -> Option<DateTime<Tz>> {
        let tz = self.timezone();
        let next_day = candidate.date_naive().succ_opt()?;
        let midnight = NaiveDateTime::new(next_day, NaiveTime::from_hms_opt(0, 0, 0)?);
        let next_midnight = match midnight.and_local_timezone(tz) {
            chrono::LocalResult::Single(dt) => dt,
            chrono::LocalResult::Ambiguous(earliest, _) => earliest,
            chrono::LocalResult::None => {
                // Midnight in a DST gap: first valid instant on that local date.
                return super::civil::resolve_local(
                    tz,
                    next_day,
                    NaiveTime::from_hms_opt(0, 0, 0)?,
                    self.dst_gap,
                    self.dst_fold,
                )
                .map(|dt| dt - Duration::nanoseconds(1));
            }
        };
        // Strictly-after cursor: one nanosecond before the next local midnight.
        Some(next_midnight - Duration::nanoseconds(1))
    }

    /// Peek next fire without consuming skip-next state.
    pub fn peek_next_fire(&self, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
        let mut clone = self.clone();
        clone.next_fire(after)
    }

    /// Next `n` fires strictly after `after`. Does not consume skip-next or
    /// mutate runs — uses a temporary clone so the live schedule is untouched
    /// except that a single shared clone walks candidates consistently.
    ///
    /// Skip-next and exclusions apply to the preview the same way as to
    /// `next_fire`. Preview does **not** permanently consume `pending_skips`
    /// on `self`; call `next_fire` for that.
    pub fn preview(&self, after: DateTime<Tz>, n: usize) -> Vec<DateTime<Tz>> {
        let mut out = Vec::with_capacity(n);
        let mut working = self.clone();
        let mut cursor = after;
        for _ in 0..n {
            match working.next_fire(cursor) {
                Some(t) => {
                    out.push(t);
                    cursor = t;
                    // Preview of recurring fires should not require record_run
                    // between steps; advance runs_done so max_runs is honored.
                    working.record_run();
                }
                None => break,
            }
        }
        out
    }

    /// Lazy iterator over upcoming fires. Same semantics as `preview`.
    pub fn iter_after(&self, after: DateTime<Tz>) -> PreviewIter {
        PreviewIter {
            working: self.clone(),
            cursor: after,
            done: false,
        }
    }

    // ---- internal candidate generation ------------------------------------

    fn raw_next_after(&self, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
        match &self.kind {
            OccurrenceKind::Once { at } => self.next_once(*at, after),
            OccurrenceKind::Interval {
                every_secs,
                anchor,
            } => self.next_interval(*every_secs, *anchor, after),
            OccurrenceKind::Daily { at } => self.next_daily(*at, after),
            OccurrenceKind::Weekly { days, at } => self.next_weekly(days, *at, after),
            OccurrenceKind::Monthly { day, at } => self.next_monthly(*day, *at, after),
            OccurrenceKind::Yearly { month, day, at } => {
                self.next_yearly(*month, *day, *at, after)
            }
            OccurrenceKind::Cron { expr } => self.next_cron(expr, after),
        }
    }

    fn resolve(&self, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Tz>> {
        resolve_local(
            self.timezone(),
            date,
            time,
            self.dst_gap,
            self.dst_fold,
        )
    }

    fn next_once(&self, at: NaiveDateTime, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
        let fire = self.resolve(at.date(), at.time())?;
        if fire > after {
            Some(fire)
        } else {
            None
        }
    }

    /// Interval anchors to UTC elapsed time — never wall-clock / DST.
    ///
    /// Arithmetic stays in `i64` seconds (no `i32` narrowing) so long-lived
    /// 1-second schedules remain correct past `i32::MAX` periods from the anchor.
    fn next_interval(
        &self,
        every_secs: u64,
        anchor: DateTime<Utc>,
        after: DateTime<Tz>,
    ) -> Option<DateTime<Tz>> {
        if every_secs == 0 {
            return None;
        }
        let every = i64::try_from(every_secs).ok()?;
        if every <= 0 {
            return None;
        }
        let after_utc = after.with_timezone(&Utc);

        let next_utc = if after_utc < anchor {
            anchor
        } else {
            let elapsed_secs = after_utc.signed_duration_since(anchor).num_seconds();
            // `n` = index of the last fire at-or-before `after` (floor division).
            // The next fire strictly after is always index n+1, including when
            // `after` lands exactly on a fire boundary.
            let n = elapsed_secs.div_euclid(every);
            let next_n = n.checked_add(1)?;
            let offset_secs = next_n.checked_mul(every)?;
            anchor.checked_add_signed(Duration::seconds(offset_secs))?
        };

        // Convert to schedule tz for a uniform return type. Instant is fixed.
        Some(next_utc.with_timezone(&self.timezone()))
    }

    fn next_daily(&self, at: NaiveTime, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
        let after_local = after.with_timezone(&self.timezone());
        let mut date = after_local.date_naive();
        // Try today first; if the resolved fire is not strictly after, step days.
        for _ in 0..366 * 5 {
            if let Some(fire) = self.resolve(date, at) {
                if fire > after_local {
                    return Some(fire);
                }
            }
            date = date.succ_opt()?;
        }
        None
    }

    fn next_weekly(
        &self,
        days: &Weekdays,
        at: NaiveTime,
        after: DateTime<Tz>,
    ) -> Option<DateTime<Tz>> {
        if days.is_empty() {
            return None;
        }
        let after_local = after.with_timezone(&self.timezone());
        let mut date = after_local.date_naive();
        for _ in 0..366 * 5 {
            if days.contains(date.weekday()) {
                if let Some(fire) = self.resolve(date, at) {
                    if fire > after_local {
                        return Some(fire);
                    }
                }
            }
            date = date.succ_opt()?;
        }
        None
    }

    fn next_monthly(&self, day: u8, at: NaiveTime, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
        let after_local = after.with_timezone(&self.timezone());
        let mut year = after_local.year();
        let mut month = after_local.month();
        for _ in 0..12 * 50 {
            let date = clamp_month_day(year, month, u32::from(day), self.invalid_monthday)?;
            if let Some(fire) = self.resolve(date, at) {
                if fire > after_local {
                    return Some(fire);
                }
            }
            // Advance one month.
            if month == 12 {
                month = 1;
                year += 1;
            } else {
                month += 1;
            }
        }
        None
    }

    fn next_yearly(
        &self,
        month: u8,
        day: u8,
        at: NaiveTime,
        after: DateTime<Tz>,
    ) -> Option<DateTime<Tz>> {
        let after_local = after.with_timezone(&self.timezone());
        for year in after_local.year().. {
            if year >= after_local.year() + 200 {
                break;
            }
            let date =
                clamp_month_day(year, u32::from(month), u32::from(day), self.invalid_monthday)?;
            if let Some(fire) = self.resolve(date, at) {
                if fire > after_local {
                    return Some(fire);
                }
            }
        }
        None
    }

    fn next_cron(&self, expr: &str, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
        // Seconds field optional so both 5-field and 6-field expressions work.
        let cron = croner::Cron::new(expr)
            .with_seconds_optional()
            .parse()
            .ok()?;
        let after_local = after.with_timezone(&self.timezone());
        // croner returns same tz as input; exclusive of `after`.
        cron.find_next_occurrence(&after_local, false).ok()
    }
}

/// Iterator produced by [`Occurrence::iter_after`].
pub struct PreviewIter {
    working: Occurrence,
    cursor: DateTime<Tz>,
    done: bool,
}

impl Iterator for PreviewIter {
    type Item = DateTime<Tz>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.working.next_fire(self.cursor) {
            Some(t) => {
                self.cursor = t;
                self.working.record_run();
                Some(t)
            }
            None => {
                self.done = true;
                None
            }
        }
    }
}

/// Convenience: parse a weekday list from short English names.
pub fn parse_weekdays(names: &[&str]) -> Result<Weekdays, String> {
    let mut w = Weekdays::new();
    for name in names {
        let day = match name.to_ascii_lowercase().as_str() {
            "mon" | "monday" => Weekday::Mon,
            "tue" | "tues" | "tuesday" => Weekday::Tue,
            "wed" | "wednesday" => Weekday::Wed,
            "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
            "fri" | "friday" => Weekday::Fri,
            "sat" | "saturday" => Weekday::Sat,
            "sun" | "sunday" => Weekday::Sun,
            other => return Err(format!("unknown weekday '{other}'")),
        };
        w.insert(day);
    }
    Ok(w)
}
