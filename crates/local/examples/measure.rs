//! Scripted latency and memory measurement of a local model, printed as one
//! tab-separated row (with a header row unless `--no-header`):
//!
//! ```text
//! cargo run --release -p oft-local --example measure -- --model qwen3.5-2b-base-q4_k_m --backend cpu --trials 200
//! ```
//!
//! The context is ~1,500 tokens of Pride and Prejudice, chapters I–III
//! (Project Gutenberg eBook #1342, public domain). Each trial appends the
//! next character of the text and times one completion of 8 tokens with the
//! cache warm, first with no alternatives and then with 2. Trials whose
//! caret sits inside a word (token healing) are also reported separately.
//!
//! The 1-minute load average is read at the start and the end. Above
//! `--max-load` (default 2.0) at the start the run is refused, unless
//! `--force`, which marks the row `under-load`. Use the CPU-only build for
//! `--backend cpu`: a GPU build can still move large CPU batches to the GPU.

mod common;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use oft_engine::complete::{CancelToken, Completer};
use oft_local::{LoadOptions, LocalModel, supports_gpu_offload};

use common::{NoStop, request};

const TEXT: &str = include_str!("data/pride-and-prejudice.txt");
const SUGGESTION_TOKENS: u32 = 8;

const COLUMNS: [&str; 30] = [
    "model",
    "desc",
    "backend",
    "gpu_build",
    "threads",
    "n_ctx",
    "kv",
    "flash_attn",
    "load_ms",
    "rss_mb",
    "peak_rss_mb",
    "vram_mib",
    "prompt_tokens",
    "prompt_tok_s",
    "gen_tok_s",
    "key_p50_ms",
    "key_p95_ms",
    "alt2_p50_ms",
    "alt2_p95_ms",
    "word_p50_ms",
    "word_p95_ms",
    "boundary_p50_ms",
    "boundary_p95_ms",
    "choice_p95_ms",
    "trials",
    "load1_start",
    "load1_end",
    "status",
    "cpu",
    "gpu",
];

struct Args {
    model: String,
    backend: Backend,
    trials: usize,
    context_chars: usize,
    max_load: f64,
    force: bool,
    header: bool,
    gpu_layers_set: bool,
    options: LoadOptions,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Cpu,
    Cuda,
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: measure --model <id|path> [--backend cpu|cuda] [--trials N] [--context-chars N]\n\
         \t[--max-load L] [--force] [--no-header] {}",
        common::LOAD_USAGE
    );
    ExitCode::from(2)
}

fn parse() -> Option<Args> {
    let mut args = Args {
        model: String::new(),
        backend: Backend::Cpu,
        trials: 200,
        context_chars: 6200,
        max_load: 2.0,
        force: false,
        header: true,
        gpu_layers_set: false,
        options: LoadOptions::default(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next();
        match a.as_str() {
            "--model" => args.model = value()?,
            "--backend" => {
                args.backend = match value()?.as_str() {
                    "cpu" => Backend::Cpu,
                    "cuda" | "gpu" => Backend::Cuda,
                    _ => return None,
                }
            }
            "--trials" => args.trials = value()?.parse().ok()?,
            "--context-chars" => args.context_chars = value()?.parse().ok()?,
            "--max-load" => args.max_load = value()?.parse().ok()?,
            "--force" => args.force = true,
            "--no-header" => args.header = false,
            flag => {
                args.gpu_layers_set |= flag == "--gpu-layers";
                if !common::load_option(flag, &mut value, &mut args.options)? {
                    return None;
                }
            }
        }
    }
    if !args.gpu_layers_set {
        args.options.gpu_layers = if args.backend == Backend::Cpu {
            0
        } else {
            u32::MAX
        };
    }
    (!args.model.is_empty()).then_some(args)
}

fn load1() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(f64::NAN)
}

fn cpu_name() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))?
                .split_once(':')
                .map(|(_, v)| v.trim().to_owned())
        })
        .unwrap_or_default()
}

fn gpu_name() -> String {
    std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().next().unwrap_or_default().trim().to_owned())
        .unwrap_or_default()
}

fn percentile(samples: &mut [Duration], q: f64) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    samples.sort_unstable();
    samples[((samples.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e3
}

struct Keystrokes {
    all: Vec<Duration>,
    in_word: Vec<Duration>,
    boundary: Vec<Duration>,
    choice: Vec<Duration>,
}

/// Each trial appends one character of `typed` and times a completion.
fn keystrokes(model: &LocalModel, split: usize, trials: usize, alternatives: u8) -> Keystrokes {
    let mut k = Keystrokes {
        all: Vec::new(),
        in_word: Vec::new(),
        boundary: Vec::new(),
        choice: Vec::new(),
    };
    let warm = request(0, &TEXT[..split], "", alternatives, SUGGESTION_TOKENS);
    model
        .complete(&warm, &mut NoStop, &CancelToken::new())
        .expect("warm-up completion");
    for (trial, (end, _)) in TEXT[split..]
        .char_indices()
        .skip(1)
        .take(trials)
        .enumerate()
    {
        let before = &TEXT[..split + end];
        let req = request(
            trial as u64 + 1,
            before,
            "",
            alternatives,
            SUGGESTION_TOKENS,
        );
        let started = Instant::now();
        model
            .complete(&req, &mut NoStop, &CancelToken::new())
            .expect("completion");
        let took = started.elapsed();
        let t = model.last_timings();
        k.choice.push(Duration::from_secs_f64(
            (t.generate_ms - t.generate_decode_ms).max(0.0) / 1e3,
        ));
        k.all.push(took);
        if before.ends_with(char::is_alphanumeric) {
            k.in_word.push(took);
        } else {
            k.boundary.push(took);
        }
    }
    k
}

fn main() -> ExitCode {
    let Some(args) = parse() else { return usage() };
    let load1_start = load1();
    let loaded = load1_start > args.max_load;
    if loaded && !args.force {
        eprintln!(
            "refusing to measure: 1-minute load average {load1_start:.2} exceeds {:.2} (--force to measure anyway; \
             the row is then marked under-load)",
            args.max_load
        );
        return ExitCode::from(3);
    }
    let gpu_build = supports_gpu_offload();
    if args.backend == Backend::Cuda && !gpu_build {
        eprintln!("--backend cuda needs a build with --features cuda");
        return ExitCode::from(2);
    }
    if args.backend == Backend::Cpu && gpu_build {
        eprintln!("note: this is a GPU build; for CPU numbers use a build without GPU features");
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
    let load_ms = started.elapsed().as_secs_f64() * 1e3;
    let info = model.info().clone();

    let want = args.context_chars.min(TEXT.len() / 2);
    let split = (0..=want)
        .rev()
        .find(|&i| TEXT.is_char_boundary(i))
        .unwrap_or(0);
    let cold = request(0, &TEXT[..split], "", 0, SUGGESTION_TOKENS);
    model
        .complete(&cold, &mut NoStop, &CancelToken::new())
        .expect("cold completion");
    let t = model.last_timings();
    let prompt_tok_s = t.decoded_tokens as f64 / t.prompt_ms * 1e3;
    let gen_tok_s = t.steps as f64 / t.generate_decode_ms * 1e3;
    eprintln!(
        "{} [{}]: load {load_ms:.0} ms; cold prompt {} tokens at {prompt_tok_s:.0} tok/s; generation {gen_tok_s:.1} tok/s",
        info.name, info.description, t.prompt_tokens
    );

    let mut single = keystrokes(&model, split, args.trials, 0);
    let mut alt2 = keystrokes(&model, split, args.trials, 2);
    let rss_mb = common::rss_mb() - rss_before;
    let vram = common::vram_mib();
    let load1_end = load1();
    let status = if loaded || load1_end > args.max_load {
        "under-load"
    } else {
        "ok"
    };

    let values: [String; COLUMNS.len()] = [
        args.model.clone(),
        info.description.clone(),
        if args.backend == Backend::Cpu {
            "cpu".into()
        } else {
            "cuda".into()
        },
        gpu_build.to_string(),
        info.threads.to_string(),
        info.n_ctx.to_string(),
        format!("{:?}", args.options.kv_cache).to_lowercase(),
        format!("{:?}", args.options.flash_attention).to_lowercase(),
        format!("{load_ms:.0}"),
        format!("{rss_mb:.0}"),
        format!("{:.0}", common::peak_rss_mb()),
        vram.map_or_else(|| "-".into(), |v| v.to_string()),
        t.prompt_tokens.to_string(),
        format!("{prompt_tok_s:.0}"),
        format!("{gen_tok_s:.1}"),
        format!("{:.1}", percentile(&mut single.all, 0.5)),
        format!("{:.1}", percentile(&mut single.all, 0.95)),
        format!("{:.1}", percentile(&mut alt2.all, 0.5)),
        format!("{:.1}", percentile(&mut alt2.all, 0.95)),
        format!("{:.1}", percentile(&mut single.in_word, 0.5)),
        format!("{:.1}", percentile(&mut single.in_word, 0.95)),
        format!("{:.1}", percentile(&mut single.boundary, 0.5)),
        format!("{:.1}", percentile(&mut single.boundary, 0.95)),
        format!("{:.2}", percentile(&mut single.choice, 0.95)),
        single.all.len().to_string(),
        format!("{load1_start:.2}"),
        format!("{load1_end:.2}"),
        status.into(),
        cpu_name(),
        if args.backend == Backend::Cuda {
            gpu_name()
        } else {
            "-".into()
        },
    ];
    if args.header {
        println!("{}", COLUMNS.join("\t"));
    }
    println!("{}", values.join("\t"));
    ExitCode::SUCCESS
}
