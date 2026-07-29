//! `bellman calendar` and `bellman agenda` — snapshot scheduled work as
//! SVG / PNG / JSON without a display.

use crate::commands::CliError;
use bellman_core::store::{OpenOptions, Store};
use bellman_core::{
    build_snapshot, local_date, month_bounds, parse_date, parse_tz, render_svg, resolve_day_phrase,
    resolve_month, svg_to_png, system_tz_name, tasks_from_store, CalendarBuildOptions,
    CalendarCaps, CalendarFormat, CalendarSnapshot, WeekStart,
};
use chrono::Utc;
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct CalendarArgs {
    pub month: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub format: String,
    pub out: Option<PathBuf>,
    pub week_start: String,
    pub tz: Option<String>,
    pub show_commands: bool,
    pub max_per_cell: Option<usize>,
}

pub struct AgendaArgs {
    pub phrase: String,
    pub format: String,
    pub out: Option<PathBuf>,
    pub tz: Option<String>,
    pub show_commands: bool,
}

/// Outcome of calendar/agenda: either printed already (binary/text to out/stdout)
/// or a JSON value for the shared envelope path.
pub enum CalendarOutcome {
    /// Caller should not print again (bytes already written).
    Written { format: &'static str, path: Option<PathBuf> },
    /// JSON body for `--json` / default human summary when format=json.
    Json(Value),
}

pub fn cmd_calendar(db: &Path, args: CalendarArgs) -> Result<CalendarOutcome, CliError> {
    const CMD: &str = "calendar";
    let fmt = CalendarFormat::parse(&args.format)
        .map_err(|m| CliError::new(CMD, "invalid_args", m))?;
    let week_start = WeekStart::parse(&args.week_start)
        .map_err(|m| CliError::new(CMD, "invalid_args", m))?;
    let tz_name = args.tz.clone().unwrap_or_else(system_tz_name);
    let tz = parse_tz(&tz_name).map_err(|m| CliError::new(CMD, "invalid_args", m))?;
    let now_utc = Utc::now();
    let now_local = local_date(now_utc, tz);

    let (from, to, month_ym) = resolve_range(&args, now_local, CMD)?;

    let store = open_store(db, CMD)?;
    let tasks = tasks_from_store(&store).map_err(|m| CliError::new(CMD, "store_error", m))?;

    let mut caps = CalendarCaps::default();
    if let Some(n) = args.max_per_cell {
        if n == 0 {
            return Err(CliError::new(
                CMD,
                "invalid_args",
                "--max-per-cell must be >= 1",
            ));
        }
        caps.max_per_cell = n;
    }

    let snap = build_snapshot(
        &tasks,
        &CalendarBuildOptions {
            from,
            to,
            month: month_ym,
            timezone: tz_name,
            week_start,
            show_commands: args.show_commands,
            now_utc,
            caps,
        },
    )
    .map_err(|m| CliError::new(CMD, "store_error", m))?;

    emit_snapshot(CMD, &snap, fmt, args.out.as_deref())
}

pub fn cmd_agenda(db: &Path, args: AgendaArgs) -> Result<CalendarOutcome, CliError> {
    const CMD: &str = "agenda";
    let fmt = CalendarFormat::parse(&args.format)
        .map_err(|m| CliError::new(CMD, "invalid_args", m))?;
    let tz_name = args.tz.clone().unwrap_or_else(system_tz_name);
    let tz = parse_tz(&tz_name).map_err(|m| CliError::new(CMD, "invalid_args", m))?;
    let now_utc = Utc::now();
    let now_local = local_date(now_utc, tz);
    let day = resolve_day_phrase(&args.phrase, now_local)
        .map_err(|m| CliError::new(CMD, "invalid_args", m))?;

    let store = open_store(db, CMD)?;
    let tasks = tasks_from_store(&store).map_err(|m| CliError::new(CMD, "store_error", m))?;

    let snap = build_snapshot(
        &tasks,
        &CalendarBuildOptions {
            from: day,
            to: day,
            month: None,
            timezone: tz_name,
            week_start: WeekStart::Mon,
            show_commands: args.show_commands,
            now_utc,
            caps: CalendarCaps::default(),
        },
    )
    .map_err(|m| CliError::new(CMD, "store_error", m))?;

    emit_snapshot(CMD, &snap, fmt, args.out.as_deref())
}

type CalendarRange = (
    chrono::NaiveDate,
    chrono::NaiveDate,
    Option<(i32, u32)>,
);

fn resolve_range(
    args: &CalendarArgs,
    now_local: chrono::NaiveDate,
    cmd: &'static str,
) -> Result<CalendarRange, CliError> {
    match (&args.month, &args.from, &args.to) {
        (Some(m), None, None) => {
            let (y, mon) = resolve_month(m, now_local)
                .map_err(|e| CliError::new(cmd, "invalid_args", e))?;
            let (from, to) = month_bounds(y, mon)
                .map_err(|e| CliError::new(cmd, "invalid_args", e))?;
            Ok((from, to, Some((y, mon))))
        }
        (None, Some(f), Some(t)) => {
            let from = parse_date(f).map_err(|e| CliError::new(cmd, "invalid_args", e))?;
            let to = parse_date(t).map_err(|e| CliError::new(cmd, "invalid_args", e))?;
            if to < from {
                return Err(CliError::new(
                    cmd,
                    "invalid_args",
                    format!("--to {t} is before --from {f}"),
                ));
            }
            Ok((from, to, None))
        }
        (None, Some(f), None) => {
            let from = parse_date(f).map_err(|e| CliError::new(cmd, "invalid_args", e))?;
            Ok((from, from, None))
        }
        (None, None, None) => {
            // Default: this month.
            let (y, mon) = (now_local.year(), now_local.month());
            let (from, to) = month_bounds(y, mon)
                .map_err(|e| CliError::new(cmd, "invalid_args", e))?;
            Ok((from, to, Some((y, mon))))
        }
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(CliError::new(
            cmd,
            "invalid_args",
            "use either --month or --from/--to, not both",
        )),
        (None, None, Some(_)) => Err(CliError::new(
            cmd,
            "invalid_args",
            "--to requires --from",
        )),
    }
}

fn emit_snapshot(
    cmd: &'static str,
    snap: &CalendarSnapshot,
    fmt: CalendarFormat,
    out: Option<&Path>,
) -> Result<CalendarOutcome, CliError> {
    match fmt {
        CalendarFormat::Json => {
            let mut body = serde_json::to_value(snap).map_err(|e| {
                CliError::new(cmd, "store_error", format!("serialize: {e}"))
            })?;
            if let Some(obj) = body.as_object_mut() {
                obj.insert("command".into(), json!(cmd));
                obj.insert("ok".into(), json!(true));
            }
            if let Some(path) = out {
                let s = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
                write_bytes(path, s.as_bytes(), cmd)?;
                Ok(CalendarOutcome::Written {
                    format: "json",
                    path: Some(path.to_path_buf()),
                })
            } else {
                Ok(CalendarOutcome::Json(body))
            }
        }
        CalendarFormat::Svg => {
            let svg = render_svg(snap);
            if let Some(path) = out {
                write_bytes(path, svg.as_bytes(), cmd)?;
                Ok(CalendarOutcome::Written {
                    format: "svg",
                    path: Some(path.to_path_buf()),
                })
            } else {
                // Raw SVG on stdout (pipeable).
                print!("{svg}");
                let _ = io::stdout().flush();
                Ok(CalendarOutcome::Written {
                    format: "svg",
                    path: None,
                })
            }
        }
        CalendarFormat::Png => {
            let svg = render_svg(snap);
            let png = svg_to_png(&svg).map_err(|e| CliError::new(cmd, "store_error", e))?;
            if let Some(path) = out {
                write_bytes(path, &png, cmd)?;
                Ok(CalendarOutcome::Written {
                    format: "png",
                    path: Some(path.to_path_buf()),
                })
            } else {
                // Binary PNG on stdout.
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(&png)
                    .map_err(|e| CliError::new(cmd, "store_error", format!("stdout: {e}")))?;
                stdout
                    .flush()
                    .map_err(|e| CliError::new(cmd, "store_error", format!("stdout: {e}")))?;
                Ok(CalendarOutcome::Written {
                    format: "png",
                    path: None,
                })
            }
        }
    }
}

fn write_bytes(path: &Path, data: &[u8], cmd: &'static str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| {
                CliError::new(
                    cmd,
                    "store_error",
                    format!("create dir {}: {e}", parent.display()),
                )
            })?;
        }
    }
    fs::write(path, data).map_err(|e| {
        CliError::new(
            cmd,
            "store_error",
            format!("write {}: {e}", path.display()),
        )
    })
}

fn open_store(db: &Path, cmd: &'static str) -> Result<Store, CliError> {
    // Missing DB is fine for empty calendar — open creates if needed via OpenOptions.
    // If parent missing and path doesn't exist, open may create. Match other cmds.
    Store::open_with(
        db,
        OpenOptions {
            refuse_network_fs: true,
            ..OpenOptions::default()
        },
    )
    .map_err(|e| CliError::new(cmd, "store_error", e.to_string()))
}

// Need Datelike for year/month on NaiveDate.
use chrono::Datelike;
