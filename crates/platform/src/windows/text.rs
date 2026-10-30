//! Text at the Windows boundary: trimming what UI Automation returns to the
//! contract's limits, splitting a UTF-16 selection, counting caret stops for
//! replacements, and turning text into Unicode keystrokes.

use unicode_segmentation::UnicodeSegmentation;

/// The end of `s`, at most `max` bytes, cut on a char boundary, and whether
/// anything was cut.
pub fn tail(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    (&s[start..], true)
}

/// The start of `s`, at most `max` bytes, cut on a char boundary.
pub fn head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The byte offset in `s` of UTF-16 offset `units`, clamped to the end. An
/// offset inside a surrogate pair moves to the start of its character.
pub fn byte_offset(s: &str, units: usize) -> usize {
    let mut seen = 0;
    for (i, c) in s.char_indices() {
        seen += c.len_utf16();
        if seen > units {
            return i;
        }
    }
    s.len()
}

/// Splits `value` at a selection given in UTF-16 offsets (as Win32 edit
/// controls report it) into before, selected and after.
pub fn split_utf16(value: &str, start: usize, end: usize) -> (&str, &str, &str) {
    let (a, b) = (
        byte_offset(value, start.min(end)),
        byte_offset(value, start.max(end)),
    );
    (&value[..a], &value[a..b], &value[b..])
}

/// How many times the caret moves to cross `s`: user-perceived characters,
/// with `\r\n` as one.
pub fn caret_stops(s: &str) -> usize {
    s.graphemes(true).count()
}

/// One keystroke of typed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stroke {
    /// A UTF-16 unit sent with `KEYEVENTF_UNICODE`.
    Unit(u16),
    /// A line break, sent as Shift+Enter so that chat apps add a line
    /// instead of sending the message.
    NewLine,
}

/// The keystrokes that type `text`.
pub fn strokes(text: &str) -> impl Iterator<Item = Stroke> + '_ {
    let mut after_cr = false;
    text.chars().flat_map(move |c| {
        let units = match c {
            '\n' if after_cr => Units::None,
            '\n' | '\r' => Units::NewLine,
            c => {
                let mut buf = [0u16; 2];
                match *c.encode_utf16(&mut buf) {
                    [u] => Units::One(u),
                    [hi, lo] => Units::Two(hi, lo),
                    _ => Units::None,
                }
            }
        };
        after_cr = c == '\r';
        units
    })
}

enum Units {
    None,
    NewLine,
    One(u16),
    Two(u16, u16),
}

impl Iterator for Units {
    type Item = Stroke;
    fn next(&mut self) -> Option<Stroke> {
        let (out, rest) = match std::mem::replace(self, Units::None) {
            Units::None => return None,
            Units::NewLine => (Stroke::NewLine, Units::None),
            Units::One(u) => (Stroke::Unit(u), Units::None),
            Units::Two(hi, lo) => (Stroke::Unit(hi), Units::One(lo)),
        };
        *self = rest;
        Some(out)
    }
}

/// Longest text typed as keystrokes; longer text is pasted, which is one
/// keystroke for the app and cannot be interleaved with the user's typing.
pub const TYPE_MAX_UNITS: usize = 200;

/// Whether to insert by pasting instead of typing: long or multi-line text
/// (typed line breaks would trigger send or submit in many apps).
pub fn paste_instead(text: &str) -> bool {
    text.contains(['\n', '\r']) || text.encode_utf16().count() > TYPE_MAX_UNITS
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn strokes_split_surrogates_and_fold_line_breaks() {
        let s: Vec<_> = strokes("a😀\r\nb\n").collect();
        let mut pair = [0u16; 2];
        '😀'.encode_utf16(&mut pair);
        assert_eq!(
            s,
            [
                Stroke::Unit(u16::from(b'a')),
                Stroke::Unit(pair[0]),
                Stroke::Unit(pair[1]),
                Stroke::NewLine,
                Stroke::Unit(u16::from(b'b')),
                Stroke::NewLine,
            ]
        );
    }

    #[test]
    fn caret_stops_count_clusters() {
        assert_eq!(caret_stops("e\u{301}x"), 2);
        assert_eq!(caret_stops("a\r\nb"), 3);
        assert_eq!(caret_stops("👍🏽!"), 2);
    }

    #[test]
    fn paste_for_long_or_multiline_text() {
        assert!(!paste_instead("a short reply"));
        assert!(paste_instead("two\nlines"));
        assert!(paste_instead(&"x".repeat(TYPE_MAX_UNITS + 1)));
    }

    #[test]
    fn split_utf16_counts_surrogate_pairs() {
        // "a😀b": 😀 is two UTF-16 units.
        assert_eq!(split_utf16("a😀b", 1, 3), ("a", "😀", "b"));
        assert_eq!(split_utf16("a😀b", 4, 4), ("a😀b", "", ""));
        // Reversed anchors and offsets inside the pair.
        assert_eq!(split_utf16("a😀b", 2, 1), ("a", "", "😀b"));
    }

    proptest! {
        #[test]
        fn trimming_stays_on_boundaries_and_within_limits(s in "\\PC{0,64}", max in 0usize..80) {
            let (t, cut) = tail(&s, max);
            prop_assert!(t.len() <= max && s.ends_with(t));
            prop_assert_eq!(cut, t.len() < s.len());
            let h = head(&s, max);
            prop_assert!(h.len() <= max && s.starts_with(h));
            // Nothing more fits: the next char would exceed the limit.
            prop_assert!(s[h.len()..].chars().next().is_none_or(|c| h.len() + c.len_utf8() > max));
        }

        #[test]
        fn byte_offsets_round_trip_utf16(s in "\\PC{0,32}") {
            let mut units = 0;
            for (i, c) in s.char_indices() {
                prop_assert_eq!(byte_offset(&s, units), i);
                units += c.len_utf16();
            }
            prop_assert_eq!(byte_offset(&s, units), s.len());
        }

        #[test]
        fn strokes_decode_back_to_the_text(s in "[^\r\n]{0,32}") {
            let units: Vec<u16> = strokes(&s)
                .map(|k| match k { Stroke::Unit(u) => u, Stroke::NewLine => unreachable!() })
                .collect();
            prop_assert_eq!(String::from_utf16(&units).unwrap(), s);
        }
    }
}
