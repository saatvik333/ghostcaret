//! Ghostcaret engine: everything that decides what to suggest and when, with
//! no UI and no OS calls.
//!
//! - [`io`]: the contract OS backends implement.
//! - [`complete`]: the contract model providers implement.
//! - [`settings`]: the settings schema.
//! - [`controller`]: the state machine that ties them together.
//! - [`text`]: Take Word units and phrase, sentence and paragraph boundaries.
//! - [`stop`]: the stop rule during generation and the cut that turns a
//!   generated continuation into a suggestion.
//! - [`filter`]: the secret filter.
//! - [`prompt`]: prompt segments for autocomplete, with their caps.
//! - [`apps`]: app categories from built-in tables.
//! - [`draft`]: reply & rewrite prompts and parsing.

pub mod apps;
pub mod complete;
pub mod controller;
pub mod draft;
pub mod filter;
pub mod io;
pub mod prompt;
pub mod settings;
pub mod stop;
pub mod text;
