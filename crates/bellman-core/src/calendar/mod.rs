//! Calendar Snapshot — render any month as SVG / PNG / JSON.
//!
//! Pure-Rust path: expand source-agnostic tasks → [`CalendarSnapshot`] (JSON
//! contract) → deterministic SVG → optional `resvg` PNG. **No webview, no
//! display, no GPU.**
//!
//! Natural-language boundary: only structured flags plus the fixed relative
//! phrases in [`period`] (`today`, `tomorrow`, `this month`, `next month`,
//! `next <weekday>`, bare month name, `YYYY-MM`). Richer phrasing is the
//! calling agent's responsibility.

mod build;
mod period;
mod png;
mod svg;
mod types;

#[cfg(test)]
mod tests;

pub use build::{
    build_snapshot, snapshot_month_from_store, tasks_from_store, ExpandableTask,
};
pub use period::{
    local_date, month_bounds, parse_date, parse_tz, resolve_day_phrase, resolve_month,
    system_tz_name,
};
pub use png::svg_to_png;
pub use svg::render_svg;
pub use types::{
    CalendarBuildOptions, CalendarCaps, CalendarDay, CalendarEntry, CalendarFormat,
    CalendarSnapshot, CalendarStatus, WeekStart, MONTH_ABBREV, MONTH_NAMES,
};
