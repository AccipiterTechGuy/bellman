#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Binary entry point for the Bellman desktop app.
//!
//! Everything lives in the `bellman_lib` crate so the same code can be built
//! as a library for tests; this file only starts it. The attribute above is
//! Tauri's scaffold: without it a release build on Windows opens a console
//! window behind the app.

fn main() {
    bellman_lib::run();
}
