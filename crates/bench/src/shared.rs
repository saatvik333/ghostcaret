//! One generation serves every configuration, in typing order.
//!
//! Configurations differ only in their stop rule, and decoding is greedy, so
//! the text a configuration's own request would generate is a prefix of what
//! a request that stops later generates. [`Shared`] therefore runs each
//! distinct prompt once, feeding the tokens to the [`Stopper`] of every
//! configuration at the same time ([`FanOut`]) until all of them have
//! stopped, and records for each the token count and time at which its own
//! stopper ended generation. A configuration's answer is the main candidate
//! cut back to that count, exactly what its own request would have returned,
//! with the time its own request would have taken.
//!
//! All configurations of a sample replay at once, one thread each, and the
//! model serves the waiting request nearest the start of the text only when
//! every replay is waiting. Requests therefore reach the model in typing
//! order, as they do in the product: the prompt only grows, so the model's
//! cache only appends, which matters most for recurrent models that cannot
//! cut their state back cheaply. The order depends only on the replays, not
//! on thread timing, so runs stay deterministic.
//!
//! Alternatives are cut to the same token count. The product's alternatives
//! decode in lockstep with the main candidate, so this matches it except
//! where token healing or a split UTF-8 character made a stream skip a token;
//! an alternative is only shown when the main candidate is silent, which the
//! report counts separately.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Instant;

use oft_engine::complete::{
    CancelToken, Candidate, Caps, CompleteError, Completer, Completion, CompletionRequest,
    StopCheck, StopParams,
};
use oft_engine::io::AppInfo;
use oft_engine::settings::Settings;
use oft_engine::stop::{Stopper, max_tokens};

use crate::metrics::Tally;
use crate::pause::PauseModel;
use crate::replay::{self, Answer, Provider, Sample};

/// Where one configuration's stopper ended generation.
#[derive(Clone, Copy, Debug, PartialEq)]
struct StopAt {
    tokens: usize,
    ms: f64,
}

#[derive(Debug)]
struct Entry {
    candidates: Vec<Candidate>,
    stops: Vec<Option<StopAt>>,
    total_ms: f64,
}

#[derive(Debug, Default)]
struct State {
    /// Replays still running.
    live: usize,
    /// Replays waiting for a prompt that is not generated yet. A replay
    /// stops counting the moment its prompt is done, before it wakes, so the
    /// next generation waits for its next request.
    blocked: usize,
    /// A generation is running.
    running: bool,
    /// Prompts not generated yet, by caret position.
    waiting: BTreeMap<(usize, String), CompletionRequest>,
    /// Replays waiting for each prompt that is waiting or being generated.
    waiters: HashMap<String, usize>,
    done: HashMap<String, Result<Entry, CompleteError>>,
    generations: u64,
    tokens: u64,
}

/// Answers every configuration's requests from one shared generation per
/// prompt; see the module documentation.
pub struct Shared<'m> {
    model: &'m dyn Completer,
    stops: Vec<StopParams>,
    max_tokens: u32,
    state: Mutex<State>,
    wake: Condvar,
}

impl std::fmt::Debug for Shared<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("stops", &self.stops)
            .finish()
    }
}

impl<'m> Shared<'m> {
    /// `stops` must hold the stop rule of every configuration that will ask.
    pub fn new(model: &'m dyn Completer, stops: Vec<StopParams>) -> Shared<'m> {
        let max_tokens = stops.iter().map(max_tokens).max().unwrap_or(0);
        Shared {
            model,
            stops,
            max_tokens,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        }
    }

    /// Model calls made so far.
    pub fn generations(&self) -> u64 {
        self.lock().generations
    }

    /// Tokens of the main candidate generated so far.
    pub fn tokens(&self) -> u64 {
        self.lock().tokens
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replays `sample` under each of `settings` at once and returns their
    /// tallies in the same order.
    pub fn replay_all(
        &self,
        sample: &Sample,
        app: &AppInfo,
        settings: &[Settings],
        pauses: &PauseModel,
    ) -> Vec<Tally> {
        {
            let mut st = self.lock();
            st.done.clear();
            st.waiting.clear();
            st.waiters.clear();
            st.live = settings.len();
            st.blocked = 0;
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = settings
                .iter()
                .map(|s| {
                    scope.spawn(move || {
                        // Leaves the set of live replays even when the replay
                        // panics, so the others never wait for it.
                        let _leave = Leave(self);
                        replay::replay(sample, app, s, pauses, &mut Client { shared: self })
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("a replay panicked"))
                .collect()
        })
    }

    fn generate(&self, request: &CompletionRequest) -> Result<Entry, CompleteError> {
        let wide = CompletionRequest {
            max_tokens: self.max_tokens,
            ..request.clone()
        };
        let mut fan = FanOut::new(&self.stops);
        let completion = self.model.complete(&wide, &mut fan, &CancelToken::new())?;
        let total_ms = fan.started.elapsed().as_secs_f64() * 1e3;
        Ok(Entry {
            candidates: completion.candidates,
            stops: fan.at,
            total_ms,
        })
    }

    /// The answer for `request` at caret byte `at`, generating waiting
    /// requests in order whenever every replay waits.
    fn answer(&self, request: &CompletionRequest, at: usize) -> Result<Answer, CompleteError> {
        let i = self
            .stops
            .iter()
            .position(|s| *s == request.stop)
            .unwrap_or_else(|| panic!("no shared stopper for {:?}", request.stop));
        let key = format!("{:?}", request.segments);
        let mut st = self.lock();
        if !st.done.contains_key(&key) {
            st.blocked += 1;
            match st.waiters.get_mut(&key) {
                Some(n) => *n += 1,
                None => {
                    st.waiters.insert(key.clone(), 1);
                    st.waiting.insert((at, key.clone()), request.clone());
                }
            }
        }
        loop {
            if let Some(result) = st.done.get(&key) {
                let entry = result.as_ref().map_err(Clone::clone)?;
                let (tokens, ms) = match entry.stops[i] {
                    Some(s) => (s.tokens, s.ms),
                    None => (usize::MAX, entry.total_ms),
                };
                return Ok(Answer {
                    completion: Completion {
                        id: request.id,
                        candidates: cut_back(&entry.candidates, tokens),
                    },
                    latency_ms: ms,
                });
            }
            if st.blocked == st.live && !st.running {
                let ((_, next_key), next) = st
                    .waiting
                    .pop_first()
                    .expect("a waiting replay has a request");
                st.running = true;
                drop(st);
                let result = self.generate(&next);
                st = self.lock();
                st.running = false;
                if let Ok(e) = &result {
                    st.generations += 1;
                    st.tokens += e.candidates.first().map_or(0, |c| c.token_ends.len()) as u64;
                }
                let woken = st.waiters.remove(&next_key).unwrap_or(0);
                st.blocked -= woken;
                st.done.insert(next_key, result);
                self.wake.notify_all();
            } else {
                st = self.wake.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        }
    }
}

struct Leave<'a>(&'a Shared<'a>);

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        self.0.lock().live -= 1;
        self.0.wake.notify_all();
    }
}

/// One replay's view of [`Shared`].
struct Client<'a> {
    shared: &'a Shared<'a>,
}

impl Provider for Client<'_> {
    fn caps(&self) -> Caps {
        self.shared.model.caps()
    }

    fn complete(
        &mut self,
        request: &CompletionRequest,
        at: usize,
    ) -> Result<Answer, CompleteError> {
        self.shared.answer(request, at)
    }
}

/// Feeds every configuration's stopper and stops once all have stopped.
struct FanOut<'a> {
    params: &'a [StopParams],
    stoppers: Vec<Stopper>,
    max: Vec<usize>,
    at: Vec<Option<StopAt>>,
    tokens: usize,
    last_len: usize,
    started: Instant,
}

impl<'a> FanOut<'a> {
    fn new(params: &'a [StopParams]) -> FanOut<'a> {
        FanOut {
            params,
            stoppers: params.iter().map(Stopper::new).collect(),
            max: params.iter().map(|p| max_tokens(p) as usize).collect(),
            at: vec![None; params.len()],
            tokens: 0,
            last_len: 0,
            started: Instant::now(),
        }
    }
}

impl StopCheck for FanOut<'_> {
    fn should_stop(&mut self, generated: &str, token_prob: Option<f32>) -> bool {
        if self.tokens > 0 && generated.len() <= self.last_len {
            // The provider started over with another main candidate.
            self.stoppers = self.params.iter().map(Stopper::new).collect();
            self.at.fill(None);
            self.tokens = 0;
        }
        self.tokens += 1;
        self.last_len = generated.len();
        let ms = self.started.elapsed().as_secs_f64() * 1e3;
        let mut all = true;
        for i in 0..self.stoppers.len() {
            if self.at[i].is_some() {
                continue;
            }
            // A provider ends a stream at the request's token cap right after
            // asking the stopper, so the cap stops at the same token.
            if self.stoppers[i].should_stop(generated, token_prob) || self.tokens >= self.max[i] {
                self.at[i] = Some(StopAt {
                    tokens: self.tokens,
                    ms,
                });
            } else {
                all = false;
            }
        }
        all
    }
}

/// The candidates cut back to their first `tokens` tokens, with empty and
/// repeated alternatives dropped as a provider drops them.
fn cut_back(candidates: &[Candidate], tokens: usize) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for (i, c) in candidates.iter().enumerate() {
        let c = truncate(c, tokens);
        if i == 0 || (!c.text.is_empty() && !out.iter().any(|o| o.text == c.text)) {
            out.push(c);
        }
    }
    out
}

fn truncate(c: &Candidate, tokens: usize) -> Candidate {
    if tokens >= c.token_ends.len() {
        return c.clone();
    }
    let end = if tokens == 0 {
        0
    } else {
        c.token_ends[tokens - 1]
    };
    Candidate {
        text: c.text[..end].to_owned(),
        token_ends: c.token_ends[..tokens].to_vec(),
        token_probs: c.token_probs[..tokens.min(c.token_probs.len())].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use oft_engine::complete::LengthMode;
    use oft_engine::io::{AppInfo, FieldId};

    use super::*;

    /// Streams a fixed text word by word with fixed probabilities, asking the
    /// stopper after every token like a real provider.
    struct Fixed {
        tokens: Vec<(&'static str, f32)>,
        calls: Mutex<u32>,
    }

    impl Completer for Fixed {
        fn caps(&self) -> Caps {
            Caps {
                token_probs: true,
                ..Caps::default()
            }
        }

        fn complete(
            &self,
            request: &CompletionRequest,
            stop: &mut dyn StopCheck,
            _: &CancelToken,
        ) -> Result<Completion, CompleteError> {
            *self.calls.lock().unwrap() += 1;
            let mut c = Candidate::default();
            for &(t, p) in &self.tokens {
                c.text.push_str(t);
                c.token_ends.push(c.text.len());
                c.token_probs.push(p);
                if stop.should_stop(&c.text, Some(p))
                    || c.token_ends.len() >= request.max_tokens as usize
                {
                    break;
                }
            }
            Ok(Completion {
                id: request.id,
                candidates: vec![c],
            })
        }
    }

    fn params(mode: LengthMode, boldness: f32) -> StopParams {
        StopParams {
            mode,
            ceiling: if mode == LengthMode::Auto {
                LengthMode::Sentence
            } else {
                mode
            },
            boldness,
            paragraph_words: 60,
        }
    }

    fn request(stop: StopParams) -> CompletionRequest {
        CompletionRequest {
            id: 1,
            field: FieldId(1),
            revision: 1,
            segments: oft_engine::complete::Segments {
                app: AppInfo::default(),
                before: "We will ".into(),
                ..Default::default()
            },
            stop,
            max_tokens: max_tokens(&stop),
            alternatives: 0,
        }
    }

    fn model() -> Fixed {
        Fixed {
            tokens: vec![
                (" see", 0.9),
                (" you", 0.9),
                (" at", 0.5),
                (" the", 0.9),
                (" station", 0.9),
                (".", 0.9),
                (" Then", 0.9),
                (" we", 0.9),
            ],
            calls: Mutex::new(0),
        }
    }

    /// What a configuration's own request returns, for comparison.
    fn own(m: &Fixed, stop: StopParams) -> Completion {
        let mut s = Stopper::new(&stop);
        m.complete(&request(stop), &mut s, &CancelToken::new())
            .unwrap()
    }

    /// Answers one request with no other replay running.
    fn solo(shared: &Shared<'_>, request: &CompletionRequest) -> Completion {
        shared.lock().live = 1;
        shared.answer(request, 0).unwrap().completion
    }

    #[test]
    fn shared_answers_equal_own_requests() {
        let configs = [
            params(LengthMode::Word, 0.5),
            params(LengthMode::Phrase, 0.5),
            params(LengthMode::Sentence, 0.5),
            params(LengthMode::Auto, 0.0),
            params(LengthMode::Auto, 0.5),
            params(LengthMode::Auto, 1.0),
        ];
        let m = model();
        let shared = Shared::new(&m, configs.to_vec());
        for &stop in &configs {
            assert_eq!(
                solo(&shared, &request(stop)),
                own(&model(), stop),
                "{stop:?}"
            );
        }
        // One generation answered all six.
        assert_eq!(*m.calls.lock().unwrap(), 1);
        assert_eq!(shared.generations(), 1);
    }

    #[test]
    fn word_mode_shows_the_first_word_from_a_shared_generation() {
        let m = model();
        let word = params(LengthMode::Word, 0.5);
        let shared = Shared::new(&m, vec![word, params(LengthMode::Sentence, 0.5)]);
        let got = solo(&shared, &request(word));
        let ctx = oft_engine::stop::CutContext {
            before: "We will ",
            after: "",
        };
        let view = oft_engine::stop::cut(&got.candidates[0], &word, &ctx).unwrap();
        assert_eq!(view.visible(), "see");
        // The shared generation itself ran on to the sentence end.
        assert!(shared.tokens() >= 6, "{}", shared.tokens());
    }

    /// Continues the text it was typed from, word by word, with a wrong word
    /// at every fourth word boundary, and records each prompt it sees.
    struct Oracle {
        text: &'static str,
        seen: Mutex<Vec<usize>>,
    }

    impl Completer for Oracle {
        fn caps(&self) -> Caps {
            Caps {
                token_probs: true,
                cheap_alternatives: false,
                prefill: false,
                local: true,
            }
        }

        fn complete(
            &self,
            request: &CompletionRequest,
            stop: &mut dyn StopCheck,
            _: &CancelToken,
        ) -> Result<Completion, CompleteError> {
            let before = &request.segments.before;
            self.seen.lock().unwrap().push(before.len());
            let rest = self.text.strip_prefix(before.as_str()).unwrap_or("");
            let mut c = Candidate::default();
            for (n, piece) in rest.split_inclusive(' ').enumerate() {
                let piece = if (before.len() + n).is_multiple_of(4) {
                    "wrong "
                } else {
                    piece
                };
                let p = if n % 3 == 2 { 0.3 } else { 0.95 };
                c.text.push_str(piece);
                c.token_ends.push(c.text.len());
                c.token_probs.push(p);
                if stop.should_stop(&c.text, Some(p))
                    || c.token_ends.len() >= request.max_tokens as usize
                {
                    break;
                }
            }
            Ok(Completion {
                id: request.id,
                candidates: vec![c],
            })
        }
    }

    /// The product's way: one request per configuration with its own stopper.
    struct Own<'a>(&'a dyn Completer);

    impl Provider for Own<'_> {
        fn caps(&self) -> Caps {
            self.0.caps()
        }

        fn complete(
            &mut self,
            request: &CompletionRequest,
            _: usize,
        ) -> Result<Answer, CompleteError> {
            let mut stop = Stopper::new(&request.stop);
            Ok(Answer {
                completion: self.0.complete(request, &mut stop, &CancelToken::new())?,
                latency_ms: 0.0,
            })
        }
    }

    #[test]
    fn replaying_together_equals_replaying_alone_and_runs_in_typing_order() {
        const TEXT: &str = "we will see you at the station and then we can walk home together. ";
        let oracle = Oracle {
            text: TEXT,
            seen: Mutex::new(Vec::new()),
        };
        let app = AppInfo {
            id: "slack".into(),
            name: "Slack".into(),
            title: String::new(),
        };
        let sample = Sample {
            id: "s".into(),
            title: String::new(),
            conversation: Vec::new(),
            before: String::new(),
            text: TEXT.into(),
        };
        let pauses = PauseModel::new(3, 30);
        let configs = crate::results::sweep(
            &[
                LengthMode::Word,
                LengthMode::Phrase,
                LengthMode::Sentence,
                LengthMode::Auto,
            ],
            &[LengthMode::Phrase, LengthMode::Sentence],
            &[0.0, 0.5, 1.0],
        );
        let settings: Vec<Settings> = configs
            .iter()
            .map(|c| c.settings(crate::corpus::Kind::Chat))
            .collect();
        let caps = oracle.caps();
        let mut stops: Vec<StopParams> = Vec::new();
        for s in &settings {
            let (stop, _) = replay::request_stop(s, &app, caps);
            if !stops.contains(&stop) {
                stops.push(stop);
            }
        }
        let shared = Shared::new(&oracle, stops);
        let together = shared.replay_all(&sample, &app, &settings, &pauses);
        let seen = std::mem::take(&mut *oracle.seen.lock().unwrap());
        assert!(
            seen.windows(2).all(|w| w[0] <= w[1]),
            "prompts out of typing order: {seen:?}"
        );
        assert_eq!(shared.generations(), seen.len() as u64);
        let mut useful = 0;
        for (s, mut t) in settings.iter().zip(together) {
            let mut alone = replay::replay(&sample, &app, s, &pauses, &mut Own(&oracle));
            t.latency_ms.clear();
            alone.latency_ms.clear();
            assert_eq!(t, alone, "{:?}", s.length);
            useful += t.accepted;
        }
        assert!(
            useful > 0,
            "the oracle should get some suggestions accepted"
        );
    }

    #[test]
    fn cut_back_drops_alternatives_that_become_empty_or_equal() {
        let c = |t: &[&str]| {
            let mut c = Candidate::default();
            for s in t {
                c.text.push_str(s);
                c.token_ends.push(c.text.len());
                c.token_probs.push(0.5);
            }
            c
        };
        let cands = [c(&["a", "b", "c"]), c(&["a", "b", "d"]), c(&["x"])];
        let cut = cut_back(&cands, 2);
        assert_eq!(
            cut.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["ab", "x"]
        );
        assert_eq!(cut[0].token_ends, [1, 2]);
        assert_eq!(cut_back(&cands, 0)[0].text, "");
    }
}
