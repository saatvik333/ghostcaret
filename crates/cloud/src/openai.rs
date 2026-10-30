//! OpenAI-compatible APIs: `/completions` (raw continuation) and
//! `/chat/completions`, both streamed as `data: {json}` events ending in
//! `data: [DONE]`.
//!
//! Logprobs come in two shapes, and both are read: the legacy parallel arrays
//! (`tokens`, `token_logprobs`) and the chat shape (`content: [{token, logprob,
//! bytes}]`, which llama-server also uses for `/completions`).

use serde_json::{Value, json};

use crate::error::ProviderError;
use crate::sse::SseEvent;
use crate::wire::{Chunk, Decoder, Step, Token, embedded_error, parse_json, temperature};

pub(crate) struct Message<'a> {
    pub role: &'static str,
    pub content: &'a str,
}

/// Which request field limits the output. OpenAI's current models reject
/// `max_tokens`; most other servers only know it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaxField {
    MaxTokens,
    MaxCompletionTokens,
}

impl MaxField {
    fn name(self) -> &'static str {
        match self {
            MaxField::MaxTokens => "max_tokens",
            MaxField::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

pub(crate) struct Options<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub max_field: MaxField,
    pub stop: &'a [&'a str],
    pub logprobs: bool,
    pub temperature: Option<f32>,
    pub reasoning_effort: Option<&'static str>,
}

pub(crate) fn completions_body(o: &Options<'_>, prompt: &str) -> String {
    let mut body = json!({ "model": o.model, "prompt": prompt, "stream": true });
    common(&mut body, o);
    if o.logprobs {
        body["logprobs"] = json!(1);
    }
    body.to_string()
}

pub(crate) fn chat_body(o: &Options<'_>, messages: &[Message<'_>]) -> String {
    let messages: Vec<Value> = messages
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();
    let mut body = json!({ "model": o.model, "messages": messages, "stream": true });
    common(&mut body, o);
    if o.logprobs {
        body["logprobs"] = json!(true);
    }
    if let Some(effort) = o.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body.to_string()
}

fn common(body: &mut Value, o: &Options<'_>) {
    body[o.max_field.name()] = json!(o.max_tokens);
    if let Some(t) = o.temperature {
        body["temperature"] = json!(temperature(t));
    }
    if !o.stop.is_empty() {
        body["stop"] = json!(o.stop);
    }
}

#[derive(Debug)]
pub(crate) struct OpenAiDecoder;

impl Decoder for OpenAiDecoder {
    fn event(&mut self, event: &SseEvent) -> Result<Step, ProviderError> {
        if event.data.trim() == "[DONE]" {
            return Ok(Step::Done);
        }
        let v = parse_json(&event.data)?;
        if let Some(e) = embedded_error(&v) {
            return Err(e);
        }
        let chunk = chunk_of(&v);
        Ok(if chunk.text.is_empty() {
            Step::Idle
        } else {
            Step::Chunk(chunk)
        })
    }

    fn whole(&mut self, body: &[u8]) -> Result<Chunk, ProviderError> {
        let v = parse_json(&String::from_utf8_lossy(body))?;
        if let Some(e) = embedded_error(&v) {
            return Err(e);
        }
        Ok(chunk_of(&v))
    }
}

fn chunk_of(v: &Value) -> Chunk {
    let Some(choice) = v.pointer("/choices/0") else {
        return Chunk::default();
    };
    let text = choice
        .get("text")
        .or_else(|| choice.pointer("/delta/content"))
        .or_else(|| choice.pointer("/message/content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let tokens = choice
        .get("logprobs")
        .filter(|l| !l.is_null())
        .map(tokens_of)
        .unwrap_or_default();
    Chunk { text, tokens }
}

fn tokens_of(logprobs: &Value) -> Vec<Token> {
    if let Some(content) = logprobs.get("content").and_then(Value::as_array) {
        return content
            .iter()
            .map(|t| Token {
                len: t.get("bytes").and_then(Value::as_array).map_or_else(
                    || t.get("token").and_then(Value::as_str).map_or(0, str::len),
                    Vec::len,
                ),
                prob: prob(t.get("logprob")),
            })
            .collect();
    }
    let (Some(tokens), Some(lps)) = (
        logprobs.get("tokens").and_then(Value::as_array),
        logprobs.get("token_logprobs").and_then(Value::as_array),
    ) else {
        return Vec::new();
    };
    tokens
        .iter()
        .zip(lps)
        .map(|(t, lp)| Token {
            len: t.as_str().map_or(0, str::len),
            prob: prob(Some(lp)),
        })
        .collect()
}

fn prob(logprob: Option<&Value>) -> f32 {
    logprob
        .and_then(Value::as_f64)
        .map_or(f32::NAN, |lp| lp.exp() as f32)
}

/// The base URL's models endpoint reply: `{"data": [{"id": ..}]}`.
pub(crate) fn models_of(v: &Value) -> Vec<(String, String)> {
    crate::models::from_list(v.get("data").or_else(|| v.get("models")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(data: &str) -> SseEvent {
        SseEvent {
            name: String::new(),
            data: data.into(),
        }
    }

    fn opts<'a>(stop: &'a [&'a str]) -> Options<'a> {
        Options {
            model: "m",
            max_tokens: 24,
            max_field: MaxField::MaxTokens,
            stop,
            logprobs: true,
            temperature: Some(0.0),
            reasoning_effort: None,
        }
    }

    #[test]
    fn completions_body_has_the_documented_fields() {
        let body: Value =
            serde_json::from_str(&completions_body(&opts(&["\n"]), "Hello wor")).unwrap();
        assert_eq!(
            body,
            json!({"model":"m","prompt":"Hello wor","stream":true,"max_tokens":24,"temperature":0.0,"stop":["\n"],"logprobs":1})
        );
    }

    #[test]
    fn chat_body_uses_the_requested_limit_field_and_effort() {
        let mut o = opts(&[]);
        o.max_field = MaxField::MaxCompletionTokens;
        o.reasoning_effort = Some("low");
        o.temperature = None;
        let body: Value = serde_json::from_str(&chat_body(
            &o,
            &[Message {
                role: "user",
                content: "hi",
            }],
        ))
        .unwrap();
        assert_eq!(
            body,
            json!({"model":"m","messages":[{"role":"user","content":"hi"}],"stream":true,"max_completion_tokens":24,"logprobs":true,"reasoning_effort":"low"})
        );
    }

    #[test]
    fn decodes_chat_deltas_with_logprobs_and_bytes() {
        let e = ev(
            r#"{"choices":[{"index":0,"delta":{"content":"é!"},"logprobs":{"content":[{"token":"é","logprob":-0.6931471805599453,"bytes":[195,169]},{"token":"!","logprob":0.0}]}}]}"#,
        );
        let Step::Chunk(c) = OpenAiDecoder.event(&e).unwrap() else {
            panic!("no chunk")
        };
        assert_eq!(c.text, "é!");
        assert_eq!(c.tokens.iter().map(|t| t.len).collect::<Vec<_>>(), [2, 1]);
        assert!((c.tokens[0].prob - 0.5).abs() < 1e-6 && c.tokens[1].prob == 1.0);
    }

    #[test]
    fn decodes_legacy_completion_arrays_and_null_first_logprob() {
        let e = ev(
            r#"{"choices":[{"text":" a b","logprobs":{"tokens":[" a"," b"],"token_logprobs":[null,-1.0]}}]}"#,
        );
        let Step::Chunk(c) = OpenAiDecoder.event(&e).unwrap() else {
            panic!("no chunk")
        };
        assert!(c.tokens[0].prob.is_nan());
        assert!((c.tokens[1].prob - (-1.0f32).exp()).abs() < 1e-6);
    }

    #[test]
    fn done_marker_usage_chunks_and_role_deltas_are_not_text() {
        assert_eq!(OpenAiDecoder.event(&ev("[DONE]")).unwrap(), Step::Done);
        assert_eq!(
            OpenAiDecoder
                .event(&ev(r#"{"choices":[],"usage":{"total_tokens":3}}"#))
                .unwrap(),
            Step::Idle
        );
        assert_eq!(
            OpenAiDecoder
                .event(&ev(
                    r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#
                ))
                .unwrap(),
            Step::Idle
        );
    }

    #[test]
    fn in_stream_errors_are_classified() {
        let e = OpenAiDecoder
            .event(&ev(r#"{"error":{"message":"busy","type":"server_error"}}"#))
            .unwrap_err();
        assert_eq!(e, ProviderError::Unavailable("busy".into()));
    }

    #[test]
    fn whole_response_reads_message_content() {
        let c = OpenAiDecoder
            .whole(br#"{"choices":[{"message":{"role":"assistant","content":"hi"}}]}"#)
            .unwrap();
        assert_eq!(c.text, "hi");
    }
}
