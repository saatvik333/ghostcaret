//! Runs one autocomplete and one Reply & rewrite request against a real endpoint.
//!
//! ```text
//! GHOSTCARET_PROVIDER=openai GHOSTCARET_BASE_URL=http://localhost:8080/v1 GHOSTCARET_MODEL=qwen cargo run -p ghostcaret-cloud --example try
//! GHOSTCARET_PROVIDER=anthropic GHOSTCARET_MODEL=claude-haiku-4-5 GHOSTCARET_API_KEY=... cargo run -p ghostcaret-cloud --example try
//! GHOSTCARET_PROVIDER=gemini GHOSTCARET_MODEL=gemini-2.5-flash GHOSTCARET_API_KEY=... cargo run -p ghostcaret-cloud --example try
//! ```
//!
//! `GHOSTCARET_PROVIDER` is `openai` (any OpenAI-compatible server; needs
//! `GHOSTCARET_BASE_URL`), `anthropic` or `gemini`. `GHOSTCARET_API_KEY` is optional for
//! local servers. `GHOSTCARET_BEFORE` replaces the sample text before the caret.

use std::process::ExitCode;
use std::time::Instant;

use ghostcaret_cloud::{SentLog, provider};
use ghostcaret_engine::complete::{
    CancelToken, ChatMessage, Completer, CompletionRequest, GenerateRequest, Generator, LengthMode,
    Role, Segments, StopCheck, StopParams,
};
use ghostcaret_engine::io::{AppInfo, Author, FieldId, Message};
use ghostcaret_engine::settings::{ProviderConfig, ProviderKind};

fn main() -> ExitCode {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let (Some(kind), Some(model)) = (env("GHOSTCARET_PROVIDER"), env("GHOSTCARET_MODEL")) else {
        eprintln!(
            "set GHOSTCARET_PROVIDER (openai|anthropic|gemini) and GHOSTCARET_MODEL; see the header of this example"
        );
        return ExitCode::from(2);
    };
    let kind = match kind.as_str() {
        "openai" => match env("GHOSTCARET_BASE_URL") {
            Some(base_url) => ProviderKind::OpenAiCompatible { base_url },
            None => {
                eprintln!(
                    "GHOSTCARET_PROVIDER=openai needs GHOSTCARET_BASE_URL, e.g. https://api.openai.com/v1"
                );
                return ExitCode::from(2);
            }
        },
        "anthropic" => ProviderKind::Anthropic,
        "gemini" => ProviderKind::Gemini,
        other => {
            eprintln!("unknown GHOSTCARET_PROVIDER {other:?}");
            return ExitCode::from(2);
        }
    };
    let config = ProviderConfig {
        id: "try".into(),
        label: "try".into(),
        kind,
        model,
    };
    let log = SentLog::default();
    let provider = match provider(&config, env("GHOSTCARET_API_KEY"), log.clone()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot build the provider: {e}");
            return ExitCode::FAILURE;
        }
    };

    let started = Instant::now();
    match provider.list_models() {
        Ok(models) => println!(
            "models: {} offered ({:.0?})",
            models.len(),
            started.elapsed()
        ),
        Err(e) => println!("models: {e}"),
    }
    if let Err(e) = provider.test_connection() {
        eprintln!("test connection failed: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "connection ok; token probabilities: {}",
        provider.caps().token_probs
    );
    provider.warm();

    let before = env("GHOSTCARET_BEFORE")
        .unwrap_or_else(|| "Thanks for the update, I'll have a look at the".into());
    let request = CompletionRequest {
        id: 1,
        field: FieldId(1),
        revision: 1,
        segments: Segments {
            app: AppInfo {
                id: "try".into(),
                name: "Chat".into(),
                title: "Project room".into(),
            },
            conversation: vec![Message {
                author: Author::Other("Sam".into()),
                text: "PR #42 is ready for review".into(),
            }],
            before: before.clone(),
            ..Segments::default()
        },
        stop: StopParams {
            mode: LengthMode::Phrase,
            ceiling: LengthMode::Sentence,
            boldness: 0.5,
            paragraph_words: 80,
        },
        max_tokens: 24,
        alternatives: 0,
    };
    struct Print(Instant, bool);
    impl StopCheck for Print {
        fn should_stop(&mut self, generated: &str, token_prob: Option<f32>) -> bool {
            if !self.1 && !generated.is_empty() {
                self.1 = true;
                println!("first visible text after {:.0?}", self.0.elapsed());
            }
            if let Some(p) = token_prob {
                print!("[{p:.2}]");
            }
            false
        }
    }
    let started = Instant::now();
    let cancel = CancelToken::new();
    match provider.complete(&request, &mut Print(started, false), &cancel) {
        Ok(done) => {
            println!("\nautocomplete in {:.0?}:", started.elapsed());
            for c in &done.candidates {
                println!("  {before}\u{1b}[2m{}\u{1b}[0m", c.text);
                println!("  token ends {:?}", c.token_ends);
            }
            if done.candidates.is_empty() {
                println!("  (no suggestion)");
            }
        }
        Err(e) => {
            eprintln!("autocomplete failed: {e}");
            return ExitCode::FAILURE;
        }
    }

    let draft = GenerateRequest {
        id: 2,
        messages: vec![
            ChatMessage {
                role: Role::System,
                content:
                    "Draft a short, friendly reply to the last message. Output only the reply."
                        .into(),
            },
            ChatMessage {
                role: Role::User,
                content: "Sam: PR #42 is ready for review".into(),
            },
        ],
        max_tokens: 120,
        temperature: 0.7,
    };
    let started = Instant::now();
    println!("\nReply & rewrite:");
    match provider.generate(&draft, &cancel, &mut |delta| print!("{delta}")) {
        Ok(_) => println!("\n({:.0?})", started.elapsed()),
        Err(e) => {
            eprintln!("\ndraft failed: {e}");
            return ExitCode::FAILURE;
        }
    }

    println!(
        "\nsent {} requests, key never among them",
        log.entries().len()
    );
    ExitCode::SUCCESS
}
