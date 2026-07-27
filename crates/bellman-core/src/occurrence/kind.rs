//! Occurrence kind variants (once | interval | daily | weekly | monthly | yearly | cron).

use chrono::{DateTime, Datelike, NaiveDateTime, NaiveTime, Utc, Weekday};
use serde::{Deserialize, Serialize};

/// Set of weekdays for the weekly variant (ISO: Mon=0 … Sun=6).
///
/// Stored as a bitmask so we stay `Ord`/`Hash`-free of chrono's `Weekday`
/// (which does not implement `Ord` / serde without extra features).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Weekdays(u8);

impl Weekdays {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn from_slice(days: &[Weekday]) -> Self {
        let mut w = Self::new();
        for d in days {
            w.insert(*d);
        }
        w
    }

    fn bit(day: Weekday) -> u8 {
        1u8 << day.num_days_from_monday()
    }

    pub fn insert(&mut self, day: Weekday) {
        self.0 |= Self::bit(day);
    }

    pub fn contains(self, day: Weekday) -> bool {
        self.0 & Self::bit(day) != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Weekday> {
        const ORDER: [Weekday; 7] = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ];
        ORDER.into_iter().enumerate().filter_map(move |(i, d)| {
            if self.0 & (1u8 << i) != 0 {
                Some(d)
            } else {
                None
            }
        })
    }
}

/// Canonical occurrence kinds. Interval is elapsed-time (UTC); all others are
/// wall-clock in the schedule's IANA timezone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "occ", rename_all = "snake_case")]
pub enum OccurrenceKind {
    /// Fire once at the given naive local datetime (interpreted in schedule tz).
    Once { at: NaiveDateTime },
    /// Fire every `every_secs` seconds from a UTC anchor. Never uses wall-clock
    /// math — DST must not stretch or shrink the period.
    Interval {
        every_secs: u64,
        anchor: DateTime<Utc>,
    },
    /// Every calendar day at `at` (local wall clock).
    Daily { at: NaiveTime },
    /// On the listed weekdays at `at` (local wall clock).
    Weekly { days: Weekdays, at: NaiveTime },
    /// On day-of-month `day` (1–31) at `at`. Invalid days clamp per policy.
    Monthly { day: u8, at: NaiveTime },
    /// On `month`/`day` each year at `at`. Feb 29 clamps in non-leap years.
    Yearly { month: u8, day: u8, at: NaiveTime },
    /// Power-user cron expression (seconds optional) evaluated via `croner`.
    Cron { expr: String },
}

impl OccurrenceKind {
    /// Minimum interval period allowed by the product spec (1 second).
    pub const MIN_INTERVAL_SECS: u64 = 1;

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Interval { every_secs, .. } => {
                if *every_secs < Self::MIN_INTERVAL_SECS {
                    return Err(format!(
                        "interval every_secs must be >= {}",
                        Self::MIN_INTERVAL_SECS
                    ));
                }
            }
            Self::Weekly { days, .. } => {
                if days.is_empty() {
                    return Err("weekly schedule requires at least one weekday".into());
                }
            }
            Self::Monthly { day, .. } => {
                if !(1..=31).contains(day) {
                    return Err(format!("monthly day must be 1..=31, got {day}"));
                }
            }
            Self::Yearly { month, day, .. } => {
                if !(1..=12).contains(month) {
                    return Err(format!("yearly month must be 1..=12, got {month}"));
                }
                if !(1..=31).contains(day) {
                    return Err(format!("yearly day must be 1..=31, got {day}"));
                }
            }
            Self::Cron { expr } => {
                if expr.trim().is_empty() {
                    return Err("cron expression must not be empty".into());
                }
            }
            Self::Once { .. } | Self::Daily { .. } => {}
        }
        Ok(())
    }

    /// Whether this kind uses UTC elapsed time (interval) vs wall-clock local.
    pub fn is_elapsed_time(&self) -> bool {
        matches!(self, Self::Interval { .. })
    }

    /// Short, type-stable kind label (lowercase, used as the webview
    /// discriminator on the wire). Mirrors `kind_label` in
    /// `src-tauri/src/commands.rs`.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Once { .. } => "once",
            Self::Interval { .. } => "interval",
            Self::Daily { .. } => "daily",
            Self::Weekly { .. } => "weekly",
            Self::Monthly { .. } => "monthly",
            Self::Yearly { .. } => "yearly",
            Self::Cron { .. } => "cron",
        }
    }

    /// Helper: last day-of-month for a year/month (1-based month).
    pub fn days_in_month(year: i32, month: u32) -> u32 {
        let (y, m) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        chrono::NaiveDate::from_ymd_opt(y, m, 1)
            .expect("valid month")
            .pred_opt()
            .expect("predecessor")
            .day()
    }
}
