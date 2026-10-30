//! The focused field as the input-method protocol describes it, and its
//! conversion to [`FieldSnapshot`] parts.

use oft_engine::io::{FieldKind, MAX_AFTER, MAX_BEFORE};

/// Clients may send at most this many bytes of surrounding text; text this
/// long is almost certainly a window onto a longer field.
const PROTOCOL_MAX: usize = 4000;

// zwp_text_input_v3.content_hint bits.
const HINT_HIDDEN_TEXT: u32 = 0x40;
const HINT_SENSITIVE_DATA: u32 = 0x80;
const HINT_MULTILINE: u32 = 0x200;

// zwp_text_input_v3.content_purpose values.
const PURPOSE_DIGITS: u32 = 2;
const PURPOSE_NUMBER: u32 = 3;
const PURPOSE_PHONE: u32 = 4;
const PURPOSE_URL: u32 = 5;
const PURPOSE_EMAIL: u32 = 6;
const PURPOSE_PASSWORD: u32 = 8;
const PURPOSE_PIN: u32 = 9;
const PURPOSE_DATE: u32 = 10;
const PURPOSE_TIME: u32 = 11;
const PURPOSE_DATETIME: u32 = 12;
const PURPOSE_TERMINAL: u32 = 13;

/// Field kind and whether it is multi-line, from the content type.
pub(crate) fn kind(hint: u32, purpose: u32) -> (FieldKind, bool) {
    let kind = if hint & (HINT_HIDDEN_TEXT | HINT_SENSITIVE_DATA) != 0 {
        FieldKind::Password
    } else {
        match purpose {
            PURPOSE_PASSWORD => FieldKind::Password,
            PURPOSE_PIN => FieldKind::Pin,
            PURPOSE_URL => FieldKind::Url,
            PURPOSE_EMAIL => FieldKind::Email,
            // Dates and times are structured numeric entry.
            PURPOSE_DIGITS | PURPOSE_NUMBER | PURPOSE_DATE | PURPOSE_TIME | PURPOSE_DATETIME => {
                FieldKind::Number
            }
            PURPOSE_PHONE => FieldKind::Phone,
            PURPOSE_TERMINAL => FieldKind::Terminal,
            _ => FieldKind::Text,
        }
    };
    (kind, hint & HINT_MULTILINE != 0)
}

/// Surrounding text with the cursor and selection anchor as byte offsets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Surrounding {
    pub text: String,
    pub cursor: u32,
    pub anchor: u32,
}

/// The parts of a snapshot, borrowed from [`Surrounding`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Parts<'a> {
    pub before: &'a str,
    pub before_truncated: bool,
    pub selection: &'a str,
    pub after: &'a str,
}

impl Surrounding {
    /// Splits around the selection, capped to the snapshot limits. Offsets a
    /// client sends past the end or inside a character are moved back to the
    /// previous character boundary.
    pub fn parts(&self) -> Parts<'_> {
        let t = self.text.as_str();
        let cursor = floor_boundary(t, self.cursor as usize);
        let anchor = floor_boundary(t, self.anchor as usize);
        let (start, end) = (cursor.min(anchor), cursor.max(anchor));
        let mut from = start.saturating_sub(MAX_BEFORE);
        while !t.is_char_boundary(from) {
            from += 1;
        }
        let to = floor_boundary(t, end.saturating_add(MAX_AFTER));
        Parts {
            before: &t[from..start],
            before_truncated: from > 0 || t.len() >= PROTOCOL_MAX,
            selection: &t[start..end],
            after: &t[end..to],
        }
    }

    /// Text before the selection start, uncapped.
    pub fn full_before(&self) -> &str {
        let t = self.text.as_str();
        let start = floor_boundary(t, self.cursor.min(self.anchor) as usize);
        &t[..start]
    }
}

fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn kinds_from_content_type() {
        assert_eq!(kind(0, 0), (FieldKind::Text, false));
        assert_eq!(kind(HINT_MULTILINE, 0), (FieldKind::Text, true));
        assert_eq!(kind(0, PURPOSE_PASSWORD).0, FieldKind::Password);
        assert_eq!(kind(HINT_SENSITIVE_DATA, 0).0, FieldKind::Password);
        assert_eq!(kind(HINT_HIDDEN_TEXT, PURPOSE_EMAIL).0, FieldKind::Password);
        assert_eq!(kind(0, PURPOSE_PIN).0, FieldKind::Pin);
        assert_eq!(kind(0, PURPOSE_URL).0, FieldKind::Url);
        assert_eq!(kind(0, PURPOSE_EMAIL).0, FieldKind::Email);
        assert_eq!(kind(0, PURPOSE_DIGITS).0, FieldKind::Number);
        assert_eq!(kind(0, PURPOSE_DATE).0, FieldKind::Number);
        assert_eq!(kind(0, PURPOSE_PHONE).0, FieldKind::Phone);
        assert_eq!(kind(0, PURPOSE_TERMINAL).0, FieldKind::Terminal);
    }

    #[test]
    fn selection_is_ordered_whichever_end_the_cursor_is() {
        let s = Surrounding {
            text: "one two three".into(),
            cursor: 7,
            anchor: 4,
        };
        let p = s.parts();
        assert_eq!((p.before, p.selection, p.after), ("one ", "two", " three"));
        let s = Surrounding {
            anchor: 7,
            cursor: 4,
            ..s
        };
        assert_eq!(s.parts().selection, "two");
    }

    #[test]
    fn offsets_inside_a_character_move_back() {
        let s = Surrounding {
            text: "héllo".into(),
            cursor: 2,
            anchor: 2,
        };
        assert_eq!(s.parts().before, "h");
        let s = Surrounding {
            text: "hi".into(),
            cursor: 99,
            anchor: 99,
        };
        assert_eq!(s.parts().before, "hi");
    }

    proptest! {
        #[test]
        fn parts_are_capped_on_char_boundaries(text in "\\PC{0,3000}", c in 0u32..13000, a in 0u32..13000) {
            let s = Surrounding { text, cursor: c, anchor: a };
            let p = s.parts();
            prop_assert!(p.before.len() <= MAX_BEFORE);
            prop_assert!(p.after.len() <= MAX_AFTER);
            // The pieces are contiguous slices of the text.
            let joined = format!("{}{}{}", p.before, p.selection, p.after);
            prop_assert!(s.text.contains(&joined));
            prop_assert!(s.full_before().ends_with(p.before));
            let trimmed = s.full_before().len() > p.before.len();
            prop_assert_eq!(p.before_truncated, trimmed || s.text.len() >= PROTOCOL_MAX);
        }
    }
}
