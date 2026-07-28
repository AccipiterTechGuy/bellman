//! Web DTO layer — the **deliberate** wire shape the GUI consumes.
//!
//! Why this exists: the core `Timer` serializes via `#[serde]` into a
//! deeply nested JSON shape (`TimerDto` from `croner` would have been
//! similar). For example the core's `OccurrenceKind` is a tagged enum
//! `{"occ":"weekly","days":21,"at":"08:00:00"}` wrapped inside an outer
//! `Occurrence` with a `kind:` field, plus DST/validity fields at the
//! top level. That's correct persistence, but it makes the GUI editor's
//! prefill messy (the React/Svelte equivalent of `t.occurrence.kind.occ`
//! and decoding the numeric weekday bitmask into a checkbox record).
//!
//! This module flattens that to a single object the dialog reads with no
//! path gymnastics:
//!
//!   {
//!     id, name, enabled, tz, nextFireUtc, lastFired, summary, action,
//!     revision,
//!     occurrence: {
//!       occ: "weekly",
//!       tz: "UTC",
//!       days: { mon: true, wed: true, fri: true },  // bitmask object
//!       at:    "08:00:00",                          // NaiveTime → HH:MM:SS
//!       onceAt: null,                              // only for "once"
//!       everySecs: null,                            // only for "interval"
//!       anchor: null,                               // only for "interval"
//!       day: null, month: null,                    // only for monthly/yearly
//!       expr: null,                                 // only for "cron"
//!     },
//!     actionKind: { type: "launch" | "notify" | "none", title?,
//!                   body?, command?, args? }
//!   }
//!
//! The shape is pinned by `tests/testdata/ui_weekly_dto.json` and the
//! `WebTimerDto` JSON assertions in `src-tauri/src/dto_serde_tests.rs`.
//! Every JS consumer in `ui/src/` reads fields from this flat shape.
//!
//! Conversion back to core is hand-rolled (`into_core_patch` on the
//! patch DTO) so the GUI can save without any nested-merge gymnastics.

use bellman_core::occurrence::{OccurrenceKind, Weekdays};
use bellman_core::store::{
    Action, MisfirePolicy, NewTimer, OverlapPolicy, RetryPolicy, Timer, TimerPatch,
};
use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Web-side (camelCase) timer DTO. Deliberately NOT derived from the core
/// `Timer` so the wire shape stays locked and auditable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebTimerDto {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub tz: String,
    pub next_fire_utc: Option<DateTime<Utc>>,
    pub last_fired: Option<DateTime<Utc>>,
    pub kind: String,
    pub summary: String,
    pub action: String,
    pub revision: i64,
    pub occurrence: WebOccurrenceDto,
    pub action_kind: WebActionDto,
    /// Participate in RTC single-next-wake election (default false).
    #[serde(default)]
    pub wake_machine: bool,
}

/// Web-side occurrence DTO. All kind-specific fields are nullable; only
/// the ones relevant to the `occ` discriminator are populated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebOccurrenceDto {
    pub occ: String,
    pub tz: String,
    /// `{mon: true, tue: true, ...}` — booleans for the 7 ISO weekdays.
    /// Populated only when `occ == "weekly"`; null otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<BTreeMap<String, bool>>,
    /// NaiveTime serialized as HH:MM:SS. Used by daily/weekly/monthly/yearly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// For `once`: the naive datetime as ISO 8601 without offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub once_at: Option<String>,
    /// For `interval`: seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_secs: Option<u64>,
    /// For `interval`: anchor UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<DateTime<Utc>>,
    /// For `monthly`/`yearly`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day: Option<u8>,
    /// For `yearly`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub month: Option<u8>,
    /// For `cron`: the cron expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
}

/// Web-side action DTO. Tagged by `type` so the dialog radio switch works.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", rename_all_fields = "snake_case")]
pub enum WebActionDto {
    None,
    Launch {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<String>,
    },
    Notify {
        title: String,
        #[serde(default)]
        body: String,
    },
}

/// Web-side patch DTO used by `update_timer`. Every field is optional —
/// `null` means "leave unchanged".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebTimerPatchDto {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub occurrence: Option<WebOccurrenceDto>,
    pub action_kind: Option<WebActionDto>,
    pub wake_machine: Option<bool>,
}

impl WebTimerPatchDto {
    /// Convert to the core's `TimerPatch`, preserving any unset fields
    /// (the dialog never sends fields the user did not edit).
    pub fn into_core_patch(self) -> Result<TimerPatch, String> {
        let occurrence = self
            .occurrence
            .map(WebOccurrenceDto::into_core_occurrence)
            .transpose()?;
        let action = self.action_kind.map(WebActionDto::into_core_action);
        Ok(TimerPatch {
            name: self.name,
            enabled: self.enabled,
            occurrence,
            action,
            wake_machine: self.wake_machine,
            ..Default::default()
        })
    }
}

impl WebActionDto {
    pub fn into_core_action(self) -> Action {
        match self {
            Self::None => Action::None,
            Self::Launch { command, args, workdir } => Action::Launch {
                command,
                args,
                workdir,
            },
            Self::Notify { title, body } => Action::Notify { title, body },
        }
    }
}

impl WebOccurrenceDto {
    /// Build the core `Occurrence` from this flat DTO. Validates (so the
    /// dialog rejects malformed input before it hits the store) and
    /// applies the product-default policies.
    pub fn into_core_occurrence(self) -> Result<bellman_core::Occurrence, String> {
        let tz_name = if self.tz.is_empty() {
            // No system-tz resolver here (the GUI path resolves it before
            // sending); fall back to UTC so the build always succeeds.
            "UTC".to_string()
        } else {
            self.tz
        };
        let kind = match self.occ.as_str() {
            "once" => {
                let once_at = self.once_at.as_deref().ok_or_else(|| {
                    "occurrence 'once' requires onceAt (YYYY-MM-DDTHH:MM:SS)".to_string()
                })?;
                let at = crate::occurrence_input::parse_once_at(once_at, &tz_name)?;
                OccurrenceKind::Once { at }
            }
            "interval" => {
                let every_secs = self.every_secs.ok_or_else(|| {
                    "occurrence 'interval' requires everySecs".to_string()
                })?;
                let anchor = self.anchor.unwrap_or_else(Utc::now);
                OccurrenceKind::Interval { every_secs, anchor }
            }
            "daily" => {
                let t = self
                    .at
                    .as_deref()
                    .ok_or_else(|| "occurrence 'daily' requires at (HH:MM:SS)".to_string())?;
                OccurrenceKind::Daily {
                    at: crate::occurrence_input::parse_clock_time(t)?,
                }
            }
            "weekly" => {
                let t = self
                    .at
                    .as_deref()
                    .ok_or_else(|| "occurrence 'weekly' requires at (HH:MM:SS)".to_string())?;
                let wd = decode_days(self.days.as_ref())?;
                OccurrenceKind::Weekly {
                    days: wd,
                    at: crate::occurrence_input::parse_clock_time(t)?,
                }
            }
            "monthly" => {
                let t = self
                    .at
                    .as_deref()
                    .ok_or_else(|| "occurrence 'monthly' requires at".to_string())?;
                let d = self.day.ok_or_else(|| {
                    "occurrence 'monthly' requires day (1..=31)".to_string()
                })?;
                OccurrenceKind::Monthly {
                    day: d,
                    at: crate::occurrence_input::parse_clock_time(t)?,
                }
            }
            "yearly" => {
                let t = self
                    .at
                    .as_deref()
                    .ok_or_else(|| "occurrence 'yearly' requires at".to_string())?;
                let m = self
                    .month
                    .ok_or_else(|| "occurrence 'yearly' requires month".to_string())?;
                let d = self.day.ok_or_else(|| {
                    "occurrence 'yearly' requires day".to_string()
                })?;
                OccurrenceKind::Yearly {
                    month: m,
                    day: d,
                    at: crate::occurrence_input::parse_clock_time(t)?,
                }
            }
            "cron" => {
                let expr = self
                    .expr
                    .as_deref()
                    .ok_or_else(|| "occurrence 'cron' requires expr".to_string())?
                    .to_string();
                OccurrenceKind::Cron { expr }
            }
            other => {
                return Err(format!(
                    "unknown occurrence kind '{other}' (expected once|interval|daily|weekly|monthly|yearly|cron)"
                ))
            }
        };
        let occ = bellman_core::Occurrence::new(kind, tz_name)?;
        // GUI never sends policies; defaults are already applied.
        Ok(occ)
    }
}

/// Wire-friendly kind→occurrence helpers (used by `update_timer`'s
/// `NewTimer` builder and by tests).
impl WebOccurrenceDto {
    /// Convert to a `NewTimer`-style flat input that `occurrence_input`
    /// can consume. Kept for the rare case where the GUI wants to send
    /// a partial "create" alongside the patch.
    pub fn into_core_new_timer(
        self,
        name: String,
    ) -> Result<NewTimer, String> {
        let occurrence = self.into_core_occurrence()?;
        let mut new = NewTimer::new(name, occurrence);
        new.tags = Vec::new();
        new.action = Action::None;
        new.misfire = MisfirePolicy::default_calendar();
        new.overlap = OverlapPolicy::default();
        new.retry = RetryPolicy::default();
        new.last_fired = None;
        Ok(new)
    }
}

/// Decode `{mon: true, ...}` into a `Weekdays` bitmask. Public so the
/// patch round-trip can validate and apply.
pub fn decode_days(
    days: Option<&BTreeMap<String, bool>>,
) -> Result<Weekdays, String> {
    let mut bits: u8 = 0;
    let mut any = false;
    if let Some(map) = days {
        for (k, v) in map {
            if !*v {
                continue;
            }
            let bit = match k.as_str() {
                "mon" => 0b0000_0001u8,
                "tue" => 0b0000_0010u8,
                "wed" => 0b0000_0100u8,
                "thu" => 0b0000_1000u8,
                "fri" => 0b0001_0000u8,
                "sat" => 0b0010_0000u8,
                "sun" => 0b0100_0000u8,
                other => return Err(format!("unknown weekday key '{other}' in days map")),
            };
            bits |= bit;
            any = true;
        }
    }
    if !any {
        return Err("weekly schedule requires at least one weekday".into());
    }
    Ok(Weekdays::from_u8(bits))
}

/// Encode a `Weekdays` bitmask as `{mon: true, ...}`. Always emits the
/// seven entries (booleans, missing → false) so the GUI can render all
/// checkboxes uniformly.
pub fn encode_days(days: Weekdays) -> BTreeMap<String, bool> {
    let bits = days.as_u8();
    let mut out = BTreeMap::new();
    for (k, bit) in [
        ("mon", 0b0000_0001u8),
        ("tue", 0b0000_0010u8),
        ("wed", 0b0000_0100u8),
        ("thu", 0b0000_1000u8),
        ("fri", 0b0001_0000u8),
        ("sat", 0b0010_0000u8),
        ("sun", 0b0100_0000u8),
    ] {
        out.insert(k.to_string(), bits & bit != 0);
    }
    out
}

/// Format a `NaiveTime` as HH:MM:SS (matches chrono's `to_string`).
pub fn fmt_clock(t: NaiveTime) -> String {
    t.format("%H:%M:%S").to_string()
}

/// Format a `NaiveDateTime` (in some tz) as ISO 8601 without offset.
pub fn fmt_naive_local(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// Convert a `Timer` from the store into our web-friendly DTO. Centralizes
/// the projection so the wire shape stays in one place.
impl From<Timer> for WebTimerDto {
    fn from(t: Timer) -> Self {
        let kind = t.occurrence.kind().kind_label().to_string();
        let summary = t.occurrence_summary();
        let action = t.action_summary();
        let occurrence = match t.occurrence.kind() {
            OccurrenceKind::Once { at } => WebOccurrenceDto {
                occ: "once".into(),
                tz: t.occurrence.tz_name().to_string(),
                days: None,
                at: None,
                once_at: Some(fmt_naive_local(*at)),
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: None,
            },
            OccurrenceKind::Interval {
                every_secs,
                anchor,
            } => WebOccurrenceDto {
                occ: "interval".into(),
                tz: t.occurrence.tz_name().to_string(),
                days: None,
                at: None,
                once_at: None,
                every_secs: Some(*every_secs),
                anchor: Some(*anchor),
                day: None,
                month: None,
                expr: None,
            },
            OccurrenceKind::Daily { at } => WebOccurrenceDto {
                occ: "daily".into(),
                tz: t.occurrence.tz_name().to_string(),
                days: None,
                at: Some(fmt_clock(*at)),
                once_at: None,
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: None,
            },
            OccurrenceKind::Weekly { days, at } => WebOccurrenceDto {
                occ: "weekly".into(),
                tz: t.occurrence.tz_name().to_string(),
                days: Some(encode_days(*days)),
                at: Some(fmt_clock(*at)),
                once_at: None,
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: None,
            },
            OccurrenceKind::Monthly { day, at } => WebOccurrenceDto {
                occ: "monthly".into(),
                tz: t.occurrence.tz_name().to_string(),
                days: None,
                at: Some(fmt_clock(*at)),
                once_at: None,
                every_secs: None,
                anchor: None,
                day: Some(*day),
                month: None,
                expr: None,
            },
            OccurrenceKind::Yearly { month, day, at } => WebOccurrenceDto {
                occ: "yearly".into(),
                tz: t.occurrence.tz_name().to_string(),
                days: None,
                at: Some(fmt_clock(*at)),
                once_at: None,
                every_secs: None,
                anchor: None,
                day: Some(*day),
                month: Some(*month),
                expr: None,
            },
            OccurrenceKind::Cron { expr } => WebOccurrenceDto {
                occ: "cron".into(),
                tz: t.occurrence.tz_name().to_string(),
                days: None,
                at: None,
                once_at: None,
                every_secs: None,
                anchor: None,
                day: None,
                month: None,
                expr: Some(expr.clone()),
            },
        };
        let action_kind = match &t.action {
            Action::None => WebActionDto::None,
            Action::Launch {
                command,
                args,
                workdir,
            } => WebActionDto::Launch {
                command: command.clone(),
                args: args.clone(),
                workdir: workdir.clone(),
            },
            Action::Notify { title, body } => WebActionDto::Notify {
                title: title.clone(),
                body: body.clone(),
            },
        };
        Self {
            id: t.id.to_string(),
            name: t.name.clone(),
            enabled: t.enabled,
            tz: t.tz.clone(),
            next_fire_utc: t.next_fire_utc,
            last_fired: t.last_fired,
            kind,
            summary,
            action,
            revision: t.revision,
            occurrence,
            action_kind,
            wake_machine: t.wake_machine,
        }
    }
}

/// Helper extension on `Timer` to derive the human summary lines used
/// by the GUI table.
trait TimerSummaryExt {
    fn occurrence_summary(&self) -> String;
    fn action_summary(&self) -> String;
}

impl TimerSummaryExt for Timer {
    fn occurrence_summary(&self) -> String {
        use bellman_core::OccurrenceKind::*;
        let tz = &self.tz;
        match self.occurrence.kind() {
            Interval { every_secs, .. } => format!("every {every_secs}s"),
            Daily { at } => format!("daily {} {tz}", at.format("%H:%M:%S")),
            Weekly { days, at } => format!(
                "weekly {} {} {tz}",
                days
                    .iter()
                    .map(|d| match d {
                        chrono::Weekday::Mon => "mon",
                        chrono::Weekday::Tue => "tue",
                        chrono::Weekday::Wed => "wed",
                        chrono::Weekday::Thu => "thu",
                        chrono::Weekday::Fri => "fri",
                        chrono::Weekday::Sat => "sat",
                        chrono::Weekday::Sun => "sun",
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                at.format("%H:%M:%S"),
            ),
            Monthly { day, at } => format!("monthly day {day} {}", at.format("%H:%M:%S")),
            Yearly { month, day, at } => format!("yearly {month}-{day} {}", at.format("%H:%M:%S")),
            Once { at } => format!("once {at} {tz}"),
            Cron { expr } => format!("cron `{expr}` {tz}"),
        }
    }
    fn action_summary(&self) -> String {
        match &self.action {
            Action::None => "none".into(),
            Action::Launch { command, args, .. } => {
                if args.is_empty() {
                    format!("launch: {command}")
                } else {
                    format!("launch: {command} {}", args.join(" "))
                }
            }
            Action::Notify { title, .. } => format!("notify: {title}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bellman_core::occurrence::OccurrenceKind;

    #[test]
    fn weekly_dto_matches_pinned_json_fixture() {
        let mut wd = Weekdays::new();
        wd.insert(chrono::Weekday::Mon);
        wd.insert(chrono::Weekday::Wed);
        wd.insert(chrono::Weekday::Fri);
        let occ = bellman_core::Occurrence::new(
            OccurrenceKind::Weekly { days: wd, at: NaiveTime::from_hms_opt(8, 0, 0).unwrap() },
            "Europe/Helsinki",
        )
        .unwrap();
        // Pin the schedule to the fixture date so the comparison is
        // deterministic regardless of when the test runs.
        let anchor = chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let timer = Timer {
            id: uuid::Uuid::nil(),
            name: "weekly-mwf".into(),
            enabled: true,
            occurrence: occ,
            tz: "Europe/Helsinki".into(),
            next_fire_utc: Some(anchor),
            last_fired: None,
            misfire: MisfirePolicy::default_calendar(),
            overlap: OverlapPolicy::default(),
            retry: RetryPolicy::default(),
            valid_from: None,
            valid_until: None,
            max_runs: None,
            tags: vec![],
            action: Action::Notify {
                title: "hello".into(),
                body: "world".into(),
            },
            revision: 1,
            jitter_secs: 0,
            accuracy_slack_secs: None,
            wake_machine: false,
        };
        let dto: WebTimerDto = timer.into();
        let json = serde_json::to_string_pretty(&dto).unwrap();
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("web_testdata")
            .join("weekly_dto.json");
        let fixture = std::fs::read_to_string(&fixture_path)
            .expect("weekly_dto.json fixture must exist");
        let expected: serde_json::Value =
            serde_json::from_str(&fixture).expect("fixture must be valid JSON");
        let actual: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(actual, expected, "WebTimerDto JSON drifted; update fixture or revert the diff");
    }

    #[test]
    fn decode_days_roundtrips_bitmask() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("mon".into(), true);
        map.insert("wed".into(), true);
        map.insert("fri".into(), true);
        let wd = decode_days(Some(&map)).unwrap();
        // Mon=1 + Wed=4 + Fri=16 = 21
        assert_eq!(wd.as_u8(), 0b0001_0101u8);
        // encode_days emits all seven keys so the GUI sees a uniform
        // record shape (true/false for every day); the round map adds
        // the four inactive entries.
        let mut expected = std::collections::BTreeMap::new();
        for (k, bit) in [
            ("mon", 0b0000_0001u8),
            ("tue", 0b0000_0010u8),
            ("wed", 0b0000_0100u8),
            ("thu", 0b0000_1000u8),
            ("fri", 0b0001_0000u8),
            ("sat", 0b0010_0000u8),
            ("sun", 0b0100_0000u8),
        ] {
            expected.insert(k.into(), (bit & 0b0001_0101u8) != 0);
        }
        let round = encode_days(wd);
        assert_eq!(round, expected);
    }

    #[test]
    fn decode_days_rejects_empty() {
        assert!(decode_days(None).is_err());
        let mut empty = std::collections::BTreeMap::new();
        empty.insert("mon".into(), false);
        assert!(decode_days(Some(&empty)).is_err());
    }

    #[test]
    fn decode_days_rejects_unknown_key() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("fri".into(), true);
        m.insert("sunday".into(), true);
        assert!(decode_days(Some(&m)).is_err());
    }

    #[test]
    fn weekly_at_decode_via_bitmask() {
        // The wire shape is `days: {mon:true, ...}` and we must be able
        // to recompute the same ISO weekday list. Used by WeekPage.
        let mut map = std::collections::BTreeMap::new();
        map.insert("mon".into(), true);
        map.insert("wed".into(), true);
        map.insert("fri".into(), true);
        let wd = decode_days(Some(&map)).unwrap();
        let dow_list: Vec<u32> = wd.iter().map(|d| d.num_days_from_monday() + 1).collect();
        assert_eq!(dow_list, vec![1, 3, 5]);
    }
}
