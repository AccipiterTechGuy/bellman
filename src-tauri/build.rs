//! Build script for the Bellman desktop app.
//!
//! Runs Tauri's code generation (capabilities, the asset manifest and the
//! platform resources) before the crate is compiled.

/// Runs Tauri's code generation before the crate is compiled.
fn main() {
    tauri_build::build()
}
