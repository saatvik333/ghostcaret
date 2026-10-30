use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread;
use std::time::{Duration, Instant};

use oft_engine::complete::{
    CancelToken, Candidate, Caps, ChatMessage, CompleteError, Completer, Completion,
    CompletionRequest, GenerateRequest, Generator, Role, StopCheck,
};
use oft_engine::settings::{ProviderConfig, ProviderKind};
use serde_json::Value;
use ureq::http::Uri;

use crate::collect::Accumulator;
use crate::error::ProviderError;
use crate::http::{Http, Settings};
use crate::models::ModelInfo;
use crate::prompt::{self, CLOSE, Shape};
use crate::sentlog::SentLog;
use crate::sse::Flow;
use crate::wire::Decoder;
use crate::{anthropic, gemini, openai};

const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com";
/// Tokens a reasoning model may spend before its first visible word; the
/// output limit counts them too.
const REASONING_HEADROOM: u32 = 1024;
const MAX_MODEL_PAGES: usize = 10;

/// Time and rate limits. [`provider`](crate::provider) uses the defaults;
/// tests and proxies use [`provider_with`](crate::provider_with).
#[derive(Clone, Debug)]
pub struct Tuning {
    /// Client-side limit on requests per minute, shared by every call.
    pub per_minute: u32,
    /// An autocomplete answer later than this is useless.
    pub complete_deadline: Duration,
    /// Covers every attempt of one Flow Draft request, backoff included.
    pub generate_deadline: Duration,
    pub connect_timeout: Duration,
    /// Attempts per Flow Draft request, the first included.
    pub max_attempts: u32,
    /// First retry delay; doubles each attempt, with jitter.
    pub backoff: Duration,
    /// Replaces the fixed address of Anthropic and Gemini (proxies, tests).
    pub endpoint: Option<String>,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            per_minute: 120,
            complete_deadline: Duration::from_secs(8),
            generate_deadline: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(5),
            max_attempts: 4,
            backoff: Duration::from_millis(400),
            endpoint: None,
        }
    }
}

#[derive(Debug)]
enum Kind {
    OpenAi { base: String, hosted: bool },
    Anthropic { base: String },
    Gemini { base: String },
}

/// Which endpoint a request used, to know what a rejection says about it.
#[derive(Clone, Copy, Debug)]
enum Tag {
    Completions,
    Chat,
    Messages,
    Gemini,
}

/// What the provider turned out to support. Everything starts optimistic where
/// that is likely and is switched off for good by the first rejection that
/// names it, so each provider costs at most one failed request per feature.
#[derive(Debug)]
struct Quirks {
    completions: AtomicBool,
    completions_logprobs: AtomicBool,
    chat_logprobs: AtomicBool,
    temperature: AtomicBool,
    reasoning_effort: AtomicBool,
    /// The default output-limit field name was rejected; use the other one.
    max_field_flipped: AtomicBool,
    anthropic_effort: AtomicBool,
    gemini_logprobs: AtomicBool,
    gemini_thinking: AtomicBool,
}

impl Quirks {
    fn new() -> Self {
        let on = || AtomicBool::new(true);
        Quirks {
            completions: on(),
            completions_logprobs: on(),
            chat_logprobs: on(),
            temperature: on(),
            reasoning_effort: AtomicBool::new(false),
            max_field_flipped: AtomicBool::new(false),
            anthropic_effort: AtomicBool::new(false),
            gemini_logprobs: on(),
            gemini_thinking: on(),
        }
    }

    /// Switches off the feature a 400-class rejection names. True when
    /// something changed, so the same request is worth sending again.
    fn relax(&self, tag: Tag, e: &ProviderError) -> bool {
        let ProviderError::Rejected {
            status: Some(status),
            message,
        } = e
        else {
            return false;
        };
        let m = message.to_ascii_lowercase();
        let off = |flag: &AtomicBool| flag.swap(false, Relaxed);
        let invalid = matches!(status, 400 | 422);
        match tag {
            Tag::Completions => {
                matches!(status, 404 | 405 | 501) && off(&self.completions)
                    || invalid
                        && (m.contains("logprobs") && off(&self.completions_logprobs)
                            || m.contains("temperature") && off(&self.temperature)
                            || (m.contains("chat model")
                                || m.contains("/completions")
                                || m.contains("not supported"))
                                && off(&self.completions))
            }
            Tag::Chat => {
                invalid
                    && (m.contains("logprobs") && off(&self.chat_logprobs)
                        || (m.contains("max_tokens") || m.contains("max_completion_tokens"))
                            && !self.max_field_flipped.swap(true, Relaxed)
                        || m.contains("temperature") && off(&self.temperature)
                        || m.contains("reasoning") && off(&self.reasoning_effort))
            }
            Tag::Messages => {
                invalid
                    && (m.contains("effort") || m.contains("output_config"))
                    && off(&self.anthropic_effort)
            }
            Tag::Gemini => {
                invalid
                    && (m.contains("logprobs") && off(&self.gemini_logprobs)
                        || m.contains("thinking") && off(&self.gemini_thinking))
            }
        }
    }
}

/// A configured cloud model. Implements [`Completer`] and [`Generator`].
pub struct Provider {
    label: String,
    model: String,
    kind: Kind,
    http: Http,
    log: SentLog,
    quirks: Quirks,
    tuning: Tuning,
    warm_url: String,
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provider")
            .field("label", &self.label)
            .field("model", &self.model)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Builds a provider with the default limits. `api_key` is optional for
/// OpenAI-compatible servers (Ollama, llama-server) and required for the rest.
pub fn provider(
    config: &ProviderConfig,
    api_key: Option<String>,
    log: SentLog,
) -> Result<Arc<Provider>, ProviderError> {
    provider_with(config, api_key, log, Tuning::default())
}

pub fn provider_with(
    config: &ProviderConfig,
    api_key: Option<String>,
    log: SentLog,
    tuning: Tuning,
) -> Result<Arc<Provider>, ProviderError> {
    if config.model.trim().is_empty() {
        return Err(ProviderError::Config(
            "no model is chosen for this provider".into(),
        ));
    }
    let key = api_key
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty());
    if key
        .as_deref()
        .is_some_and(|k| k.chars().any(char::is_control))
    {
        return Err(ProviderError::Config(
            "the API key contains control characters".into(),
        ));
    }
    let model = config.model.trim().to_owned();
    let no_key = || ProviderError::Config("this provider needs an API key".into());
    let (kind, headers, quirks) = match &config.kind {
        ProviderKind::OpenAiCompatible { base_url } => {
            let base = parse_base(base_url, key.is_some())?;
            let hosted = base_host(&base) == "api.openai.com";
            let reasoning = hosted && is_reasoning_model(&model);
            let quirks = Quirks {
                completions: AtomicBool::new(!hosted || model.contains("instruct")),
                completions_logprobs: AtomicBool::new(true),
                chat_logprobs: AtomicBool::new(!reasoning),
                temperature: AtomicBool::new(!reasoning),
                reasoning_effort: AtomicBool::new(reasoning),
                ..Quirks::new()
            };
            let headers = key
                .iter()
                .map(|k| ("Authorization", format!("Bearer {k}")))
                .collect();
            (Kind::OpenAi { base, hosted }, headers, quirks)
        }
        ProviderKind::Anthropic => {
            let key = key.ok_or_else(no_key)?;
            let base = parse_base(tuning.endpoint.as_deref().unwrap_or(ANTHROPIC_BASE), true)?;
            let quirks = Quirks {
                anthropic_effort: AtomicBool::new(anthropic_takes_effort(&model)),
                ..Quirks::new()
            };
            (
                Kind::Anthropic { base },
                vec![
                    ("x-api-key", key),
                    ("anthropic-version", anthropic::VERSION.to_owned()),
                ],
                quirks,
            )
        }
        ProviderKind::Gemini => {
            let key = key.ok_or_else(no_key)?;
            let base = parse_base(tuning.endpoint.as_deref().unwrap_or(GEMINI_BASE), true)?;
            (
                Kind::Gemini { base },
                vec![("x-goog-api-key", key)],
                Quirks::new(),
            )
        }
    };
    let base = match &kind {
        Kind::OpenAi { base, .. } | Kind::Anthropic { base } | Kind::Gemini { base } => base,
    };
    let warm_url = origin(base);
    let http = Http::new(
        &Settings {
            connect_timeout: tuning.connect_timeout,
        },
        headers,
        tuning.per_minute,
    );
    Ok(Arc::new(Provider {
        label: config.label.clone(),
        model,
        kind,
        http,
        log,
        quirks,
        tuning,
        warm_url,
    }))
}

impl Provider {
    /// Opens the HTTPS connection ahead of the first request, unless one was
    /// kept alive recently. Blocks for one round trip; sends no credentials.
    pub fn warm(&self) {
        let deadline = Instant::now() + self.tuning.connect_timeout * 2;
        self.http
            .warm(&self.warm_url, deadline, &CancelToken::new());
    }

    /// Changes the client-side limit on requests per minute.
    pub fn set_rate_limit(&self, per_minute: u32) {
        self.http.limiter.set_per_minute(per_minute);
    }

    /// Sends one minimal request. `Err` carries the provider's own error text.
    pub fn test_connection(&self) -> Result<(), ProviderError> {
        self.take_slot_now()?;
        let request = GenerateRequest {
            id: 0,
            messages: vec![ChatMessage {
                role: Role::User,
                content: "Reply with the single word: ok".into(),
            }],
            max_tokens: 16,
            temperature: 0.0,
        };
        let deadline = Instant::now() + self.tuning.complete_deadline.max(Duration::from_secs(15));
        self.generate_once(&request, &CancelToken::new(), deadline, &mut |_| {})
            .map(drop)
    }

    /// The models the provider offers, in its own order.
    pub fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.take_slot_now()?;
        let cancel = CancelToken::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut found: Vec<(String, String)> = Vec::new();
        match &self.kind {
            Kind::OpenAi { base, .. } => {
                found = openai::models_of(&self.http.get_json(
                    &format!("{base}/models"),
                    deadline,
                    &cancel,
                )?);
            }
            Kind::Anthropic { base } => {
                let mut after: Option<String> = None;
                for _ in 0..MAX_MODEL_PAGES {
                    let mut url = format!("{base}/v1/models?limit=1000");
                    if let Some(id) = &after {
                        url.push_str(&format!("&after_id={}", encode(id)));
                    }
                    let page = self.http.get_json(&url, deadline, &cancel)?;
                    found.extend(crate::models::from_list(page.get("data")));
                    after = match page.get("has_more").and_then(Value::as_bool) {
                        Some(true) => page
                            .get("last_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        _ => None,
                    };
                    if after.is_none() {
                        break;
                    }
                }
            }
            Kind::Gemini { base } => {
                let mut token: Option<String> = None;
                for _ in 0..MAX_MODEL_PAGES {
                    let mut url = format!("{base}/v1beta/models?pageSize=1000");
                    if let Some(t) = &token {
                        url.push_str(&format!("&pageToken={}", encode(t)));
                    }
                    let (models, next) =
                        gemini::models_page(&self.http.get_json(&url, deadline, &cancel)?);
                    found.extend(models);
                    token = next;
                    if token.is_none() {
                        break;
                    }
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        Ok(found
            .into_iter()
            .filter(|(id, _)| seen.insert(id.clone()))
            .map(|(id, name)| ModelInfo { id, name })
            .collect())
    }

    fn take_slot_now(&self) -> Result<(), ProviderError> {
        self.http
            .limiter
            .acquire(Instant::now())
            .map_err(|wait| self.limit_error(wait))
    }

    fn limit_error(&self, wait: Duration) -> ProviderError {
        ProviderError::RateLimited {
            message: format!(
                "client limit of {} requests per minute reached",
                self.http.limiter.per_minute()
            ),
            retry_after: Some(wait),
        }
    }

    /// Resends while a rejection teaches the provider's quirks; each lesson
    /// is learned once, so this ends.
    fn negotiated<T>(
        &self,
        mut once: impl FnMut() -> (Tag, Result<T, ProviderError>),
    ) -> Result<T, ProviderError> {
        loop {
            match once() {
                (tag, Err(e)) if self.quirks.relax(tag, &e) => {
                    log::info!("{}: adapting after rejection: {e}", self.label)
                }
                (_, result) => return result,
            }
        }
    }

    fn max_field(&self, hosted: bool) -> openai::MaxField {
        match hosted != self.quirks.max_field_flipped.load(Relaxed) {
            true => openai::MaxField::MaxCompletionTokens,
            false => openai::MaxField::MaxTokens,
        }
    }

    fn run_complete(
        &self,
        req: &CompletionRequest,
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
    ) -> Result<Completion, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        self.take_slot_now()?;
        let deadline = Instant::now() + self.tuning.complete_deadline;
        let candidate = self.negotiated(|| self.complete_once(req, stop, cancel, deadline))?;
        Ok(Completion {
            id: req.id,
            candidates: candidate.into_iter().collect(),
        })
    }

    fn complete_once(
        &self,
        req: &CompletionRequest,
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> (Tag, Result<Option<Candidate>, ProviderError>) {
        let q = &self.quirks;
        let before = req.segments.before.as_str();
        let multiline = req.segments.multiline;
        let on = |flag: &AtomicBool| flag.load(Relaxed);
        match &self.kind {
            Kind::OpenAi { base, hosted } if on(&q.completions) => {
                let o = openai::Options {
                    model: &self.model,
                    max_tokens: req.max_tokens,
                    max_field: self.max_field(*hosted),
                    stop: prompt::raw_stops(&req.segments),
                    logprobs: on(&q.completions_logprobs),
                    temperature: on(&q.temperature).then_some(0.0),
                    reasoning_effort: None,
                };
                let body = openai::completions_body(&o, &prompt::raw_prompt(req));
                let shape = Shape {
                    before,
                    chat: false,
                    multiline,
                };
                let url = format!("{base}/completions");
                (
                    Tag::Completions,
                    self.collect(
                        &url,
                        body,
                        &mut openai::OpenAiDecoder,
                        shape,
                        stop,
                        cancel,
                        deadline,
                    ),
                )
            }
            Kind::OpenAi { base, hosted } => {
                let p = prompt::chat_prompt(req);
                let effort = on(&q.reasoning_effort).then_some("low");
                let o = openai::Options {
                    model: &self.model,
                    max_tokens: req.max_tokens
                        + if effort.is_some() {
                            REASONING_HEADROOM
                        } else {
                            0
                        },
                    max_field: self.max_field(*hosted),
                    stop: &[CLOSE],
                    logprobs: on(&q.chat_logprobs),
                    temperature: on(&q.temperature).then_some(0.0),
                    reasoning_effort: effort,
                };
                let messages = [
                    openai::Message {
                        role: "system",
                        content: &p.system,
                    },
                    openai::Message {
                        role: "user",
                        content: &p.user,
                    },
                ];
                let body = openai::chat_body(&o, &messages);
                let shape = Shape {
                    before,
                    chat: true,
                    multiline,
                };
                let url = format!("{base}/chat/completions");
                (
                    Tag::Chat,
                    self.collect(
                        &url,
                        body,
                        &mut openai::OpenAiDecoder,
                        shape,
                        stop,
                        cancel,
                        deadline,
                    ),
                )
            }
            Kind::Anthropic { base } => {
                let p = prompt::chat_prompt(req);
                let effort = on(&q.anthropic_effort).then_some("low");
                let o = anthropic::Options {
                    model: &self.model,
                    max_tokens: req.max_tokens
                        + if effort.is_some() {
                            REASONING_HEADROOM
                        } else {
                            0
                        },
                    stop: &[CLOSE],
                    effort,
                    cache_system: true,
                };
                let body = anthropic::body(&o, Some(&p.system), &[("user", &p.user)]);
                let shape = Shape {
                    before,
                    chat: true,
                    multiline,
                };
                let url = format!("{base}/v1/messages");
                (
                    Tag::Messages,
                    self.collect(
                        &url,
                        body,
                        &mut anthropic::AnthropicDecoder,
                        shape,
                        stop,
                        cancel,
                        deadline,
                    ),
                )
            }
            Kind::Gemini { base } => {
                let p = prompt::chat_prompt(req);
                let thinking = on(&q.gemini_thinking)
                    .then(|| gemini::thinking_for(&self.model))
                    .flatten();
                let o = gemini::Options {
                    max_tokens: req.max_tokens
                        + gemini::thinking_headroom(&self.model, thinking.is_some()),
                    stop: &[CLOSE],
                    logprobs: on(&q.gemini_logprobs),
                    // Gemini 3 and later are tuned for their default temperature.
                    temperature: thinking
                        .as_ref()
                        .is_none_or(|t| t.get("thinkingLevel").is_none())
                        .then_some(0.0),
                    thinking,
                };
                let body = gemini::body(&o, Some(&p.system), &[("user", &p.user)]);
                let shape = Shape {
                    before,
                    chat: true,
                    multiline,
                };
                let url = self.gemini_url(base);
                (
                    Tag::Gemini,
                    self.collect(
                        &url,
                        body,
                        &mut gemini::GeminiDecoder,
                        shape,
                        stop,
                        cancel,
                        deadline,
                    ),
                )
            }
        }
    }

    fn gemini_url(&self, base: &str) -> String {
        format!(
            "{base}/v1beta/{}:streamGenerateContent?alt=sse",
            gemini::model_path(&self.model)
        )
    }

    fn collect(
        &self,
        url: &str,
        body: String,
        decoder: &mut dyn Decoder,
        shape: Shape<'_>,
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<Option<Candidate>, ProviderError> {
        self.log.record(&self.label, &self.model, &body);
        let mut acc = Accumulator::new(shape);
        self.http
            .stream(url, &body, deadline, cancel, decoder, &mut |chunk| {
                Ok(acc.push(&chunk, stop))
            })?;
        Ok(acc.finish())
    }

    /// One attempt at a Flow Draft request; streams text to `on_delta`.
    fn generate_once(
        &self,
        req: &GenerateRequest,
        cancel: &CancelToken,
        deadline: Instant,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, ProviderError> {
        self.negotiated(|| {
            let (tag, url, body, mut decoder) = self.generate_wire(req);
            self.log.record(&self.label, &self.model, &body);
            let mut text = String::new();
            let result =
                self.http
                    .stream(&url, &body, deadline, cancel, &mut *decoder, &mut |chunk| {
                        on_delta(&chunk.text);
                        text.push_str(&chunk.text);
                        Ok(Flow::Continue)
                    });
            (tag, result.map(|()| text))
        })
    }

    fn generate_wire(&self, req: &GenerateRequest) -> (Tag, String, String, Box<dyn Decoder>) {
        let q = &self.quirks;
        let on = |flag: &AtomicBool| flag.load(Relaxed);
        match &self.kind {
            Kind::OpenAi { base, hosted } => {
                let effort = on(&q.reasoning_effort).then_some("low");
                let o = openai::Options {
                    model: &self.model,
                    max_tokens: req.max_tokens
                        + if effort.is_some() {
                            REASONING_HEADROOM
                        } else {
                            0
                        },
                    max_field: self.max_field(*hosted),
                    stop: &[],
                    logprobs: false,
                    temperature: on(&q.temperature).then_some(req.temperature),
                    reasoning_effort: effort,
                };
                let messages: Vec<_> = req
                    .messages
                    .iter()
                    .map(|m| openai::Message {
                        role: role_name(m.role),
                        content: &m.content,
                    })
                    .collect();
                (
                    Tag::Chat,
                    format!("{base}/chat/completions"),
                    openai::chat_body(&o, &messages),
                    Box::new(openai::OpenAiDecoder),
                )
            }
            Kind::Anthropic { base } => {
                let (system, turns) = split_system(&req.messages);
                let effort = on(&q.anthropic_effort).then_some("low");
                let o = anthropic::Options {
                    model: &self.model,
                    max_tokens: req.max_tokens
                        + if effort.is_some() {
                            REASONING_HEADROOM
                        } else {
                            0
                        },
                    stop: &[],
                    effort,
                    cache_system: false,
                };
                let body = anthropic::body(&o, Some(&system), &turns);
                (
                    Tag::Messages,
                    format!("{base}/v1/messages"),
                    body,
                    Box::new(anthropic::AnthropicDecoder),
                )
            }
            Kind::Gemini { base } => {
                let (system, turns) = split_system(&req.messages);
                let turns: Vec<(&'static str, &str)> = turns
                    .into_iter()
                    .map(|(role, text)| (if role == "assistant" { "model" } else { role }, text))
                    .collect();
                let thinking = on(&q.gemini_thinking)
                    .then(|| gemini::thinking_for(&self.model))
                    .flatten();
                let o = gemini::Options {
                    max_tokens: req.max_tokens
                        + gemini::thinking_headroom(&self.model, thinking.is_some()),
                    stop: &[],
                    logprobs: false,
                    temperature: on(&q.temperature).then_some(req.temperature),
                    thinking,
                };
                let body = gemini::body(&o, Some(&system), &turns);
                (
                    Tag::Gemini,
                    self.gemini_url(base),
                    body,
                    Box::new(gemini::GeminiDecoder),
                )
            }
        }
    }

    /// Waits for a rate-limit slot, within the deadline.
    fn wait_for_slot(&self, deadline: Instant, cancel: &CancelToken) -> Result<(), ProviderError> {
        loop {
            match self.http.limiter.acquire(Instant::now()) {
                Ok(()) => return Ok(()),
                Err(wait) if Instant::now() + wait <= deadline => sleep(wait, cancel)?,
                Err(wait) => return Err(self.limit_error(wait)),
            }
        }
    }
}

impl Completer for Provider {
    fn caps(&self) -> Caps {
        let q = &self.quirks;
        let on = |flag: &AtomicBool| flag.load(Relaxed);
        let token_probs = match self.kind {
            Kind::OpenAi { .. } => {
                on(&q.completions) && on(&q.completions_logprobs) || on(&q.chat_logprobs)
            }
            Kind::Anthropic { .. } => false,
            Kind::Gemini { .. } => on(&q.gemini_logprobs),
        };
        Caps {
            token_probs,
            cheap_alternatives: false,
            prefill: false,
            local: false,
        }
    }

    fn complete(
        &self,
        request: &CompletionRequest,
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
    ) -> Result<Completion, CompleteError> {
        self.run_complete(request, stop, cancel).map_err(Into::into)
    }

    /// Cloud models have no cache to warm from here; what helps is a ready
    /// connection.
    fn prefill(&self, _request: &CompletionRequest, cancel: &CancelToken) {
        let deadline = Instant::now() + self.tuning.connect_timeout * 2;
        self.http.warm(&self.warm_url, deadline, cancel);
    }
}

impl Generator for Provider {
    fn generate(
        &self,
        request: &GenerateRequest,
        cancel: &CancelToken,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, CompleteError> {
        let deadline = Instant::now() + self.tuning.generate_deadline;
        let mut delivered = false;
        let mut attempt = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(CompleteError::Cancelled);
            }
            self.wait_for_slot(deadline, cancel)?;
            let result = self.generate_once(request, cancel, deadline, &mut |delta| {
                delivered |= !delta.is_empty();
                on_delta(delta);
            });
            let error = match result {
                Ok(text) => return Ok(text),
                Err(e) => e,
            };
            attempt += 1;
            // Text already shown cannot be taken back, so a stream that broke
            // midway is an error, not a retry.
            if !error.is_transient() || delivered || attempt >= self.tuning.max_attempts {
                return Err(error.into());
            }
            let delay = self.retry_delay(&error, attempt);
            if Instant::now() + delay >= deadline {
                return Err(error.into());
            }
            log::info!(
                "{}: attempt {attempt} failed ({error}); retrying in {delay:?}",
                self.label
            );
            sleep(delay, cancel)?;
        }
    }
}

impl Provider {
    /// The server's `Retry-After` when it gave one; otherwise exponential
    /// backoff. Both get jitter so clients that failed together do not return
    /// together.
    fn retry_delay(&self, error: &ProviderError, attempt: u32) -> Duration {
        match error.retry_after() {
            Some(wait) => wait.mul_f64(1.0 + 0.1 * fastrand::f64()),
            None => {
                let cap = self
                    .tuning
                    .backoff
                    .saturating_mul(1 << attempt.saturating_sub(1).min(6));
                cap.mul_f64(0.5 + 0.5 * fastrand::f64())
            }
        }
    }
}

fn sleep(total: Duration, cancel: &CancelToken) -> Result<(), ProviderError> {
    let end = Instant::now() + total;
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        thread::sleep(left.min(Duration::from_millis(25)));
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

/// System messages joined into one prompt, and the remaining turns.
fn split_system(messages: &[ChatMessage]) -> (String, Vec<(&'static str, &str)>) {
    let system = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let turns = messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| (role_name(m.role), m.content.as_str()))
        .collect();
    (system, turns)
}

fn is_reasoning_model(model: &str) -> bool {
    ["gpt-5", "o1", "o3", "o4"]
        .iter()
        .any(|p| model.starts_with(p))
}

/// Claude 5 and later always think and are steered with `effort`; earlier
/// models reject the parameter.
fn anthropic_takes_effort(model: &str) -> bool {
    let id = model.to_ascii_lowercase();
    id.contains("fable")
        || id.contains("mythos")
        || id
            .split('-')
            .filter_map(|p| p.parse::<u32>().ok())
            .find(|&n| n < 100)
            .is_some_and(|major| major >= 5)
}

fn parse_base(raw: &str, has_key: bool) -> Result<String, ProviderError> {
    let base = raw.trim().trim_end_matches('/');
    let uri: Uri = base
        .parse()
        .map_err(|e| ProviderError::Config(format!("invalid base URL: {e}")))?;
    match uri.scheme_str() {
        Some("https") => {}
        Some("http") => {
            if has_key && !is_private_host(uri.host().unwrap_or("")) {
                return Err(ProviderError::Config(
                    "refusing to send an API key over plain http to a public host; use https"
                        .into(),
                ));
            }
        }
        _ => {
            return Err(ProviderError::Config(
                "the base URL must start with https:// or http://".into(),
            ));
        }
    }
    if uri.host().is_none() {
        return Err(ProviderError::Config("the base URL has no host".into()));
    }
    Ok(base.to_owned())
}

fn base_host(base: &str) -> String {
    base.parse::<Uri>()
        .ok()
        .and_then(|u| u.host().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// `scheme://authority/`, the page a warm-up request asks for.
fn origin(base: &str) -> String {
    match base.parse::<Uri>() {
        Ok(uri) => format!(
            "{}://{}/",
            uri.scheme_str().unwrap_or("https"),
            uri.authority().map_or("", |a| a.as_str())
        ),
        Err(_) => format!("{base}/"),
    }
}

/// Hosts where plain http cannot cross the public internet.
fn is_private_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            IpAddr::V6(v6) => {
                let s = v6.segments()[0];
                v6.is_loopback() || s & 0xfe00 == 0xfc00 || s & 0xffc0 == 0xfe80
            }
        };
    }
    let host = host.to_ascii_lowercase();
    !host.contains('.')
        || [".local", ".lan", ".internal", ".home.arpa", ".localhost"]
            .iter()
            .any(|s| host.ends_with(s))
}

/// Percent-encoding for a query value.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_is_allowed_only_where_the_network_is_private() {
        for ok in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080",
            "http://192.168.1.5/v1",
            "http://[::1]:8080",
            "http://gpu-box:8000/v1",
            "http://llm.lan/v1",
            "http://[fd00::1]/v1",
        ] {
            assert!(parse_base(ok, true).is_ok(), "{ok}");
        }
        for bad in [
            "http://api.example.com/v1",
            "http://8.8.8.8/v1",
            "http://[2001:db8::1]/",
        ] {
            assert!(
                matches!(parse_base(bad, true), Err(ProviderError::Config(_))),
                "{bad}"
            );
            assert!(
                parse_base(bad, false).is_ok(),
                "no key, nothing to leak: {bad}"
            );
        }
    }

    #[test]
    fn base_urls_must_be_http_or_https_with_a_host() {
        assert!(parse_base("ftp://x/v1", false).is_err());
        assert!(parse_base("not a url", false).is_err());
        assert_eq!(
            parse_base(" https://api.openai.com/v1/ ", true).unwrap(),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn warm_target_is_the_origin_only() {
        assert_eq!(
            origin("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/"
        );
        assert_eq!(
            origin("http://localhost:11434/v1"),
            "http://localhost:11434/"
        );
    }

    #[test]
    fn anthropic_effort_applies_to_the_always_thinking_generation_only() {
        assert!(anthropic_takes_effort("claude-opus-5-5"));
        assert!(anthropic_takes_effort("claude-haiku-5-5"));
        assert!(anthropic_takes_effort("claude-fable-5"));
        assert!(!anthropic_takes_effort("claude-haiku-4-5-20251001"));
        assert!(!anthropic_takes_effort("claude-3-5-haiku-20241022"));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("a b/c+d=é"), "a%20b%2Fc%2Bd%3D%C3%A9");
    }

    #[test]
    fn a_rejection_that_names_a_feature_switches_only_that_feature_off_once() {
        let q = Quirks::new();
        let rejected = |status, message: &str| ProviderError::Rejected {
            status: Some(status),
            message: message.into(),
        };
        assert!(q.relax(
            Tag::Chat,
            &rejected(400, "logprobs is not supported with this model")
        ));
        assert!(!q.chat_logprobs.load(Relaxed) && q.temperature.load(Relaxed));
        assert!(
            !q.relax(
                Tag::Chat,
                &rejected(400, "logprobs is not supported with this model")
            ),
            "nothing left to change"
        );
        assert!(q.relax(
            Tag::Chat,
            &rejected(
                400,
                "Unsupported parameter: 'max_tokens'. Use 'max_completion_tokens'"
            )
        ));
        assert!(
            !q.relax(Tag::Chat, &rejected(400, "max_tokens again")),
            "the limit field flips once"
        );
        assert!(
            !q.relax(Tag::Chat, &rejected(401, "logprobs")),
            "a bad key is not a quirk"
        );
        assert!(q.relax(Tag::Completions, &rejected(404, "Not Found")));
        assert!(!q.completions.load(Relaxed));
    }
}
