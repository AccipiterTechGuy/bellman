//! Wake-action execution: launch / write-output-slot / desktop notification.
//!
//! - **Launch**: arg array, no shell, timeout kill, stdout/stderr cap,
//!   `BELLMAN_RUN_ID` env.
//! - **Write output slot**: atomic JSON publish of the fire notification.
//! - **Notify**: interface stub (real toast lands with the Tauri plugin in C7).
//!
//! Overlap policy default is **skip**. Retry: product default 1× after 30 s,
//! then emit `wake_failed` (`FAILED` path) to the event log.

mod launch;
mod notify;
mod runner;
mod write_slot;

pub use launch::{run_launch, LaunchConfig, LaunchError, LaunchOutcome, DEFAULT_OUTPUT_CAP_BYTES};
pub use notify::{notify_stub, NotifyOutcome};
pub use runner::{ActionRunner, ActionRunnerConfig, ActionRunnerError};
pub use write_slot::{write_output_slot, WriteSlotPayload};

#[cfg(test)]
mod tests;
