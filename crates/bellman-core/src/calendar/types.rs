//! Calendar snapshot domain types (JSON contract + render inputs).

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// First day of the week on the calendar grid (Europe default: Monday).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WeekStart {
    #[default]
    Mon,
    Sun,
}

impl WeekStart {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mon" | "monday" => Ok(Self::Mon),
            "sun" | "sunday" => Ok(Self::Sun),
            other => Err(format!("unknown week-start '{other}' (expected mon|sun)")),
        }
    }

    /// Offset of `weekday` (Mon=0 … Sun=6) from the grid's first column.
    pub fn column(self, monday_based: u32) -> u32 {
        match self {
            Self::Mon => monday_based % 7,
            Self::Sun => (monday_based + 1) % 7,
        }
    }
}

/// Per-entry status colour bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarStatus {
    Upcoming,
    Ok,
    Failed,
    Disabled,
    Unknown,
}

impl CalendarStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upcoming => "upcoming",
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
            Self::Unknown => "unknown",
        }
    }

    /// Fixed hex colour used in SVG (deterministic, not theme-dependent).
    pub fn color(self) -> &'static str {
        match self {
            Self::Upcoming => "#3b82f6",
            Self::Ok => "#22c55e",
            Self::Failed => "#ef4444",
            Self::Disabled => "#9ca3af",
            Self::Unknown => "#a855f7",
        }
    }
}

/// One scheduled fire placed on a calendar day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEntry {
    pub task_id: String,
    pub name: String,
    /// Local wall time in the snapshot timezone: `HH:MM` (24h, zero-padded).
    pub time: String,
    /// Seconds past local midnight (for stable sort); not always serialised to UI.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub time_secs: Option<u32>,
    /// ISO date `YYYY-MM-DD` of the local day this entry belongs to.
    pub date: String,
    pub repeats: bool,
    pub status: CalendarStatus,
    /// Present only when the caller opted in with `--show-commands`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub command: Option<String>,
    pub source_kind: String,
}

/// One cell on the month grid (in-month or adjacent greyed day).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDay {
    /// ISO date `YYYY-MM-DD`.
    pub date: String,
    pub day: u32,
    /// `false` for leading/trailing days of adjacent months (greyed in the image).
    pub in_month: bool,
    /// Entries drawn in the cell (already capped); sorted by time then name then id.
    pub entries: Vec<CalendarEntry>,
    /// Count of additional entries not drawn (`+N more`).
    pub overflow: usize,
    /// Total entries on this day before the per-cell cap.
    pub total: usize,
}

/// Full calendar snapshot — the JSON contract; SVG/PNG are views of this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSnapshot {
    /// Inclusive range start (`YYYY-MM-DD`) in the snapshot timezone.
    pub from: String,
    /// Inclusive range end (`YYYY-MM-DD`) in the snapshot timezone.
    pub to: String,
    /// Primary month when built from `--month` (1–12); `None` for arbitrary ranges.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub month: Option<u32>,
    /// IANA timezone all `time` fields are expressed in.
    pub timezone: String,
    pub week_start: WeekStart,
    /// Whether command lines were included.
    pub show_commands: bool,
    /// Grid cells covering full weeks that span `[from, to]` (month view).
    pub days: Vec<CalendarDay>,
    /// Flat list of every entry considered (before per-cell overflow trim),
    /// sorted; used so JSON and image views describe the same set.
    pub entries: Vec<CalendarEntry>,
    /// Caps applied while building.
    pub caps: CalendarCaps,
    /// Non-fatal notes (e.g. truncated high-frequency intervals).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Hard bounds so a noisy schedule never hangs the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarCaps {
    /// Max entries drawn inside one day cell (rest become `+N more`).
    pub max_per_cell: usize,
    /// Max fires expanded per single task inside the window.
    pub max_fires_per_task: usize,
    /// Max total entries across the whole snapshot.
    pub max_total_entries: usize,
    /// Max tasks considered from the source.
    pub max_tasks: usize,
}

impl Default for CalendarCaps {
    fn default() -> Self {
        Self {
            max_per_cell: 5,
            max_fires_per_task: 500,
            max_total_entries: 5_000,
            max_tasks: 2_000,
        }
    }
}

/// Options for building a snapshot.
#[derive(Debug, Clone)]
pub struct CalendarBuildOptions {
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// When set, the grid is a full month view for this year/month.
    pub month: Option<(i32, u32)>,
    pub timezone: String,
    pub week_start: WeekStart,
    pub show_commands: bool,
    /// Instant used to classify upcoming vs past (injectable for tests).
    pub now_utc: chrono::DateTime<chrono::Utc>,
    pub caps: CalendarCaps,
}

/// Output shape requested by the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarFormat {
    Svg,
    Png,
    Json,
}

impl CalendarFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "svg" => Ok(Self::Svg),
            "png" => Ok(Self::Png),
            "json" => Ok(Self::Json),
            other => Err(format!("unknown format '{other}' (expected svg|png|json)")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Png => "png",
            Self::Json => "json",
        }
    }
}

/// English month names (fixed; never locale-dependent — determinism).
pub const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub const MONTH_ABBREV: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
