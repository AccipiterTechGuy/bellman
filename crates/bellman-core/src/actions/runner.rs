//! [`ActionRunner`]: [`FireAction`] impl with overlap, retry, and event logging.

use super::concurrency::{ActionLimiter, DEFAULT_MAX_CONCURRENT_ACTIONS};
use super::launch::{run_launch, LaunchConfig, DEFAULT_OUTPUT_CAP_BYTES, DEFAULT_TIMEOUT};
use super::notify_sink::{NotifySink, StubNotifySink};
use super::write_slot::{write_output_slot, WriteSlotPayload, FIRE_SCHEMA_V1};
use crate::events::{EventRecord, RunState};
use crate::scheduler::{FireAction, FireContext, FireKind};
use crate::store::{Action, OverlapPolicy, TimerId};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Runner configuration (timeouts / caps / optional write-slot root).
#[derive(Debug, Clone)]
pub struct ActionRunnerConfig {
    pub launch_timeout: Duration,
    pub output_cap: usize,
    /// When set, a successful fire (any action type, including Launch) also
    /// writes a fire-notification JSON here (`run-<run_id>.json`). Production
    /// `run-now` points this at `slots/done/` so integrators see trigger data
    /// alongside the launch (PLAN: launch + write JSON).
    pub write_slot_dir: Option<PathBuf>,
    /// Optional fixed filename under `write_slot_dir` (e.g. `slot-0001.json`).
    /// When `None`, uses `run-<run_id>.json`.
    pub write_slot_file: Option<String>,
    /// When true (tests), sleep for retry delay is skipped — only the retry
    /// *count* is honored. Production leaves this false.
    pub skip_retry_sleep: bool,
    /// Global concurrent wake-action cap (default 16). Launch work runs under
    /// an [`ActionLimiter`] so a mass-fire cannot fork-bomb the host.
    pub max_concurrent_actions: usize,
}

impl Default for ActionRunnerConfig {
    fn default() -> Self {
        Self {
            launch_timeout: DEFAULT_TIMEOUT,
            output_cap: DEFAULT_OUTPUT_CAP_BYTES,
            write_slot_dir: None,
            write_slot_file: None,
            skip_retry_sleep: false,
            max_concurrent_actions: DEFAULT_MAX_CONCURRENT_ACTIONS,
        }
    }
}

/// Errors that surface as [`FireAction::on_fire`] `Err(String)`.
#[derive(Debug)]
pub enum ActionRunnerError {
    Failed(String),
    OverlapSkip,
}

impl std::fmt::Display for ActionRunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(s) => f.write_str(s),
            Self::OverlapSkip => f.write_str("overlap policy skip: prior run still in flight"),
        }
    }
}

/// Executes timer wake actions and enqueues lifecycle events into the R11
/// outbox (the elected publisher appends them — producers never write the
/// log directly). The `fired` event itself is not emitted here: it commits
/// with the R10 fire transaction.
///
/// Overlap: tracks in-flight timer ids; [`OverlapPolicy::Skip`] (default) drops
/// a new fire while one is active. Retry: uses the timer's [`RetryPolicy`]
/// (default 1× / 30 s); after exhaustion emits `wake_failed` and returns Err.
pub struct ActionRunner {
    pub config: ActionRunnerConfig,
    event_sink: Option<crate::store::Store>,
    /// Timers with an action currently executing (overlap tracking).
    pub(crate) in_flight: HashSet<TimerId>,
    /// Last human-readable message (run-now JSON).
    pub last_message: Option<String>,
    /// Pluggable desktop-notification sink. C6 leaves it as the stub; C7 wires
    /// the Tauri plugin so notification timers surface a real toast.
    notify_sink: Arc<dyn NotifySink>,
    /// Global concurrency gate for wake actions.
    limiter: Arc<ActionLimiter>,
}

impl ActionRunner {
    pub fn new(config: ActionRunnerConfig) -> Self {
        let limiter = Arc::new(ActionLimiter::new(config.max_concurrent_actions));
        Self {
            config,
            event_sink: None,
            in_flight: HashSet::new(),
            last_message: None,
            notify_sink: Arc::new(StubNotifySink),
            limiter,
        }
    }

    /// Shared concurrency limiter (tests inspect peak / completed counts).
    pub fn limiter(&self) -> &ActionLimiter {
        &self.limiter
    }

    /// Install a custom notification sink (real toast in the Tauri shell).
    pub fn with_notify_sink(mut self, sink: Arc<dyn NotifySink>) -> Self {
        self.notify_sink = sink;
        self
    }

    /// Current notification sink (read-only; for tests that want to assert
    /// that a fire actually surfaced a toast).
    pub fn notify_sink(&self) -> &Arc<dyn NotifySink> {
        &self.notify_sink
    }

    /// Install the R11 outbox sink (a dedicated store connection — SQLite
    /// serialises across connections, which is the point of the funnel).
    pub fn with_event_sink(mut self, store: crate::store::Store) -> Self {
        self.event_sink = Some(store);
        self
    }

    /// Enqueue one event for the elected publisher. Enqueue errors surface
    /// on stderr; the row is never silently dropped by the outbox itself.
    #[allow(clippy::needless_pass_by_value)]
    fn emit(&mut self, rec: EventRecord) {
        if let Some(sink) = self.event_sink.as_ref() {
            if let Err(e) = sink.enqueue_event(&rec) {
                eprintln!("bellman: event enqueue failed: {e}");
            }
        }
    }

    fn overlap_blocks(&self, ctx: &FireContext<'_>) -> bool {
        match &ctx.timer.overlap {
            OverlapPolicy::Skip => self.in_flight.contains(&ctx.timer.id),
            OverlapPolicy::QueueOne | OverlapPolicy::Replace => {
                // v1: treat like skip for the in-process runner; full queue/
                // cancel needs the scheduler concurrency pool (P5).
                self.in_flight.contains(&ctx.timer.id)
            }
            OverlapPolicy::Parallel { cap } => {
                // Count only this timer's in-flight (1 bit today).
                *cap == 0 || self.in_flight.contains(&ctx.timer.id) && *cap < 2
            }
        }
    }

    fn execute_once(&mut self, ctx: &FireContext<'_>) -> Result<String, String> {
        // All wake work takes a concurrency permit so multi-threaded mass-fire
        // paths cannot fork-bomb. Sequential scheduler ticks still peak at 1.
        let limiter = Arc::clone(&self.limiter);
        let launch_timeout = self.config.launch_timeout;
        let output_cap = self.config.output_cap;
        let notify_sink = Arc::clone(&self.notify_sink);
        let action = ctx.timer.action.clone();
        let run_id = ctx.run_id;

        let primary = limiter.run(|| -> Result<String, String> {
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
                    };
                    // Timeout + output cap are enforced inside run_launch.
                    let outcome = run_launch(&cfg).map_err(|e| e.to_string())?;
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
        })?;

        // PLAN: launch + write JSON — always write the fire notification when a
        // slots/output dir is configured, for every successful wake path.
        if let Some(path) = self.write_fire_slot(ctx)? {
            Ok(format!("{primary}; write-output-slot {}", path.display()))
        } else {
            Ok(primary)
        }
    }

    /// Write fire trigger JSON into `write_slot_dir` when configured.
    fn write_fire_slot(&self, ctx: &FireContext<'_>) -> Result<Option<PathBuf>, String> {
        let Some(dir) = &self.config.write_slot_dir else {
            return Ok(None);
        };
        let file = self
            .config
            .write_slot_file
            .clone()
            .unwrap_or_else(|| format!("run-{}.json", ctx.run_id));
        let payload = WriteSlotPayload {
            schema: FIRE_SCHEMA_V1.to_string(),
            kind: fire_event_kind(&ctx.kind).to_string(),
            timer_id: ctx.timer.id,
            timer_name: ctx.timer.name.clone(),
            run_id: ctx.run_id,
            scheduled_for: ctx.scheduled_for,
            fired_at: chrono::Utc::now(),
            occurrence_kind: fire_kind_label(&ctx.kind),
        };
        let path = write_output_slot(dir, &file, &payload)?;
        Ok(Some(path))
    }
}

/// R2: top-level `kind` is the event kind (R5 vocabulary), mirroring the
/// [`RunState`] the runner logs for the same fire.
fn fire_event_kind(kind: &FireKind) -> &'static str {
    match kind {
        FireKind::OnTime => RunState::Fired.as_str(),
        FireKind::Late { .. } => RunState::FiredLate.as_str(),
        FireKind::Coalesced { .. } => RunState::Coalesced.as_str(),
        FireKind::CatchUp { .. } => RunState::Fired.as_str(),
    }
}

fn fire_kind_label(kind: &FireKind) -> String {
    match kind {
        FireKind::OnTime => "on_time".into(),
        FireKind::Late { .. } => "late".into(),
        FireKind::Coalesced { .. } => "coalesced".into(),
        FireKind::CatchUp { index } => format!("catch_up_{index}"),
    }
}

impl FireAction for ActionRunner {
    fn on_fire(&mut self, ctx: &FireContext<'_>) -> Result<(), String> {
        if self.overlap_blocks(ctx) {
            self.emit(
                EventRecord::new(RunState::SkippedMisfire)
                    .with_timer(ctx.timer.id, ctx.timer.name.clone())
                    .with_run(ctx.run_id)
                    .with_message("overlap_skip"),
            );
            self.last_message = Some("overlap policy skip".into());
            // Overlap skip is a soft success for the claim ledger — we do not
            // want crash-recovery to re-run forever. Return Ok.
            return Ok(());
        }

        self.in_flight.insert(ctx.timer.id);

        let max_retries = ctx.timer.retry.max_retries;
        let delay = Duration::from_secs(ctx.timer.retry.delay_secs);
        let mut attempt = 0u32;

        let result = loop {
            match self.execute_once(ctx) {
                Ok(msg) => break Ok(msg),
                Err(e) => {
                    if attempt >= max_retries {
                        break Err(e);
                    }
                    attempt += 1;
                    if !self.config.skip_retry_sleep && !delay.is_zero() {
                        thread::sleep(delay);
                    }
                }
            }
        };

        self.in_flight.remove(&ctx.timer.id);

        match result {
            Ok(msg) => {
                self.emit(
                    EventRecord::new(RunState::WakeDelivered)
                        .with_timer(ctx.timer.id, ctx.timer.name.clone())
                        .with_run(ctx.run_id)
                        .with_scheduled_for(ctx.scheduled_for)
                        .with_message(msg.clone())
                        .with_count(attempt),
                );
                self.last_message = Some(msg);
                Ok(())
            }
            Err(e) => {
                // FAILED path after retries exhausted.
                self.emit(
                    EventRecord::new(RunState::WakeFailed)
                        .with_timer(ctx.timer.id, ctx.timer.name.clone())
                        .with_run(ctx.run_id)
                        .with_scheduled_for(ctx.scheduled_for)
                        .with_error(e.clone())
                        .with_message("FAILED")
                        .with_count(attempt)
                        .with_detail(serde_json::json!({ "status": "FAILED" })),
                );
                self.last_message = Some(format!("FAILED: {e}"));
                Err(format!("FAILED: {e}"))
            }
        }
    }
}

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
