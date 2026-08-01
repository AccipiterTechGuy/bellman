//! Bellman CLI — AI-skill and human-facing command surface.
//!
//! Headless: talks to `bellman-core` store (+ fire path) directly. No daemon.
//! Every command accepts `--json` for a stable machine-readable envelope
//! documented in `docs/CLI.md`.

mod calendar_cmd;
mod commands;
mod output;
mod parse;
mod resolve;
mod visible_cmd;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// Bellman — task scheduler CLI (AI-skill surface).
#[derive(Debug, Parser)]
#[command(
    name = "bellman",
    version,
    about = "Bellman task scheduler CLI",
    long_about = "Create, list, edit, preview, pause, run-now, and delete timers \
                  against the local Bellman store. Primary consumer: AI agents \
                  (use --json on every command).\n\n\
                  Data directory: ~/.bellman/ (timers.db, logs/, slots/, timers/; \
                  %USERPROFILE%\\.bellman\\ on Windows). The desktop app uses its \
                  own per-OS app-data dir instead — see docs/LOCAL.md."
)]
struct Cli {
    /// Emit machine-readable JSON on stdout (stable schema; see docs/CLI.md).
    #[arg(long, global = true)]
    json: bool,

    /// Path to the timers SQLite database.
    ///
    /// Defaults to `$BELLMAN_DB` when set, else `~/.bellman/timers.db`; the
    /// data directory (logs/, slots/, timers/) is the database's parent.
    #[arg(long, global = true, env = "BELLMAN_DB", value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Add a timer.
    Add {
        /// Timer name (unique among non-deleted timers by convention).
        #[arg(long)]
        name: String,

        /// Occurrence kind: once | interval | daily | weekly | monthly | yearly | cron
        #[arg(long, value_name = "KIND")]
        occurrence: String,

        /// Wall-clock time or once-at datetime.
        ///
        /// - once: `YYYY-MM-DDTHH:MM:SS` (local in --tz) or full RFC3339
        /// - daily / weekly / monthly / yearly: `HH:MM` or `HH:MM:SS`
        /// - interval / cron: optional (ignored for kind construction)
        #[arg(long, value_name = "TIME")]
        time: Option<String>,

        /// IANA timezone (default: system local, falling back to UTC).
        #[arg(long, value_name = "TZ")]
        tz: Option<String>,

        /// Interval period in seconds (required for `--occurrence interval`).
        #[arg(long, value_name = "SECS")]
        every_secs: Option<u64>,

        /// Weekdays for weekly, comma-separated (mon,tue,… or monday,…).
        #[arg(long, value_name = "DAYS")]
        days: Option<String>,

        /// Day of month (1–31) for monthly / yearly.
        #[arg(long, value_name = "N")]
        day: Option<u8>,

        /// Month (1–12) for yearly.
        #[arg(long, value_name = "N")]
        month: Option<u8>,

        /// Cron expression (required for `--occurrence cron`).
        /// Seconds field optional (croner). Example: `0 0 12 * * *`.
        #[arg(long, value_name = "EXPR")]
        cron: Option<String>,

        /// Optional tags (repeatable).
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,

        /// Fire-delivery transport (IK6): auto | json | ipc (default json).
        #[arg(long, value_name = "MODE")]
        transport: Option<String>,

        /// Wake action: none | launch | notify (default none).
        ///
        /// `launch` requires --command and runs it (arg array, no shell)
        /// when the timer fires; `notify` requires --title and shows a
        /// desktop notification.
        #[arg(long, value_name = "KIND")]
        action: Option<String>,

        /// Command to run for `--action launch` (absolute path recommended).
        #[arg(long, value_name = "PATH")]
        command: Option<String>,

        /// One argument for the launch command; repeat per argument
        /// (e.g. `--args -m --args "hello world"`). No shell splitting.
        /// Hyphen-leading values are accepted (each `--args` takes exactly one).
        #[arg(long = "args", value_name = "ARG", allow_hyphen_values = true)]
        args: Vec<String>,

        /// Working directory for `--action launch`.
        #[arg(long, value_name = "DIR")]
        workdir: Option<String>,

        /// Notification title for `--action notify`.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,

        /// Notification body for `--action notify`.
        #[arg(long, value_name = "TEXT")]
        body: Option<String>,
    },

    /// List all timers.
    List,

    /// Edit a timer (name, time, and/or enabled).
    Edit {
        /// Timer name or id (UUID).
        name_or_id: String,

        /// New display name.
        #[arg(long)]
        name: Option<String>,

        /// New wall-clock time (same formats as `add --time`).
        ///
        /// Updates the time component of the existing occurrence kind.
        #[arg(long, value_name = "TIME")]
        time: Option<String>,

        /// Interval every_secs (only when the timer is an interval).
        #[arg(long, value_name = "SECS")]
        every_secs: Option<u64>,

        /// Cron expression (only when the timer is cron).
        #[arg(long, value_name = "EXPR")]
        cron: Option<String>,

        /// Weekdays for weekly (comma-separated).
        #[arg(long, value_name = "DAYS")]
        days: Option<String>,

        /// Day of month for monthly / yearly.
        #[arg(long, value_name = "N")]
        day: Option<u8>,

        /// Month for yearly.
        #[arg(long, value_name = "N")]
        month: Option<u8>,

        /// Set enabled explicitly (`true` / `false`).
        #[arg(long, value_name = "BOOL")]
        enabled: Option<String>,

        /// Fire-delivery transport (IK6): auto | json | ipc.
        #[arg(long, value_name = "MODE")]
        transport: Option<String>,

        /// Replace the wake action: none | launch | notify.
        ///
        /// Sets the whole action: `launch` requires --command (args reset
        /// to --args, default empty), `notify` requires --title.
        #[arg(long, value_name = "KIND")]
        action: Option<String>,

        /// Command to run for `--action launch` (absolute path recommended).
        #[arg(long, value_name = "PATH")]
        command: Option<String>,

        /// One argument for the launch command; repeat per argument.
        /// Hyphen-leading values are accepted (each `--args` takes exactly one).
        #[arg(long = "args", value_name = "ARG", allow_hyphen_values = true)]
        args: Vec<String>,

        /// Working directory for `--action launch`.
        #[arg(long, value_name = "DIR")]
        workdir: Option<String>,

        /// Notification title for `--action notify`.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,

        /// Notification body for `--action notify`.
        #[arg(long, value_name = "TEXT")]
        body: Option<String>,
    },

    /// Delete a timer.
    Rm {
        /// Timer name or id (UUID).
        name_or_id: String,
    },

    /// Preview the next N fire times for a timer.
    Next {
        /// Timer name or id (UUID).
        name_or_id: String,

        /// How many upcoming fires to preview (default 5).
        #[arg(default_value_t = 5)]
        n: usize,
    },

    /// Execute the timer action immediately through the core fire path.
    ///
    /// Runs the real wake action (launch / notify stub / write-slot) and
    /// appends lifecycle events to the JSONL event log.
    #[command(name = "run-now")]
    RunNow {
        /// Timer name or id (UUID).
        name_or_id: String,
    },

    /// Disable a timer (pause scheduling).
    Pause {
        /// Timer name or id (UUID).
        name_or_id: String,
    },

    /// Re-enable a paused timer.
    Resume {
        /// Timer name or id (UUID).
        name_or_id: String,
    },

    /// Publish a slot request JSON and process it against the local store.
    ///
    /// Integrators can also write free/ directly (see docs/INTEGRATION.md);
    /// this helper is the one-shot CLI path used by agents and scripts.
    #[command(name = "slot-submit")]
    SlotSubmit {
        /// Path to a complete `bellman-slot/1` request JSON file.
        request: PathBuf,

        /// Slots root directory (`free/`, `work/`, `done/`, `bad/`).
        ///
        /// Defaults to `$BELLMAN_SLOTS` or `~/.bellman/slots`.
        #[arg(long, env = "BELLMAN_SLOTS", value_name = "DIR")]
        slots: Option<PathBuf>,
    },

    /// Discover every schedule on this machine (cron, systemd, at, Bellman, …).
    Scan {
        /// Filter: all|cron|cron.d|systemd|at|bellman|anacron|run-parts
        #[arg(long, value_name = "SRC")]
        source: Option<String>,

        /// Only tasks owned by this user.
        #[arg(long, value_name = "USER")]
        user: Option<String>,

        /// Diff against the previous scan snapshot (drift detection).
        #[arg(long)]
        diff: bool,
    },

    /// Inspect or safely mutate a discovered task (`bellman scan` ids).
    Task {
        #[command(subcommand)]
        action: TaskCommands,
    },

    /// Render a month (or date range) as a calendar snapshot (SVG / PNG / JSON).
    ///
    /// Pure Rust, headless — no display or GPU required. JSON is the contract;
    /// SVG/PNG are views of the same data. Commands are hidden unless
    /// `--show-commands`. Natural language is limited to fixed phrases
    /// (`this`/`next` month, bare month names) — richer phrasing is the
    /// calling agent's job.
    Calendar {
        /// Month: `YYYY-MM`, `this`, `next`, or bare English month name.
        #[arg(long, value_name = "SPEC")]
        month: Option<String>,

        /// Range start (`YYYY-MM-DD`). Use with `--to` instead of `--month`.
        #[arg(long, value_name = "DATE")]
        from: Option<String>,

        /// Range end (`YYYY-MM-DD`), inclusive.
        #[arg(long, value_name = "DATE")]
        to: Option<String>,

        /// Output format: svg | png | json (default: svg).
        #[arg(long, default_value = "svg", value_name = "FMT")]
        format: String,

        /// Write to this path instead of stdout.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,

        /// First day of week: mon (default) | sun.
        #[arg(long, default_value = "mon", value_name = "DAY")]
        week_start: String,

        /// IANA timezone for displayed times (default: system local).
        #[arg(long, value_name = "TZ")]
        tz: Option<String>,

        /// Include full command lines (private; opt-in).
        #[arg(long)]
        show_commands: bool,

        /// Max items drawn per day cell before `+N more` (default 5).
        #[arg(long, value_name = "N")]
        max_per_cell: Option<usize>,
    },

    /// One-day agenda for a fixed relative phrase or `YYYY-MM-DD`.
    ///
    /// Phrases: `today`, `tomorrow`, `next <weekday>`, bare weekday, or a date.
    Agenda {
        /// Day phrase or `YYYY-MM-DD`.
        phrase: String,

        /// Output format: svg | png | json (default: json).
        #[arg(long, default_value = "json", value_name = "FMT")]
        format: String,

        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,

        #[arg(long, value_name = "TZ")]
        tz: Option<String>,

        #[arg(long)]
        show_commands: bool,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommands {
    /// Show full fields for a task id.
    Show {
        /// Task id from `bellman scan`.
        id: String,
    },
    /// Human explanation of the schedule.
    Explain {
        id: String,
    },
    /// Best-effort logs (journal for systemd; honest unknown for cron).
    Logs {
        id: String,
        #[arg(long, default_value_t = 50)]
        lines: usize,
    },
    /// Enable a disabled task (user crontab / Bellman). Default is dry-run.
    Enable {
        id: String,
        /// Preview only (default behaviour when --apply is absent).
        #[arg(long)]
        dry_run: bool,
        /// Actually write (backs up first). Required to change anything.
        #[arg(long)]
        apply: bool,
    },
    /// Disable a task (user crontab / Bellman). Default is dry-run.
    Disable {
        id: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
    /// Run the task command now (requires --confirm).
    Run {
        id: String,
        /// Explicit confirmation — never implied.
        #[arg(long)]
        confirm: bool,
    },
    /// Create a new scheduled task (cron fence or Bellman store).
    New {
        #[arg(long)]
        command: String,
        #[arg(long)]
        cron: String,
        /// cron (user crontab fence) or bellman (store timer).
        #[arg(long, default_value = "cron")]
        source: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
    /// Edit a writable task's schedule/command. Default is dry-run.
    Edit {
        id: String,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
}

fn main() -> ExitCode {
    // try_parse so --json parse failures still emit the documented JSON error
    // envelope (Cli::parse would print human help to stderr and exit 2).
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(err) => return handle_clap_error(&err),
    };
    let db_path = resolve_db_path(cli.db.as_ref());

    // Visible Scheduler commands use a parallel payload type.
    let visible = match &cli.command {
        Commands::Scan {
            source,
            user,
            diff,
        } => Some(visible_cmd::cmd_scan(
            &db_path,
            source.as_deref(),
            user.as_deref(),
            *diff,
        )),
        Commands::Task { action } => Some(match action {
            TaskCommands::Show { id } => visible_cmd::cmd_task_show(&db_path, id),
            TaskCommands::Explain { id } => visible_cmd::cmd_task_explain(&db_path, id),
            TaskCommands::Logs { id, lines } => {
                visible_cmd::cmd_task_logs(&db_path, id, *lines)
            }
            TaskCommands::Enable {
                id,
                dry_run,
                apply,
            } => {
                let do_apply = *apply && !*dry_run;
                visible_cmd::cmd_task_enable(&db_path, id, do_apply)
            }
            TaskCommands::Disable {
                id,
                dry_run,
                apply,
            } => {
                let do_apply = *apply && !*dry_run;
                visible_cmd::cmd_task_disable(&db_path, id, do_apply)
            }
            TaskCommands::Run { id, confirm } => {
                visible_cmd::cmd_task_run(&db_path, id, *confirm)
            }
            TaskCommands::New {
                command,
                cron,
                source,
                dry_run,
                apply,
            } => {
                let do_apply = *apply && !*dry_run;
                visible_cmd::cmd_task_new(&db_path, command, cron, Some(source), do_apply)
            }
            TaskCommands::Edit {
                id,
                command,
                cron,
                dry_run,
                apply,
            } => {
                let do_apply = *apply && !*dry_run;
                visible_cmd::cmd_task_edit(
                    &db_path,
                    id,
                    command.as_deref(),
                    cron.as_deref(),
                    do_apply,
                )
            }
        }),
        _ => None,
    };

    if let Some(result) = visible {
        return match result {
            Ok(payload) => {
                emit_visible(cli.json, &payload);
                ExitCode::SUCCESS
            }
            Err(err) => {
                output::emit_error(cli.json, err.command, &err);
                ExitCode::FAILURE
            }
        };
    }

    // Calendar / agenda — may write binary or raw SVG to stdout.
    let calendar = match &cli.command {
        Commands::Calendar {
            month,
            from,
            to,
            format,
            out,
            week_start,
            tz,
            show_commands,
            max_per_cell,
        } => Some(calendar_cmd::cmd_calendar(
            &db_path,
            calendar_cmd::CalendarArgs {
                month: month.clone(),
                from: from.clone(),
                to: to.clone(),
                format: format.clone(),
                out: out.clone(),
                week_start: week_start.clone(),
                tz: tz.clone(),
                show_commands: *show_commands,
                max_per_cell: *max_per_cell,
            },
        )),
        Commands::Agenda {
            phrase,
            format,
            out,
            tz,
            show_commands,
        } => Some(calendar_cmd::cmd_agenda(
            &db_path,
            calendar_cmd::AgendaArgs {
                phrase: phrase.clone(),
                format: format.clone(),
                out: out.clone(),
                tz: tz.clone(),
                show_commands: *show_commands,
            },
        )),
        _ => None,
    };

    if let Some(result) = calendar {
        return match result {
            Ok(calendar_cmd::CalendarOutcome::Written { format, path }) => {
                // Raw SVG/PNG already went to stdout when `path` is None — do not
                // append a JSON envelope (would corrupt the stream). When a file
                // was written, optionally acknowledge.
                if let Some(p) = path {
                    if cli.json {
                        let body = serde_json::json!({
                            "ok": true,
                            "command": "calendar",
                            "format": format,
                            "path": p.display().to_string(),
                            "written": true,
                        });
                        println!("{}", serde_json::to_string(&body).unwrap_or_default());
                    } else {
                        eprintln!("wrote {format} → {}", p.display());
                    }
                }
                ExitCode::SUCCESS
            }
            Ok(calendar_cmd::CalendarOutcome::Json(body)) => {
                // format=json path (stdout).
                let s = if std::env::var_os("BELLMAN_JSON_PRETTY").is_some() || !cli.json {
                    // Pretty when human mode or pretty env; compact with --json.
                    if cli.json && std::env::var_os("BELLMAN_JSON_PRETTY").is_none() {
                        serde_json::to_string(&body).unwrap_or_else(|_| body.to_string())
                    } else {
                        serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string())
                    }
                } else {
                    serde_json::to_string(&body).unwrap_or_else(|_| body.to_string())
                };
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                output::emit_error(cli.json, err.command, &err);
                ExitCode::FAILURE
            }
        };
    }

    let result = match cli.command {
        Commands::Add {
            name,
            occurrence,
            time,
            tz,
            every_secs,
            days,
            day,
            month,
            cron,
            tags,
            transport,
            action,
            command,
            args,
            workdir,
            title,
            body,
        } => commands::add(
            &db_path,
            commands::AddArgs {
                name,
                occurrence,
                time,
                tz,
                every_secs,
                days,
                day,
                month,
                cron,
                tags,
                transport,
                action,
                command,
                args,
                workdir,
                title,
                body,
            },
        ),
        Commands::List => commands::list(&db_path),
        Commands::Edit {
            name_or_id,
            name,
            time,
            every_secs,
            cron,
            days,
            day,
            month,
            enabled,
            transport,
            action,
            command,
            args,
            workdir,
            title,
            body,
        } => commands::edit(
            &db_path,
            &name_or_id,
            commands::EditArgs {
                name,
                time,
                every_secs,
                cron,
                days,
                day,
                month,
                enabled,
                transport,
                action,
                command,
                args,
                workdir,
                title,
                body,
            },
        ),
        Commands::Rm { name_or_id } => commands::rm(&db_path, &name_or_id),
        Commands::Next { name_or_id, n } => commands::next(&db_path, &name_or_id, n),
        Commands::RunNow { name_or_id } => commands::run_now(&db_path, &name_or_id),
        Commands::Pause { name_or_id } => commands::pause(&db_path, &name_or_id),
        Commands::Resume { name_or_id } => commands::resume(&db_path, &name_or_id),
        Commands::SlotSubmit { request, slots } => {
            let slots_dir = resolve_slots_dir(slots.as_ref());
            commands::slot_submit(&db_path, &request, &slots_dir)
        }
        Commands::Scan { .. }
        | Commands::Task { .. }
        | Commands::Calendar { .. }
        | Commands::Agenda { .. } => unreachable!("handled above"),
    };

    match result {
        Ok(payload) => {
            output::emit_success(cli.json, &payload);
            ExitCode::SUCCESS
        }
        Err(err) => {
            output::emit_error(cli.json, err.command, &err);
            ExitCode::FAILURE
        }
    }
}

fn emit_visible(as_json: bool, payload: &visible_cmd::VisiblePayload) {
    if as_json {
        let mut body = payload.to_json();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("ok".into(), serde_json::json!(true));
        }
        let s = if std::env::var_os("BELLMAN_JSON_PRETTY").is_some() {
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string())
        } else {
            serde_json::to_string(&body).unwrap_or_else(|_| body.to_string())
        };
        println!("{s}");
    } else {
        println!("{}", payload.to_human());
    }
}

fn resolve_db_path(cli_db: Option<&PathBuf>) -> PathBuf {
    if let Some(p) = cli_db {
        return p.clone();
    }
    if let Ok(p) = std::env::var("BELLMAN_DB") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    default_db_path()
}

fn default_db_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".bellman").join("timers.db")
}

fn resolve_slots_dir(cli_slots: Option<&PathBuf>) -> PathBuf {
    if let Some(p) = cli_slots {
        return p.clone();
    }
    if let Ok(p) = std::env::var("BELLMAN_SLOTS") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".bellman").join("slots")
}

/// Whether the raw argv asked for machine-readable JSON (parse may have failed).
fn argv_wants_json() -> bool {
    std::env::args().any(|a| a == "--json")
}

/// Best-effort subcommand name from argv for the error envelope.
fn argv_command_name() -> &'static str {
    command_name_from_args(std::env::args().skip(1))
}

/// Walk argv left-to-right, skipping option values so a path like `--db list`
/// cannot be mistaken for the `list` subcommand.
///
/// Handles `--opt value`, `--opt=value`, and flag-only options (`--json`).
fn command_name_from_args<I, S>(args: I) -> &'static str
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    const SUBS: &[&str] = &[
        "add",
        "list",
        "edit",
        "rm",
        "next",
        "run-now",
        "pause",
        "resume",
        "slot-submit",
        "scan",
        "task",
        "calendar",
        "agenda",
    ];
    // Options that consume the next argv token as a value (global + common).
    const VALUE_OPTS: &[&str] = &[
        "--db",
        "--name",
        "--occurrence",
        "--time",
        "--tz",
        "--every-secs",
        "--days",
        "--day",
        "--month",
        "--cron",
        "--tag",
        "--enabled",
        "--slots",
        "--source",
        "--user",
        "--command",
        "--action",
        "--args",
        "--workdir",
        "--title",
        "--body",
        "--lines",
        "--format",
        "--out",
        "--week-start",
        "--max-per-cell",
        "--from",
        "--to",
    ];
    // Boolean / count flags that do not take a value.
    const FLAG_OPTS: &[&str] = &[
        "--json",
        "-h",
        "--help",
        "-V",
        "--version",
        "--diff",
        "--apply",
        "--dry-run",
        "--confirm",
        "--show-commands",
    ];

    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            // Positional-only region: first known subcommand wins.
            i += 1;
            while i < args.len() {
                if let Some(cmd) = match_sub(args[i].as_str(), SUBS) {
                    return cmd;
                }
                i += 1;
            }
            break;
        }
        if a.starts_with('-') {
            // --opt=value (never consumes a following token)
            if let Some((opt, _val)) = a.split_once('=') {
                let _ = opt;
                i += 1;
                continue;
            }
            if FLAG_OPTS.contains(&a) {
                i += 1;
                continue;
            }
            if VALUE_OPTS.contains(&a) {
                // Skip option and its value (if present).
                i = i.saturating_add(2);
                continue;
            }
            // Unknown flag: skip it; if the next token looks like a bare value
            // (not a flag and not a known subcommand), skip that too.
            i += 1;
            if i < args.len() {
                let next = args[i].as_str();
                if !next.starts_with('-') && match_sub(next, SUBS).is_none() {
                    i += 1;
                }
            }
            continue;
        }
        if let Some(cmd) = match_sub(a, SUBS) {
            return cmd;
        }
        // Bare token that is not a subcommand (should be rare before a command).
        i += 1;
    }
    "unknown"
}

fn match_sub(token: &str, subs: &[&'static str]) -> Option<&'static str> {
    subs.iter().copied().find(|&s| s == token)
}

/// Map clap parse / help errors into the stable CLI contract.
///
/// With `--json`, argument/parse failures print
/// `{ok:false,command,error:{code,message}}` on **stdout** and exit 1.
/// Help/version keep clap's normal human output (even if `--json` is present).
fn handle_clap_error(err: &clap::Error) -> ExitCode {
    match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        | ErrorKind::DisplayVersion => {
            // clap already chose stdout vs stderr; preserve that UX.
            let _ = err.print();
            if err.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        _ if argv_wants_json() => {
            let message = clap_error_message(err);
            output::emit_parse_error(argv_command_name(), "invalid_args", &message);
            ExitCode::FAILURE
        }
        _ => {
            let _ = err.print();
            // clap defaults to exit 2; our documented contract is 1 on error.
            ExitCode::FAILURE
        }
    }
}

fn clap_error_message(err: &clap::Error) -> String {
    // Prefer a plain single-line-ish message for agents (no ANSI, no trailing
    // "For more information, try '--help'." noise when possible).
    let rendered = err.render().to_string();
    let mut lines: Vec<&str> = rendered
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Drop the usage trailer; keep the error + missing-args body.
    if let Some(pos) = lines.iter().position(|l| l.starts_with("Usage:")) {
        lines.truncate(pos);
    }
    lines.retain(|l| !l.starts_with("For more information"));
    let msg = lines.join(" ");
    if msg.is_empty() {
        err.to_string()
    } else {
        msg
    }
}

#[cfg(test)]
mod argv_command_tests {
    use super::command_name_from_args;

    #[test]
    fn db_path_named_list_does_not_steal_add() {
        // Auditor REPRO: --db list add --name broken → command=add
        let cmd = command_name_from_args(["--json", "--db", "list", "add", "--name", "broken"]);
        assert_eq!(cmd, "add");
    }

    #[test]
    fn db_equals_form_then_add() {
        let cmd = command_name_from_args(["--json", "--db=list", "add", "--name", "broken"]);
        assert_eq!(cmd, "add");
    }

    #[test]
    fn plain_add_after_json() {
        let cmd = command_name_from_args(["--json", "add", "--name", "broken"]);
        assert_eq!(cmd, "add");
    }

    #[test]
    fn list_subcommand_still_detected() {
        let cmd = command_name_from_args(["--json", "list"]);
        assert_eq!(cmd, "list");
    }

    #[test]
    fn run_now_hyphenated() {
        let cmd = command_name_from_args(["--db", "timers.db", "run-now", "tick"]);
        assert_eq!(cmd, "run-now");
    }
}
