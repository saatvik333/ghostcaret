//! Token healing. The partial word at the caret is taken off the prompt, and
//! the first generated tokens must agree with the typed bytes, so the model
//! picks the natural tokenization of the word instead of continuing from an
//! odd split such as `" sch"`. Candidate text then holds only what comes after
//! the typed letters.

use crate::llama::Token;

/// Longest tail that is healed. Longer runs (URLs, long compounds) keep their
/// prompt tokens up to the last few characters.
const MAX_TAIL: usize = 24;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Punct,
    Word,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_ascii_punctuation() {
        Class::Punct
    } else {
        Class::Word
    }
}

/// Byte index in `before` where the healed tail starts: the trailing run of
/// one character class (a word with its leading space, a whitespace run, or a
/// punctuation run), capped at [`MAX_TAIL`] bytes.
pub fn tail_start(before: &str) -> usize {
    let Some(last) = before.chars().next_back() else {
        return 0;
    };
    let cls = class(last);
    let mut start = before.len();
    for (i, c) in before.char_indices().rev() {
        if class(c) != cls {
            break;
        }
        if before.len() - i > MAX_TAIL {
            return start;
        }
        start = i;
    }
    if cls == Class::Word && before[..start].ends_with(' ') && before.len() - start < MAX_TAIL {
        start -= 1;
    }
    start
}

/// Every token's bytes, and the tokens that may be generated sorted by those
/// bytes, so the tokens agreeing with a typed prefix are found by binary
/// search. Built once per model.
pub struct PieceTable {
    bytes: Vec<u8>,
    /// `ends[t]` is where token `t`'s bytes end in `bytes`.
    ends: Vec<u32>,
    /// Generatable tokens (non-empty, not control), sorted by bytes.
    sorted: Vec<Token>,
}

impl PieceTable {
    /// `piece` appends a token's bytes; `generatable` excludes control tokens.
    pub fn new(
        n_vocab: usize,
        mut piece: impl FnMut(Token, &mut Vec<u8>),
        mut generatable: impl FnMut(Token) -> bool,
    ) -> PieceTable {
        let mut bytes = Vec::with_capacity(n_vocab * 8);
        let mut ends = Vec::with_capacity(n_vocab);
        for t in 0..n_vocab as Token {
            piece(t, &mut bytes);
            ends.push(bytes.len() as u32);
        }
        let mut table = PieceTable {
            bytes,
            ends,
            sorted: Vec::new(),
        };
        let mut sorted: Vec<Token> = (0..n_vocab as Token)
            .filter(|&t| !table.piece(t).is_empty() && generatable(t))
            .collect();
        sorted.sort_unstable_by(|&a, &b| table.piece(a).cmp(table.piece(b)));
        table.sorted = sorted;
        table
    }

    pub fn piece(&self, t: Token) -> &[u8] {
        let t = t as usize;
        let start = if t == 0 { 0 } else { self.ends[t - 1] as usize };
        &self.bytes[start..self.ends[t] as usize]
    }

    /// Appends to `out` every generatable token that agrees with `rem`: its
    /// bytes start with `rem`, or are a non-empty proper prefix of it.
    pub fn agreeing(&self, rem: &[u8], out: &mut Vec<Token>) {
        let lo = self.sorted.partition_point(|&t| self.piece(t) < rem);
        let hi = lo + self.sorted[lo..].partition_point(|&t| self.piece(t).starts_with(rem));
        out.extend_from_slice(&self.sorted[lo..hi]);
        for k in 1..rem.len() {
            let prefix = &rem[..k];
            let lo = self.sorted.partition_point(|&t| self.piece(t) < prefix);
            out.extend(
                self.sorted[lo..]
                    .iter()
                    .take_while(|&&t| self.piece(t) == prefix),
            );
        }
    }
}

/// What a token does to the remaining typed bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum Step<'a> {
    /// The token covers all typed bytes; these bytes follow them.
    Done(&'a [u8]),
    /// The token is a prefix of the typed bytes; this much is still to match.
    Partial(&'a [u8]),
    /// The token contradicts the typed bytes.
    Disagrees,
}

pub fn step<'a>(rem: &'a [u8], piece: &'a [u8]) -> Step<'a> {
    if let Some(rest) = piece.strip_prefix(rem) {
        Step::Done(rest)
    } else if !piece.is_empty()
        && let Some(rest) = rem.strip_prefix(piece)
    {
        Step::Partial(rest)
    } else {
        Step::Disagrees
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn tail_is_the_partial_word_with_its_space() {
        assert_eq!(
            &"I would like to sch"[tail_start("I would like to sch")..],
            " sch"
        );
        assert_eq!(&"Hello"[tail_start("Hello")..], "Hello");
        assert_eq!(&"to "[tail_start("to ")..], " ");
        assert_eq!(&"Hi,\n\n"[tail_start("Hi,\n\n")..], "\n\n");
        assert_eq!(&"Really?!"[tail_start("Really?!")..], "?!");
        assert_eq!(tail_start(""), 0);
        assert_eq!(&"naïve caf"[tail_start("naïve caf")..], " caf");
        assert_eq!(&"日本語"[tail_start("日本語")..], "日本語");
    }

    #[test]
    fn long_runs_heal_only_their_end() {
        let before = "see aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tail = &before[tail_start(before)..];
        assert!(tail.len() <= MAX_TAIL);
        assert!(tail.chars().all(|c| c == 'a'));
    }

    fn table(pieces: &[&str]) -> PieceTable {
        PieceTable::new(
            pieces.len(),
            |t, out| out.extend_from_slice(pieces[t as usize].as_bytes()),
            |t| t != 0,
        )
    }

    #[test]
    fn agreeing_finds_extensions_and_prefixes() {
        let t = table(&[
            "<ctl>",
            " s",
            " sch",
            " school",
            " schedule",
            " sc",
            " t",
            "s",
            " ",
            "ch",
            "",
        ]);
        let mut out = Vec::new();
        t.agreeing(b" sch", &mut out);
        out.sort_unstable();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 8]);
        out.clear();
        t.agreeing(b"", &mut out);
        assert_eq!(
            out.len(),
            9,
            "empty remainder allows every generatable token"
        );
    }

    #[test]
    fn step_advances_through_the_typed_bytes() {
        assert_eq!(step(b" sch", b" schedule"), Step::Done(b"edule"));
        assert_eq!(step(b" sch", b" sch"), Step::Done(b""));
        assert_eq!(step(b" sch", b" s"), Step::Partial(b"ch"));
        assert_eq!(step(b" sch", b" t"), Step::Disagrees);
        assert_eq!(step(b" sch", b""), Step::Disagrees);
        assert_eq!(step(b"", b"x"), Step::Done(b"x"));
    }

    proptest! {
        #[test]
        fn tail_start_is_a_char_boundary_within_cap(before in "\\PC{0,40}") {
            let start = tail_start(&before);
            prop_assert!(before.is_char_boundary(start));
            prop_assert!(before.len() - start <= MAX_TAIL);
            prop_assert!(start < before.len() || before.is_empty());
        }

        #[test]
        fn agreeing_matches_brute_force(
            pieces in proptest::collection::vec("[ab ]{0,4}", 1..40),
            rem in "[ab ]{0,4}",
        ) {
            let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
            let t = PieceTable::new(refs.len(), |i, out| out.extend_from_slice(refs[i as usize].as_bytes()), |_| true);
            let mut got = Vec::new();
            t.agreeing(rem.as_bytes(), &mut got);
            got.sort_unstable();
            let want: Vec<Token> = (0..refs.len() as Token)
                .filter(|&i| !refs[i as usize].is_empty())
                .filter(|&i| step(rem.as_bytes(), refs[i as usize].as_bytes()) != Step::Disagrees)
                .collect();
            prop_assert_eq!(got, want);
        }
    }
}
