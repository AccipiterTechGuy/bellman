//! Cancellation tokens for action-policy cancellation (SCH1 `Replace`).
//!
//! A token is signalled by the dispatcher when the durable `cancel_requested`
//! flag appears on an active claim (the `Replace` fire transaction set it —
//! possibly in another process; SQLite is the handoff). The launch
//! `try_wait` loop and the retry backoff observe the token and stop early.
//! This is action-policy cancellation, unrelated to IK3's watchdog rule that
//! never kills an app.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared cancellation flag for one executing claim.
#[derive(Debug, Default)]
pub struct CancellationToken {
    cancelled: AtomicBool,
}

impl CancellationToken {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Signal cancellation (idempotent).
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_sticky_and_visible() {
        let t = CancellationToken::new();
        assert!(!t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
    }
}
