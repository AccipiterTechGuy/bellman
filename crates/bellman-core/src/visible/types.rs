//! Visible-scheduler domain types: discovered tasks, source kinds, last result.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Stable task id (deterministic across scans for the same source line/unit).
pub type TaskId = String;

/// High-level schedule source family (CLI `--source` filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFilter {
    /// Everything the platform can discover.
    All,
    /// Any crontab-shaped source (user, system, `cron.d`, run-parts).
    Cron,
    /// `/etc/cron.d/*` only.
    CronD,
    /// systemd timer units, system and user.
    Systemd,
    /// The `at` queue.
    At,
    /// Bellman's own store timers.
    Bellman,
    /// `/etc/anacrontab` entries.
    Anacron,
    /// Scripts in the `cron.{hourly,daily,weekly,monthly}` directories.
    RunParts,
}

impl SourceFilter {
    /// Parse a `--source` value, accepting the spellings people actually
    /// type (`cron.d`, `crond`, `cron_d`). The error names every accepted
    /// value, because a filter typo otherwise looks like an empty machine.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "" => Ok(Self::All),
            "cron" => Ok(Self::Cron),
            "cron.d" | "crond" | "cron_d" => Ok(Self::CronD),
            "systemd" => Ok(Self::Systemd),
            "at" => Ok(Self::At),
            "bellman" => Ok(Self::Bellman),
            "anacron" => Ok(Self::Anacron),
            "run-parts" | "runparts" | "run_parts" => Ok(Self::RunParts),
            other => Err(format!(
                "unknown source filter '{other}' (expected cron|cron.d|systemd|at|bellman|anacron|run-parts|all)"
            )),
        }
    }
}

/// Exact origin kind of a discovered task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// User crontab (`crontab -l` / spool file).
    CronUser,
    /// `/etc/crontab`
    CronSystem,
    /// `/etc/cron.d/*`
    CronD,
    /// Script in `/etc/cron.{hourly,daily,weekly,monthly}/`
    CronRunParts,
    /// `/etc/anacrontab` entry
    Anacron,
    /// system systemd timer unit
    SystemdSystem,
    /// user systemd timer unit
    SystemdUser,
    /// `at` queue job
    At,
    /// Bellman store timer
    Bellman,
    /// Platform not implemented (Windows Task Scheduler / macOS launchd)
    Unsupported,
}

impl SourceKind {
    /// The stable `source_kind` string in `--json` output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CronUser => "cron_user",
            Self::CronSystem => "cron_system",
            Self::CronD => "cron_d",
            Self::CronRunParts => "cron_run_parts",
            Self::Anacron => "anacron",
            Self::SystemdSystem => "systemd_system",
            Self::SystemdUser => "systemd_user",
            Self::At => "at",
            Self::Bellman => "bellman",
            Self::Unsupported => "unsupported",
        }
    }

    /// Whether this origin belongs in a scan restricted to `f`. `All`
    /// deliberately excludes [`SourceKind::Unsupported`] — a platform stub is
    /// a note, not a task.
    pub fn matches_filter(self, f: SourceFilter) -> bool {
        match f {
            SourceFilter::All => !matches!(self, Self::Unsupported),
            SourceFilter::Cron => matches!(
                self,
                Self::CronUser | Self::CronSystem | Self::CronD | Self::CronRunParts
            ),
            SourceFilter::CronD => matches!(self, Self::CronD),
            SourceFilter::Systemd => matches!(self, Self::SystemdSystem | Self::SystemdUser),
            SourceFilter::At => matches!(self, Self::At),
            SourceFilter::Bellman => matches!(self, Self::Bellman),
            SourceFilter::Anacron => matches!(self, Self::Anacron),
            SourceFilter::RunParts => matches!(self, Self::CronRunParts),
        }
    }

    /// v1: only the invoking user's own crontab entries are writable.
    pub fn is_system_readonly(self) -> bool {
        matches!(
            self,
            Self::CronSystem
                | Self::CronD
                | Self::CronRunParts
                | Self::Anacron
                | Self::SystemdSystem
                | Self::Unsupported
        )
    }
}

/// Honest last-result reporting.
///
/// Cron does **not** record exit status — only that it invoked the job.
/// Never invent `ok` from silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LastResult {
    /// Never observed a run.
    Never,
    /// Exit status is genuinely unknown (typical for cron).
    Unknown,
    /// Real exit status observed (systemd, run-now, Bellman events).
    Ok {
        /// The observed exit status (zero).
        exit_code: i32,
    },
    /// Real non-zero exit status observed.
    Failed {
        /// The observed non-zero exit status.
        exit_code: i32,
    },
}

impl LastResult {
    /// True when nothing is actually known about the last run — the case a
    /// UI must not render as success.
    pub fn is_unknown_or_never(&self) -> bool {
        matches!(self, Self::Unknown | Self::Never)
    }
}

/// A single discovered scheduled task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredTask {
    /// Deterministic id: the same source line scans to the same id.
    pub id: TaskId,
    /// Which scheduler this came from.
    pub source_kind: SourceKind,
    /// Exact file path, unit name, or Bellman timer id origin.
    pub source: String,
    /// The user the task runs as, as the source records it.
    pub owner: String,
    /// Shell-executable command with **literal** `%` characters (already
    /// unescaped from crontab `\%`). Never includes the cron stdin region.
    pub command: String,
    /// Cron stdin payload (text after the first unescaped `%` on the line).
    /// Newlines are real newlines here; rewritten via [`crate::visible::cron::parse::join_percent`].
    #[serde(default)]
    pub stdin_payload: Option<String>,
    /// Raw schedule expression (cron fields, OnCalendar, at time, etc.).
    pub schedule_expr: String,
    /// `schedule_expr` rendered as English, for people who do not read cron.
    pub human_explanation: String,
    /// Next fire, when the source gives enough to compute one.
    pub next_run: Option<DateTime<Utc>>,
    /// Last observed run, when the source records one.
    pub last_run: Option<DateTime<Utc>>,
    /// How the last run ended — see [`LastResult`]; absence is not success.
    pub last_result: LastResult,
    /// Whether the source has it active (a commented cron line is disabled).
    pub enabled: bool,
    /// Whether Bellman may mutate this entry (v1: only own user crontab).
    pub writable: bool,
    /// When not writable, human reason (never empty if `writable == false`
    /// for system sources; may be empty for platform stubs).
    #[serde(default)]
    pub write_block_reason: Option<String>,
    /// IANA timezone used for next-run math (if known).
    #[serde(default)]
    pub timezone: Option<String>,
    /// Line number in source file (1-based), when applicable.
    #[serde(default)]
    pub line_no: Option<u32>,
    /// Exact original crontab line (for byte-identical restore).
    #[serde(default)]
    pub raw_line: Option<String>,
    /// When this entry was disabled by Bellman, the original un-commented line.
    #[serde(default)]
    pub disabled_original: Option<String>,
    /// Platform note (e.g. "not implemented on this platform yet").
    #[serde(default)]
    pub platform_note: Option<String>,
}

/// Result of a full or filtered scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    /// When the scan ran.
    pub scanned_at: DateTime<Utc>,
    /// Platform the scan ran on; names what the list can and cannot contain.
    pub platform: String,
    /// The `--source` filter in force, echoed back.
    pub filter: String,
    /// `tasks.len()`, carried so a consumer need not count.
    pub count: usize,
    /// Everything discovered, in scan order.
    pub tasks: Vec<DiscoveredTask>,
    /// Non-fatal discovery warnings (permission denied on a spool, etc.).
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Planned mutation (always computed before write).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritePlan {
    /// The task being changed; `None` when creating one.
    pub task_id: Option<TaskId>,
    /// What was asked for: `enable`, `disable`, `edit`, `new`.
    pub action: String,
    /// The file or unit that would be written.
    pub target: String,
    /// Whether the write actually happened (`--apply` was given).
    pub applied: bool,
    /// Whether this was a dry run — the default, and the safe one.
    pub dry_run: bool,
    /// The target's relevant content before the change.
    pub before: String,
    /// What it would contain after.
    pub after: String,
    /// Where the pre-write backup was put, when one was taken.
    pub backup_path: Option<String>,
    /// Why Bellman declined, when it did (system file, wrong owner).
    #[serde(default)]
    pub refused: Option<String>,
    /// Anything else the operator should read before applying.
    #[serde(default)]
    pub note: Option<String>,
}

/// Outcome of `task run --confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcome {
    /// Which discovered task was run.
    pub task_id: TaskId,
    /// The exact command line that was executed.
    pub command: String,
    /// Its exit status — a real one, unlike most `last_result` values.
    pub exit_code: i32,
    /// Captured stdout (bounded by the executor's output cap).
    pub stdout: String,
    /// Captured stderr (same cap).
    pub stderr: String,
    /// When it started.
    pub started_at: DateTime<Utc>,
    /// When it finished.
    pub finished_at: DateTime<Utc>,
}

/// Diff between two scans (drift detection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanDiff {
    /// When the scan being compared against was taken; `None` on first run.
    pub previous_at: Option<DateTime<Utc>>,
    /// When this scan was taken.
    pub current_at: DateTime<Utc>,
    /// Tasks present now and absent before.
    pub added: Vec<DiscoveredTask>,
    /// Tasks present before and absent now.
    pub removed: Vec<DiscoveredTask>,
    /// Tasks present in both, field by changed field.
    pub changed: Vec<TaskChange>,
}

/// One field of one task that differs between two scans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskChange {
    /// The task the change belongs to.
    pub id: TaskId,
    /// Which field changed, by name.
    pub field: String,
    /// Its value in the earlier scan.
    pub before: String,
    /// Its value now.
    pub after: String,
}
