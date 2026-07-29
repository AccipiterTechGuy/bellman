//! Crontab line parser (5-field user, 6-field system, specials, env, `%`).
//!
//! Parsing is pure: given text + mode, produce structured entries. Writing
//! preserves unknown lines byte-for-byte; only structured ops touch content.

use serde::{Deserialize, Serialize};

/// Whether the crontab includes a user column (system `/etc/crontab`, `/etc/cron.d`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrontabMode {
    /// 5 schedule fields + command (user crontab).
    User,
    /// 5 schedule fields + user + command (system / cron.d).
    System,
}

/// @special nicknames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecialSchedule {
    Reboot,
    Yearly,
    Monthly,
    Weekly,
    Daily,
    Hourly,
}

impl SpecialSchedule {
    pub fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "@reboot" => Some(Self::Reboot),
            "@yearly" | "@annually" => Some(Self::Yearly),
            "@monthly" => Some(Self::Monthly),
            "@weekly" => Some(Self::Weekly),
            "@daily" | "@midnight" => Some(Self::Daily),
            "@hourly" => Some(Self::Hourly),
            _ => None,
        }
    }

    /// Equivalent 5-field expression (not for @reboot).
    pub fn as_five_field(self) -> String {
        match self {
            Self::Reboot => String::new(),
            Self::Yearly => "0 0 1 1 *".into(),
            Self::Monthly => "0 0 1 * *".into(),
            Self::Weekly => "0 0 * * 0".into(),
            Self::Daily => "0 0 * * *".into(),
            Self::Hourly => "0 * * * *".into(),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reboot => "@reboot",
            Self::Yearly => "@yearly",
            Self::Monthly => "@monthly",
            Self::Weekly => "@weekly",
            Self::Daily => "@daily",
            Self::Hourly => "@hourly",
        }
    }
}

/// Parsed schedule part of a cron line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CronSchedule {
    Special(SpecialSchedule),
    Fields {
        minute: String,
        hour: String,
        dom: String,
        month: String,
        dow: String,
    },
}

impl CronSchedule {
    pub fn expression(&self) -> String {
        match self {
            Self::Special(s) => s.as_str().to_string(),
            Self::Fields {
                minute,
                hour,
                dom,
                month,
                dow,
            } => format!("{minute} {hour} {dom} {month} {dow}"),
        }
    }

    /// 5-field form for next-run (None for @reboot).
    pub fn five_field(&self) -> Option<String> {
        match self {
            Self::Special(SpecialSchedule::Reboot) => None,
            Self::Special(s) => Some(s.as_five_field()),
            Self::Fields {
                minute,
                hour,
                dom,
                month,
                dow,
            } => Some(format!("{minute} {hour} {dom} {month} {dow}")),
        }
    }
}

/// A single crontab content line classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrontabLine {
    /// Blank or whitespace-only — preserve as-is.
    Blank { raw: String, line_no: u32 },
    /// Comment (including disabled jobs we didn't create).
    Comment { raw: String, line_no: u32 },
    /// SHELL=/ PATH=/ MAILTO=/ CRON_TZ=/ etc.
    Env {
        raw: String,
        line_no: u32,
        key: String,
        value: String,
    },
    /// Active job line.
    Job {
        raw: String,
        line_no: u32,
        schedule: CronSchedule,
        /// Present in system mode.
        user: Option<String>,
        /// Command portion **before** first unescaped `%` (arguments).
        command: String,
        /// Everything after first unescaped `%` (stdin payload, may be empty).
        stdin_payload: Option<String>,
        /// True when line was a `#`-commented job that Bellman disabled.
        disabled_by_bellman: bool,
        /// When disabled by Bellman, the exact original line (without our marker).
        original_line: Option<String>,
    },
    /// Unparseable non-empty line — keep byte-for-byte, never rewrite.
    Unknown { raw: String, line_no: u32 },
}

impl CrontabLine {
    pub fn raw(&self) -> &str {
        match self {
            Self::Blank { raw, .. }
            | Self::Comment { raw, .. }
            | Self::Env { raw, .. }
            | Self::Job { raw, .. }
            | Self::Unknown { raw, .. } => raw,
        }
    }

    pub fn line_no(&self) -> u32 {
        match self {
            Self::Blank { line_no, .. }
            | Self::Comment { line_no, .. }
            | Self::Env { line_no, .. }
            | Self::Job { line_no, .. }
            | Self::Unknown { line_no, .. } => *line_no,
        }
    }
}

/// Marker prefixes Bellman uses when disabling a line.
pub const DISABLE_PREFIX: &str = "# bellman-disabled: ";
pub const DISABLE_ORIG_PREFIX: &str = "# bellman-disabled-orig: ";

/// Fence sentinels for Bellman-managed blocks.
pub const FENCE_BEGIN: &str = "# BEGIN bellman-managed";
pub const FENCE_END: &str = "# END bellman-managed";

/// Split a crontab body into lines **without** stripping trailing content.
/// Uses `\n` separators; a trailing newline is represented by a final empty
/// raw only if the file ends with `\n` and we iterate lines via split — we
/// keep each line **without** the newline character.
pub fn split_lines(text: &str) -> Vec<String> {
    // Preserve exact line content; only drop the `\n` / `\r\n` terminators.
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // If file ends with newline, split yields a final empty string — keep
        // it only when it is not the sole trailing artifact we should drop for
        // rejoin. We track join via reconstruct.
        if i == text.split('\n').count() - 1 && line.is_empty() && text.ends_with('\n') {
            // skip the phantom empty after final newline
            break;
        }
        out.push(line.to_string());
    }
    out
}

/// Rejoin lines with `\n` and restore trailing newline if `had_trailing_nl`.
pub fn join_lines(lines: &[String], had_trailing_nl: bool) -> String {
    let mut s = lines.join("\n");
    if had_trailing_nl && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

pub fn had_trailing_newline(text: &str) -> bool {
    text.ends_with('\n')
}

/// Parse full crontab text.
pub fn parse_crontab(text: &str, mode: CrontabMode) -> Vec<CrontabLine> {
    let lines = split_lines(text);
    lines
        .into_iter()
        .enumerate()
        .map(|(i, raw)| parse_line(&raw, (i + 1) as u32, mode))
        .collect()
}

fn parse_line(raw: &str, line_no: u32, mode: CrontabMode) -> CrontabLine {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return CrontabLine::Blank {
            raw: raw.to_string(),
            line_no,
        };
    }

    // Bellman disable markers (two-line form: marker + optional orig).
    if let Some(rest) = trimmed.strip_prefix(DISABLE_PREFIX) {
        // rest is the original line content (may start mid-line)
        if let Some(job) = try_parse_job(rest, mode) {
            return CrontabLine::Job {
                raw: raw.to_string(),
                line_no,
                schedule: job.schedule,
                user: job.user,
                command: job.command,
                stdin_payload: job.stdin_payload,
                disabled_by_bellman: true,
                original_line: Some(rest.to_string()),
            };
        }
        return CrontabLine::Comment {
            raw: raw.to_string(),
            line_no,
        };
    }
    if trimmed.starts_with(DISABLE_ORIG_PREFIX) {
        return CrontabLine::Comment {
            raw: raw.to_string(),
            line_no,
        };
    }

    if trimmed.starts_with('#') {
        return CrontabLine::Comment {
            raw: raw.to_string(),
            line_no,
        };
    }

    // Environment assignment: NAME=value (NAME is [A-Za-z_][A-Za-z0-9_]*)
    if let Some((key, value)) = parse_env_assignment(trimmed) {
        return CrontabLine::Env {
            raw: raw.to_string(),
            line_no,
            key,
            value,
        };
    }

    if let Some(job) = try_parse_job(trimmed, mode) {
        return CrontabLine::Job {
            raw: raw.to_string(),
            line_no,
            schedule: job.schedule,
            user: job.user,
            command: job.command,
            stdin_payload: job.stdin_payload,
            disabled_by_bellman: false,
            original_line: None,
        };
    }

    CrontabLine::Unknown {
        raw: raw.to_string(),
        line_no,
    }
}

struct ParsedJob {
    schedule: CronSchedule,
    user: Option<String>,
    command: String,
    stdin_payload: Option<String>,
}

fn parse_env_assignment(trimmed: &str) -> Option<(String, String)> {
    let eq = trimmed.find('=')?;
    let key = &trimmed[..eq];
    if key.is_empty()
        || !key
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    // Job lines never look like KEY=value with a pure identifier key and no
    // spaces in key — but cron env must not have spaces around `=`.
    if key.contains(' ') {
        return None;
    }
    // Heuristic: if key looks like a schedule field, not env.
    if key.chars().any(|c| c == '*' || c == '/' || c == ',' || c == '-') {
        return None;
    }
    Some((key.to_string(), trimmed[eq + 1..].to_string()))
}

fn try_parse_job(trimmed: &str, mode: CrontabMode) -> Option<ParsedJob> {
    let tokens: Vec<&str> = split_ws_preserving(trimmed);
    if tokens.is_empty() {
        return None;
    }

    // @special
    if tokens[0].starts_with('@') {
        let special = SpecialSchedule::parse(tokens[0])?;
        let rest = match mode {
            CrontabMode::User => {
                if tokens.len() < 2 {
                    return None;
                }
                tokens[1..].join(" ")
            }
            CrontabMode::System => {
                if tokens.len() < 3 {
                    return None;
                }
                // tokens[1] = user, rest = command
                let user = tokens[1].to_string();
                let cmd = tokens[2..].join(" ");
                let (command, stdin_payload) = split_percent(&cmd);
                return Some(ParsedJob {
                    schedule: CronSchedule::Special(special),
                    user: Some(user),
                    command,
                    stdin_payload,
                });
            }
        };
        let (command, stdin_payload) = split_percent(&rest);
        return Some(ParsedJob {
            schedule: CronSchedule::Special(special),
            user: None,
            command,
            stdin_payload,
        });
    }

    // 5 schedule fields required
    if tokens.len() < 5 {
        return None;
    }
    let minute = tokens[0].to_string();
    let hour = tokens[1].to_string();
    let dom = tokens[2].to_string();
    let month = tokens[3].to_string();
    let dow = tokens[4].to_string();
    if !looks_like_field(&minute)
        || !looks_like_field(&hour)
        || !looks_like_field(&dom)
        || !looks_like_field(&month)
        || !looks_like_field(&dow)
    {
        return None;
    }

    match mode {
        CrontabMode::User => {
            if tokens.len() < 6 {
                return None;
            }
            let cmd = tokens[5..].join(" ");
            let (command, stdin_payload) = split_percent(&cmd);
            Some(ParsedJob {
                schedule: CronSchedule::Fields {
                    minute,
                    hour,
                    dom,
                    month,
                    dow,
                },
                user: None,
                command,
                stdin_payload,
            })
        }
        CrontabMode::System => {
            if tokens.len() < 7 {
                return None;
            }
            let user = tokens[5].to_string();
            let cmd = tokens[6..].join(" ");
            let (command, stdin_payload) = split_percent(&cmd);
            Some(ParsedJob {
                schedule: CronSchedule::Fields {
                    minute,
                    hour,
                    dom,
                    month,
                    dow,
                },
                user: Some(user),
                command,
                stdin_payload,
            })
        }
    }
}

fn looks_like_field(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '*' | '/' | ',' | '-' | '#'))
}

/// Split on whitespace runs (spaces/tabs).
fn split_ws_preserving(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

/// Cron `%` rule: first unescaped `%` starts stdin; remaining `%` → newlines
/// in the stdin payload. Backslash-escaped `\%` is a literal percent.
///
/// Returns (command with literal `%` chars, optional stdin).
///
/// Pair with [`join_percent`] when writing a crontab line so literal `%` is
/// re-escaped as `\%` and is not misread as a stdin separator.
pub fn split_percent(cmd: &str) -> (String, Option<String>) {
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    let mut cmd_out = String::new();
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == '%' {
            cmd_out.push('%');
            i += 2;
            continue;
        }
        if chars[i] == '%' {
            // stdin starts after this %
            let rest: String = chars[i + 1..].iter().collect();
            let stdin = rest.replace('%', "\n");
            return (cmd_out, Some(stdin));
        }
        cmd_out.push(chars[i]);
        i += 1;
    }
    (cmd_out, None)
}

/// Inverse of [`split_percent`]: encode a shell command (+ optional stdin) for
/// a crontab line. Every literal `%` in `command` becomes `\%`; stdin is
/// appended after an unescaped `%`, with newlines turned into `%`.
pub fn join_percent(command: &str, stdin: Option<&str>) -> String {
    let mut out = String::with_capacity(command.len() + 8);
    for ch in command.chars() {
        if ch == '%' {
            out.push('\\');
            out.push('%');
        } else {
            out.push(ch);
        }
    }
    if let Some(payload) = stdin {
        out.push('%');
        // Cron: unescaped % in the stdin region → newline on delivery.
        for ch in payload.chars() {
            if ch == '\n' {
                out.push('%');
            } else {
                out.push(ch);
            }
        }
    }
    out
}

/// Effective CRON_TZ walking lines top-to-bottom (last wins before a job).
pub fn env_tz_before(lines: &[CrontabLine], before_line_no: u32, default_tz: &str) -> String {
    let mut tz = default_tz.to_string();
    for line in lines {
        if line.line_no() >= before_line_no {
            break;
        }
        if let CrontabLine::Env { key, value, .. } = line {
            if key.eq_ignore_ascii_case("CRON_TZ") || key.eq_ignore_ascii_case("TZ") {
                tz = value.clone();
            }
        }
    }
    tz
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_five_field() {
        let text = "0 6 * * MON-FRI /usr/bin/backup\n";
        let lines = parse_crontab(text, CrontabMode::User);
        assert_eq!(lines.len(), 1);
        match &lines[0] {
            CrontabLine::Job {
                command, schedule, ..
            } => {
                assert_eq!(command, "/usr/bin/backup");
                assert_eq!(schedule.expression(), "0 6 * * MON-FRI");
            }
            other => panic!("expected job, got {other:?}"),
        }
    }

    #[test]
    fn system_six_field() {
        let text = "17 * * * * root cd / && run-parts --report /etc/cron.hourly\n";
        let lines = parse_crontab(text, CrontabMode::System);
        match &lines[0] {
            CrontabLine::Job {
                user, command, ..
            } => {
                assert_eq!(user.as_deref(), Some("root"));
                assert!(command.contains("run-parts"));
            }
            other => panic!("expected job, got {other:?}"),
        }
    }

    #[test]
    fn percent_splits_stdin() {
        let (cmd, stdin) = split_percent("cat %hello%world");
        assert_eq!(cmd, "cat ");
        assert_eq!(stdin.as_deref(), Some("hello\nworld"));
    }

    #[test]
    fn escaped_percent_in_command() {
        let (cmd, stdin) = split_percent(r"echo 100\% done");
        assert_eq!(cmd, "echo 100% done");
        assert!(stdin.is_none());
    }

    #[test]
    fn join_percent_reescapes_literal_percent() {
        let encoded = join_percent("echo 100% pure", None);
        assert_eq!(encoded, r"echo 100\% pure");
        let (cmd, stdin) = split_percent(&encoded);
        assert_eq!(cmd, "echo 100% pure");
        assert!(stdin.is_none());
    }

    #[test]
    fn join_split_roundtrip_with_stdin() {
        let encoded = join_percent("cat ", Some("hello\nworld"));
        assert_eq!(encoded, "cat %hello%world");
        let (cmd, stdin) = split_percent(&encoded);
        assert_eq!(cmd, "cat ");
        assert_eq!(stdin.as_deref(), Some("hello\nworld"));
    }

    #[test]
    fn special_daily() {
        let text = "@daily /bin/true\n";
        let lines = parse_crontab(text, CrontabMode::User);
        match &lines[0] {
            CrontabLine::Job { schedule, .. } => {
                assert_eq!(schedule.expression(), "@daily");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn env_and_comments_preserved() {
        let text = "# keep me\n\nSHELL=/bin/bash\nPATH=/usr/bin\n0 * * * * echo hi\n";
        let lines = parse_crontab(text, CrontabMode::User);
        assert!(matches!(lines[0], CrontabLine::Comment { .. }));
        assert!(matches!(lines[1], CrontabLine::Blank { .. }));
        assert!(matches!(lines[2], CrontabLine::Env { .. }));
        let rebuilt = join_lines(
            &lines.iter().map(|l| l.raw().to_string()).collect::<Vec<_>>(),
            true,
        );
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn roundtrip_preserves_hand_written() {
        let text = "# hand comment\n\n0 1 * * * /bin/true\n# trailing\n";
        let lines = parse_crontab(text, CrontabMode::User);
        let rebuilt = join_lines(
            &lines.iter().map(|l| l.raw().to_string()).collect::<Vec<_>>(),
            had_trailing_newline(text),
        );
        assert_eq!(rebuilt, text);
    }
}
