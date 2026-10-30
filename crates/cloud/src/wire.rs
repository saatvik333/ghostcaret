//! What the three protocols have in common once their JSON is decoded.

use serde_json::Value;

use crate::error::ProviderError;
use crate::sse::SseEvent;

/// One generated token as the provider reported it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Token {
    /// Bytes of UTF-8 the token adds to the text.
    pub len: usize,
    /// `NAN` when the provider gave no usable probability.
    pub prob: f32,
}

/// A piece of generated text; `tokens` is empty when the provider reports no
/// probabilities.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Chunk {
    pub text: String,
    pub tokens: Vec<Token>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    /// Nothing to act on (pings, role announcements, usage).
    Idle,
    Chunk(Chunk),
    /// The provider says the answer is complete; the rest of the stream is
    /// only its tail.
    Done,
}

/// Turns one protocol's stream events and whole responses into [`Chunk`]s.
pub(crate) trait Decoder {
    fn event(&mut self, event: &SseEvent) -> Result<Step, ProviderError>;
    /// A response that is not an event stream: servers may ignore `stream`.
    fn whole(&mut self, body: &[u8]) -> Result<Chunk, ProviderError>;
}

/// A temperature as a short JSON number: 0.7, not 0.699999988079071.
pub(crate) fn temperature(t: f32) -> f64 {
    (f64::from(t) * 1000.0).round() / 1000.0
}

pub(crate) fn parse_json(text: &str) -> Result<Value, ProviderError> {
    serde_json::from_str(text).map_err(|e| ProviderError::Malformed(format!("invalid JSON: {e}")))
}

/// The provider's own words for what went wrong, from an error body.
/// Understands `{"error":{"message":..}}`, `{"error":".."}`, `{"message":..}`
/// and `{"detail":..}`; anything else is shown as text.
pub(crate) fn error_message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        let candidates = [
            v.pointer("/error/message"),
            v.get("error"),
            v.get("message"),
            v.get("detail"),
        ];
        for c in candidates.into_iter().flatten() {
            if let Some(s) = c.as_str().filter(|s| !s.is_empty()) {
                return s.to_owned();
            }
        }
    }
    let text = text.trim();
    let mut end = text.len().min(300);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if text.is_empty() {
        "empty error response".to_owned()
    } else {
        text[..end].to_owned()
    }
}

/// Classifies an error the provider reported inside a stream or a 200 body.
pub(crate) fn stream_error(kind: &str, message: String) -> ProviderError {
    let kind = kind.to_ascii_lowercase();
    if kind.contains("rate_limit") || kind.contains("resource_exhausted") || kind.contains("quota")
    {
        ProviderError::RateLimited {
            message,
            retry_after: None,
        }
    } else if [
        "overload",
        "server_error",
        "unavailable",
        "timeout",
        "internal",
        "api_error",
    ]
    .iter()
    .any(|k| kind.contains(k))
    {
        ProviderError::Unavailable(message)
    } else {
        ProviderError::Rejected {
            status: None,
            message,
        }
    }
}

/// Reads an `{"error": {...}}` object, if `v` is one.
pub(crate) fn embedded_error(v: &Value) -> Option<ProviderError> {
    let err = v.get("error")?;
    if err.is_null() {
        return None;
    }
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| err.as_str())
        .unwrap_or("the provider reported an error")
        .to_owned();
    let kind = ["type", "status", "code"]
        .iter()
        .find_map(|k| err.get(k).and_then(Value::as_str))
        .unwrap_or("");
    Some(stream_error(kind, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_understands_the_common_shapes() {
        assert_eq!(
            error_message(br#"{"error":{"message":"bad key","type":"x"}}"#),
            "bad key"
        );
        assert_eq!(
            error_message(br#"{"error":"model not found"}"#),
            "model not found"
        );
        assert_eq!(error_message(br#"{"message":"nope"}"#), "nope");
        assert_eq!(
            error_message(b"  upstream timed out \n"),
            "upstream timed out"
        );
        assert_eq!(error_message(b""), "empty error response");
    }

    #[test]
    fn error_message_cuts_long_text_on_a_char_boundary() {
        let long = "é".repeat(400);
        let m = error_message(long.as_bytes());
        assert_eq!(m.len(), 300);
    }

    #[test]
    fn stream_errors_are_classified() {
        assert!(matches!(
            stream_error("overloaded_error", "x".into()),
            ProviderError::Unavailable(_)
        ));
        assert!(matches!(
            stream_error("rate_limit_error", "x".into()),
            ProviderError::RateLimited { .. }
        ));
        assert!(matches!(
            stream_error("invalid_request_error", "x".into()),
            ProviderError::Rejected { status: None, .. }
        ));
    }
}
