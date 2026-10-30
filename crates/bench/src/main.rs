//! Replay benchmark: keystroke savings, wrong-suggestion rate and latency.
//!
//! ```text
//! ghostcaret-bench run [--models ID[:GPU_LAYERS],…] [--corpora ID,…] [--samples N]
//!               [--modes word,phrase,sentence,auto] [--ceilings phrase,sentence,paragraph]
//!               [--boldness 0,0.25,0.5,0.75,1] [--out DIR] [--data-dir DIR]
//!               [--seed N] [--max-load F] [--ctx N] [--threads N]
//! ghostcaret-bench corpora [--data-dir DIR] [--show]
//! ghostcaret-bench report [--out DIR]
//! ```
//!
//! `run` downloads and verifies the corpora, replays every sample under every
//! configuration for each model in turn, and writes `results.json` and
//! `report.md` to `--out` after each model. Runs of other models already in
//! `results.json` are kept when the engine and method are unchanged, so
//! models can be run one at a time. See `crates/bench/README.md`.

mod corpus;
mod mail;
mod metrics;
mod pause;
mod replay;
mod report;
mod results;
mod shared;
mod system;
mod user;
mod wiki;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use ghostcaret_engine::complete::{Completer, LengthMode, StopParams};
use ghostcaret_engine::io::AppInfo;
use ghostcaret_engine::settings::Settings;
use ghostcaret_local::store::{Manifest, ModelStore, sha256_file};
use ghostcaret_local::{LoadOptions, LocalModel};

use crate::corpus::{CORPORA, Corpus};
use crate::metrics::Tally;
use crate::pause::PauseModel;
use crate::results::{Config, CorpusInfo, EngineInfo, Method, ModelOut, Results, Row, Run, SCHEMA};
use crate::shared::Shared;

struct Args {
    command: String,
    models: Vec<(String, u32)>,
    corpora: Vec<String>,
    samples: Option<usize>,
    modes: Vec<LengthMode>,
    ceilings: Vec<LengthMode>,
    boldness: Vec<f32>,
    out: PathBuf,
    data_dir: PathBuf,
    seed: u64,
    max_load: f64,
    n_ctx: u32,
    threads: u32,
    show: bool,
}

const USAGE: &str = "usage: ghostcaret-bench run|corpora|report [options]; see the source header or crates/bench/README.md";

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().unwrap_or_else(|| "run".into());
    let data_dir = ModelStore::default_dir()
        .and_then(|d| d.parent().map(|p| p.join("bench")))
        .unwrap_or_else(|| PathBuf::from("bench-data"));
    let mut a = Args {
        command,
        models: Manifest::builtin()
            .entries()
            .iter()
            .map(|e| (e.id.to_owned(), u32::MAX))
            .collect(),
        corpora: CORPORA.iter().map(|c| c.id.to_owned()).collect(),
        samples: None,
        modes: vec![
            LengthMode::Word,
            LengthMode::Phrase,
            LengthMode::Sentence,
            LengthMode::Auto,
        ],
        ceilings: vec![
            LengthMode::Phrase,
            LengthMode::Sentence,
            LengthMode::Paragraph,
        ],
        boldness: vec![0.0, 0.25, 0.5, 0.75, 1.0],
        out: PathBuf::from("target/bench-report"),
        data_dir,
        seed: 1,
        max_load: 2.0,
        n_ctx: LoadOptions::default().n_ctx,
        threads: 0,
        show: false,
    };
    let list = |v: String| {
        v.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--models" => {
                a.models = list(value()?)
                    .into_iter()
                    .map(|m| match m.split_once(':') {
                        Some((id, layers)) => layers
                            .parse()
                            .map(|l| (id.to_owned(), l))
                            .map_err(|_| format!("bad GPU layer count in {m}")),
                        None => Ok((m, u32::MAX)),
                    })
                    .collect::<Result<_, _>>()?
            }
            "--corpora" => a.corpora = list(value()?),
            "--samples" => {
                a.samples = Some(value()?.parse().map_err(|_| "--samples needs a number")?)
            }
            "--modes" => {
                a.modes = list(value()?)
                    .iter()
                    .map(|m| results::parse_mode(m).ok_or(format!("unknown mode {m}")))
                    .collect::<Result<_, _>>()?
            }
            "--ceilings" => {
                a.ceilings = list(value()?)
                    .iter()
                    .map(|m| match results::parse_mode(m) {
                        Some(LengthMode::Auto) | None => Err(format!("bad ceiling {m}")),
                        Some(c) => Ok(c),
                    })
                    .collect::<Result<_, _>>()?
            }
            "--boldness" => {
                a.boldness = list(value()?)
                    .iter()
                    .map(|b| {
                        b.parse::<f32>()
                            .ok()
                            .filter(|b| (0.0..=1.0).contains(b))
                            .ok_or(format!("bad boldness {b}"))
                    })
                    .collect::<Result<_, _>>()?
            }
            "--out" => a.out = PathBuf::from(value()?),
            "--data-dir" => a.data_dir = PathBuf::from(value()?),
            "--seed" => a.seed = value()?.parse().map_err(|_| "--seed needs a number")?,
            "--max-load" => {
                a.max_load = value()?.parse().map_err(|_| "--max-load needs a number")?
            }
            "--ctx" => a.n_ctx = value()?.parse().map_err(|_| "--ctx needs a number")?,
            "--threads" => a.threads = value()?.parse().map_err(|_| "--threads needs a number")?,
            "--show" => a.show = true,
            "-h" | "--help" => return Err(USAGE.into()),
            _ => return Err(format!("unknown option {flag}; {USAGE}")),
        }
    }
    for c in &a.corpora {
        if corpus::spec(c).is_none() {
            return Err(format!(
                "unknown corpus {c}; known: {}",
                CORPORA.iter().map(|c| c.id).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let result = parse_args().and_then(|a| match a.command.as_str() {
        "run" => run(&a),
        "corpora" => corpora(&a).map(|_| ()),
        "report" => report(&a),
        other => Err(format!("unknown command {other}; {USAGE}")),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ghostcaret-bench: {e}");
            ExitCode::FAILURE
        }
    }
}

fn load_corpora(a: &Args) -> Result<Vec<Corpus>, String> {
    a.corpora
        .iter()
        .map(|id| {
            corpus::load(
                corpus::spec(id).expect("checked in parse_args"),
                &a.data_dir,
            )
        })
        .collect()
}

fn corpora(a: &Args) -> Result<Vec<Corpus>, String> {
    let corpora = load_corpora(a)?;
    for c in &corpora {
        let info = CorpusInfo::of(c);
        println!(
            "{}: {} samples, {} words, {} characters",
            info.id, info.samples, info.words, info.chars
        );
        if a.show {
            for s in &c.samples {
                println!(
                    "--- {} [{}] conversation: {} messages",
                    s.id,
                    s.title,
                    s.conversation.len()
                );
                if let Some(last) = s.conversation.last() {
                    println!("    last: {:?}", last.text);
                }
                if !s.before.is_empty() {
                    println!("    before: {:?}", s.before);
                }
                println!("    text: {:?}", s.text);
            }
        }
    }
    Ok(corpora)
}

fn report(a: &Args) -> Result<(), String> {
    let path = a.out.join("results.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let results: Results =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    write_report(&a.out, &results)
}

fn write_report(out: &Path, results: &Results) -> Result<(), String> {
    fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let json = serde_json::to_string_pretty(results).expect("results serialize");
    fs::write(out.join("results.json"), json).map_err(|e| format!("{}: {e}", out.display()))?;
    fs::write(out.join("report.md"), report::render(results))
        .map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(())
}

fn backend() -> &'static str {
    if cfg!(feature = "cuda") {
        "cuda"
    } else if cfg!(feature = "vulkan") {
        "vulkan"
    } else if cfg!(feature = "metal") {
        "metal"
    } else {
        "cpu"
    }
}

fn run(a: &Args) -> Result<(), String> {
    let corpora = load_corpora(a)?;
    let configs = results::sweep(&a.modes, &a.ceilings, &a.boldness);
    let pauses = PauseModel::new(a.seed, Settings::default().pause_ms);
    let mut results = Results {
        schema: SCHEMA,
        tool: concat!("ghostcaret-bench ", env!("CARGO_PKG_VERSION")).into(),
        engine: EngineInfo::current(),
        method: Method {
            pause: pauses,
            within_word_pause_rate: pauses.within_word_rate(),
        },
        corpora: corpora.iter().map(CorpusInfo::of).collect(),
        runs: Vec::new(),
    };
    if let Some(old) = fs::read_to_string(a.out.join("results.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Results>(&t).ok())
    {
        if old.schema == results.schema
            && old.engine == results.engine
            && old.method == results.method
        {
            let ours: Vec<&str> = a.models.iter().map(|(id, _)| id.as_str()).collect();
            results.runs = old
                .runs
                .into_iter()
                .filter(|r| !ours.contains(&r.model.id.as_str()))
                .collect();
            for c in old.corpora {
                if !results.corpora.iter().any(|x| x.id == c.id) {
                    results.corpora.push(c);
                }
            }
        } else {
            eprintln!(
                "existing results.json was made with another engine, method or schema; starting over"
            );
        }
    }
    for (id, layers) in &a.models {
        let run = run_model(a, id, *layers, &corpora, &configs, &pauses)?;
        results.runs.retain(|r| r.model.id != run.model.id);
        results.runs.push(run);
        write_report(&a.out, &results)?;
        eprintln!("wrote {}", a.out.join("report.md").display());
    }
    Ok(())
}

fn run_model(
    a: &Args,
    id: &str,
    gpu_layers: u32,
    corpora: &[Corpus],
    configs: &[Config],
    pauses: &PauseModel,
) -> Result<Run, String> {
    let entry = Manifest::builtin().get(id);
    let path = ModelStore::default_dir()
        .map(ModelStore::new)
        .and_then(|s| s.resolve(id))
        .unwrap_or_else(|| PathBuf::from(id));
    if !path.is_file() {
        return Err(format!("{id}: not installed and not a GGUF file"));
    }
    eprintln!("{id}: hashing {}", path.display());
    let sha256 = sha256_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let options = LoadOptions {
        n_ctx: a.n_ctx,
        threads: a.threads,
        gpu_layers,
        ..LoadOptions::default()
    };
    let model = LocalModel::load(&path, options).map_err(|e| format!("{id}: {e}"))?;
    let info = model.info().clone();
    let caps = model.caps();
    let started = Instant::now();
    let started_at = system::utc_now();
    let load1_start = system::load1().unwrap_or(0.0);
    let mut load1_max = load1_start;
    let (mut generations, mut tokens) = (0, 0);
    let mut rows = Vec::new();
    let mut samples_used = BTreeMap::new();
    for corpus in corpora {
        let spec = corpus.spec;
        let app = AppInfo {
            id: spec.app_id.into(),
            name: spec.app_name.into(),
            title: String::new(),
        };
        let settings: Vec<Settings> = configs.iter().map(|c| c.settings(spec.kind)).collect();
        let stops: Vec<(StopParams, u32)> = settings
            .iter()
            .map(|s| replay::request_stop(s, &app, caps))
            .collect();
        let mut distinct: Vec<StopParams> = Vec::new();
        for (s, _) in &stops {
            if !distinct.contains(s) {
                distinct.push(*s);
            }
        }
        let shared = Shared::new(&model as &dyn Completer, distinct);
        let mut tallies = vec![Tally::default(); configs.len()];
        let n = a
            .samples
            .map_or(corpus.samples.len(), |n| n.min(corpus.samples.len()));
        samples_used.insert(spec.id.to_owned(), n);
        for (k, sample) in corpus.samples[..n].iter().enumerate() {
            for (t, s) in tallies
                .iter_mut()
                .zip(shared.replay_all(sample, &app, &settings, pauses))
            {
                t.add(&s);
            }
            let load = system::load1().unwrap_or(0.0);
            load1_max = load1_max.max(load);
            eprintln!(
                "{id} {} {}/{n}: {} generations, {:.0} s, load {load:.1}",
                spec.id,
                k + 1,
                generations + shared.generations(),
                started.elapsed().as_secs_f64(),
            );
        }
        generations += shared.generations();
        tokens += shared.tokens();
        for ((config, tally), (stop, max_tokens)) in configs.iter().zip(tallies).zip(stops) {
            rows.push(Row {
                corpus: spec.id.into(),
                config: *config,
                stop,
                max_tokens,
                metrics: tally.metrics(),
                tally,
            });
        }
    }
    Ok(Run {
        model: ModelOut {
            id: id.into(),
            name: entry.map_or_else(|| info.name.clone(), |e| e.name.into()),
            file: path
                .file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().into_owned()),
            size_bytes: fs::metadata(&path).map_or(0, |m| m.len()),
            sha256_verified: entry.is_some_and(|e| e.sha256 == sha256),
            sha256,
            license: entry.map_or("unknown", |e| e.license).into(),
            tier: entry.map_or_else(
                || "uncurated".into(),
                |e| format!("{:?}", e.tier).to_lowercase(),
            ),
            description: info.description.clone(),
            architecture: info.architecture.clone(),
            parameters: info.parameters,
        },
        backend: backend().into(),
        gpu_layers,
        n_ctx: info.n_ctx,
        threads: info.threads,
        machine: system::Machine::detect(),
        started: started_at,
        wall_s: started.elapsed().as_secs_f64(),
        load1_start,
        load1_max,
        max_load: a.max_load,
        latency_valid: load1_max <= a.max_load,
        samples: samples_used,
        generations,
        tokens,
        rows,
    })
}
