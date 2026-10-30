//! Gemini API: `models/{model}:streamGenerateContent?alt=sse`. Every event is
//! a complete `GenerateContentResponse` holding the next piece of text.

use serde_json::{Value, json};

use crate::error::ProviderError;
use crate::sse::SseEvent;
use crate::wire::{Chunk, Decoder, Step, Token, embedded_error, parse_json, temperature};

pub(crate) struct Options<'a> {
    pub max_tokens: u32,
    pub stop: &'a [&'a str],
    pub logprobs: bool,
    pub temperature: Option<f32>,
    pub thinking: Option<Value>,
}

pub(crate) fn body(
    o: &Options<'_>,
    system: Option<&str>,
    contents: &[(&'static str, &str)],
) -> String {
    let contents: Vec<Value> = contents
        .iter()
        .map(|(role, text)| json!({ "role": role, "parts": [{ "text": text }] }))
        .collect();
    let mut config = json!({ "maxOutputTokens": o.max_tokens });
    if let Some(t) = o.temperature {
        config["temperature"] = json!(temperature(t));
    }
    if !o.stop.is_empty() {
        config["stopSequences"] = json!(o.stop);
    }
    if o.logprobs {
        config["responseLogprobs"] = json!(true);
        config["logprobs"] = json!(1);
    }
    if let Some(thinking) = &o.thinking {
        config["thinkingConfig"] = thinking.clone();
    }
    let mut body = json!({ "contents": contents, "generationConfig": config });
    if let Some(system) = system.filter(|s| !s.is_empty()) {
        body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }
    body.to_string()
}

/// `models/gemini-x` whether the user typed the prefix or not.
pub(crate) fn model_path(model: &str) -> String {
    let id = model.strip_prefix("models/").unwrap_or(model);
    format!("models/{id}")
}

/// Thinking settings that keep a model's reasoning short, for the models that
/// accept them. `thinkingBudget` is the 2.5 family's control; Gemini 3 and
/// later use `thinkingLevel` and reject the budget. Earlier models reject
/// both, and an unknown name gets none.
pub(crate) fn thinking_for(model: &str) -> Option<Value> {
    let id = model.strip_prefix("models/").unwrap_or(model);
    if id.contains("gemini-2.5") {
        // 2.5 Pro cannot switch thinking off; 128 tokens is its floor.
        return Some(json!({ "thinkingBudget": if id.contains("pro") { 128 } else { 0 } }));
    }
    let major: u32 = id
        .strip_prefix("gemini-")?
        .split(['.', '-'])
        .next()?
        .parse()
        .ok()?;
    (major >= 3).then(|| json!({ "thinkingLevel": "low" }))
}

/// Tokens a model may spend thinking before it writes, which `maxOutputTokens`
/// also counts.
pub(crate) fn thinking_headroom(model: &str, configured: bool) -> u32 {
    if configured {
        256
    } else if model.contains("latest") {
        1024
    } else {
        0
    }
}

#[derive(Debug)]
pub(crate) struct GeminiDecoder;

impl Decoder for GeminiDecoder {
    fn event(&mut self, event: &SseEvent) -> Result<Step, ProviderError> {
        let chunk = self.whole(event.data.as_bytes())?;
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
        let Some(candidate) = v.pointer("/candidates/0") else {
            return match v
                .pointer("/promptFeedback/blockReason")
                .and_then(Value::as_str)
            {
                Some(reason) => Err(ProviderError::Rejected {
                    status: None,
                    message: format!("prompt blocked by the provider: {reason}"),
                }),
                None => Ok(Chunk::default()),
            };
        };
        let text: String = candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        if text.is_empty() {
            return match candidate.get("finishReason").and_then(Value::as_str) {
                Some(
                    reason @ ("SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII"
                    | "IMAGE_SAFETY"),
                ) => Err(ProviderError::Rejected {
                    status: None,
                    message: format!("response blocked by the provider: {reason}"),
                }),
                _ => Ok(Chunk::default()),
            };
        }
        let tokens = candidate
            .pointer("/logprobsResult/chosenCandidates")
            .and_then(Value::as_array)
            .map(|c| {
                c.iter()
                    .map(|t| Token {
                        len: t.get("token").and_then(Value::as_str).map_or(0, str::len),
                        prob: t
                            .get("logProbability")
                            .and_then(Value::as_f64)
                            .map_or(f32::NAN, |lp| lp.exp() as f32),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Chunk { text, tokens })
    }
}

/// One page of `GET /v1beta/models`: usable models and the next page token.
pub(crate) fn models_page(v: &Value) -> (Vec<(String, String)>, Option<String>) {
    let models = v
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| {
            m.get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_none_or(|methods| {
                    methods
                        .iter()
                        .any(|x| x.as_str() == Some("generateContent"))
                })
        })
        .filter_map(|m| {
            let id = m.get("name")?.as_str()?.strip_prefix("models/")?.to_owned();
            let name = m
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(&id)
                .to_owned();
            Some((id, name))
        })
        .collect();
    let next = v
        .get("nextPageToken")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    (models, next)
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
    fn body_carries_logprobs_stops_and_thinking_in_generation_config() {
        let o = Options {
            max_tokens: 12,
            stop: &["</continuation>"],
            logprobs: true,
            temperature: Some(0.0),
            thinking: thinking_for("gemini-2.5-flash"),
        };
        let v: Value = serde_json::from_str(&body(&o, Some("sys"), &[("user", "hi")])).unwrap();
        assert_eq!(
            v,
            json!({
                "contents":[{"role":"user","parts":[{"text":"hi"}]}],
                "systemInstruction":{"parts":[{"text":"sys"}]},
                "generationConfig":{"maxOutputTokens":12,"temperature":0.0,"stopSequences":["</continuation>"],
                    "responseLogprobs":true,"logprobs":1,"thinkingConfig":{"thinkingBudget":0}}
            })
        );
    }

    #[test]
    fn thinking_settings_follow_the_model_family() {
        assert_eq!(
            thinking_for("gemini-2.5-flash-lite"),
            Some(json!({"thinkingBudget":0}))
        );
        assert_eq!(
            thinking_for("models/gemini-2.5-pro"),
            Some(json!({"thinkingBudget":128}))
        );
        assert_eq!(
            thinking_for("gemini-3-flash-preview"),
            Some(json!({"thinkingLevel":"low"}))
        );
        assert_eq!(
            thinking_for("gemini-3.8-flash"),
            Some(json!({"thinkingLevel":"low"}))
        );
        assert_eq!(thinking_for("gemini-2.0-flash"), None);
        assert_eq!(thinking_for("gemini-flash-latest"), None);
        assert_eq!(thinking_for("gemma-3-27b-it"), None);
    }

    #[test]
    fn model_path_accepts_either_spelling() {
        assert_eq!(model_path("gemini-x"), "models/gemini-x");
        assert_eq!(model_path("models/gemini-x"), "models/gemini-x");
    }

    #[test]
    fn event_yields_text_without_thoughts_and_chosen_token_probabilities() {
        let e = ev(
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"plan","thought":true},{"text":"Hi!"}]},
            "logprobsResult":{"chosenCandidates":[{"token":"Hi","logProbability":-0.6931471805599453},{"token":"!","logProbability":0}]}}]}"#,
        );
        let Step::Chunk(c) = GeminiDecoder.event(&e).unwrap() else {
            panic!("no chunk")
        };
        assert_eq!(c.text, "Hi!");
        assert_eq!(c.tokens.len(), 2);
        assert!((c.tokens[0].prob - 0.5).abs() < 1e-6);
    }

    #[test]
    fn blocked_prompt_and_blocked_answer_are_errors_with_the_reason() {
        let prompt = GeminiDecoder
            .event(&ev(r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#))
            .unwrap_err();
        assert!(prompt.to_string().contains("SAFETY"));
        let answer = GeminiDecoder
            .event(&ev(r#"{"candidates":[{"finishReason":"SAFETY"}]}"#))
            .unwrap_err();
        assert!(answer.to_string().contains("SAFETY"));
        assert_eq!(
            GeminiDecoder
                .event(&ev(r#"{"candidates":[{"finishReason":"STOP"}]}"#))
                .unwrap(),
            Step::Idle
        );
    }

    #[test]
    fn models_page_keeps_generating_models_and_strips_the_prefix() {
        let v = json!({"models":[
            {"name":"models/gemini-a","displayName":"Gemini A","supportedGenerationMethods":["generateContent","countTokens"]},
            {"name":"models/embed","supportedGenerationMethods":["embedContent"]},
            {"name":"models/gemini-b"}],
            "nextPageToken":"t"});
        let (models, next) = models_page(&v);
        assert_eq!(
            models,
            [
                ("gemini-a".into(), "Gemini A".into()),
                ("gemini-b".into(), "gemini-b".into())
            ]
        );
        assert_eq!(next.as_deref(), Some("t"));
    }
}
