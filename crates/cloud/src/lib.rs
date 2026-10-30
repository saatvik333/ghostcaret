//! Cloud model providers: OpenAI-compatible servers, Anthropic and Gemini.
//!
//! [`provider`] builds a [`Provider`] that serves autocomplete
//! ([`ghostcaret_engine::complete::Completer`]) and Reply & rewrite
//! ([`ghostcaret_engine::complete::Generator`]) over one pooled HTTPS connection.
//!
//! - Autocomplete requests are sent once and never retried; a late answer is
//!   useless. Reply & rewrite retries transient failures with jittered backoff inside
//!   a deadline. A per-minute client limit applies to both.
//! - Token probabilities are requested wherever the provider offers them and
//!   mapped onto the candidate text.
//! - Every request body is recorded in a bounded [`SentLog`]; keys travel in
//!   headers, which are never recorded.

mod anthropic;
mod collect;
mod error;
mod gemini;
mod http;
mod limit;
mod models;
mod openai;
mod prompt;
mod provider;
mod sentlog;
mod sse;
mod wire;

pub use error::ProviderError;
pub use models::ModelInfo;
pub use provider::{Provider, Tuning, provider, provider_with};
pub use sentlog::{SentEntry, SentLog};
