//! Engine-spike measurements for techniques that are adopted only if they
//! pay off (docs/architecture.md, "Local inference"). Ignored by default:
//!
//! ```text
//! GHOSTCARET_TEST_MODEL=<id|path> cargo test --release -p ghostcaret-local spike -- --ignored --nocapture
//! ```

use std::time::Instant;

use super::*;
use crate::store::ModelStore;

/// Prompt-lookup drafting: the `k` tokens that followed the latest earlier
/// occurrence of the last `n` tokens of `history`.
fn ngram_draft(history: &[Token], n: usize, k: usize) -> &[Token] {
    if n == 0 || history.len() <= n {
        return &[];
    }
    let key = &history[history.len() - n..];
    (0..history.len() - n)
        .rev()
        .find(|&start| &history[start..start + n] == key)
        .map_or(&[], |start| {
            &history[start + n..(start + n + k).min(history.len())]
        })
}

#[test]
fn ngram_draft_continues_the_latest_match() {
    let h = [1, 2, 3, 9, 1, 2, 3, 7, 8, 1, 2, 3];
    assert_eq!(ngram_draft(&h, 3, 2), &[7, 8]);
    assert_eq!(ngram_draft(&h, 2, 5), &[7, 8, 1, 2, 3]);
    assert_eq!(ngram_draft(&[5, 6], 3, 2), &[] as &[Token]);
    assert_eq!(ngram_draft(&[1, 2, 4, 5], 1, 2), &[] as &[Token]);
}

fn load() -> LocalModel {
    let model = std::env::var("GHOSTCARET_TEST_MODEL").expect("set GHOSTCARET_TEST_MODEL");
    let path = ModelStore::default_dir()
        .map(ModelStore::new)
        .and_then(|s| s.resolve(&model))
        .unwrap_or_else(|| PathBuf::from(model));
    let gpu_layers = std::env::var("GHOSTCARET_TEST_GPU_LAYERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(u32::MAX);
    LocalModel::load(
        &path,
        LoadOptions {
            gpu_layers,
            ..LoadOptions::default()
        },
    )
    .expect("model loads")
}

const TEXT: &str = include_str!("../../examples/data/pride-and-prejudice.txt");
const CONTEXT: usize = 6200;

fn request(before: &str) -> CompletionRequest {
    CompletionRequest {
        id: 1,
        field: ghostcaret_engine::io::FieldId(1),
        revision: 1,
        segments: ghostcaret_engine::complete::Segments {
            before: before.into(),
            ..Default::default()
        },
        stop: ghostcaret_engine::complete::StopParams {
            mode: ghostcaret_engine::complete::LengthMode::Auto,
            ceiling: ghostcaret_engine::complete::LengthMode::Sentence,
            boldness: 0.5,
            paragraph_words: 60,
        },
        max_tokens: 8,
        alternatives: 0,
    }
}

struct NoStop;

impl StopCheck for NoStop {
    fn should_stop(&mut self, _: &str, _: Option<f32>) -> bool {
        false
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Cost of decoding a batch of `b` tokens on a stream at the end of the
/// context, in ms (median of 20).
fn batch_cost(e: &mut Engine, n_prompt: usize, b: usize) -> f64 {
    let seq = FIRST_STREAM;
    let mut samples = Vec::new();
    for _ in 0..20 {
        e.ctx.seq_rm(seq, -1, -1);
        e.ctx.seq_cp(COMPLETE_SEQ, seq);
        e.batch.clear();
        for i in 0..b {
            e.batch
                .push(e.tokens[n_prompt - 1 - i], n_prompt + i, seq, true);
        }
        let t = Instant::now();
        e.ctx.decode(&e.batch).expect("decode");
        samples.push(t.elapsed().as_secs_f64() * 1e3);
    }
    e.ctx.seq_rm(seq, -1, -1);
    median(samples)
}

/// N-gram (prompt-lookup) speculative decoding: how many generated tokens
/// the drafts would have confirmed, and what a batch with drafts costs.
#[test]
#[ignore = "needs GHOSTCARET_TEST_MODEL"]
fn spike_ngram_speculation() {
    const K: usize = 3;
    let model = load();
    let mut steps = 0usize;
    let mut tokens = 0usize;
    let mut drafted = 0usize;
    let mut batch_sizes = Vec::new();
    let boundaries: Vec<usize> = TEXT[CONTEXT..]
        .match_indices(' ')
        .map(|(i, _)| CONTEXT + i + 1)
        .step_by(5)
        .take(60)
        .collect();
    for &end in &boundaries {
        let req = request(&TEXT[..end]);
        let completion = model
            .complete(&req, &mut NoStop, &CancelToken::new())
            .expect("completion");
        let main = &completion.candidates[0].text;
        let mut guard = model.lock();
        let l = guard.as_mut().expect("loaded");
        l.engine.prompt(&l.model, &req);
        let head = l.engine.tokens.clone();
        let mut full = Vec::new();
        let text = format!("{}{main}", l.engine.text);
        l.model.tokenize(&text, true, false, &mut full);
        let Some(generated) = full.strip_prefix(head.as_slice()) else {
            continue;
        };
        let mut history = head.clone();
        let mut i = 0;
        while i < generated.len() {
            history.push(generated[i]);
            let draft = {
                let d = ngram_draft(&history, 3, K);
                if d.is_empty() {
                    ngram_draft(&history, 2, K)
                } else {
                    d
                }
            };
            let accepted = draft
                .iter()
                .zip(&generated[i + 1..])
                .take_while(|(a, b)| a == b)
                .count();
            drafted += draft.len();
            batch_sizes.push(1 + draft.len());
            history.extend_from_slice(&generated[i + 1..i + 1 + accepted]);
            i += 1 + accepted;
            steps += 1;
            tokens += 1 + accepted;
        }
    }
    let mut guard = model.lock();
    let l = guard.as_mut().expect("loaded");
    l.engine.prompt(&l.model, &request(&TEXT[..CONTEXT]));
    let n_prompt = l.engine.tokens.len();
    let cost: Vec<f64> = (1..=K + 1)
        .map(|b| batch_cost(&mut l.engine, n_prompt, b))
        .collect();
    let spec_ms: f64 = batch_sizes.iter().map(|&b| cost[b - 1]).sum();
    let plain_ms = tokens as f64 * cost[0];
    println!(
        "{}: {} suggestions, {tokens} tokens in {steps} steps ({:.2} tokens/step, {drafted} drafted, {:.0}% accepted)",
        model.info().name,
        boundaries.len(),
        tokens as f64 / steps as f64,
        (tokens - steps) as f64 * 100.0 / drafted.max(1) as f64
    );
    println!(
        "batch cost ms for 1..={} tokens: {:?}; estimated generation time with drafts {:.0}% of plain",
        K + 1,
        cost.iter().map(|c| format!("{c:.2}")).collect::<Vec<_>>(),
        spec_ms * 100.0 / plain_ms
    );
    assert!(cost.iter().all(|&c| c > 0.0));
}

/// Size and cost of one state checkpoint for models with recurrent state.
#[test]
#[ignore = "needs GHOSTCARET_TEST_MODEL"]
fn spike_checkpoint_cost() {
    let model = load();
    model
        .complete(&request(&TEXT[..CONTEXT]), &mut NoStop, &CancelToken::new())
        .expect("completion");
    let mut guard = model.lock();
    let l = guard.as_mut().expect("loaded");
    let mut data = Vec::new();
    let mut save = Vec::new();
    let mut restore = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        assert!(
            l.engine
                .ctx
                .state_seq_get(COMPLETE_SEQ, llama::STATE_PARTIAL_ONLY, &mut data)
        );
        save.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        assert!(
            l.engine
                .ctx
                .state_seq_set(COMPLETE_SEQ, llama::STATE_PARTIAL_ONLY, &data)
        );
        restore.push(t.elapsed().as_secs_f64() * 1e3);
    }
    println!(
        "{} ({:?} memory): partial state {:.1} MiB, save {:.2} ms, restore {:.2} ms (medians)",
        model.info().name,
        model.info().memory,
        data.len() as f64 / (1 << 20) as f64,
        median(save),
        median(restore)
    );
}
