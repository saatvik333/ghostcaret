//! Anthropic Messages API (`POST /v1/messages`, streamed).
//!
//! Assistant prefill is rejected by current models (Claude 4.6 and later
//! return 400) and sampling parameters are removed from the newest ones, so
//! requests carry neither: the instructions in the prompt do the work.

use serde_json::{Value, json};

use crate::error::ProviderError;
use crate::sse::SseEvent;
use crate::wire::{Chunk, Decoder, Step, embedded_error, parse_json};

pub(crate) const VERSION: &str = "2023-06-01";

pub(crate) struct Options<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub stop: &'a [&'a str],
    /// Thinking depth for models that always think and take `effort`.
    pub effort: Option<&'static str>,
    /// Mark the system prompt as cacheable; it is the part that repeats.
    pub cache_system: bool,
}

pub(crate) fn body(
    o: &Options<'_>,
    system: Option<&str>,
    messages: &[(&'static str, &str)],
) -> String {
    let messages: Vec<Value> = messages
        .iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();
    let mut body = json!({ "model": o.model, "max_tokens": o.max_tokens, "messages": messages, "stream": true });
    if let Some(system) = system.filter(|s| !s.is_empty()) {
        body["system"] = if o.cache_system {
            json!([{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }])
        } else {
            json!(system)
        };
    }
    if !o.stop.is_empty() {
        body["stop_sequences"] = json!(o.stop);
    }
    if let Some(effort) = o.effort {
        body["output_config"] = json!({ "effort": effort });
    }
    body.to_string()
}

#[derive(Debug)]
pub(crate) struct AnthropicDecoder;

impl Decoder for AnthropicDecoder {
    fn event(&mut self, event: &SseEvent) -> Result<Step, ProviderError> {
        let v = parse_json(&event.data)?;
        match v.get("type").and_then(Value::as_str) {
            Some("content_block_delta")
                if v.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta") =>
            {
                let text = v
                    .pointer("/delta/text")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Ok(if text.is_empty() {
                    Step::Idle
                } else {
                    Step::Chunk(Chunk {
                        text: text.to_owned(),
                        tokens: Vec::new(),
                    })
                })
            }
            Some("message_stop") => Ok(Step::Done),
            Some("error") => Err(embedded_error(&v)
                .unwrap_or_else(|| ProviderError::Malformed("error event without detail".into()))),
            _ => Ok(Step::Idle),
        }
    }

    fn whole(&mut self, body: &[u8]) -> Result<Chunk, ProviderError> {
        let v = parse_json(&String::from_utf8_lossy(body))?;
        if v.get("type").and_then(Value::as_str) == Some("error") {
            return Err(embedded_error(&v)
                .unwrap_or_else(|| ProviderError::Malformed("error without detail".into())));
        }
        let text: String = v
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        Ok(Chunk {
            text,
            tokens: Vec::new(),
        })
    }
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

    #[test]
    fn body_marks_the_system_prompt_cacheable_and_sets_stops() {
        let o = Options {
            model: "m",
            max_tokens: 9,
            stop: &["</continuation>"],
            effort: Some("low"),
            cache_system: true,
        };
        let v: Value = serde_json::from_str(&body(&o, Some("sys"), &[("user", "hi")])).unwrap();
        assert_eq!(
            v,
            json!({
                "model":"m","max_tokens":9,"stream":true,
                "system":[{"type":"text","text":"sys","cache_control":{"type":"ephemeral"}}],
                "messages":[{"role":"user","content":"hi"}],
                "stop_sequences":["</continuation>"],
                "output_config":{"effort":"low"}
            })
        );
    }

    #[test]
    fn body_never_carries_prefill_or_sampling_parameters() {
        let o = Options {
            model: "m",
            max_tokens: 9,
            stop: &[],
            effort: None,
            cache_system: false,
        };
        let v: Value = serde_json::from_str(&body(&o, Some("sys"), &[("user", "hi")])).unwrap();
        assert_eq!(v["system"], "sys");
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
        for k in [
            "temperature",
            "top_p",
            "top_k",
            "thinking",
            "output_config",
            "stop_sequences",
        ] {
            assert!(v.get(k).is_none(), "{k}");
        }
    }

    #[test]
    fn only_text_deltas_become_chunks() {
        let text = ev(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        );
        assert_eq!(
            AnthropicDecoder.event(&text).unwrap(),
            Step::Chunk(Chunk {
                text: "Hi".into(),
                tokens: vec![]
            })
        );
        for idle in [
            r#"{"type":"message_start","message":{"id":"x"}}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        ] {
            assert_eq!(
                AnthropicDecoder.event(&ev(idle)).unwrap(),
                Step::Idle,
                "{idle}"
            );
        }
        assert_eq!(
            AnthropicDecoder
                .event(&ev(r#"{"type":"message_stop"}"#))
                .unwrap(),
            Step::Done
        );
    }

    #[test]
    fn overload_inside_the_stream_is_transient() {
        let e = AnthropicDecoder
            .event(&ev(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            ))
            .unwrap_err();
        assert_eq!(e, ProviderError::Unavailable("Overloaded".into()));
    }

    #[test]
    fn whole_response_joins_text_blocks() {
        let c = AnthropicDecoder
            .whole(br#"{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#)
            .unwrap();
        assert_eq!(c.text, "ab");
    }
}
