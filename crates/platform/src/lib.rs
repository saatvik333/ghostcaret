//! OS backends implementing [`ghostcaret_engine::io::Backend`].
//!
//! - Linux: a native Wayland input method (`zwp_input_method_v2`), with the
//!   overlay drawn in an input-method popup surface.
//! - Windows: UI Automation, a low-level keyboard hook, a layered overlay
//!   window and `SendInput`.
//!
//! macOS is next; its backend arrives behind the same [`start`] entry point.

use std::sync::Arc;

use crossbeam_channel::Sender;
use ghostcaret_engine::io::{Backend, Event};

#[cfg(any(target_os = "linux", target_os = "windows", test))]
mod gate;
#[cfg(any(target_os = "linux", target_os = "windows", test))]
pub mod latency;
#[cfg(target_os = "linux")]
pub mod linux;
// The Windows module's pure helpers also build on other hosts for their
// tests.
#[cfg(any(target_os = "windows", test))]
pub mod windows;

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The session lacks what the backend needs; the text names it.
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Failed(String),
}

/// Starts the backend for this OS. Events are sent on `events` from the
/// backend's own threads; the returned handle executes controller outputs.
pub fn start(events: Sender<Event>) -> Result<Arc<dyn Backend>, StartError> {
    #[cfg(target_os = "linux")]
    {
        linux::start(events)
    }
    #[cfg(target_os = "windows")]
    {
        windows::start(events)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = events;
        Err(StartError::Unsupported(
            "no desktop backend for this OS yet".into(),
        ))
    }
}
