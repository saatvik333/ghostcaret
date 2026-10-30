//! On-device stores shared by the app and the runtime, each a
//! SQLite file in the data directory written on its own thread:
//!
//! - [`stats`]: daily usage counters per app (`stats.sqlite`), never text.
//! - [`style`]: style memory (`style.sqlite`), opt-in.

pub mod stats;
pub mod style;
