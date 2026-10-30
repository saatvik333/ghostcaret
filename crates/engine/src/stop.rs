//! Length and boldness: the stop rule while tokens are generated
//! ([`Stopper`]) and the cut that turns a finished continuation into a
//! suggestion ([`cut`]). Both apply the same plan to the boundaries
//! [`crate::text`] finds, so generation never runs past what the cut could
//! use.
//!
//! # Boldness
//!
//! Boldness sets a threshold τ on the cumulative probability of the shown
//! text, the product of its token probabilities:
//!
//! τ(b) = 0.8^(1−b) · 0.05^b
//!
//! so careful (b = 0) needs 80 % confidence, the middle (b = 0.5) 20 %, and
//! bold (b = 1) 5 %. The interpolation is log-linear because confidence
//! multiplies token by token. The two end points are first targets; the
//! benchmark tunes them (docs/quality.md).
//!
//! # The plan
//!
//! Every mode first requires the first word to clear τ; otherwise nothing is
//! shown, because silence beats a wrong guess. Then:
//!
//! - Word shows the first word; Sentence the first sentence; Paragraph up to
//!   the line break, or the last sentence within the word cap (the last word
//!   within it when no sentence ends there).
//! - Phrase shows up to the first phrase boundary after at least
//!   [`PHRASE_MIN_WORDS`] words, never crosses a sentence end, and stops at
//!   [`PHRASE_MAX_WORDS`] words when no pause comes.
//! - Auto shows the longest prefix ending at a word boundary whose cumulative
//!   probability stays at or above τ, bounded by the ceiling's limit as
//!   above. Without token probabilities it shows up to the first sentence end,
//!   or less when the ceiling is Word or Phrase.
//!
//! A fixed mode whose boundary never came shows the longest complete prefix.
//! Generation stops once the shown end is settled and one more word has
//! completed past it: that word confirms the boundary (an abbreviation or a
//! conjunction is only known from what follows) and gives Show More and Take
//! Word something to use without a new request.
//!
//! # The cut
//!
//! [`cut`] also joins the continuation to the field: it drops leading
//! whitespace when the text before the caret already ends in whitespace,
//! stops before the first line break (which also discards CR LF), ends before
//! any text that would repeat what already follows the caret, and refuses
//! suggestions without a letter or digit in them.

use crate::complete::{Candidate, LengthMode, StopCheck, StopParams};
use crate::io::SuggestionView;
use crate::text::{Boundary, Level, Scanner, Sink, is_horizontal_space};

/// Threshold at boldness 0 (careful).
pub const CAREFUL_THRESHOLD: f32 = 0.8;
/// Threshold at boldness 1 (bold).
pub const BOLD_THRESHOLD: f32 = 0.05;
/// Fewest words a Phrase suggestion shows before a pause may end it.
pub const PHRASE_MIN_WORDS: u32 = 2;
/// Most words a Phrase suggestion shows when no pause comes.
pub const PHRASE_MAX_WORDS: u32 = 8;

/// The cumulative probability the shown text must keep at this boldness; see
/// the module documentation.
pub fn threshold(boldness: f32) -> f32 {
    let b = if boldness.is_nan() {
        0.5
    } else {
        boldness.clamp(0.0, 1.0)
    };
    CAREFUL_THRESHOLD.powf(1.0 - b) * BOLD_THRESHOLD.powf(b)
}

fn level(mode: LengthMode) -> Level {
    match mode {
        LengthMode::Word => Level::Word,
        LengthMode::Phrase => Level::Phrase,
        LengthMode::Sentence | LengthMode::Auto => Level::Sentence,
        LengthMode::Paragraph => Level::Paragraph,
    }
}

/// The furthest level the stop rule may reach.
fn limit(stop: &StopParams) -> Level {
    level(if stop.mode == LengthMode::Auto {
        stop.ceiling
    } else {
        stop.mode
    })
}

/// Hard cap on generated tokens for a request with this stop rule: enough for
/// the longest suggestion the rule allows plus the one-word lookahead, at
/// about 1.3 tokens per English word, and no more, so a cloud request never
/// pays for text that cannot be shown.
pub fn max_tokens(stop: &StopParams) -> u32 {
    match limit(stop) {
        Level::Word => 8,
        Level::Phrase => 24,
        Level::Sentence => 64,
        Level::Paragraph => stop
            .paragraph_words
            .saturating_mul(2)
            .saturating_add(24)
            .min(1024),
    }
}

/// Natural log of a token probability; missing or invalid values count as
/// near-impossible so they can never make a suggestion look confident.
fn ln_prob(p: f32) -> f32 {
    if p.is_nan() {
        f32::MIN_POSITIVE.ln()
    } else {
        p.clamp(f32::MIN_POSITIVE, 1.0).ln()
    }
}

/// Decides, boundary by boundary, how far a suggestion shows.
#[derive(Clone, Debug)]
struct Plan {
    limit: Level,
    /// Auto with probabilities: confidence decides the cut.
    auto: bool,
    /// ln τ, when probabilities are known.
    ln_tau: Option<f32>,
    paragraph_cap: u32,
    first_checked: bool,
    show: Option<Boundary>,
    last_sentence: Option<Boundary>,
    /// No later boundary can move `show`.
    settled: bool,
    /// The first word failed the threshold.
    silent: bool,
}

impl Plan {
    fn new(stop: &StopParams, probs: bool) -> Plan {
        let auto = stop.mode == LengthMode::Auto;
        let limit = if auto && !probs {
            limit(stop).min(Level::Sentence)
        } else {
            limit(stop)
        };
        Plan {
            limit,
            auto: auto && probs,
            ln_tau: probs.then(|| threshold(stop.boldness).ln()),
            paragraph_cap: stop.paragraph_words.max(1).saturating_mul(2),
            first_checked: false,
            show: None,
            last_sentence: None,
            settled: false,
            silent: false,
        }
    }

    /// The level Show More steps by.
    fn step(&self) -> Level {
        if self.limit == Level::Word {
            Level::Word
        } else {
            Level::Phrase
        }
    }

    /// Feeds a boundary with the cumulative ln-probability of the text before
    /// it (ignored without probabilities).
    fn boundary(&mut self, b: Boundary, ln_p: f32) {
        if self.settled || b.half_words == 0 {
            return;
        }
        if let Some(ln_tau) = self.ln_tau {
            let clears = ln_p >= ln_tau;
            if !self.first_checked {
                self.first_checked = true;
                if !clears {
                    self.silent = true;
                    self.settled = true;
                    return;
                }
            } else if self.auto && !clears {
                self.settled = true;
                return;
            }
        }
        let settles = match self.limit {
            Level::Word => true,
            Level::Phrase => {
                if b.half_words > 2 * PHRASE_MAX_WORDS && self.show.is_some() {
                    self.settled = true;
                    return;
                }
                b.level >= Level::Sentence
                    || (b.level >= Level::Phrase && b.half_words >= 2 * PHRASE_MIN_WORDS)
                    || b.half_words >= 2 * PHRASE_MAX_WORDS
            }
            Level::Sentence => b.level >= Level::Sentence,
            Level::Paragraph => {
                if b.half_words > self.paragraph_cap && self.show.is_some() {
                    if let Some(s) = self.last_sentence {
                        self.show = Some(s);
                    }
                    self.settled = true;
                    return;
                }
                if b.level >= Level::Sentence {
                    self.last_sentence = Some(b);
                }
                b.level >= Level::Paragraph
            }
        };
        self.show = Some(b);
        self.settled = settles;
    }
}

/// The stop rule while a model generates, built from the request's
/// [`StopParams`]. It ends generation one word past the furthest point that
/// could be shown, at a line break, or as soon as the first word fails the
/// boldness threshold.
///
/// Expects the generated text to grow by appending; a shorter text than
/// before restarts it. Work per token is proportional to the new text only,
/// and nothing is allocated beyond one entry per token.
#[derive(Clone, Debug)]
pub struct Stopper {
    params: StopParams,
    plan: Option<Plan>,
    scanner: Scanner,
    /// `(end, cumulative ln-probability)` per token so far.
    tokens: Vec<(usize, f32)>,
    ln_p: f32,
    /// First token that may cover the next boundary.
    next: usize,
    ahead: bool,
    stopped: bool,
}

impl Stopper {
    /// A stopper for one generation with this stop rule.
    pub fn new(params: &StopParams) -> Stopper {
        Stopper {
            params: *params,
            plan: None,
            scanner: Scanner::new(""),
            tokens: Vec::with_capacity(max_tokens(params) as usize),
            ln_p: 0.0,
            next: 0,
            ahead: false,
            stopped: false,
        }
    }
}

impl StopCheck for Stopper {
    fn should_stop(&mut self, generated: &str, token_prob: Option<f32>) -> bool {
        if generated.len() < self.scanner.pos()
            || self.tokens.last().is_some_and(|t| generated.len() < t.0)
        {
            *self = Stopper::new(&self.params);
        }
        if self.stopped {
            return true;
        }
        if let Some(p) = token_prob {
            self.ln_p += ln_prob(p);
        }
        self.tokens.push((generated.len(), self.ln_p));
        let Stopper {
            params,
            plan,
            scanner,
            tokens,
            next,
            ahead,
            stopped,
            ..
        } = self;
        let plan = plan.get_or_insert_with(|| Plan::new(params, token_prob.is_some()));
        scanner.feed(
            generated,
            false,
            &mut StopSink {
                plan,
                tokens,
                next,
                ahead,
            },
        );
        // A line break ends everything usable even before the scanner decides
        // on it as the newest grapheme.
        let line_end = scanner.at_line_end()
            || generated[scanner.pos()..].starts_with(crate::text::is_newline);
        *stopped = plan.silent || line_end || (plan.settled && *ahead);
        *stopped
    }
}

/// Cumulative ln-probability of the text before byte `end`: the running sum
/// through the first token that ends at or after it.
fn covering(tokens: &[(usize, f32)], next: &mut usize, end: usize) -> f32 {
    while *next + 1 < tokens.len() && tokens[*next].0 < end {
        *next += 1;
    }
    tokens.get(*next).map_or(0.0, |t| t.1)
}

struct StopSink<'a> {
    plan: &'a mut Plan,
    tokens: &'a [(usize, f32)],
    next: &'a mut usize,
    ahead: &'a mut bool,
}

impl Sink for StopSink<'_> {
    fn boundary(&mut self, b: Boundary) {
        let ln_p = covering(self.tokens, self.next, b.end);
        self.plan.boundary(b, ln_p);
    }

    fn word(&mut self, start: usize) {
        if self.plan.settled && self.plan.show.is_none_or(|s| start >= s.end) {
            *self.ahead = true;
        }
    }
}

/// What the cut needs to know about the field.
#[derive(Clone, Copy, Debug, Default)]
pub struct CutContext<'a> {
    /// Text before the caret.
    pub before: &'a str,
    /// Text after the caret.
    pub after: &'a str,
}

/// Turns a generated continuation into a suggestion body: `text` (the
/// buffered continuation, ending at its last boundary), `shown`, `word_ends`
/// and `step_ends`. `field`, `revision` and `alternative` are left for the
/// caller. Returns `None` when nothing should be shown. The result always
/// satisfies [`SuggestionView::is_valid`]. O(n) in the candidate's length.
pub fn cut(
    candidate: &Candidate,
    stop: &StopParams,
    ctx: &CutContext<'_>,
) -> Option<SuggestionView> {
    let text = candidate.text.as_str();
    let start = if ctx.before.is_empty() || ctx.before.ends_with(char::is_whitespace) {
        text.len() - text.trim_start_matches(is_horizontal_space).len()
    } else {
        0
    };
    let body = &text[start..];
    let probs = !candidate.token_probs.is_empty()
        && candidate.token_probs.len() == candidate.token_ends.len();
    let plan = Plan::new(stop, probs);
    let mut sink = CutSink {
        step: plan.step(),
        plan,
        limit: repeat_start(body, ctx.after),
        units: Vec::new(),
        steps: Vec::new(),
        last: None,
        past_limit: false,
        tokens: if probs {
            Some((&candidate.token_ends, &candidate.token_probs))
        } else {
            None
        },
        offset: start,
        next: 0,
        ln_p: 0.0,
    };
    Scanner::new(ctx.before).feed(body, true, &mut sink);
    let CutSink {
        plan,
        mut units,
        steps,
        last,
        ..
    } = sink;
    if plan.silent {
        return None;
    }
    let show = plan.show?;
    let end = last?.end;
    if !body[..show.end].contains(char::is_alphanumeric) {
        return None;
    }
    units.truncate(units.partition_point(|&e| e < end));
    units.push(end);
    let mut step_ends = Vec::new();
    let mut words = show.half_words;
    for b in steps {
        if b.end > show.end && b.half_words > words {
            step_ends.push(b.end);
            words = b.half_words;
        }
    }
    if end > show.end && step_ends.last() != Some(&end) {
        step_ends.push(end);
    }
    Some(SuggestionView {
        text: body[..end].to_owned(),
        shown: show.end,
        word_ends: units,
        step_ends,
        ..SuggestionView::default()
    })
}

struct CutSink<'a> {
    plan: Plan,
    step: Level,
    /// Boundaries past this would repeat text after the caret.
    limit: usize,
    units: Vec<usize>,
    steps: Vec<Boundary>,
    last: Option<Boundary>,
    past_limit: bool,
    tokens: Option<(&'a [usize], &'a [f32])>,
    /// Bytes dropped from the start of the candidate.
    offset: usize,
    next: usize,
    ln_p: f32,
}

impl CutSink<'_> {
    fn ln_p(&mut self, end: usize) -> f32 {
        let Some((ends, probs)) = self.tokens else {
            return 0.0;
        };
        let end = self.offset + end;
        while self.next < ends.len() && ends[self.next] < end {
            self.ln_p += ln_prob(probs[self.next]);
            self.next += 1;
        }
        self.ln_p + probs.get(self.next).map_or(0.0, |&p| ln_prob(p))
    }
}

impl Sink for CutSink<'_> {
    fn unit(&mut self, end: usize) {
        if end <= self.limit {
            self.units.push(end);
        }
    }

    fn boundary(&mut self, b: Boundary) {
        if b.end > self.limit {
            self.past_limit = true;
            return;
        }
        let ln_p = self.ln_p(b.end);
        self.plan.boundary(b, ln_p);
        self.last = Some(b);
        if b.level >= self.step {
            self.steps.push(b);
        }
    }

    fn done(&self) -> bool {
        self.past_limit
    }
}

/// Where `body` starts repeating the text after the caret, or `body.len()`.
///
/// The probe is the start of `after`: a single punctuation mark (`)` or `.`
/// already in the field), or its first word, extended by the next word when
/// shorter than four bytes. A word probe only matches whole words.
fn repeat_start(body: &str, after: &str) -> usize {
    let after = after.trim_start_matches(is_horizontal_space);
    let Some(first) = after
        .chars()
        .next()
        .filter(|&c| !crate::text::is_newline(c))
    else {
        return body.len();
    };
    let word = first.is_alphanumeric();
    let probe = if word {
        let word_end = |from: usize| {
            after[from..]
                .find(|c: char| !(c.is_alphanumeric() || matches!(c, '\'' | '’' | '_')))
                .map_or(after.len(), |i| from + i)
        };
        let mut end = word_end(0);
        if end < 4 {
            let rest = &after[end..];
            let spaces = rest.len() - rest.trim_start_matches(' ').len();
            if spaces > 0 && rest[spaces..].starts_with(char::is_alphanumeric) {
                end = word_end(end + spaces);
            }
        }
        &after[..end]
    } else {
        &after[..first.len_utf8()]
    };
    let mut from = 0;
    while let Some(i) = body[from..].find(probe) {
        let at = from + i;
        let whole = !word
            || (!body[..at].ends_with(char::is_alphanumeric)
                && !body[at + probe.len()..].starts_with(char::is_alphanumeric));
        if whole {
            return at;
        }
        from = at + first.len_utf8();
    }
    body.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(mode: LengthMode, ceiling: LengthMode, boldness: f32) -> StopParams {
        StopParams {
            mode,
            ceiling,
            boldness,
            paragraph_words: 60,
        }
    }

    fn fixed(mode: LengthMode) -> StopParams {
        params(mode, mode, 0.5)
    }

    /// A candidate split into tokens at spaces, each with probability `p(i)`.
    fn tokens(text: &str, p: impl Fn(usize) -> f32) -> Candidate {
        let mut ends: Vec<usize> = text
            .match_indices(' ')
            .map(|(i, _)| i)
            .filter(|&i| i > 0)
            .collect();
        ends.push(text.len());
        let probs = (0..ends.len()).map(p).collect();
        Candidate {
            text: text.into(),
            token_ends: ends,
            token_probs: probs,
        }
    }

    fn plain(text: &str) -> Candidate {
        Candidate {
            text: text.into(),
            ..Candidate::default()
        }
    }

    fn shown(c: &Candidate, stop: &StopParams, before: &str, after: &str) -> Option<String> {
        cut(c, stop, &CutContext { before, after }).map(|v| {
            assert!(v.is_valid(), "{v:?}");
            v.visible().to_owned()
        })
    }

    #[test]
    fn threshold_spans_careful_to_bold() {
        assert!((threshold(0.0) - 0.8).abs() < 1e-6);
        assert!((threshold(0.5) - 0.2).abs() < 1e-6);
        assert!((threshold(1.0) - 0.05).abs() < 1e-6);
        assert!(threshold(0.2) > threshold(0.8));
        assert_eq!(threshold(f32::NAN), threshold(0.5));
    }

    #[test]
    fn fixed_modes_cut_at_their_boundary() {
        let c = plain(" see you tomorrow, at noon. Bye then");
        assert_eq!(
            shown(&c, &fixed(LengthMode::Word), "I'll", "").as_deref(),
            Some(" see")
        );
        assert_eq!(
            shown(&c, &fixed(LengthMode::Phrase), "I'll", "").as_deref(),
            Some(" see you tomorrow,")
        );
        assert_eq!(
            shown(&c, &fixed(LengthMode::Sentence), "I'll", "").as_deref(),
            Some(" see you tomorrow, at noon.")
        );
        assert_eq!(
            shown(&c, &fixed(LengthMode::Paragraph), "I'll", "").as_deref(),
            Some(" see you tomorrow, at noon. Bye")
        );
    }

    #[test]
    fn paragraph_respects_the_word_cap_at_a_sentence() {
        let c = plain(" One two three. Four five six seven. Eight");
        let stop = StopParams {
            paragraph_words: 5,
            ..fixed(LengthMode::Paragraph)
        };
        assert_eq!(
            shown(&c, &stop, "Go", "").as_deref(),
            Some(" One two three.")
        );
    }

    #[test]
    fn phrase_needs_two_words_and_stops_at_eight() {
        let c = plain(" Yes, I can do that and then more");
        assert_eq!(
            shown(&c, &fixed(LengthMode::Phrase), "So", "").as_deref(),
            Some(" Yes, I can do that")
        );
        let c = plain(" one two three four five six seven eight nine ten");
        assert_eq!(
            shown(&c, &fixed(LengthMode::Phrase), "So", "").as_deref(),
            Some(" one two three four five six seven eight")
        );
    }

    #[test]
    fn auto_shows_the_confident_prefix_within_the_ceiling() {
        // Cumulative: 0.9, 0.81, 0.73, then 0.07.
        let c = tokens(
            " see you at noon tomorrow",
            |i| if i < 3 { 0.9 } else { 0.1 },
        );
        let auto = |ceiling, boldness| params(LengthMode::Auto, ceiling, boldness);
        assert_eq!(
            shown(&c, &auto(LengthMode::Sentence, 0.5), "I'll", "").as_deref(),
            Some(" see you at")
        );
        assert_eq!(
            shown(&c, &auto(LengthMode::Word, 0.5), "I'll", "").as_deref(),
            Some(" see")
        );
        assert_eq!(
            shown(&c, &auto(LengthMode::Sentence, 1.0), "I'll", "").as_deref(),
            Some(" see you at noon")
        );
    }

    #[test]
    fn every_mode_needs_a_confident_first_word() {
        let c = tokens(" maybe later", |_| 0.1);
        for mode in [
            LengthMode::Word,
            LengthMode::Phrase,
            LengthMode::Sentence,
            LengthMode::Paragraph,
            LengthMode::Auto,
        ] {
            assert_eq!(
                shown(&c, &params(mode, LengthMode::Sentence, 0.5), "ok", ""),
                None,
                "{mode:?}"
            );
        }
        assert!(
            shown(
                &c,
                &params(LengthMode::Word, LengthMode::Word, 1.0),
                "ok",
                ""
            )
            .is_some()
        );
    }

    #[test]
    fn auto_without_probabilities_cuts_at_the_first_sentence() {
        let c = plain(" see you soon. And then more. Bye");
        let stop = params(LengthMode::Auto, LengthMode::Paragraph, 0.5);
        assert_eq!(
            shown(&c, &stop, "I'll", "").as_deref(),
            Some(" see you soon.")
        );
        let stop = params(LengthMode::Auto, LengthMode::Phrase, 0.5);
        assert_eq!(
            shown(&c, &stop, "I'll", "").as_deref(),
            Some(" see you soon.")
        );
    }

    #[test]
    fn join_drops_a_doubled_space() {
        let c = plain(" tomorrow then.");
        assert_eq!(
            shown(&c, &fixed(LengthMode::Word), "see you ", "").as_deref(),
            Some("tomorrow")
        );
        assert_eq!(
            shown(&c, &fixed(LengthMode::Word), "see you", "").as_deref(),
            Some(" tomorrow")
        );
    }

    #[test]
    fn never_repeats_text_after_the_caret() {
        let c = plain(" quick reply. See you soon. Bye");
        let stop = fixed(LengthMode::Paragraph);
        assert_eq!(
            shown(&c, &stop, "Thanks for the", ". See you").as_deref(),
            Some(" quick reply")
        );
        assert_eq!(
            shown(&c, &stop, "Thanks for the", " See you soon").as_deref(),
            Some(" quick reply.")
        );
        let c = plain(" below) for details");
        assert_eq!(shown(&c, &stop, "(see", ")").as_deref(), Some(" below"));
        let c = plain(" world");
        assert_eq!(shown(&c, &stop, "hello", "world"), None);
        let c = plain(" the theme park.");
        assert_eq!(shown(&c, &stop, "go to", "the").as_deref(), None);
        assert_eq!(
            shown(&c, &stop, "go to", "them").as_deref(),
            Some(" the theme park.")
        );
    }

    #[test]
    fn stops_at_a_line_break() {
        let c = plain(" thanks\r\nBest, Anna");
        assert_eq!(
            shown(&c, &fixed(LengthMode::Paragraph), "Many", "").as_deref(),
            Some(" thanks")
        );
        assert_eq!(
            shown(
                &plain("\nNext line"),
                &fixed(LengthMode::Paragraph),
                "Done",
                ""
            ),
            None
        );
    }

    #[test]
    fn never_shows_only_punctuation_or_whitespace() {
        assert_eq!(
            shown(&plain(". ... !"), &fixed(LengthMode::Sentence), "Done", ""),
            None
        );
        assert_eq!(
            shown(&plain("   "), &fixed(LengthMode::Sentence), "Done", ""),
            None
        );
        assert_eq!(
            shown(&plain(". Bye now"), &fixed(LengthMode::Word), "Done", "").as_deref(),
            Some(". Bye")
        );
    }

    #[test]
    fn buffer_keeps_the_lookahead_as_steps() {
        let c = plain(" see you tomorrow, at noon. Bye ");
        let v = cut(
            &c,
            &fixed(LengthMode::Word),
            &CutContext {
                before: "I'll",
                after: "",
            },
        )
        .unwrap();
        assert_eq!(v.visible(), " see");
        assert_eq!(v.text, " see you tomorrow, at noon. Bye");
        assert_eq!(v.step_ends.first().map(|&e| &v.text[..e]), Some(" see you"));
        let v = cut(
            &c,
            &fixed(LengthMode::Sentence),
            &CutContext {
                before: "I'll",
                after: "",
            },
        )
        .unwrap();
        assert_eq!(
            v.step_ends
                .iter()
                .map(|&e| &v.text[..e])
                .collect::<Vec<_>>(),
            [" see you tomorrow, at noon. Bye"]
        );
        let v = cut(
            &plain(" see you. Bye"),
            &fixed(LengthMode::Word),
            &CutContext {
                before: "I'll",
                after: "",
            },
        )
        .unwrap();
        assert_eq!(
            v.text, " see you.",
            "a trailing word without a separator may be cut off"
        );
    }

    #[test]
    fn max_tokens_grows_with_the_limit() {
        let w = max_tokens(&fixed(LengthMode::Word));
        let p = max_tokens(&fixed(LengthMode::Phrase));
        let s = max_tokens(&params(LengthMode::Auto, LengthMode::Sentence, 0.5));
        let g = max_tokens(&fixed(LengthMode::Paragraph));
        assert!(w < p && p < s && s < g);
    }

    /// Runs a stopper over the given tokens; returns the text generated when
    /// it stopped.
    fn generate(stop: &StopParams, pieces: &[(&str, Option<f32>)]) -> String {
        let mut s = Stopper::new(stop);
        let mut text = String::new();
        for (piece, p) in pieces {
            text.push_str(piece);
            if s.should_stop(&text, *p) {
                break;
            }
        }
        text
    }

    #[test]
    fn stopper_ends_one_word_past_the_shown_end() {
        let toks = [
            (" see", None),
            (" you", None),
            (" at", None),
            (" noon", None),
            (".", None),
            (" Bye", None),
            (" now", None),
        ];
        assert_eq!(generate(&fixed(LengthMode::Word), &toks), " see you at");
        assert_eq!(
            generate(&fixed(LengthMode::Sentence), &toks),
            " see you at noon. Bye now"
        );
    }

    #[test]
    fn stopper_stops_when_the_first_word_fails() {
        let toks = [
            (" may", Some(0.1)),
            ("be", Some(0.9)),
            (" later", Some(0.9)),
            (" on", Some(0.9)),
            (" today", Some(0.9)),
        ];
        assert_eq!(
            generate(&params(LengthMode::Auto, LengthMode::Sentence, 0.5), &toks),
            " maybe later on"
        );
    }

    #[test]
    fn stopper_stops_after_confidence_drops_in_auto() {
        let toks = [
            (" see", Some(0.9)),
            (" you", Some(0.9)),
            (" at", Some(0.1)),
            (" noon", Some(0.9)),
            (" or", Some(0.9)),
            (" so", Some(0.9)),
        ];
        assert_eq!(
            generate(&params(LengthMode::Auto, LengthMode::Paragraph, 0.5), &toks),
            " see you at noon or"
        );
    }

    #[test]
    fn stopper_stops_at_a_line_break() {
        let toks = [(" thanks", None), ("\n", None), ("Best", None)];
        assert_eq!(generate(&fixed(LengthMode::Paragraph), &toks), " thanks\n");
    }

    /// Whatever the stopper lets through, the cut of the full text shows no
    /// more than the cut of what the stopper kept: generation never stops
    /// before the shown end.
    #[test]
    fn stopper_never_stops_before_what_the_cut_shows() {
        let text = " Sure, I can do that tomorrow and then we talk. Mr. Smith agrees, etc. Fine";
        let pieces: Vec<&str> = text.split_inclusive(' ').collect();
        for mode in [
            LengthMode::Word,
            LengthMode::Phrase,
            LengthMode::Sentence,
            LengthMode::Paragraph,
        ] {
            let stop = fixed(mode);
            let toks: Vec<(&str, Option<f32>)> = pieces.iter().map(|p| (*p, None)).collect();
            let kept = generate(&stop, &toks);
            let ctx = CutContext {
                before: "Yes",
                after: "",
            };
            let full = cut(&plain(text), &stop, &ctx).map(|v| v.visible().to_owned());
            let partial = cut(&plain(&kept), &stop, &ctx).map(|v| v.visible().to_owned());
            assert_eq!(partial, full, "{mode:?} stopped at {kept:?}");
        }
    }

    mod props {
        use proptest::prelude::*;

        use super::*;

        fn mode() -> impl Strategy<Value = LengthMode> {
            prop_oneof![
                Just(LengthMode::Word),
                Just(LengthMode::Phrase),
                Just(LengthMode::Sentence),
                Just(LengthMode::Paragraph),
                Just(LengthMode::Auto)
            ]
        }

        fn text() -> impl Strategy<Value = String> {
            proptest::string::string_regex(
                "([a-zA-Z]{1,7}|[ .,!?;:'\"()/\\-—…0-9]|[你好。，]|é|e\u{301}|👍|\r\n|\n){0,40}",
            )
            .expect("valid regex")
        }

        fn candidate() -> impl Strategy<Value = Candidate> {
            (
                text(),
                proptest::collection::vec(0.0f32..=1.0, 0..20),
                any::<bool>(),
            )
                .prop_map(|(text, probs, with)| {
                    let bounds: Vec<usize> = (1..=text.len())
                        .filter(|&i| text.is_char_boundary(i))
                        .collect();
                    let n = probs.len().min(bounds.len());
                    let step = (bounds.len() / n.max(1)).max(1);
                    let ends: Vec<usize> = bounds.iter().copied().step_by(step).take(n).collect();
                    let probs = probs[..ends.len()].to_vec();
                    if with {
                        Candidate {
                            text,
                            token_ends: ends,
                            token_probs: probs,
                        }
                    } else {
                        Candidate {
                            text,
                            ..Candidate::default()
                        }
                    }
                })
        }

        #[derive(Clone, Debug)]
        enum Op {
            Word,
            All,
            More,
            Type(usize),
        }

        fn op() -> impl Strategy<Value = Op> {
            prop_oneof![
                Just(Op::Word),
                Just(Op::All),
                Just(Op::More),
                (1usize..4).prop_map(Op::Type)
            ]
        }

        proptest! {
            #[test]
            fn cut_is_always_valid_and_operations_keep_it_valid(
                c in candidate(),
                mode in mode(),
                ceiling in mode().prop_filter("not auto", |m| *m != LengthMode::Auto),
                boldness in 0.0f32..=1.0,
                words in 1u32..80,
                before in text(),
                after in text(),
                ops in proptest::collection::vec(op(), 0..12),
            ) {
                let stop = StopParams { mode, ceiling: if mode == LengthMode::Auto { ceiling } else { mode }, boldness, paragraph_words: words };
                let Some(mut v) = cut(&c, &stop, &CutContext { before: &before, after: &after }) else { return Ok(()) };
                prop_assert!(v.is_valid(), "{:?}", v);
                prop_assert!(v.shown > 0 && v.visible().contains(char::is_alphanumeric));
                prop_assert!(!v.text.contains(crate::text::is_newline));
                for op in ops {
                    match op {
                        Op::Word => { v.take_word(); }
                        Op::All => { v.take_all(); }
                        Op::More => { v.show_more(); }
                        Op::Type(n) => {
                            let end = v.visible().char_indices().nth(n).map_or(v.shown, |(i, _)| i);
                            let typed = v.visible()[..end].to_owned();
                            v.type_through(&typed);
                        }
                    }
                    prop_assert!(v.is_valid(), "{:?}", v);
                }
            }

            #[test]
            fn stopper_agrees_with_the_cut(c in candidate(), mode in mode(), boldness in 0.0f32..=1.0) {
                let stop = StopParams { mode, ceiling: LengthMode::Sentence, boldness, paragraph_words: 20 };
                let probs = !c.token_probs.is_empty();
                let mut s = Stopper::new(&stop);
                let mut kept = c.text.clone();
                let mut ends = c.token_ends.clone();
                if !probs { ends = (1..=c.text.len()).filter(|&i| c.text.is_char_boundary(i)).collect(); }
                for (i, &e) in ends.iter().enumerate() {
                    let p = probs.then(|| c.token_probs[i]);
                    if s.should_stop(&c.text[..e], p) {
                        kept.truncate(e);
                        break;
                    }
                }
                let trimmed = |text: &str| {
                    let n = ends.iter().take_while(|&&e| e <= text.len()).count();
                    Candidate { text: text.to_owned(), token_ends: if probs { ends[..n].to_vec() } else { Vec::new() }, token_probs: if probs { c.token_probs[..n].to_vec() } else { Vec::new() } }
                };
                let ctx = CutContext { before: "x", after: "" };
                let full = cut(&trimmed(&c.text), &stop, &ctx).map(|v| v.visible().to_owned());
                let part = cut(&trimmed(&kept), &stop, &ctx).map(|v| v.visible().to_owned());
                prop_assert_eq!(part, full, "kept {:?}", kept);
            }
        }
    }
}
