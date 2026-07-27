//! Bellman CLI — AI-skill and human-facing command surface.
//!
//! Headless: talks to `bellman-core` store (+ fire path) directly. No daemon.
//! Every command accepts `--json` for a stable machine-readable envelope
//! documented in `docs/CLI.md`.

mod commands;
mod output;
mod parse;
mod resolve;

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
    let cli = Cli::parse();
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
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".bellman").join("timers.db")
}
