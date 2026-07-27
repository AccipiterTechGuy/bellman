//! [`ActionRunner`]: [`FireAction`] impl with overlap, retry, and event logging.

use super::launch::{run_launch, LaunchConfig, DEFAULT_OUTPUT_CAP_BYTES, DEFAULT_TIMEOUT};
use super::notify::notify_stub;
use super::write_slot::{write_output_slot, WriteSlotPayload};
use crate::events::{EventKind, EventLog, EventRecord};
use crate::scheduler::{FireAction, FireContext, FireKind};
use crate::store::{Action, OverlapPolicy, TimerId};
use std::collections::HashSet;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Runner configuration (timeouts / caps / optional write-slot root).
#[derive(Debug, Clone)]
pub struct ActionRunnerConfig {
    pub launch_timeout: Duration,
    pub output_cap: usize,
    /// Directory for `Action::None` write-slot side channel or explicit write-slot
    /// tests. When set, a successful fire also writes a notification JSON here.
    pub write_slot_dir: Option<PathBuf>,
    /// When true (tests), sleep for retry delay is skipped — only the retry
    /// *count* is honored. Production leaves this false.
    pub skip_retry_sleep: bool,
}

impl Default for ActionRunnerConfig {
    fn default() -> Self {
        Self {
            launch_timeout: DEFAULT_TIMEOUT,
            output_cap: DEFAULT_OUTPUT_CAP_BYTES,
            write_slot_dir: None,
            skip_retry_sleep: false,
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

/// Executes timer wake actions and appends lifecycle events to an [`EventLog`].
///
/// Overlap: tracks in-flight timer ids; [`OverlapPolicy::Skip`] (default) drops
/// a new fire while one is active. Retry: uses the timer's [`RetryPolicy`]
/// (default 1× / 30 s); after exhaustion emits `wake_failed` and returns Err.
pub struct ActionRunner {
    pub config: ActionRunnerConfig,
    event_log: Option<EventLog>,
    /// Timers with an action currently executing (overlap tracking).
    pub(crate) in_flight: HashSet<TimerId>,
    /// Last human-readable message (run-now JSON).
    pub last_message: Option<String>,
}

impl ActionRunner {
    pub fn new(config: ActionRunnerConfig) -> Self {
        Self {
            config,
            event_log: None,
            in_flight: HashSet::new(),
            last_message: None,
        }
    }

    pub fn with_event_log(mut self, log: EventLog) -> Self {
        self.event_log = Some(log);
        self
    }

    pub fn event_log_mut(&mut self) -> Option<&mut EventLog> {
        self.event_log.as_mut()
    }

    pub fn take_event_log(&mut self) -> Option<EventLog> {
        self.event_log.take()
    }

    fn emit(&mut self, rec: EventRecord) {
        if let Some(log) = self.event_log.as_mut() {
            let _ = log.append(&rec);
        }
    }

    fn log_fire_kind(&mut self, ctx: &FireContext<'_>) {
        let base = || {
            EventRecord::new(EventKind::Fired)
                .with_timer(ctx.timer.id, ctx.timer.name.clone())
                .with_run(ctx.run_id)
                .with_scheduled_for(ctx.scheduled_for)
        };
        match &ctx.kind {
            FireKind::OnTime => self.emit(base()),
            FireKind::Late { lateness } => {
                self.emit(
                    EventRecord::new(EventKind::FiredLate)
                        .with_timer(ctx.timer.id, ctx.timer.name.clone())
                        .with_run(ctx.run_id)
                        .with_scheduled_for(ctx.scheduled_for)
                        .with_duration_ms(lateness.num_milliseconds()),
                );
            }
            FireKind::Coalesced { missed_count } => {
                self.emit(
                    EventRecord::new(EventKind::Coalesced)
                        .with_timer(ctx.timer.id, ctx.timer.name.clone())
                        .with_run(ctx.run_id)
                        .with_scheduled_for(ctx.scheduled_for)
                        .with_count(*missed_count),
                );
            }
            FireKind::CatchUp { index } => {
                self.emit(base().with_count(*index).with_message("catch_up"));
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
        match &ctx.timer.action {
            Action::None => {
                // Optional write-slot side channel when configured.
                if let Some(dir) = &self.config.write_slot_dir {
                    let file = format!("run-{}.json", ctx.run_id);
                    let payload = WriteSlotPayload {
                        schema: "bellman-fire/1",
                        timer_id: ctx.timer.id,
                        timer_name: ctx.timer.name.clone(),
                        run_id: ctx.run_id,
                        scheduled_for: ctx.scheduled_for,
                        fired_at: chrono::Utc::now(),
                        kind: format!("{:?}", ctx.kind),
                    };
                    write_output_slot(dir, &file, &payload)?;
                    Ok(format!("write-output-slot {}", dir.join(file).display()))
                } else {
                    Ok("action=none".into())
                }
            }
            Action::Launch {
                command,
                args,
                workdir,
            } => {
                let cfg = LaunchConfig {
                    command: command.clone(),
                    args: args.clone(),
                    workdir: workdir.clone(),
                    timeout: self.config.launch_timeout,
                    output_cap: self.config.output_cap,
                    run_id: ctx.run_id,
                };
                let outcome = run_launch(&cfg).map_err(|e| e.to_string())?;
                if outcome.timed_out {
                    return Err(format!(
                        "launch timed out after {:?} (killed={})",
                        self.config.launch_timeout, outcome.killed
                    ));
                }
                match outcome.exit_code {
                    Some(0) | None => {
                        // None can happen for signal-killed without timeout flag
                        // if the process exited by signal without timeout — treat
                        // non-timeout signal as failure.
                        if outcome.exit_code.is_none() {
                            return Err("launch exited by signal".into());
                        }
                        Ok(format!(
                            "launch ok exit=0 duration={:?}",
                            outcome.duration
                        ))
                    }
                    Some(code) => Err(format!(
                        "launch exit={code} output={}",
                        truncate(&outcome.output, 200)
                    )),
                }
            }
            Action::Notify { title, body } => {
                let out = notify_stub(title, body);
                Ok(format!("notify stub title={:?}", out.title))
            }
        }
    }
}

impl FireAction for ActionRunner {
    fn on_fire(&mut self, ctx: &FireContext<'_>) -> Result<(), String> {
        self.log_fire_kind(ctx);

        if self.overlap_blocks(ctx) {
            self.emit(
                EventRecord::new(EventKind::SkippedMisfire)
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
                    EventRecord::new(EventKind::WakeDelivered)
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
                    EventRecord::new(EventKind::WakeFailed)
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
