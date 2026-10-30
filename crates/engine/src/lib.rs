//! open-flowtype engine: everything that decides what to suggest and when,
//! with no UI and no OS calls.
//!
//! - [`io`]: the contract OS backends implement.
//! - [`complete`]: the contract model providers implement.
//! - [`settings`]: the settings schema.
//! - [`controller`]: the state machine that ties them together.

pub mod complete;
pub mod controller;
pub mod io;
pub mod settings;
