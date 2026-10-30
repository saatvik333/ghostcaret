//! Windows backend: a low-level keyboard hook runs the key gate, UI
//! Automation reads the focused field, a layered window draws the overlay
//! and `SendInput` inserts text.
//!
//! Four threads, each blocked in its own message loop when idle:
//! - hook: owns the gate and the `WH_KEYBOARD_LL` hook; accepted text is
//!   typed from inside the hook callback so it lands before the next key;
//! - uia: focus and text events, field reads, conversation reads;
//! - overlay: renders and positions the layered window;
//! - insert: controller insertions, typed or pasted.
//!
//! `keys`, `text`, `place`, `field` and `conversation` are pure and also
//! build (and are tested) on other hosts.

mod conversation;
mod field;
mod keys;
mod place;
mod text;

#[cfg(target_os = "windows")]
mod backend;
#[cfg(target_os = "windows")]
mod hook;
#[cfg(target_os = "windows")]
mod insert;
#[cfg(target_os = "windows")]
mod mailbox;
#[cfg(target_os = "windows")]
mod overlay;
#[cfg(target_os = "windows")]
mod send;
#[cfg(target_os = "windows")]
mod uia;

#[cfg(target_os = "windows")]
pub use backend::start;
#[cfg(target_os = "windows")]
pub use hook::gate_latency;
