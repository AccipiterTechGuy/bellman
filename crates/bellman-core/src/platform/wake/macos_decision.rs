//! macOS capability decision tree — pure function over SMAppService + sentinel.
//!
//! Unit-tested against mocked API answers (real hardware QA lands in C11).
//! See synthesis §2-macOS and §3.

use super::{Caveat, DisabledReason, WakeCapability, WakeMechanism};

/// SMAppService.daemon.status equivalents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperStatus {
    Enabled,
    RequiresApproval,
    NotRegistered,
    NotFound,
}

/// Snapshot of macOS probe facts.
#[derive(Debug, Clone)]
pub struct MacosProbeFacts {
    pub helper: HelperStatus,
    /// Through-daemon sentinel schedule/cancel round-trip succeeded.
    pub sentinel_ok: bool,
    /// Running on Apple Silicon.
    pub apple_silicon: bool,
    pub sentinel_error: Option<String>,
}

/// Pure decision tree matching synthesis §2-macOS.
pub fn decide(facts: &MacosProbeFacts) -> WakeCapability {
    match facts.helper {
        HelperStatus::RequiresApproval => WakeCapability::Disabled {
            reason: DisabledReason::HelperAwaitingApproval,
        },
        HelperStatus::NotRegistered | HelperStatus::NotFound => WakeCapability::Disabled {
            reason: DisabledReason::HelperNotInstalled,
        },
        HelperStatus::Enabled => {
            if !facts.sentinel_ok {
                return WakeCapability::Disabled {
                    reason: DisabledReason::ProbeFailed {
                        detail: facts
                            .sentinel_error
                            .clone()
                            .unwrap_or_else(|| "daemon sentinel schedule/cancel failed".into()),
                    },
                };
            }
            let mut caveats = vec![Caveat::HibernateNotGuaranteed];
            if facts.apple_silicon {
                caveats.push(Caveat::AppleSiliconBestEffort);
            }
            WakeCapability::Enabled {
                mechanism: WakeMechanism::MacPmDaemon,
                caveats,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_enabled_sentinel_ok() {
        let cap = decide(&MacosProbeFacts {
            helper: HelperStatus::Enabled,
            sentinel_ok: true,
            apple_silicon: false,
            sentinel_error: None,
        });
        assert!(cap.is_enabled());
        match cap {
            WakeCapability::Enabled { mechanism, .. } => {
                assert_eq!(mechanism, WakeMechanism::MacPmDaemon);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn apple_silicon_caveat() {
        let cap = decide(&MacosProbeFacts {
            helper: HelperStatus::Enabled,
            sentinel_ok: true,
            apple_silicon: true,
            sentinel_error: None,
        });
        match cap {
            WakeCapability::Enabled { caveats, .. } => {
                assert!(caveats.contains(&Caveat::AppleSiliconBestEffort));
            }
            _ => panic!("expected Enabled"),
        }
    }

    #[test]
    fn awaiting_approval_is_optional_not_broken() {
        let cap = decide(&MacosProbeFacts {
            helper: HelperStatus::RequiresApproval,
            sentinel_ok: false,
            apple_silicon: false,
            sentinel_error: None,
        });
        match cap {
            WakeCapability::Disabled {
                reason: DisabledReason::HelperAwaitingApproval,
            } => {
                let line = cap.status_line();
                assert!(line.contains("Login Items"));
                // Fix hint present so Settings can deep-link.
                assert!(reason_fix_hint(&cap).is_some());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn not_registered() {
        let cap = decide(&MacosProbeFacts {
            helper: HelperStatus::NotRegistered,
            sentinel_ok: false,
            apple_silicon: false,
            sentinel_error: None,
        });
        match cap {
            WakeCapability::Disabled {
                reason: DisabledReason::HelperNotInstalled,
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sentinel_failure_when_enabled() {
        let cap = decide(&MacosProbeFacts {
            helper: HelperStatus::Enabled,
            sentinel_ok: false,
            apple_silicon: false,
            sentinel_error: Some("xpc timeout".into()),
        });
        match cap {
            WakeCapability::Disabled {
                reason: DisabledReason::ProbeFailed { detail },
            } => assert!(detail.contains("xpc")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    fn reason_fix_hint(cap: &WakeCapability) -> Option<&'static str> {
        match cap {
            WakeCapability::Disabled { reason } => reason.fix_hint(),
            _ => None,
        }
    }
}
