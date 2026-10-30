//! Contract between the engine and model providers (`oft-local`, `oft-cloud`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::io::{AppInfo, FieldId, FieldKind, Message};

/// How much a suggestion may show. See docs/product.md, "Length and boldness".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LengthMode {
    Word,
    Phrase,
    Sentence,
    Paragraph,
    #[default]
    Auto,
}

/// The stop rule for one request, after per-app rules were applied.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StopParams {
    /// The effective mode. `Auto` stays `Auto` and is bounded by `ceiling`;
    /// for fixed modes `ceiling == mode`.
    pub mode: LengthMode,
    /// The furthest boundary Auto may reach; never `Auto`.
    pub ceiling: LengthMode,
    /// 0.0 = careful, 1.0 = bold.
    pub boldness: f32,
    /// Word cap for Paragraph.
    pub paragraph_words: u32,
}

/// Prompt material, ordered from least to most frequently changing. Every
/// string here has already passed the secret filter and its length cap.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Segments {
    pub app: AppInfo,
    pub kind: FieldKind,
    pub multiline: bool,
    pub about_me: String,
    /// Recent messages the user sent, for style.
    pub style: Vec<String>,
    pub clipboard: Vec<String>,
    pub conversation: Vec<Message>,
    /// Text after the caret.
    pub after: String,
    /// Text before the caret.
    pub before: String,
    /// True when `before` does not start at the start of the field.
    pub before_truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionRequest {
    pub id: u64,
    pub field: FieldId,
    pub revision: u64,
    pub segments: Segments,
    pub stop: StopParams,
    /// Hard cap on generated tokens, derived from the stop rule.
    pub max_tokens: u32,
    /// Extra alternatives wanted besides the main candidate (0–2).
    pub alternatives: u8,
}

/// One generated continuation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Candidate {
    pub text: String,
    /// Byte offset in `text` where each generated token ends.
    pub token_ends: Vec<usize>,
    /// Probability of each token, parallel to `token_ends`. Empty when the
    /// provider reports none.
    pub token_probs: Vec<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Completion {
    pub id: u64,
    /// The main candidate first, then alternatives.
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CompleteError {
    #[error("cancelled")]
    Cancelled,
    #[error("rate limited")]
    RateLimited,
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
}

/// What a provider supports; the controller adapts requests to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub token_probs: bool,
    pub cheap_alternatives: bool,
    pub prefill: bool,
    pub local: bool,
}

/// Cooperative cancellation shared between the controller and a request.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Feeds the stop rule while tokens are generated, so generation ends at the
/// last boundary that could be shown plus one unit of lookahead.
pub trait StopCheck {
    /// Called after each token with the text generated so far and the token's
    /// probability, if known. Returns true when generation should stop.
    fn should_stop(&mut self, generated: &str, token_prob: Option<f32>) -> bool;
}

/// Autocomplete provider.
pub trait Completer: Send + Sync {
    fn caps(&self) -> Caps;
    /// Generates continuations. `stop` is built by the engine from
    /// `request.stop`; providers that stream call it per token.
    fn complete(
        &self,
        request: &CompletionRequest,
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
    ) -> Result<Completion, CompleteError>;
    /// Warms caches for this context while the user types. Cheap; may no-op.
    fn prefill(&self, _request: &CompletionRequest, _cancel: &CancelToken) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenerateRequest {
    pub id: u64,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: u32,
    pub temperature: f32,
}

/// Chat generation for Flow Draft.
pub trait Generator: Send + Sync {
    /// Streams text through `on_delta` and returns the full result.
    fn generate(
        &self,
        request: &GenerateRequest,
        cancel: &CancelToken,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, CompleteError>;
}
