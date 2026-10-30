//! One completion from the command line, with timings:
//!
//! ```text
//! cargo run --release -p ghostcaret-local --example complete -- --model qwen3.5-2b-base-q4_k_m "I would like to sch"
//! ```
//!
//! `--model` takes a manifest id (looked up in the model store) or a GGUF
//! path; `--download` first fetches a manifest model that is not installed
//! yet, resuming a partial file. `--chat "message"` runs Reply & rewrite
//! generation instead. Latency measurements are in the `measure` example.

mod common;

use std::process::ExitCode;
use std::time::Instant;

use ghostcaret_engine::complete::{
    CancelToken, ChatMessage, Completer, GenerateRequest, Generator, Role,
};
use ghostcaret_local::store::{Manifest, ModelStore};
use ghostcaret_local::{LoadOptions, LocalModel};

use common::{NoStop, request};

struct Args {
    model: String,
    text: String,
    after: String,
    chat: Option<String>,
    alternatives: u8,
    max_tokens: u32,
    download: bool,
    options: LoadOptions,
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: complete --model <id|path> [--download] [--alternatives N] [--max-tokens N] [--after TEXT]\n\
         \t{}\n\t[--chat MESSAGE] [TEXT]",
        common::LOAD_USAGE
    );
    ExitCode::from(2)
}

fn parse() -> Option<Args> {
    let mut args = Args {
        model: String::new(),
        text: String::new(),
        after: String::new(),
        chat: None,
        alternatives: 0,
        max_tokens: 16,
        download: false,
        options: LoadOptions::default(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next();
        match a.as_str() {
            "--model" => args.model = value()?,
            "--after" => args.after = value()?,
            "--chat" => args.chat = Some(value()?),
            "--alternatives" => args.alternatives = value()?.parse().ok()?,
            "--max-tokens" => args.max_tokens = value()?.parse().ok()?,
            "--download" => args.download = true,
            flag if flag.starts_with("--") => {
                if !common::load_option(flag, &mut value, &mut args.options)? {
                    return None;
                }
            }
            _ => args.text = a,
        }
    }
    (!args.model.is_empty()).then_some(args)
}

fn main() -> ExitCode {
    let Some(args) = parse() else { return usage() };
    if args.download
        && let Some(entry) = Manifest::builtin().get(&args.model)
        && let Some(store) = ModelStore::default_dir().map(ModelStore::new)
        && !store.is_installed(entry)
    {
        let started = Instant::now();
        let mut last = u64::MAX;
        let result = store.download(
            entry,
            &mut |p| {
                let percent = p.downloaded * 100 / p.total.max(1);
                if percent != last {
                    last = percent;
                    eprint!(
                        "\r{}: {percent}% of {:.2} GB",
                        entry.name,
                        p.total as f64 / 1e9
                    );
                }
            },
            &CancelToken::new(),
        );
        eprintln!();
        match result {
            Ok(path) => println!(
                "downloaded and verified {} in {:.0} s",
                path.display(),
                started.elapsed().as_secs_f64()
            ),
            Err(e) => {
                eprintln!("download failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let rss_before = common::rss_mb();
    let started = Instant::now();
    let model = match LocalModel::load(&common::model_path(&args.model), args.options.clone()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let info = model.info();
    println!(
        "model: {} [{}], {:.2} GB, {:?} memory, n_ctx {}, {} threads, chat template: {}",
        info.name,
        info.description,
        info.size_bytes as f64 / 1e9,
        info.memory,
        info.n_ctx,
        info.threads,
        info.chat_template
    );
    println!(
        "load: {:.0} ms, RSS +{:.0} MB",
        started.elapsed().as_secs_f64() * 1e3,
        common::rss_mb() - rss_before
    );
    match &args.chat {
        Some(message) => chat(&model, message, args.max_tokens.max(64)),
        None => complete_once(&model, &args),
    }
    ExitCode::SUCCESS
}

fn complete_once(model: &LocalModel, args: &Args) {
    let text = if args.text.is_empty() {
        "It is a truth universally acknowledged, that a single man"
    } else {
        &args.text
    };
    for (label, run) in [("cold", 0), ("warm", 1)] {
        let req = request(
            run + 1,
            text,
            &args.after,
            args.alternatives,
            args.max_tokens,
        );
        let started = Instant::now();
        let result = model.complete(&req, &mut NoStop, &CancelToken::new());
        let total = started.elapsed();
        let t = model.last_timings();
        println!(
            "{label}: {:.1} ms total; prompt {} tokens, {} decoded in {:.1} ms; {} generation steps in {:.1} ms \
             ({:.1} ms in llama.cpp)",
            total.as_secs_f64() * 1e3,
            t.prompt_tokens,
            t.decoded_tokens,
            t.prompt_ms,
            t.steps,
            t.generate_ms,
            t.generate_decode_ms
        );
        match result {
            Ok(c) if run == 1 => {
                for (i, cand) in c.candidates.iter().enumerate() {
                    let probs: Vec<String> =
                        cand.token_probs.iter().map(|p| format!("{p:.2}")).collect();
                    println!(
                        "  [{i}] {text:?} + {:?}\n      p = {}",
                        cand.text,
                        probs.join(" ")
                    );
                }
            }
            Ok(_) => {}
            Err(e) => println!("  error: {e}"),
        }
    }
}

fn chat(model: &LocalModel, message: &str, max_tokens: u32) {
    let req = GenerateRequest {
        id: 1,
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: "Write a short, friendly reply.".into(),
            },
            ChatMessage {
                role: Role::User,
                content: message.into(),
            },
        ],
        max_tokens,
        temperature: 0.7,
    };
    let started = Instant::now();
    let mut first = None;
    let result = model.generate(&req, &CancelToken::new(), &mut |d| {
        first.get_or_insert_with(|| started.elapsed());
        print!("{d}");
    });
    println!();
    let t = model.last_timings();
    println!(
        "first text after {:.0} ms; prompt {} tokens in {:.1} ms; {} tokens in {:.1} ms ({:.1} tok/s)",
        first.unwrap_or_default().as_secs_f64() * 1e3,
        t.prompt_tokens,
        t.prompt_ms,
        t.steps,
        t.generate_ms,
        t.steps as f64 / t.generate_ms * 1e3
    );
    if let Err(e) = result {
        println!("error: {e}");
    }
}
