//! Turns streamed chunks into a [`Candidate`]: runs the engine's stop rule on
//! the cleaned text as it grows and maps provider tokens onto the final text.

use oft_engine::complete::{Candidate, StopCheck};

use crate::prompt::{Shape, clean};
use crate::sse::Flow;
use crate::wire::Chunk;

#[derive(Debug)]
pub(crate) struct Accumulator<'a> {
    shape: Shape<'a>,
    raw: String,
    /// Byte offset in `raw` where each reported token ends, and its probability.
    ends: Vec<usize>,
    probs: Vec<f32>,
    /// False once the provider's tokens stopped matching the text, after which
    /// no probabilities are reported: wrong ones would cut suggestions wrongly.
    tokens_ok: bool,
}

impl<'a> Accumulator<'a> {
    pub fn new(shape: Shape<'a>) -> Self {
        Accumulator {
            shape,
            raw: String::new(),
            ends: Vec::new(),
            probs: Vec::new(),
            tokens_ok: true,
        }
    }

    /// Adds a chunk and asks the stop rule after each token (or once per chunk
    /// when the provider reports no tokens). `Stop` means generation can end;
    /// text after the stopping token is discarded.
    pub fn push(&mut self, chunk: &Chunk, stop: &mut dyn StopCheck) -> Flow {
        let base = self.raw.len();
        self.raw.push_str(&chunk.text);
        let covered = self.ends.last().copied().unwrap_or(0);
        let exact = !chunk.tokens.is_empty()
            && chunk.tokens.iter().map(|t| t.len).sum::<usize>() == chunk.text.len()
            && chunk.tokens.iter().all(|t| !t.prob.is_nan());
        if !exact {
            if !chunk.text.is_empty() {
                self.tokens_ok = false;
            }
            return self.ask(self.raw.len(), None, stop);
        }
        if base != covered {
            self.tokens_ok = false;
        }
        let mut end = base;
        for t in &chunk.tokens {
            end += t.len;
            self.ends.push(end);
            self.probs.push(t.prob);
            if self.ask(end, Some(t.prob), stop) == Flow::Stop {
                return Flow::Stop;
            }
        }
        Flow::Continue
    }

    fn ask(&mut self, end: usize, prob: Option<f32>, stop: &mut dyn StopCheck) -> Flow {
        // A token can end inside a character when the provider splits bytes.
        if !self.raw.is_char_boundary(end) {
            return Flow::Continue;
        }
        let prefix = &self.raw[..end];
        let shown = &prefix[clean(self.shape, prefix, false)];
        if stop.should_stop(shown, prob) {
            self.raw.truncate(end);
            Flow::Stop
        } else {
            Flow::Continue
        }
    }

    /// The cleaned candidate, or `None` when nothing usable was generated.
    pub fn finish(self) -> Option<Candidate> {
        let range = clean(self.shape, &self.raw, true);
        if range.is_empty() {
            return None;
        }
        let text = self.raw[range.clone()].to_owned();
        let mut candidate = Candidate {
            text,
            ..Candidate::default()
        };
        if !self.tokens_ok || self.ends.last() != Some(&self.raw.len()) {
            return Some(candidate);
        }
        for (&end, &prob) in self.ends.iter().zip(&self.probs) {
            let mut e = end.saturating_sub(range.start).min(candidate.text.len());
            while !candidate.text.is_char_boundary(e) {
                e += 1;
            }
            if e == 0 {
                continue;
            }
            match candidate.token_ends.last() {
                // Tokens that were cut by trimming, or that share a character,
                // merge; the probability of the pair is the product.
                Some(&last) if last == e => *candidate.token_probs.last_mut().unwrap() *= prob,
                _ => {
                    candidate.token_ends.push(e);
                    candidate.token_probs.push(prob);
                }
            }
        }
        Some(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Token;

    struct StopAfter(usize, Vec<String>);

    impl StopCheck for StopAfter {
        fn should_stop(&mut self, generated: &str, _: Option<f32>) -> bool {
            self.1.push(generated.to_owned());
            self.1.len() >= self.0
        }
    }

    fn never() -> StopAfter {
        StopAfter(usize::MAX, Vec::new())
    }

    fn chunk(text: &str, tokens: &[(usize, f32)]) -> Chunk {
        Chunk {
            text: text.into(),
            tokens: tokens
                .iter()
                .map(|&(len, prob)| Token { len, prob })
                .collect(),
        }
    }

    fn plain(before: &str) -> Shape<'_> {
        Shape {
            before,
            chat: false,
            multiline: false,
        }
    }

    #[test]
    fn maps_tokens_onto_the_text_and_asks_once_per_token() {
        let mut acc = Accumulator::new(plain("I am "));
        let mut stop = never();
        acc.push(&chunk("going to", &[(5, 0.9), (3, 0.5)]), &mut stop);
        acc.push(&chunk(" bed", &[(4, 0.1)]), &mut stop);
        assert_eq!(stop.1, ["going", "going to", "going to bed"]);
        let c = acc.finish().unwrap();
        assert_eq!(
            (c.text.as_str(), c.token_ends, c.token_probs),
            ("going to bed", vec![5, 8, 12], vec![0.9, 0.5, 0.1])
        );
    }

    #[test]
    fn stop_discards_the_tokens_after_the_stopping_one() {
        let mut acc = Accumulator::new(plain("x "));
        let mut stop = StopAfter(2, Vec::new());
        let flow = acc.push(&chunk("a b c", &[(1, 0.9), (2, 0.8), (2, 0.7)]), &mut stop);
        assert_eq!(flow, Flow::Stop);
        let c = acc.finish().unwrap();
        assert_eq!((c.text.as_str(), c.token_ends), ("a b", vec![1, 3]));
    }

    #[test]
    fn chat_tags_and_repeats_are_removed_and_token_ends_shift_with_them() {
        let shape = Shape {
            before: "See you at the",
            chat: true,
            multiline: false,
        };
        let mut acc = Accumulator::new(shape);
        let mut stop = never();
        acc.push(&chunk("<continuation>", &[(14, 1.0)]), &mut stop);
        acc.push(&chunk(" station", &[(8, 0.75)]), &mut stop);
        acc.push(&chunk("</continuation>", &[(15, 1.0)]), &mut stop);
        let c = acc.finish().unwrap();
        assert_eq!(
            (c.text.as_str(), c.token_ends, c.token_probs),
            (" station", vec![8], vec![0.75])
        );
    }

    #[test]
    fn trimmed_tail_merges_into_the_last_token() {
        let mut acc = Accumulator::new(plain("a"));
        let mut stop = never();
        acc.push(&chunk(" b c ", &[(2, 0.5), (2, 0.5), (1, 0.5)]), &mut stop);
        let c = acc.finish().unwrap();
        assert_eq!(
            (c.text.as_str(), c.token_ends, c.token_probs),
            (" b c", vec![2, 4], vec![0.5, 0.25])
        );
    }

    #[test]
    fn token_ends_inside_a_character_snap_forward() {
        let mut acc = Accumulator::new(plain("a "));
        let mut stop = never();
        acc.push(&chunk("é!", &[(1, 0.5), (1, 0.5), (1, 0.5)]), &mut stop);
        assert_eq!(stop.1, ["é", "é!"], "no stop check inside a character");
        let c = acc.finish().unwrap();
        assert_eq!(c.token_ends, vec![2, 3]);
        assert_eq!(c.token_probs, vec![0.25, 0.5]);
    }

    #[test]
    fn mismatched_or_missing_tokens_report_no_probabilities() {
        for tokens in [&[(1, 0.5)][..], &[(2, f32::NAN), (1, 0.5)][..], &[][..]] {
            let mut acc = Accumulator::new(plain("a "));
            acc.push(&chunk("abc", tokens), &mut never());
            let c = acc.finish().unwrap();
            assert_eq!(c.text, "abc");
            assert!(c.token_ends.is_empty() && c.token_probs.is_empty());
        }
    }

    #[test]
    fn a_gap_in_token_coverage_drops_probabilities() {
        let mut acc = Accumulator::new(plain("a "));
        let mut stop = never();
        acc.push(&chunk("ab", &[(1, 0.5), (1, 0.5)]), &mut stop);
        acc.push(&chunk("cd", &[]), &mut stop);
        acc.push(&chunk("ef", &[(1, 0.5), (1, 0.5)]), &mut stop);
        assert!(acc.finish().unwrap().token_probs.is_empty());
    }

    #[test]
    fn nothing_usable_is_none() {
        let mut acc = Accumulator::new(plain("a "));
        acc.push(&chunk("  ", &[]), &mut never());
        assert!(acc.finish().is_none());
    }
}
