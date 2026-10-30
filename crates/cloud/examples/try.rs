//! Runs one autocomplete and one Flow Draft request against a real endpoint.
//!
//! ```text
//! OFT_PROVIDER=openai OFT_BASE_URL=http://localhost:8080/v1 OFT_MODEL=qwen cargo run -p oft-cloud --example try
//! OFT_PROVIDER=anthropic OFT_MODEL=claude-haiku-4-5 OFT_API_KEY=... cargo run -p oft-cloud --example try
//! OFT_PROVIDER=gemini OFT_MODEL=gemini-2.5-flash OFT_API_KEY=... cargo run -p oft-cloud --example try
//! ```
//!
//! `OFT_PROVIDER` is `openai` (any OpenAI-compatible server; needs
//! `OFT_BASE_URL`), `anthropic` or `gemini`. `OFT_API_KEY` is optional for
//! local servers. `OFT_BEFORE` replaces the sample text before the caret.

use std::process::ExitCode;
use std::time::Instant;

use oft_cloud::{SentLog, provider};
use oft_engine::complete::{
    CancelToken, ChatMessage, Completer, CompletionRequest, GenerateRequest, Generator, LengthMode,
    Role, Segments, StopCheck, StopParams,
};
use oft_engine::io::{AppInfo, Author, FieldId, Message};
use oft_engine::settings::{ProviderConfig, ProviderKind};

fn main() -> ExitCode {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let (Some(kind), Some(model)) = (env("OFT_PROVIDER"), env("OFT_MODEL")) else {
        eprintln!(
            "set OFT_PROVIDER (openai|anthropic|gemini) and OFT_MODEL; see the header of this example"
        );
        return ExitCode::from(2);
    };
    let kind = match kind.as_str() {
        "openai" => match env("OFT_BASE_URL") {
            Some(base_url) => ProviderKind::OpenAiCompatible { base_url },
            None => {
                eprintln!("OFT_PROVIDER=openai needs OFT_BASE_URL, e.g. https://api.openai.com/v1");
                return ExitCode::from(2);
            }
        },
        "anthropic" => ProviderKind::Anthropic,
        "gemini" => ProviderKind::Gemini,
        other => {
            eprintln!("unknown OFT_PROVIDER {other:?}");
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
    let provider = match provider(&config, env("OFT_API_KEY"), log.clone()) {
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

    let before = env("OFT_BEFORE")
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
    println!("\nFlow Draft:");
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
