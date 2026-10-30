//! Counts from replays and the metrics derived from them
//! (docs/quality.md, "Suggestion quality").

use serde::{Deserialize, Serialize};

/// What one or more replays counted. Adding tallies adds their counts, so
/// metrics of a corpus are computed from its pooled counts, not averaged
/// over samples.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tally {
    pub samples: u64,
    /// Words in the typed text (Unicode word segmentation).
    pub words: u64,
    /// Characters the user had to produce: the keystrokes without Ghostcaret.
    pub chars: u64,
    /// Characters typed by hand.
    pub typed: u64,
    pub take_word: u64,
    pub take_all: u64,
    /// Characters inserted by Take Word and Take All.
    pub accepted_chars: u64,
    /// Completion requests the controller sent.
    pub requests: u64,
    pub errors: u64,
    /// Suggestions that appeared.
    pub shown: u64,
    /// Shown suggestions whose first Take Word unit was not what came next.
    pub wrong: u64,
    /// Shown suggestions from which at least one unit was taken.
    pub accepted: u64,
    /// Shown suggestions that came from an alternative because the main
    /// candidate was silent.
    pub from_alternative: u64,
    /// Words shown in each suggestion when it appeared, summed.
    pub shown_words: u64,
    /// Time to suggestion per request, in ms; summarised in [`Metrics`].
    #[serde(skip)]
    pub latency_ms: Vec<f64>,
}

impl Tally {
    pub fn add(&mut self, o: &Tally) {
        self.samples += o.samples;
        self.words += o.words;
        self.chars += o.chars;
        self.typed += o.typed;
        self.take_word += o.take_word;
        self.take_all += o.take_all;
        self.accepted_chars += o.accepted_chars;
        self.requests += o.requests;
        self.errors += o.errors;
        self.shown += o.shown;
        self.wrong += o.wrong;
        self.accepted += o.accepted;
        self.from_alternative += o.from_alternative;
        self.shown_words += o.shown_words;
        self.latency_ms.extend_from_slice(&o.latency_ms);
    }

    /// Keystrokes with Ghostcaret: typed characters plus accept keys.
    pub fn keystrokes(&self) -> u64 {
        self.typed + self.take_word + self.take_all
    }

    pub fn metrics(&self) -> Metrics {
        let ratio = |a: u64, b: u64| if b == 0 { 0.0 } else { a as f64 / b as f64 };
        Metrics {
            keystroke_savings: if self.chars == 0 {
                0.0
            } else {
                1.0 - ratio(self.keystrokes(), self.chars)
            },
            wrong_rate: ratio(self.wrong, self.shown),
            acceptance_rate: ratio(self.accepted, self.shown),
            chars_per_accepted: ratio(self.accepted_chars, self.accepted),
            chars_per_shown: ratio(self.accepted_chars, self.shown),
            shown_per_100_words: 100.0 * ratio(self.shown, self.words),
            wrong_per_100_words: 100.0 * ratio(self.wrong, self.words),
            words_per_shown: ratio(self.shown_words, self.shown),
            latency: Latency::of(&self.latency_ms),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// 1 − keystrokes with Ghostcaret / keystrokes without; accept keys count.
    pub keystroke_savings: f64,
    /// Wrong / shown.
    pub wrong_rate: f64,
    /// Accepted / shown.
    pub acceptance_rate: f64,
    /// Accepted characters per suggestion that was accepted at all.
    pub chars_per_accepted: f64,
    /// Accepted characters per shown suggestion.
    pub chars_per_shown: f64,
    pub shown_per_100_words: f64,
    pub wrong_per_100_words: f64,
    /// Mean words visible when a suggestion appeared.
    pub words_per_shown: f64,
    pub latency: Latency,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Latency {
    pub requests: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
}

impl Latency {
    /// Nearest-rank percentiles.
    pub fn of(samples: &[f64]) -> Latency {
        if samples.is_empty() {
            return Latency::default();
        }
        let mut s = samples.to_vec();
        s.sort_by(f64::total_cmp);
        let rank = |p: f64| s[((p * s.len() as f64).ceil() as usize).clamp(1, s.len()) - 1];
        Latency {
            requests: s.len(),
            p50_ms: rank(0.50),
            p95_ms: rank(0.95),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_by_hand() {
        // 100 characters; 40 typed, 6 Tabs and 4 backticks inserted the other
        // 60. 20 suggestions shown, 5 wrong, 8 accepted, over 20 words.
        let t = Tally {
            samples: 1,
            words: 20,
            chars: 100,
            typed: 40,
            take_word: 6,
            take_all: 4,
            accepted_chars: 60,
            shown: 20,
            wrong: 5,
            accepted: 8,
            shown_words: 50,
            ..Tally::default()
        };
        let m = t.metrics();
        assert_eq!(t.keystrokes(), 50);
        assert!((m.keystroke_savings - 0.5).abs() < 1e-12);
        assert!((m.wrong_rate - 0.25).abs() < 1e-12);
        assert!((m.acceptance_rate - 0.4).abs() < 1e-12);
        assert!((m.chars_per_accepted - 7.5).abs() < 1e-12);
        assert!((m.chars_per_shown - 3.0).abs() < 1e-12);
        assert!((m.shown_per_100_words - 100.0).abs() < 1e-12);
        assert!((m.wrong_per_100_words - 25.0).abs() < 1e-12);
        assert!((m.words_per_shown - 2.5).abs() < 1e-12);
    }

    #[test]
    fn empty_tally_has_zero_metrics() {
        assert_eq!(Tally::default().metrics(), Metrics::default());
    }

    #[test]
    fn no_suggestions_means_no_savings() {
        let t = Tally {
            chars: 10,
            typed: 10,
            words: 2,
            ..Tally::default()
        };
        assert_eq!(t.metrics().keystroke_savings, 0.0);
    }

    #[test]
    fn pooling_adds_counts_not_rates() {
        // One sample saves 50 % of 10 keys, the other 0 % of 90: pooled
        // savings are 5 / 100, not the mean of the two rates.
        let a = Tally {
            chars: 10,
            typed: 4,
            take_all: 1,
            accepted_chars: 6,
            ..Tally::default()
        };
        let b = Tally {
            chars: 90,
            typed: 90,
            ..Tally::default()
        };
        let mut sum = a.clone();
        sum.add(&b);
        assert_eq!(sum.chars, 100);
        assert!((sum.metrics().keystroke_savings - 0.05).abs() < 1e-12);
    }

    #[test]
    fn nearest_rank_percentiles() {
        let samples: Vec<f64> = (1..=20).map(f64::from).collect();
        let l = Latency::of(&samples);
        assert_eq!(l.requests, 20);
        // ceil(0.5·20) = 10th value; ceil(0.95·20) = 19th.
        assert_eq!(l.p50_ms, 10.0);
        assert_eq!(l.p95_ms, 19.0);
        assert_eq!(Latency::of(&[3.0]).p95_ms, 3.0);
    }
}
