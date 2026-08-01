//! Deterministic pure-Rust SVG calendar renderer.
//!
//! Same input always produces byte-identical SVG: fixed English labels, fixed
//! colours, no timestamps, no random ids, no locale formatting.
//!
//! Note: SVG colour literals use `r##"…"##` raw strings so `#rrggbb` is not
//! parsed as the end of a `r#"…"#` delimiter.

use super::types::{CalendarDay, CalendarSnapshot, WeekStart, MONTH_NAMES};

/// Canvas metrics (device-independent; SVG user units).
const WIDTH: u32 = 980;
const HEADER_H: u32 = 56;
const WEEKDAY_H: u32 = 28;
// Tall enough for max_per_cell (5) entry lines + day number + `+N more`.
const CELL_H: u32 = 124;
const PAD: u32 = 16;
const CELL_PAD: u32 = 6;
const LINE_H: u32 = 14;

/// Render a snapshot to SVG text (UTF-8).
pub fn render_svg(snap: &CalendarSnapshot) -> String {
    let weeks = (snap.days.len() / 7).max(1) as u32;
    // If days not a multiple of 7, still reserve full rows.
    let rows = if snap.days.is_empty() {
        0
    } else {
        snap.days.len().div_ceil(7) as u32
    };
    let rows = rows.max(weeks);
    let height = PAD + HEADER_H + WEEKDAY_H + rows * CELL_H + PAD;
    let inner_w = WIDTH - 2 * PAD;
    let cell_w = inner_w as f64 / 7.0;

    let mut out = String::with_capacity(16_384);
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" viewBox="0 0 {WIDTH} {height}">"#
    ));
    out.push('\n');
    // Background
    out.push_str(&format!(
        r##"  <rect width="{WIDTH}" height="{height}" fill="#ffffff"/>"##
    ));
    out.push('\n');

    // Header: "August 2026 · Europe/Helsinki"
    let title = header_title(snap);
    out.push_str(&format!(
        r##"  <text x="{}" y="{}" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="22" font-weight="600" fill="#111827">{}</text>"##,
        PAD,
        PAD + 28,
        xml_escape(&title)
    ));
    out.push('\n');

    // Weekday headers
    let labels = weekday_labels(snap.week_start);
    let weekday_y = PAD + HEADER_H + 18;
    for (i, lab) in labels.iter().enumerate() {
        let x = PAD as f64 + (i as f64 + 0.5) * cell_w;
        out.push_str(&format!(
            r##"  <text x="{x:.2}" y="{weekday_y}" text-anchor="middle" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="12" font-weight="600" fill="#6b7280">{lab}</text>"##
        ));
        out.push('\n');
    }

    let grid_top = PAD + HEADER_H + WEEKDAY_H;

    // Cells
    for (idx, day) in snap.days.iter().enumerate() {
        let col = (idx % 7) as u32;
        let row = (idx / 7) as u32;
        let x = PAD as f64 + col as f64 * cell_w;
        let y = grid_top as f64 + row as f64 * CELL_H as f64;
        render_cell(&mut out, day, x, y, cell_w, CELL_H as f64);
    }

    // Empty month still draws the grid outline via cells above; if no days at
    // all (should not happen for a valid month), draw a note.
    if snap.days.is_empty() {
        out.push_str(&format!(
            r##"  <text x="{}" y="{}" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="14" fill="#6b7280">empty range</text>"##,
            PAD,
            grid_top + 24
        ));
        out.push('\n');
    }

    out.push_str("</svg>\n");
    out
}

fn header_title(snap: &CalendarSnapshot) -> String {
    if let (Some(y), Some(m)) = (snap.year, snap.month) {
        let name = MONTH_NAMES
            .get((m as usize).saturating_sub(1))
            .copied()
            .unwrap_or("?");
        format!("{name} {y} · {}", snap.timezone)
    } else {
        format!("{} – {} · {}", snap.from, snap.to, snap.timezone)
    }
}

fn weekday_labels(ws: WeekStart) -> [&'static str; 7] {
    match ws {
        WeekStart::Mon => ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
        WeekStart::Sun => ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
    }
}

fn render_cell(out: &mut String, day: &CalendarDay, x: f64, y: f64, w: f64, h: f64) {
    let bg = if day.in_month { "#ffffff" } else { "#f3f4f6" };
    let day_fill = if day.in_month { "#111827" } else { "#9ca3af" };

    out.push_str(&format!(r#"  <g data-date="{}">"#, xml_escape(&day.date)));
    out.push('\n');
    out.push_str(&format!(
        r##"    <rect x="{x:.2}" y="{y:.2}" width="{w:.2}" height="{h:.2}" fill="{bg}" stroke="#e5e7eb" stroke-width="1"/>"##
    ));
    out.push('\n');
    // Day number
    out.push_str(&format!(
        r##"    <text x="{:.2}" y="{:.2}" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="13" font-weight="600" fill="{day_fill}">{}</text>"##,
        x + CELL_PAD as f64,
        y + 16.0,
        day.day
    ));
    out.push('\n');

    let mut line_y = y + 32.0;
    let max_y = y + h - 8.0;
    // Reserve one line for `+N more` so overflow is never clipped off the cell.
    let overflow_reserve = if day.overflow > 0 { LINE_H as f64 } else { 0.0 };
    for entry in &day.entries {
        if line_y + LINE_H as f64 + overflow_reserve > max_y {
            break;
        }
        let bar = entry.status.color();
        // Status colour bar
        out.push_str(&format!(
            r#"    <rect x="{:.2}" y="{:.2}" width="3" height="11" fill="{bar}" rx="1"/>"#,
            x + CELL_PAD as f64,
            line_y - 10.0
        ));
        out.push('\n');
        let marker = if entry.repeats { " ↻" } else { "" };
        let label = format!("{} {}{marker}", entry.time, entry.name);
        let clipped = clip_text(&label, 22);
        out.push_str(&format!(
            r##"    <text data-task-id="{}" data-time="{}" data-status="{}" x="{:.2}" y="{:.2}" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="11" fill="#374151">{}</text>"##,
            xml_escape(&entry.task_id),
            xml_escape(&entry.time),
            entry.status.as_str(),
            x + CELL_PAD as f64 + 6.0,
            line_y,
            xml_escape(&clipped)
        ));
        out.push('\n');
        // Optional command (rare; opt-in) — second line, muted
        if let Some(cmd) = &entry.command {
            line_y += LINE_H as f64;
            if line_y + LINE_H as f64 + overflow_reserve > max_y {
                break;
            }
            let c = clip_text(cmd, 24);
            out.push_str(&format!(
                r##"    <text x="{:.2}" y="{:.2}" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="9" fill="#9ca3af">{}</text>"##,
                x + CELL_PAD as f64 + 6.0,
                line_y,
                xml_escape(&c)
            ));
            out.push('\n');
        }
        line_y += LINE_H as f64;
    }

    if day.overflow > 0 {
        out.push_str(&format!(
            r##"    <text data-overflow="{}" x="{:.2}" y="{:.2}" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="11" fill="#6b7280">+{} more</text>"##,
            day.overflow,
            x + CELL_PAD as f64,
            line_y.min(max_y),
            day.overflow
        ));
        out.push('\n');
    }

    out.push_str("  </g>\n");
}

fn clip_text(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::types::{CalendarCaps, CalendarEntry, CalendarStatus, WeekStart};

    fn empty_snap() -> CalendarSnapshot {
        // Minimal empty August 2026 grid without entries — one day is enough
        // for unit tests of renderer determinism.
        let days = vec![CalendarDay {
            date: "2026-08-01".into(),
            day: 1,
            in_month: true,
            entries: vec![],
            overflow: 0,
            total: 0,
        }];
        CalendarSnapshot {
            from: "2026-08-01".into(),
            to: "2026-08-01".into(),
            year: Some(2026),
            month: Some(8),
            timezone: "UTC".into(),
            week_start: WeekStart::Mon,
            show_commands: false,
            days,
            entries: vec![],
            caps: CalendarCaps::default(),
            warnings: vec![],
        }
    }

    #[test]
    fn deterministic_twice() {
        let s = empty_snap();
        let a = render_svg(&s);
        let b = render_svg(&s);
        assert_eq!(a, b);
        assert!(a.contains("August 2026 · UTC"));
        assert!(a.contains("data-date=\"2026-08-01\""));
    }

    #[test]
    fn overflow_and_status_in_svg() {
        let mut s = empty_snap();
        s.days[0].entries = vec![CalendarEntry {
            task_id: "t1".into(),
            name: "backup".into(),
            time: "09:00".into(),
            time_secs: Some(9 * 3600),
            date: "2026-08-01".into(),
            repeats: true,
            status: CalendarStatus::Upcoming,
            command: None,
            source_kind: "bellman".into(),
        }];
        s.days[0].overflow = 15;
        s.days[0].total = 16;
        let svg = render_svg(&s);
        assert!(svg.contains("+15 more"));
        assert!(svg.contains("data-status=\"upcoming\""));
        assert!(svg.contains("data-task-id=\"t1\""));
        assert!(!svg.contains("secret-token")); // no commands by default
    }

    #[test]
    fn commands_hidden_unless_present() {
        let mut s = empty_snap();
        s.show_commands = true;
        s.days[0].entries = vec![CalendarEntry {
            task_id: "t1".into(),
            name: "job".into(),
            time: "10:00".into(),
            time_secs: None,
            date: "2026-08-01".into(),
            repeats: false,
            status: CalendarStatus::Ok,
            command: Some("curl https://x/token=SECRET".into()),
            source_kind: "bellman".into(),
        }];
        let with = render_svg(&s);
        assert!(with.contains("token=SECRET") || with.contains("curl"));
        s.days[0].entries[0].command = None;
        let without = render_svg(&s);
        assert!(!without.contains("SECRET"));
    }
}
