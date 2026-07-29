//! Visible-scheduler domain types: discovered tasks, source kinds, last result.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Stable task id (deterministic across scans for the same source line/unit).
pub type TaskId = String;

/// High-level schedule source family (CLI `--source` filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFilter {
    All,
    Cron,
    CronD,
    Systemd,
    At,
    Bellman,
    Anacron,
    RunParts,
}

impl SourceFilter {
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
    Ok { exit_code: i32 },
    /// Real non-zero exit status observed.
    Failed { exit_code: i32 },
}

impl LastResult {
    pub fn is_unknown_or_never(&self) -> bool {
        matches!(self, Self::Unknown | Self::Never)
    }
}

/// A single discovered scheduled task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredTask {
    pub id: TaskId,
    pub source_kind: SourceKind,
    /// Exact file path, unit name, or Bellman timer id origin.
    pub source: String,
    pub owner: String,
    pub command: String,
    /// Raw schedule expression (cron fields, OnCalendar, at time, etc.).
    pub schedule_expr: String,
    pub human_explanation: String,
    pub next_run: Option<DateTime<Utc>>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_result: LastResult,
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
    pub scanned_at: DateTime<Utc>,
    pub platform: String,
    pub filter: String,
    pub count: usize,
    pub tasks: Vec<DiscoveredTask>,
    /// Non-fatal discovery warnings (permission denied on a spool, etc.).
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Planned mutation (always computed before write).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritePlan {
    pub task_id: Option<TaskId>,
    pub action: String,
    pub target: String,
    pub applied: bool,
    pub dry_run: bool,
    pub before: String,
    pub after: String,
    pub backup_path: Option<String>,
    #[serde(default)]
    pub refused: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Outcome of `task run --confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub task_id: TaskId,
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

/// Diff between two scans (drift detection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanDiff {
    pub previous_at: Option<DateTime<Utc>>,
    pub current_at: DateTime<Utc>,
    pub added: Vec<DiscoveredTask>,
    pub removed: Vec<DiscoveredTask>,
    pub changed: Vec<TaskChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskChange {
    pub id: TaskId,
    pub field: String,
    pub before: String,
    pub after: String,
}
