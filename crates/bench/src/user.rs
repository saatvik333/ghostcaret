//! The simulated user: ideal but honest.
//!
//! The user knows exactly what they are going to write (the rest of the
//! sample) and accepts only text that is right as shown:
//!
//! 1. When the whole visible suggestion is what comes next, Take All (one
//!    key, `` ` ``).
//! 2. Otherwise, when the next Take Word unit is what comes next, Take Word
//!    (one key, Tab), then look again.
//! 3. Otherwise type the next character; a character the suggestion predicted
//!    types through it, as in the product's key gate.
//!
//! "What comes next" means the rest of the text starts with the suggested
//! text **and** the suggested text ends where the real text has a word unit
//! end or a boundary ([`ghostcaret_engine::text`]). So `see` is not accepted when the
//! user is about to write `seeing`: the suggestion predicted another word,
//! and the user would not accept a fragment of the wrong word.
//!
//! An accept that inserts a single character saves nothing (one key for one
//! character), so the user types it instead; the count of keystrokes is the
//! same either way.

use ghostcaret_engine::io::SuggestionView;
use ghostcaret_engine::text::{boundaries, word_units};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    TakeAll,
    TakeWord,
    Type,
}

/// Bytes of the real continuation scanned for unit ends and boundaries;
/// longer than any suggestion the stop rule allows.
const WINDOW: usize = 2048;

/// What the user does with `view` showing, `rest` still to write, and
/// `before` in the field.
pub fn decide(view: &SuggestionView, rest: &str, before: &str) -> Decision {
    let visible = view.visible();
    if visible.chars().nth(1).is_some() && matches(rest, visible, before) {
        return Decision::TakeAll;
    }
    let unit = first_unit(view);
    if unit.chars().nth(1).is_some() && matches(rest, unit, before) {
        return Decision::TakeWord;
    }
    Decision::Type
}

/// What Take Word would insert now.
pub fn first_unit(view: &SuggestionView) -> &str {
    &view.text[..view.word_end()]
}

/// Whether `suggested` is what the user writes next: `rest` starts with it
/// and it ends at a unit end or boundary of `rest`.
pub fn matches(rest: &str, suggested: &str, before: &str) -> bool {
    !suggested.is_empty() && rest.starts_with(suggested) && aligned(rest, suggested.len(), before)
}

fn aligned(rest: &str, end: usize, before: &str) -> bool {
    if end == rest.len() {
        return true;
    }
    let mut cut = rest.len().min(WINDOW);
    while !rest.is_char_boundary(cut) {
        cut -= 1;
    }
    let window = &rest[..cut];
    word_units(window).contains(&end) || boundaries(before, window).iter().any(|b| b.end == end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view with these Take Word unit ends, `shown` bytes visible.
    fn view(text: &str, shown: usize, words: &[usize]) -> SuggestionView {
        let v = SuggestionView {
            text: text.into(),
            shown,
            word_ends: words.into(),
            ..SuggestionView::default()
        };
        assert!(v.is_valid());
        v
    }

    #[test]
    fn whole_match_takes_all() {
        let v = view("see you tomorrow", 16, &[4, 8, 16]);
        assert_eq!(
            decide(&v, "see you tomorrow at nine", "I'll "),
            Decision::TakeAll
        );
    }

    #[test]
    fn partial_match_takes_words_then_types() {
        let mut v = view("see you tomorrow", 16, &[4, 8, 16]);
        let rest = "see you on Friday";
        assert_eq!(decide(&v, rest, "I'll "), Decision::TakeWord);
        v.take_word();
        assert_eq!(decide(&v, &rest[4..], "I'll see "), Decision::TakeWord);
        v.take_word();
        assert_eq!(v.visible(), "tomorrow");
        assert_eq!(decide(&v, &rest[8..], "I'll see you "), Decision::Type);
    }

    #[test]
    fn fragment_of_a_longer_word_is_not_a_match() {
        // Word mode shows "see"; the user writes "seeing".
        let v = view("see you", 3, &[4, 7]);
        assert_eq!(first_unit(&v), "see");
        assert!(!matches("seeing you", "see", "I'm "));
        assert_eq!(decide(&v, "seeing you", "I'm "), Decision::Type);
        // The same suggestion is right before a space or punctuation.
        assert!(matches("see you", "see", "I'll "));
        assert!(matches("see, you", "see", "I'll "));
        assert!(matches("see\nyou", "see", "I'll "));
    }

    #[test]
    fn unit_with_wrong_trailing_space_is_not_a_match() {
        assert!(!matches("see, you", "see ", "I'll "));
    }

    #[test]
    fn healed_word_completion_matches_its_rest() {
        // The caret is inside "schedule"; the suggestion finishes the word.
        assert!(matches("edule a call", "edule ", "Let's sch"));
        assert!(matches("edule a call", "edule a", "Let's sch"));
        assert!(!matches("edules", "edule", "two sch"));
    }

    #[test]
    fn end_of_text_counts_as_aligned() {
        assert!(matches("thanks", "thanks", "ok "));
        assert!(!matches("thanks", "thanks!", "ok "));
    }

    #[test]
    fn single_character_accepts_are_typed_instead() {
        let v = view(",", 1, &[1]);
        assert_eq!(decide(&v, ", then", "ok"), Decision::Type);
        let v = view("a cat", 5, &[2, 5]);
        // First unit "a " has two characters and matches.
        assert_eq!(decide(&v, "a dog", "saw "), Decision::TakeWord);
    }

    #[test]
    fn scripts_without_spaces_accept_per_grapheme() {
        // Han text: every grapheme is a unit, so a two-character prefix is
        // aligned.
        assert!(matches("你好世界", "你好", ""));
        let v = view("你好世界", 6, &[3, 6, 9, 12]);
        assert_eq!(decide(&v, "你好朋友", ""), Decision::TakeAll);
    }
}
