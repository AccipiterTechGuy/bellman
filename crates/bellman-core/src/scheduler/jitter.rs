//! Per-timer execution jitter and accuracy slack.
//!
//! Jitter (`±jitter_secs`) is applied to **execution** time only — the stored
//! and displayed `next_fire_utc` stays on the clean civil/interval schedule.
//! Accuracy slack (systemd `AccuracySec` analogue) lets high-frequency timers
//! fire a little early so the loop can batch wakeups.

use crate::occurrence::OccurrenceKind;
use crate::scheduler::config::HIGH_FREQ_PERIOD_SECS;
use crate::store::Timer;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;
use uuid::Uuid;

/// Deterministic offset in `[-jitter_secs, +jitter_secs]` derived from
/// `(timer_id, scheduled_for)`. Determinism keeps tests stable and still
/// spreads load across a fleet of high-frequency timers.
pub fn jitter_offset_secs(timer_id: Uuid, scheduled_for: DateTime<Utc>, jitter_secs: u32) -> i64 {
    if jitter_secs == 0 {
        return 0;
    }
    let mut h = DefaultHasher::new();
    timer_id.hash(&mut h);
    scheduled_for
        .timestamp_nanos_opt()
        .unwrap_or(0)
        .hash(&mut h);
    let raw = h.finish();
    let span = i64::from(jitter_secs).saturating_mul(2).saturating_add(1);
    // span is at least 1; rem is in 0..span
    let rem = i64::try_from(raw % u64::try_from(span).unwrap_or(1)).unwrap_or(0);
    rem - i64::from(jitter_secs)
}

/// Apply execution jitter to a clean fire instant.
///
/// Returns `fire_at + offset` where `offset ∈ [-jitter, +jitter]`.
pub fn apply_execution_jitter(
    timer_id: Uuid,
    fire_at: DateTime<Utc>,
    jitter_secs: u32,
) -> DateTime<Utc> {
    let off = jitter_offset_secs(timer_id, fire_at, jitter_secs);
    fire_at + ChronoDuration::seconds(off)
}

/// Resolve the accuracy slack for a timer (per-timer override or global default).
pub fn accuracy_slack_for(timer: &Timer, global_default: Duration) -> Duration {
    match timer.accuracy_slack_secs {
        Some(s) => Duration::from_secs(u64::from(s)),
        None if is_high_frequency(timer) => global_default,
        None => Duration::ZERO,
    }
}

/// High-frequency interval (period &lt; 5 minutes).
pub fn is_high_frequency(timer: &Timer) -> bool {
    match timer.occurrence.kind() {
        OccurrenceKind::Interval { every_secs, .. } => *every_secs < HIGH_FREQ_PERIOD_SECS,
        _ => false,
    }
}

/// Effective due threshold: fire when `now + slack >= fire_at` (allows early
/// fire within accuracy). For zero slack this is plain `now >= fire_at`.
pub fn is_due_with_accuracy(fire_at: DateTime<Utc>, now: DateTime<Utc>, slack: Duration) -> bool {
    let slack_c = ChronoDuration::from_std(slack).unwrap_or(ChronoDuration::zero());
    now + slack_c >= fire_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn jitter_within_bounds() {
        let id = Uuid::new_v4();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        for i in 0..200 {
            let at = t0 + ChronoDuration::seconds(i);
            let off = jitter_offset_secs(id, at, 5);
            assert!((-5..=5).contains(&off), "offset {off} out of ±5");
        }
    }

    #[test]
    fn zero_jitter_is_identity() {
        let id = Uuid::nil();
        let at = Utc::now();
        assert_eq!(apply_execution_jitter(id, at, 0), at);
        assert_eq!(jitter_offset_secs(id, at, 0), 0);
    }

    #[test]
    fn accuracy_due_early_within_slack() {
        let fire = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let now = fire - ChronoDuration::milliseconds(500);
        assert!(is_due_with_accuracy(fire, now, Duration::from_secs(1)));
        assert!(!is_due_with_accuracy(fire, now, Duration::ZERO));
    }
}
