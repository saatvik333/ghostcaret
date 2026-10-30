//! Ghostcaret desktop entry point.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    open_flowtype_lib::run();
}
