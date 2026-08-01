//! Application-facing service helpers built on top of the core engine.
//!
//! These are the reusable building blocks the CLI and the Tauri shell both
//! call. Keeping them in one place means there is exactly one implementation
//! of "run a timer right now through the real fire path" / "read the event
//! log" / "look up the log tail" — the C6 work lives here, and the C7 GUI
//! commands wrap it.

pub mod log_query;
pub mod run_now;
