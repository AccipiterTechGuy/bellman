//! Bellman CLI — AI-skill and human-facing command surface.
//!
//! Headless: talks to `bellman-core` store (+ fire path) directly. No daemon.
//! Every command accepts `--json` for a stable machine-readable envelope
//! documented in `docs/CLI.md`.

mod commands;
mod output;
mod parse;
mod resolve;

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
                  (use --json on every command)."
)]
struct Cli {
    /// Emit machine-readable JSON on stdout (stable schema; see docs/CLI.md).
    #[arg(long, global = true)]
    json: bool,

    /// Path to the timers SQLite database.
    ///
    /// Defaults to `$BELLMAN_DB` when set, else `~/.bellman/timers.db`.
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
    /// C5/C6 actions are not merged yet: the stub action records a log line.
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
}

fn main() -> ExitCode {
    // try_parse so --json parse failures still emit the documented JSON error
    // envelope (Cli::parse would print human help to stderr and exit 2).
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(err) => return handle_clap_error(&err),
    };
    let db_path = resolve_db_path(cli.db.as_ref());
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
            },
        ),
        Commands::Rm { name_or_id } => commands::rm(&db_path, &name_or_id),
        Commands::Next { name_or_id, n } => commands::next(&db_path, &name_or_id, n),
        Commands::RunNow { name_or_id } => commands::run_now(&db_path, &name_or_id),
        Commands::Pause { name_or_id } => commands::pause(&db_path, &name_or_id),
        Commands::Resume { name_or_id } => commands::resume(&db_path, &name_or_id),
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
        "add", "list", "edit", "rm", "next", "run-now", "pause", "resume",
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
    ];
    // Boolean / count flags that do not take a value.
    const FLAG_OPTS: &[&str] = &["--json", "-h", "--help", "-V", "--version"];

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
