//! The local model: loading, autocomplete ([`Completer`]) and chat generation
//! ([`Generator`]).
//!
//! Sequences in the context: [`COMPLETE_SEQ`] holds the autocomplete prompt
//! between requests, the stream sequences hold the main candidate and the
//! alternatives while they are generated (copies of the prompt sequence,
//! decoded together in one batch per step), and [`DRAFT_SEQ`] holds the last
//! Reply & rewrite conversation.

use std::ffi::{CStr, CString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use oft_engine::complete::{
    CancelToken, Candidate, Caps, ChatMessage, CompleteError, Completer, Completion,
    CompletionRequest, GenerateRequest, Generator, Role, StopCheck,
};

use crate::heal::{self, PieceTable, Step};
use crate::kv::{Lane, Memory, Synced};
use crate::llama::{
    self, Batch, Context, ContextParams, DecodeError, KvType, Model, ModelParams, Seq, Token,
};
use crate::prompt::{self, Layout};
use crate::sample::{self, Norm, Over, Rng};

const COMPLETE_SEQ: Seq = 0;
/// The main candidate, two alternatives, and one extra first token tried
/// while healing a partial word.
const MAX_STREAMS: usize = 4;
const FIRST_STREAM: Seq = 1;
const DRAFT_SEQ: Seq = FIRST_STREAM + MAX_STREAMS as Seq;
const N_BATCH: u32 = 512;
/// Prefill leaves the prompt's last token undecoded: the prompt ends where
/// the healed tail begins, so only that token can still merge with text typed
/// next, and the completion then decodes it and gets fresh logits.
const PREFILL_HOLDBACK: usize = 1;
/// Over-long prompts lose tokens from the front in steps this large, so the
/// start of the prompt, and the cache behind it, stays put while typing.
const TRIM_STEP: usize = 256;
/// Room kept per stream for healing tokens on top of `max_tokens`.
const HEAL_ROOM: usize = 8;

/// When to use flash attention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlashAttention {
    /// Wherever the backend supports it.
    #[default]
    Auto,
    On,
    Off,
}

/// Element type of the KV cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KvCacheType {
    #[default]
    F16,
    Q8_0,
    Q4_0,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadOptions {
    /// Context length in tokens, shared by all sequences. Capped at the
    /// model's training context.
    pub n_ctx: u32,
    /// CPU threads; 0 picks a count suited to this machine.
    pub threads: u32,
    /// Layers placed on the GPU; `u32::MAX` places all. Ignored without a
    /// GPU backend.
    pub gpu_layers: u32,
    /// Memory-map the weights, so loading is fast and the OS can page them.
    pub mmap: bool,
    pub flash_attention: FlashAttention,
    pub kv_cache: KvCacheType,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions {
            n_ctx: 4096,
            threads: 0,
            gpu_layers: u32::MAX,
            mmap: true,
            flash_attention: FlashAttention::Auto,
            kv_cache: KvCacheType::F16,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("model file not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("cannot load the model: {0}")]
    Invalid(String),
    #[error("not enough memory: {0}")]
    OutOfMemory(String),
}

/// How the model's memory is reused between requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// Plain attention: cut back at the first changed token.
    Attention,
    /// Sliding-window attention, kept at full size so it cuts back like plain
    /// attention.
    SlidingWindow,
    /// Recurrent or hybrid layers: cut back by restoring a state checkpoint.
    Recurrent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub name: String,
    pub architecture: String,
    pub parameters: u64,
    pub size_bytes: u64,
    pub n_ctx: u32,
    pub threads: u32,
    pub memory: MemoryKind,
    pub chat_template: bool,
    /// llama.cpp's summary: architecture, size and weight type.
    pub description: String,
}

/// Stage timings of the last request.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timings {
    /// Tokens in the prompt.
    pub prompt_tokens: usize,
    /// Prompt tokens that had to be decoded (the rest came from the cache).
    pub decoded_tokens: usize,
    pub prompt_ms: f64,
    /// Decode steps of generation (each step decodes every live stream).
    pub steps: usize,
    pub generate_ms: f64,
    /// Part of `generate_ms` spent in llama.cpp; the rest is token choice.
    pub generate_decode_ms: f64,
}

/// A GGUF model loaded through llama.cpp. One request runs at a time; a
/// prefill in progress yields to any request waiting for the model.
pub struct LocalModel {
    state: Mutex<Option<Loaded>>,
    waiting: AtomicUsize,
    info: ModelInfo,
}

impl fmt::Debug for LocalModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalModel")
            .field("info", &self.info)
            .field("loaded", &self.is_loaded())
            .finish()
    }
}

/// Field order is drop order: the engine owns the context, which must go
/// before the model.
struct Loaded {
    engine: Engine,
    pieces: PieceTable,
    model: Model,
}

struct Engine {
    ctx: Context,
    params: ContextParams,
    batch: Batch,
    complete: Lane,
    draft: Lane,
    text: String,
    tokens: Vec<Token>,
    /// The stable part of the last prompt and its token count.
    stable: (String, usize),
    allowed: Vec<Token>,
    picks: Vec<(Token, f32)>,
    streams: Vec<Stream>,
    timings: Timings,
}

impl LocalModel {
    pub fn load(path: &Path, options: LoadOptions) -> Result<LocalModel, LoadError> {
        LocalModel::load_with_progress(path, options, &mut |_| {})
    }

    /// Loads the model, reporting progress from 0.0 to 1.0 on this thread.
    pub fn load_with_progress(
        path: &Path,
        options: LoadOptions,
        progress: &mut dyn FnMut(f32),
    ) -> Result<LocalModel, LoadError> {
        check_gguf(path)?;
        let params = ModelParams {
            gpu_layers: i32::try_from(options.gpu_layers).unwrap_or(-1),
            mmap: options.mmap,
        };
        let model = Model::load(path, &params, progress)
            .ok_or_else(|| LoadError::Invalid(format!("llama.cpp rejected {}", path.display())))?;
        if model.has_encoder() {
            return Err(LoadError::Invalid(
                "encoder-decoder models are not supported".into(),
            ));
        }
        let train = model.n_ctx_train();
        let n_ctx = if train > 0 {
            options.n_ctx.min(train)
        } else {
            options.n_ctx
        }
        .max(512);
        let threads = if options.threads == 0 {
            default_threads()
        } else {
            options.threads
        };
        let params = ContextParams {
            n_ctx,
            n_batch: N_BATCH,
            n_seq_max: DRAFT_SEQ as u32 + 1,
            threads: threads as i32,
            threads_batch: threads as i32,
            flash_attn: match options.flash_attention {
                FlashAttention::Auto => None,
                FlashAttention::On => Some(true),
                FlashAttention::Off => Some(false),
            },
            kv_type: match options.kv_cache {
                KvCacheType::F16 => KvType::F16,
                KvCacheType::Q8_0 => KvType::Q8_0,
                KvCacheType::Q4_0 => KvType::Q4_0,
            },
        };
        let ctx = Context::new(&model, &params).ok_or_else(|| {
            LoadError::OutOfMemory(format!("cannot allocate a context of {n_ctx} tokens"))
        })?;
        let pieces = PieceTable::new(
            model.n_vocab(),
            |t, out| model.piece(t, false, out),
            |t| !model.is_control(t) && !model.is_eog(t),
        );
        let recurrent = model.has_recurrent_state();
        let info = ModelInfo {
            name: model
                .meta("general.name")
                .unwrap_or_else(|| file_stem(path)),
            architecture: model.meta("general.architecture").unwrap_or_default(),
            parameters: model.n_params(),
            size_bytes: model.size_bytes(),
            n_ctx: ctx.n_ctx(),
            threads,
            memory: if recurrent {
                MemoryKind::Recurrent
            } else if model.n_swa() > 0 {
                MemoryKind::SlidingWindow
            } else {
                MemoryKind::Attention
            },
            chat_template: model.chat_template().is_some(),
            description: model.description(),
        };
        let engine = Engine {
            ctx,
            params,
            batch: Batch::new(N_BATCH as usize),
            complete: Lane::new(COMPLETE_SEQ, recurrent, N_BATCH as usize),
            draft: Lane::new(DRAFT_SEQ, recurrent, N_BATCH as usize),
            text: String::new(),
            tokens: Vec::new(),
            stable: (String::new(), 0),
            allowed: Vec::new(),
            picks: Vec::new(),
            streams: Vec::new(),
            timings: Timings::default(),
        };
        log::info!(
            "loaded {} ({}, {:.2} GB, {:?} memory, n_ctx {}, {} threads)",
            info.name,
            info.architecture,
            info.size_bytes as f64 / 1e9,
            info.memory,
            info.n_ctx,
            threads
        );
        Ok(LocalModel {
            state: Mutex::new(Some(Loaded {
                engine,
                pieces,
                model,
            })),
            waiting: AtomicUsize::new(0),
            info,
        })
    }

    pub fn info(&self) -> &ModelInfo {
        &self.info
    }

    /// Frees the weights and the context. Later requests fail with
    /// [`CompleteError::Unavailable`].
    pub fn unload(&self) {
        *self.lock() = None;
    }

    pub fn is_loaded(&self) -> bool {
        self.lock().is_some()
    }

    /// Timings of the last request.
    pub fn last_timings(&self) -> Timings {
        self.lock()
            .as_ref()
            .map(|l| l.engine.timings)
            .unwrap_or_default()
    }

    fn lock(&self) -> MutexGuard<'_, Option<Loaded>> {
        self.waiting.fetch_add(1, Ordering::AcqRel);
        let guard = self.state.lock();
        self.waiting.fetch_sub(1, Ordering::AcqRel);
        // A panic inside llama.cpp glue leaves the state unknown: drop it.
        guard.unwrap_or_else(|poisoned| {
            let mut guard = poisoned.into_inner();
            *guard = None;
            self.state.clear_poison();
            guard
        })
    }

    /// Runs `f` on the loaded model; drops the model when `f` reports it
    /// unusable.
    fn run<R>(
        &self,
        f: impl FnOnce(&mut Loaded) -> Result<R, CompleteError>,
    ) -> Result<R, CompleteError> {
        let mut guard = self.lock();
        let loaded = guard
            .as_mut()
            .ok_or_else(|| CompleteError::Unavailable("the local model is not loaded".into()))?;
        let result = f(loaded);
        if let Err(CompleteError::Unavailable(reason)) = &result {
            log::warn!("unloading the local model: {reason}");
            *guard = None;
        }
        result
    }
}

fn check_gguf(path: &Path) -> Result<(), LoadError> {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let mut file = std::fs::File::open(path).map_err(|_| LoadError::NotFound(path.to_owned()))?;
    file.read_exact(&mut magic)
        .map_err(|e| LoadError::Invalid(e.to_string()))?;
    if &magic != b"GGUF" {
        return Err(LoadError::Invalid(format!(
            "{} is not a GGUF file",
            path.display()
        )));
    }
    Ok(())
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Generation is bound by memory bandwidth, which a few cores saturate;
/// more threads add synchronization cost, and on hybrid processors the slow
/// cores hold back the fast ones. Use the physical performance cores.
fn default_threads() -> u32 {
    let logical = std::thread::available_parallelism().map_or(4, |n| n.get() as u32);
    let performance = performance_cpus().unwrap_or(logical);
    // Two hardware threads per core where SMT is on.
    (performance / 2).clamp(1, 8).max(performance.min(4))
}

/// Logical CPUs of the performance cluster on a hybrid processor.
#[cfg(target_os = "linux")]
fn performance_cpus() -> Option<u32> {
    let list = std::fs::read_to_string("/sys/devices/cpu_core/cpus").ok()?;
    let mut n = 0;
    for range in list.trim().split(',') {
        let (a, b) = range.split_once('-').unwrap_or((range, range));
        n += b.parse::<u32>().ok()? - a.parse::<u32>().ok()? + 1;
    }
    Some(n)
}

#[cfg(not(target_os = "linux"))]
fn performance_cpus() -> Option<u32> {
    None
}

/// The model's memory as the lanes see it.
struct Mem<'a> {
    ctx: &'a mut Context,
    batch: &'a mut Batch,
}

impl Memory for Mem<'_> {
    fn truncate(&mut self, seq: Seq, pos: usize) -> bool {
        self.ctx.seq_rm(seq, pos as i32, -1)
    }

    fn clear_seq(&mut self, seq: Seq) {
        self.ctx.seq_rm(seq, -1, -1);
    }

    fn save(&mut self, seq: Seq, out: &mut Vec<u8>) -> bool {
        self.ctx.state_seq_get(seq, llama::STATE_PARTIAL_ONLY, out)
    }

    fn restore(&mut self, seq: Seq, data: &[u8]) -> bool {
        self.ctx.state_seq_set(seq, llama::STATE_PARTIAL_ONLY, data)
    }

    fn decode(
        &mut self,
        seq: Seq,
        start: usize,
        tokens: &[Token],
        logits: bool,
        logits_out: &mut Vec<f32>,
    ) -> Result<(), DecodeError> {
        self.batch.clear();
        for (i, &t) in tokens.iter().enumerate() {
            self.batch
                .push(t, start + i, seq, logits && i + 1 == tokens.len());
        }
        self.ctx.decode(self.batch)?;
        if logits {
            let l = self
                .ctx
                .logits(tokens.len() - 1)
                .ok_or(DecodeError::Failed(-1))?;
            logits_out.clear();
            logits_out.extend_from_slice(l);
        }
        Ok(())
    }

    fn len(&self, seq: Seq) -> usize {
        (self.ctx.seq_pos_max(seq) + 1).max(0) as usize
    }
}

/// One candidate being generated.
struct Stream {
    seq: Seq,
    /// The token to decode next, at `pos`.
    next: Token,
    pos: usize,
    /// Bytes of the healed tail matched so far.
    matched: usize,
    bytes: Vec<u8>,
    /// Probability of tokens that have not completed a character yet.
    pending: f32,
    candidate: Candidate,
    done: bool,
}

impl Stream {
    /// Applies a chosen token. Returns true when the text grew.
    fn take(&mut self, piece: &[u8], prob: f32, tail: &[u8]) -> bool {
        if self.matched < tail.len() {
            match heal::step(&tail[self.matched..], piece) {
                Step::Done(rest) => {
                    self.matched = tail.len();
                    self.bytes.extend_from_slice(rest);
                }
                Step::Partial(_) => self.matched += piece.len(),
                Step::Disagrees => {
                    self.done = true;
                    return false;
                }
            }
        } else {
            self.bytes.extend_from_slice(piece);
        }
        self.pending *= prob;
        let end = self.candidate.text.len();
        let valid = match std::str::from_utf8(&self.bytes[end..]) {
            Ok(s) => s.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => {
                // Not UTF-8 and never will be: stop here.
                self.done = true;
                return false;
            }
        };
        if valid == 0 {
            return false;
        }
        let added = std::str::from_utf8(&self.bytes[end..end + valid]).expect("validated above");
        self.candidate.text.push_str(added);
        self.candidate.token_ends.push(self.candidate.text.len());
        self.candidate.token_probs.push(self.pending);
        self.pending = 1.0;
        true
    }
}

impl Engine {
    /// Renders and tokenizes the autocomplete prompt into `self.tokens`.
    /// Returns the token index where the stable sections end, and the byte
    /// range of the healed tail in `self.text`.
    fn prompt(&mut self, model: &Model, request: &CompletionRequest) -> (usize, Layout) {
        let layout = prompt::render(&request.segments, &mut self.text);
        let stable = &self.text[..layout.stable_end];
        if self.stable.0 != stable {
            self.tokens.clear();
            model.tokenize(stable, true, false, &mut self.tokens);
            self.stable = (stable.to_owned(), self.tokens.len());
        }
        self.tokens.clear();
        model.tokenize(
            &self.text[..layout.tail_start],
            true,
            false,
            &mut self.tokens,
        );
        let reserve = MAX_STREAMS * (request.max_tokens as usize + HEAL_ROOM);
        let budget = (self.ctx.n_ctx() as usize).saturating_sub(reserve);
        let mut anchor = self.stable.1;
        if self.tokens.len() > budget {
            // Keep a leading BOS, drop whole steps after it.
            let keep_bos = usize::from(model.adds_bos());
            let drop = (self.tokens.len() - budget).div_ceil(TRIM_STEP) * TRIM_STEP;
            let drop = drop.min(self.tokens.len() - keep_bos);
            self.tokens.drain(keep_bos..keep_bos + drop);
            anchor = anchor.saturating_sub(drop);
        }
        (anchor, layout)
    }

    /// Brings `lane` to `self.tokens[..end]`. When the memory is full, the
    /// other lane is emptied and the sync tried once more.
    fn sync(
        &mut self,
        which: Which,
        end: usize,
        anchor: usize,
        logits: bool,
        check: &(dyn Fn() -> bool + Sync),
    ) -> Result<Synced, DecodeError> {
        for attempt in 0..2 {
            let Engine {
                ctx,
                batch,
                complete,
                draft,
                tokens,
                ..
            } = self;
            let (lane, other) = match which {
                Which::Complete => (complete, draft),
                Which::Draft => (draft, complete),
            };
            let result = ctx.with_abort(check, |ctx| {
                lane.sync(&mut Mem { ctx, batch }, tokens, end, anchor, logits, check)
            });
            match result {
                Err(DecodeError::NoSlot) if attempt == 0 => other.clear(&mut Mem { ctx, batch }),
                other => return other,
            }
        }
        unreachable!("the loop returns on the second attempt")
    }

    /// Maps a decode failure to the request's error. A failed decode leaves
    /// the context in an unknown state, so it is recreated; if that cannot
    /// allocate, the model reports itself unavailable.
    fn fail(&mut self, model: &Model, error: DecodeError) -> CompleteError {
        match error {
            DecodeError::Aborted => CompleteError::Cancelled,
            DecodeError::NoSlot => CompleteError::Failed("the context is full".into()),
            DecodeError::Failed(code) => {
                log::warn!("llama_decode failed ({code}); recreating the context");
                self.complete.reset();
                self.draft.reset();
                self.stable = (String::new(), 0);
                match Context::new(model, &self.params) {
                    Some(ctx) => {
                        self.ctx = ctx;
                        CompleteError::Failed(format!("decoding failed ({code})"))
                    }
                    None => CompleteError::Unavailable("out of memory".into()),
                }
            }
        }
    }

    fn complete(
        &mut self,
        model: &Model,
        pieces: &PieceTable,
        request: &CompletionRequest,
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
    ) -> Result<Completion, CompleteError> {
        let started = Instant::now();
        let (anchor, layout) = self.prompt(model, request);
        let decoded_before = self.complete.decoded();
        let check = || cancel.is_cancelled();
        match self.sync(Which::Complete, self.tokens.len(), anchor, true, &check) {
            Ok(Synced::Done) => {}
            Ok(Synced::Cancelled) => return Err(CompleteError::Cancelled),
            Err(e) => return Err(self.fail(model, e)),
        }
        let prompt_done = Instant::now();
        self.timings = Timings {
            prompt_tokens: self.tokens.len(),
            decoded_tokens: self.complete.decoded() - decoded_before,
            prompt_ms: ms(started, prompt_done),
            ..Timings::default()
        };
        let tail = self.text.as_bytes()[layout.tail_start..].to_vec();
        let result = self.generate_candidates(model, pieces, request, &tail, stop, cancel);
        // Free the stream sequences' cells for the next request.
        for s in &self.streams {
            self.ctx.seq_rm(s.seq, -1, -1);
        }
        self.timings.generate_ms = ms(prompt_done, Instant::now());
        result
    }

    fn generate_candidates(
        &mut self,
        model: &Model,
        pieces: &PieceTable,
        request: &CompletionRequest,
        tail: &[u8],
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
    ) -> Result<Completion, CompleteError> {
        let wanted = 1 + usize::from(request.alternatives).min(MAX_STREAMS - 2);
        let n_prompt = self.tokens.len();
        if n_prompt == 0 {
            return Ok(Completion {
                id: request.id,
                candidates: vec![Candidate::default()],
            });
        }
        // With a typed tail the most likely single token is often the bare
        // prefix (" sch"), whose continuations are less likely than a
        // whole-word token (" schedule"). One more first token is tried, and
        // the streams are ranked by the probability of their first text once
        // each has some.
        let first = if tail.is_empty() { wanted } else { wanted + 1 };
        let mut ranked = tail.is_empty();
        self.picks.clear();
        let over = if tail.is_empty() {
            Over::All
        } else {
            self.allowed.clear();
            pieces.agreeing(tail, &mut self.allowed);
            Over::Only(&self.allowed)
        };
        // The first pick is the same for every stream, so its probability is
        // given the typed letters; later picks are relative to the whole
        // vocabulary, so streams healed in more steps are not inflated.
        sample::top(
            &self.complete.logits,
            over,
            Norm::Within,
            first,
            |t| pieces.piece(t).is_empty(),
            &mut self.picks,
        );
        self.streams.clear();
        for (i, &(token, prob)) in self.picks.iter().enumerate() {
            let seq = FIRST_STREAM + i as Seq;
            self.ctx.seq_rm(seq, -1, -1);
            self.ctx.seq_cp(COMPLETE_SEQ, seq);
            let mut s = Stream {
                seq,
                next: token,
                pos: n_prompt,
                matched: 0,
                bytes: Vec::new(),
                pending: 1.0,
                candidate: Candidate::default(),
                done: false,
            };
            if s.take(pieces.piece(token), prob, tail) && ranked && i == 0 {
                s.done =
                    stop.should_stop(&s.candidate.text, s.candidate.token_probs.last().copied());
            }
            self.streams.push(s);
        }
        let max_tokens = request.max_tokens as usize;
        let check = || cancel.is_cancelled();
        for _ in 0..max_tokens + tail.len() {
            if !ranked
                && self
                    .streams
                    .iter()
                    .all(|s| s.done || !s.candidate.text.is_empty())
            {
                self.rank(wanted, stop);
                ranked = true;
            }
            if cancel.is_cancelled() {
                return Err(CompleteError::Cancelled);
            }
            let Some(main) = self.streams.first() else {
                break;
            };
            if ranked && (main.done || main.candidate.token_ends.len() >= max_tokens) {
                break;
            }
            self.batch.clear();
            for s in self.streams.iter().filter(|s| !s.done) {
                self.batch.push(s.next, s.pos, s.seq, true);
            }
            if self.batch.len() == 0 {
                break;
            }
            let Engine { ctx, batch, .. } = self;
            let decode_started = Instant::now();
            if let Err(e) = ctx.with_abort(&check, |ctx| ctx.decode(batch)) {
                return Err(self.fail(model, e));
            }
            ctx.synchronize();
            self.timings.generate_decode_ms += ms(decode_started, Instant::now());
            self.timings.steps += 1;
            let mut row = 0;
            for (i, s) in self.streams.iter_mut().enumerate() {
                if s.done {
                    continue;
                }
                let logits = self
                    .ctx
                    .logits(row)
                    .ok_or_else(|| CompleteError::Failed("no logits".into()))?;
                row += 1;
                s.pos += 1;
                self.picks.clear();
                let over = if s.matched < tail.len() {
                    self.allowed.clear();
                    pieces.agreeing(&tail[s.matched..], &mut self.allowed);
                    Over::Only(&self.allowed)
                } else {
                    Over::All
                };
                sample::top(
                    logits,
                    over,
                    Norm::Vocabulary,
                    1,
                    |_| false,
                    &mut self.picks,
                );
                let Some(&(token, prob)) = self.picks.first() else {
                    s.done = true;
                    continue;
                };
                let piece = pieces.piece(token);
                if piece.is_empty() || model.is_eog(token) {
                    s.done = true;
                    continue;
                }
                s.next = token;
                if s.take(piece, prob, tail) && ranked && i == 0 {
                    s.done = stop
                        .should_stop(&s.candidate.text, s.candidate.token_probs.last().copied());
                }
                if s.candidate.token_ends.len() >= max_tokens {
                    s.done = true;
                }
            }
        }
        if !ranked {
            self.rank(wanted, stop);
        }
        let mut candidates: Vec<Candidate> = Vec::with_capacity(self.streams.len());
        for (i, s) in self.streams.iter_mut().enumerate() {
            let c = std::mem::take(&mut s.candidate);
            if i == 0 || (!c.text.is_empty() && !candidates.iter().any(|o| o.text == c.text)) {
                candidates.push(c);
            }
        }
        if candidates.is_empty() {
            candidates.push(Candidate::default());
        }
        Ok(Completion {
            id: request.id,
            candidates,
        })
    }

    /// Orders the streams by the probability of their first text, keeps the
    /// `wanted` best, and runs the stop rule over the main candidate's tokens
    /// so far.
    fn rank(&mut self, wanted: usize, stop: &mut dyn StopCheck) {
        let first_prob = |s: &Stream| s.candidate.token_probs.first().copied().unwrap_or(0.0);
        self.streams
            .sort_by(|a, b| first_prob(b).total_cmp(&first_prob(a)));
        for s in self.streams.drain(wanted.min(self.streams.len())..) {
            self.ctx.seq_rm(s.seq, -1, -1);
        }
        let Some(main) = self.streams.first_mut() else {
            return;
        };
        let c = &mut main.candidate;
        for i in 0..c.token_ends.len() {
            if stop.should_stop(&c.text[..c.token_ends[i]], Some(c.token_probs[i])) {
                c.text.truncate(c.token_ends[i]);
                c.token_ends.truncate(i + 1);
                c.token_probs.truncate(i + 1);
                main.done = true;
                break;
            }
        }
    }

    fn prefill(
        &mut self,
        model: &Model,
        request: &CompletionRequest,
        cancel: &CancelToken,
        waiting: &AtomicUsize,
    ) -> Result<(), CompleteError> {
        let (anchor, _) = self.prompt(model, request);
        let end = self.tokens.len().saturating_sub(PREFILL_HOLDBACK);
        let check = || cancel.is_cancelled() || waiting.load(Ordering::Acquire) > 0;
        match self.sync(Which::Complete, end, anchor, false, &check) {
            Ok(_) => Ok(()),
            Err(e) => Err(self.fail(model, e)),
        }
    }

    fn generate(
        &mut self,
        model: &Model,
        pieces: &PieceTable,
        request: &GenerateRequest,
        cancel: &CancelToken,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, CompleteError> {
        let started = Instant::now();
        let stop_marker = chat_tokens(model, &request.messages, &mut self.tokens);
        let n_ctx = self.ctx.n_ctx() as usize;
        if self.tokens.len() + request.max_tokens as usize > n_ctx {
            return Err(CompleteError::Failed(format!(
                "the conversation ({} tokens) does not fit the context ({n_ctx} tokens)",
                self.tokens.len()
            )));
        }
        let check = || cancel.is_cancelled();
        let end = self.tokens.len();
        let decoded_before = self.draft.decoded();
        match self.sync(Which::Draft, end, end, true, &check) {
            Ok(Synced::Done) => {}
            Ok(Synced::Cancelled) => return Err(CompleteError::Cancelled),
            Err(e) => return Err(self.fail(model, e)),
        }
        let prompt_done = Instant::now();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let mut rng = Rng::new(request.id ^ seed);
        let mut bytes = Vec::new();
        let mut text = String::new();
        let mut emitted = 0;
        let mut steps = 0;
        let mut decode_ms = 0.0;
        for i in 0..request.max_tokens {
            if cancel.is_cancelled() {
                return Err(CompleteError::Cancelled);
            }
            let Some(token) = sample::sample(
                &self.draft.logits,
                request.temperature,
                |_| false,
                &mut rng,
                &mut self.picks,
            ) else {
                break;
            };
            if model.is_eog(token) || pieces.piece(token).is_empty() {
                break;
            }
            bytes.extend_from_slice(pieces.piece(token));
            match std::str::from_utf8(&bytes) {
                Ok(s) => {
                    text.push_str(s);
                    bytes.clear();
                }
                Err(e) if e.error_len().is_none() => {
                    let valid = e.valid_up_to();
                    text.push_str(std::str::from_utf8(&bytes[..valid]).expect("validated"));
                    bytes.drain(..valid);
                }
                Err(_) => break,
            }
            if let Some(marker) = stop_marker
                && let Some(at) = text.find(marker)
            {
                text.truncate(at);
                break;
            }
            let safe = stop_marker.map_or(text.len(), |m| text.len() - held_back(&text, m));
            if safe > emitted {
                on_delta(&text[emitted..safe]);
                emitted = safe;
            }
            steps += 1;
            if i + 1 == request.max_tokens {
                break;
            }
            let Engine {
                ctx, batch, draft, ..
            } = self;
            let decode_started = Instant::now();
            if let Err(e) = ctx.with_abort(&check, |ctx| draft.push(&mut Mem { ctx, batch }, token))
            {
                return Err(self.fail(model, e));
            }
            decode_ms += ms(decode_started, Instant::now());
        }
        if text.len() > emitted {
            on_delta(&text[emitted..]);
        }
        self.timings = Timings {
            prompt_tokens: end,
            decoded_tokens: self.draft.decoded() - decoded_before,
            prompt_ms: ms(started, prompt_done),
            steps,
            generate_ms: ms(prompt_done, Instant::now()),
            generate_decode_ms: decode_ms,
        };
        Ok(text)
    }
}

#[derive(Clone, Copy)]
enum Which {
    Complete,
    Draft,
}

fn ms(a: Instant, b: Instant) -> f64 {
    b.duration_since(a).as_secs_f64() * 1e3
}

/// Bytes at the end of `text` that could be the start of `marker`; they are
/// held back from streaming until the next token decides.
fn held_back(text: &str, marker: &str) -> usize {
    (1..marker.len().min(text.len() + 1))
        .rev()
        .find(|&n| {
            text.is_char_boundary(text.len() - n) && marker.starts_with(&text[text.len() - n..])
        })
        .unwrap_or(0)
}

/// Tokenizes a chat for the model: through its chat template when llama.cpp
/// knows it, else the plain transcript. Message contents are tokenized as
/// text, so they cannot inject special tokens. Returns the stop marker of
/// the plain format.
fn chat_tokens(
    model: &Model,
    messages: &[ChatMessage],
    out: &mut Vec<Token>,
) -> Option<&'static str> {
    out.clear();
    if let Some(tmpl) = model.chat_template()
        && templated(model, &tmpl, messages, out)
    {
        return None;
    }
    out.clear();
    model.tokenize(&prompt::plain_chat(messages), true, false, out);
    Some(prompt::PLAIN_STOP)
}

fn templated(model: &Model, tmpl: &CStr, messages: &[ChatMessage], out: &mut Vec<Token>) -> bool {
    let marks: Vec<String> = (0..messages.len())
        .map(|i| format!("\u{E000}{i}\u{E001}"))
        .collect();
    let roles: [CString; 3] =
        ["system", "user", "assistant"].map(|r| CString::new(r).expect("no NUL"));
    let contents: Vec<CString> = marks
        .iter()
        .map(|m| CString::new(m.as_str()).expect("no NUL"))
        .collect();
    let chat: Vec<(&CStr, &CStr)> = messages
        .iter()
        .zip(&contents)
        .map(|(m, c)| {
            let role = match m.role {
                Role::System => &roles[0],
                Role::User => &roles[1],
                Role::Assistant => &roles[2],
            };
            (role.as_c_str(), c.as_c_str())
        })
        .collect();
    let Some(formatted) = Model::apply_chat_template(tmpl, &chat) else {
        return false;
    };
    let mut rest = formatted.as_str();
    let mut first = true;
    for (mark, message) in marks.iter().zip(messages) {
        let Some(at) = rest.find(mark.as_str()) else {
            return false;
        };
        model.tokenize(&rest[..at], first, true, out);
        model.tokenize(&message.content, false, false, out);
        rest = &rest[at + mark.len()..];
        first = false;
    }
    model.tokenize(rest, first, true, out);
    true
}

impl Completer for LocalModel {
    fn caps(&self) -> Caps {
        Caps {
            token_probs: true,
            cheap_alternatives: true,
            prefill: true,
            local: true,
        }
    }

    fn complete(
        &self,
        request: &CompletionRequest,
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
    ) -> Result<Completion, CompleteError> {
        self.run(|l| {
            if cancel.is_cancelled() {
                return Err(CompleteError::Cancelled);
            }
            l.engine
                .complete(&l.model, &l.pieces, request, stop, cancel)
        })
    }

    fn prefill(&self, request: &CompletionRequest, cancel: &CancelToken) {
        if cancel.is_cancelled() {
            return;
        }
        if let Err(e) = self.run(|l| l.engine.prefill(&l.model, request, cancel, &self.waiting)) {
            log::debug!("prefill: {e}");
        }
    }
}

impl Generator for LocalModel {
    fn generate(
        &self,
        request: &GenerateRequest,
        cancel: &CancelToken,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, CompleteError> {
        self.run(|l| {
            l.engine
                .generate(&l.model, &l.pieces, request, cancel, on_delta)
        })
    }
}

#[cfg(test)]
mod spike;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_back_keeps_a_possible_marker_start() {
        assert_eq!(held_back("Sure.\n", "\nUser:"), 1);
        assert_eq!(held_back("Sure.\nUs", "\nUser:"), 3);
        assert_eq!(held_back("Sure.", "\nUser:"), 0);
        assert_eq!(held_back("", "\nUser:"), 0);
        assert_eq!(held_back("é", "\nUser:"), 0);
    }

    #[test]
    fn stream_returns_only_the_continuation() {
        let tail = b" sch";
        let mut s = Stream {
            seq: 1,
            next: 0,
            pos: 0,
            matched: 0,
            bytes: Vec::new(),
            pending: 1.0,
            candidate: Candidate::default(),
            done: false,
        };
        assert!(
            !s.take(b" s", 0.5, tail),
            "a prefix of the typed word adds no text"
        );
        assert!(s.take(b"chedule", 0.8, tail));
        assert!(s.take(b" a", 0.9, tail));
        assert_eq!(s.candidate.text, "edule a");
        assert_eq!(s.candidate.token_ends, vec![5, 7]);
        assert!((s.candidate.token_probs[0] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn stream_merges_tokens_that_split_a_character() {
        let mut s = Stream {
            seq: 1,
            next: 0,
            pos: 0,
            matched: 0,
            bytes: Vec::new(),
            pending: 1.0,
            candidate: Candidate::default(),
            done: false,
        };
        let e = "é".as_bytes();
        assert!(!s.take(&e[..1], 0.5, b""));
        assert!(s.take(&e[1..], 0.5, b""));
        assert_eq!(s.candidate.text, "é");
        assert_eq!(s.candidate.token_ends, vec![2]);
        assert_eq!(s.candidate.token_probs, vec![0.25]);
        assert!(!s.take(&[0xFF], 1.0, b""));
        assert!(s.done, "invalid UTF-8 ends the stream");
    }
}
