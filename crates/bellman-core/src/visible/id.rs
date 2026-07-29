//! Stable task identifiers for discovered schedules.

use crate::visible::types::SourceKind;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Build a deterministic id from source kind + stable key material.
///
/// Format: `{kind}:{hex16}` where hex is a hash of the key parts so ids stay
/// short and stable across scans when the underlying entry is unchanged.
pub fn task_id(kind: SourceKind, parts: &[&str]) -> String {
    let mut h = DefaultHasher::new();
    kind.as_str().hash(&mut h);
    for p in parts {
        p.hash(&mut h);
    }
    let hash = h.finish();
    format!("{}:{:016x}", kind.as_str(), hash)
}

/// Short content fingerprint (for disable markers / fence ids).
pub fn short_hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:08x}", (h.finish() as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_across_calls() {
        let a = task_id(SourceKind::CronUser, &["sami", "0 * * * *", "echo hi"]);
        let b = task_id(SourceKind::CronUser, &["sami", "0 * * * *", "echo hi"]);
        assert_eq!(a, b);
        assert!(a.starts_with("cron_user:"));
    }

    #[test]
    fn different_inputs_differ() {
        let a = task_id(SourceKind::CronUser, &["sami", "0 * * * *", "a"]);
        let b = task_id(SourceKind::CronUser, &["sami", "0 * * * *", "b"]);
        assert_ne!(a, b);
    }
}
