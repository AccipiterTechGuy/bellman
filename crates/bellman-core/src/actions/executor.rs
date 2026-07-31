//! [`ActionExecutor`]: the thread-safe half of the old `ActionRunner` (SCH1).
//!
//! Owns only the immutable action inputs (timeouts, caps), the notification
//! sink and the shared `Arc<ActionLimiter>` — no `in_flight` set, no
//! `last_message`, no event-log handle behind a mutex. The durable overlap
//! decision lives in the fire transaction; lane order lives in the
//! dispatcher; this type just executes `Launch` / `Notify` / `None` with
//! retries and reports an outcome. Every worker lane can hold one at once.

use super::cancel::CancellationToken;
use super::concurrency::ActionLimiter;
use super::launch::{run_launch, LaunchConfig, DEFAULT_OUTPUT_CAP_BYTES, DEFAULT_TIMEOUT};
use super::notify_sink::{NotifySink, StubNotifySink};
use crate::store::{Action, Timer};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Executor configuration (timeouts / caps / test knobs).
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    pub launch_timeout: Duration,
    pub output_cap: usize,
    /// When true (tests), sleep for retry delay is skipped — only the retry
    /// *count* is honored. Production leaves this false.
    pub skip_retry_sleep: bool,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            launch_timeout: DEFAULT_TIMEOUT,
            output_cap: DEFAULT_OUTPUT_CAP_BYTES,
            skip_retry_sleep: false,
        }
    }
}

/// The result of executing one claim's configured action (with retries).
#[derive(Debug, Clone)]
pub enum ExecOutcome {
    /// Delivered; carries the human-readable summary (run-now message and
    /// the `wake_delivered` event message) and the retries used.
    Delivered { message: String, attempts: u32 },
    /// Failed after retries; carries the error and the retry count used.
    Failed { error: String, attempts: u32 },
    /// The cancellation token interrupted the run (`Replace`) — mid-launch
    /// or during retry backoff. Recorded `wake_failed(overlap_replace)`.
    Cancelled,
}

/// Executes wake actions under the shared global limiter. Cheap to clone;
/// every field is shared or immutable.
#[derive(Clone)]
pub struct ActionExecutor {
    config: ExecutorConfig,
    notify_sink: Arc<dyn NotifySink>,
    limiter: Arc<ActionLimiter>,
}

impl ActionExecutor {
    pub fn new(config: ExecutorConfig, notify_sink: Arc<dyn NotifySink>, limiter: Arc<ActionLimiter>) -> Self {
        Self {
            config,
            notify_sink,
            limiter,
        }
    }

    /// Default sink (stub) constructor.
    pub fn with_defaults(config: ExecutorConfig, limiter: Arc<ActionLimiter>) -> Self {
        Self::new(config, Arc::new(StubNotifySink), limiter)
    }

    /// Shared concurrency limiter (tests inspect peak / completed counts).
    pub fn limiter(&self) -> &ActionLimiter {
        &self.limiter
    }

    /// Current notification sink (tests assert a fire surfaced a toast).
    pub fn notify_sink(&self) -> &Arc<dyn NotifySink> {
        &self.notify_sink
    }

    /// Execute the timer's action for one run, honoring the retry policy and
    /// the cancellation token (checked before each attempt, inside the
    /// launch `try_wait` loop, and across the retry backoff).
    pub fn execute(
        &self,
        timer: &Timer,
        run_id: Uuid,
        cancel: &Arc<CancellationToken>,
    ) -> ExecOutcome {
        let max_retries = timer.retry.max_retries;
        let delay = Duration::from_secs(timer.retry.delay_secs);
        let mut attempt = 0u32;

        loop {
            if cancel.is_cancelled() {
                return ExecOutcome::Cancelled;
            }
            match self.execute_once(timer, run_id, cancel) {
                Ok(msg) => {
                    return ExecOutcome::Delivered {
                        message: msg,
                        attempts: attempt,
                    }
                }
                Err(e) if e == CANCELLED => return ExecOutcome::Cancelled,
                Err(e) => {
                    if attempt >= max_retries {
                        return ExecOutcome::Failed {
                            error: e,
                            attempts: attempt,
                        };
                    }
                    attempt += 1;
                    if !self.config.skip_retry_sleep && !delay.is_zero() {
                        // Cancellable backoff: the same token that kills a
                        // launch interrupts the wait and prevents another
                        // attempt. Sleep in 20 ms slices like the launch loop.
                        let start = std::time::Instant::now();
                        while start.elapsed() < delay {
                            if cancel.is_cancelled() {
                                return ExecOutcome::Cancelled;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                }
            }
        }
    }

    /// One attempt under a global concurrency permit.
    fn execute_once(
        &self,
        timer: &Timer,
        run_id: Uuid,
        cancel: &Arc<CancellationToken>,
    ) -> Result<String, String> {
        let launch_timeout = self.config.launch_timeout;
        let output_cap = self.config.output_cap;
        let notify_sink = Arc::clone(&self.notify_sink);
        let action = timer.action.clone();
        let cancel = Arc::clone(cancel);

        self.limiter.run(|| -> Result<String, String> {
            match &action {
                Action::None => Ok("action=none".into()),
                Action::Launch {
                    command,
                    args,
                    workdir,
                } => {
                    let cfg = LaunchConfig {
                        command: command.clone(),
                        args: args.clone(),
                        workdir: workdir.clone(),
                        timeout: launch_timeout,
                        output_cap,
                        run_id,
                        cancel: Some(cancel),
                    };
                    // Timeout + output cap are enforced inside run_launch.
                    let outcome = run_launch(&cfg).map_err(|e| e.to_string())?;
                    if outcome.cancelled {
                        return Err(CANCELLED.to_string());
                    }
                    if outcome.timed_out {
                        return Err(format!(
                            "launch timed out after {launch_timeout:?} (killed={})",
                            outcome.killed
                        ));
                    }
                    match outcome.exit_code {
                        Some(0) => Ok(format!(
                            "launch ok exit=0 duration={:?}",
                            outcome.duration
                        )),
                        None => Err("launch exited by signal".into()),
                        Some(code) => Err(format!(
                            "launch exit={code} output={}",
                            truncate_utf8(&outcome.output, 200)
                        )),
                    }
                }
                Action::Notify { title, body } => {
                    let out = notify_sink.show(title, body);
                    let label = if notify_sink.is_stub() {
                        "notify stub"
                    } else {
                        "notify"
                    };
                    Ok(format!("{label} title={:?}", out.title))
                }
            }
        })
    }
}

/// Sentinel error string for a cancelled launch (never surfaces to users —
/// mapped to [`ExecOutcome::Cancelled`]).
const CANCELLED: &str = "cancelled by overlap policy (replace)";

/// Truncate to at most `max` bytes on a UTF-8 char boundary (never panics).
fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate_utf8;

    #[test]
    fn truncate_utf8_does_not_panic_on_multibyte() {
        // Euro sign is 3 bytes; 100 of them = 300 bytes. Cap at 200 must not panic.
        let s = "€".repeat(100);
        let out = truncate_utf8(&s, 200);
        assert!(out.ends_with('…'));
        assert!(out.is_char_boundary(out.len() - '…'.len_utf8()) || out.ends_with('…'));
        // Re-parse as valid UTF-8 (already a String).
        assert!(out.chars().all(|c| c == '€' || c == '…'));
    }

    #[test]
    fn truncate_utf8_short_passthrough() {
        assert_eq!(truncate_utf8("hi", 200), "hi");
    }
}
