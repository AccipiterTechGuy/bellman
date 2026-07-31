//! Wake-action execution: launch / desktop notification, on worker lanes.
//!
//! - **Launch**: arg array, no shell, timeout kill, stdout/stderr cap,
//!   `BELLMAN_RUN_ID` env, cancellation token (SCH1 `Replace`).
//! - **Notify**: interface stub (real toast lands with the Tauri plugin in C7).
//! - **Dispatcher** (SCH1): the bounded worker pool that executes actions off
//!   the scheduler loop; the fire notification is published at fire time by
//!   the producer (`reply::publication`), never by workers.
//!
//! Overlap policy is decided durably in the fire transaction and enforced by
//! the dispatcher lanes. Retry: product default 1× after 30 s, then the
//! worker records `wake_failed` to the claim ledger and the R11 outbox.

mod cancel;
mod concurrency;
mod dispatcher;
mod executor;
mod launch;
mod notify;
mod notify_sink;

pub use cancel::CancellationToken;
pub use concurrency::{
    run_parallel_under_cap, ActionLimiter, LimiterStats, DEFAULT_MAX_CONCURRENT_ACTIONS,
};
pub use dispatcher::{Dispatcher, DispatcherConfig, DISPATCHER_LOCK_NAME};
pub use executor::{ActionExecutor, ExecOutcome, ExecutorConfig};
pub use launch::{run_launch, LaunchConfig, LaunchError, LaunchOutcome, DEFAULT_OUTPUT_CAP_BYTES};
pub use notify::{notify_stub, NotifyOutcome};
pub use notify_sink::{NotifySink, StubNotifySink};

#[cfg(test)]
mod tests;
