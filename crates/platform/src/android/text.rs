//! Conversions at the JNI boundary. Kotlin counts UTF-16 units; the engine
//! counts UTF-8 bytes. Each value is converted once, here.
//!
//! The integer codes mirror `io.github.saatvik333.ghostcaret.ime.Codes` in the keyboard.

use oft_engine::io::{
    Action, CardKey, Chord, DismissReason, FieldKind, Key, MAX_AFTER, MAX_BEFORE, Modifier, Trigger,
};

/// UTF-16 offsets of ascending UTF-8 byte offsets into `text`, in one pass.
/// Every offset must be a char boundary of `text`.
pub(crate) fn utf16_offsets(text: &str, offsets: &[usize]) -> Vec<i32> {
    let mut out = Vec::with_capacity(offsets.len());
    let mut chars = text.char_indices().peekable();
    let mut units = 0usize;
    for &target in offsets {
        while let Some(&(at, c)) = chars.peek() {
            if at >= target {
                break;
            }
            units += c.len_utf16();
            chars.next();
        }
        out.push(i32::try_from(units).unwrap_or(i32::MAX));
    }
    out
}

/// Keeps at most [`MAX_BEFORE`] bytes from the end of `before`, cut on a char
/// boundary. Returns whether anything was cut.
pub(crate) fn trim_before(before: &mut String) -> bool {
    if before.len() <= MAX_BEFORE {
        return false;
    }
    let mut cut = before.len() - MAX_BEFORE;
    while !before.is_char_boundary(cut) {
        cut += 1;
    }
    before.drain(..cut);
    true
}

/// Keeps at most [`MAX_AFTER`] bytes from the start of `after`, cut on a char
/// boundary.
pub(crate) fn trim_after(after: &mut String) {
    if after.len() <= MAX_AFTER {
        return;
    }
    let mut cut = MAX_AFTER;
    while !after.is_char_boundary(cut) {
        cut -= 1;
    }
    after.truncate(cut);
}

pub(crate) fn field_kind(code: i32) -> FieldKind {
    match code {
        0 => FieldKind::Text,
        1 => FieldKind::Password,
        2 => FieldKind::Pin,
        3 => FieldKind::Search,
        4 => FieldKind::Url,
        5 => FieldKind::Email,
        6 => FieldKind::Number,
        7 => FieldKind::Phone,
        8 => FieldKind::Terminal,
        9 => FieldKind::Code,
        _ => FieldKind::Unknown,
    }
}

pub(crate) fn dismiss_reason(code: i32) -> Option<DismissReason> {
    Some(match code {
        0 => DismissReason::Escape,
        1 => DismissReason::Mismatch,
        2 => DismissReason::CaretMoved,
        3 => DismissReason::FocusLost,
        4 => DismissReason::Clicked,
        _ => return None,
    })
}

/// Action codes, also the order of [`binding_slots`].
pub(crate) const ACTIONS: [Action; 7] = [
    Action::TakeWord,
    Action::TakeAll,
    Action::ShowMore,
    Action::NextAlternative,
    Action::PrevAlternative,
    Action::Dismiss,
    Action::FlowDraft,
];

pub(crate) fn action(code: i32) -> Option<Action> {
    usize::try_from(code).ok().and_then(|i| ACTIONS.get(i).copied())
}

pub(crate) fn card_key(code: i32, value: i32) -> Option<CardKey> {
    Some(match code {
        0 => CardKey::Up,
        1 => CardKey::Down,
        2 => CardKey::Pick(u8::try_from(value).ok().filter(|d| (1..=9).contains(d))?),
        3 => CardKey::Enter,
        4 => CardKey::Escape,
        5 => CardKey::Tab,
        6 => CardKey::Backspace,
        7 => CardKey::Char(char::from_u32(u32::try_from(value).ok()?)?),
        _ => return None,
    })
}

/// A binding as three integers for Kotlin: kind (-1 unbound, 0 key, 1
/// character, 2 modifier tap), value (key code, code point or modifier code)
/// and a modifier mask (1 ctrl, 2 alt, 4 shift, 8 meta).
pub(crate) type Slot = [i32; 3];

pub(crate) const UNBOUND: Slot = [-1, 0, 0];

pub(crate) fn trigger_slot(trigger: &Trigger) -> Slot {
    match trigger {
        Trigger::Chord(chord) => chord_slot(chord),
        Trigger::ModifierTap(modifier) => [2, modifier_code(*modifier), 0],
    }
}

pub(crate) fn chord_slot(chord: &Chord) -> Slot {
    let m = chord.mods;
    let mask = i32::from(m.ctrl) | i32::from(m.alt) << 1 | i32::from(m.shift) << 2 | i32::from(m.meta) << 3;
    let (kind, value) = match chord.key {
        Key::Char(c) => (1, c as i32),
        Key::Tab => (0, 0),
        Key::Escape => (0, 1),
        Key::Enter => (0, 2),
        Key::Backspace => (0, 3),
        Key::Space => (0, 4),
        Key::Left => (0, 5),
        Key::Right => (0, 6),
        Key::Up => (0, 7),
        Key::Down => (0, 8),
        Key::Home => (0, 9),
        Key::End => (0, 10),
        Key::PageUp => (0, 11),
        Key::PageDown => (0, 12),
        Key::F(n) => (0, 100 + i32::from(n)),
    };
    [kind, value, mask]
}

fn modifier_code(modifier: Modifier) -> i32 {
    match modifier {
        Modifier::LeftAlt => 0,
        Modifier::RightAlt => 1,
        Modifier::LeftCtrl => 2,
        Modifier::RightCtrl => 3,
        Modifier::LeftShift => 4,
        Modifier::RightShift => 5,
        Modifier::LeftMeta => 6,
        Modifier::RightMeta => 7,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn boundaries(text: &str) -> Vec<usize> {
        text.char_indices().map(|(i, _)| i).chain([text.len()]).collect()
    }

    #[test]
    fn offsets_count_surrogate_pairs_as_two_units() {
        let text = "a😀é";
        assert_eq!(utf16_offsets(text, &[0, 1, 5, 7]), vec![0, 1, 3, 4]);
    }

    #[test]
    fn trimming_keeps_char_boundaries() {
        let mut before = "é".repeat(MAX_BEFORE);
        assert!(trim_before(&mut before));
        assert!(before.len() <= MAX_BEFORE && before.chars().all(|c| c == 'é'));
        let mut after = "😀".repeat(MAX_AFTER);
        trim_after(&mut after);
        assert!(after.len() <= MAX_AFTER && after.chars().all(|c| c == '😀'));
    }

    proptest! {
        #[test]
        fn offsets_match_encoding_of_each_prefix(text in "\\PC{0,40}", picks in proptest::collection::vec(any::<prop::sample::Index>(), 0..8)) {
            let all = boundaries(&text);
            let mut offsets: Vec<usize> = picks.iter().map(|i| all[i.index(all.len())]).collect();
            offsets.sort_unstable();
            offsets.dedup();
            let expected: Vec<i32> = offsets.iter().map(|&o| text[..o].encode_utf16().count() as i32).collect();
            prop_assert_eq!(utf16_offsets(&text, &offsets), expected);
        }

        #[test]
        fn trim_before_keeps_the_longest_fitting_end(text in "\\PC{0,3000}") {
            let mut kept = text.repeat(4);
            let original = kept.clone();
            let cut = trim_before(&mut kept);
            prop_assert_eq!(cut, original.len() > MAX_BEFORE);
            prop_assert!(original.ends_with(&kept));
            prop_assert!(kept.len() <= MAX_BEFORE);
            if cut {
                prop_assert!(kept.len() + 3 >= MAX_BEFORE);
            }
        }

        #[test]
        fn trim_after_keeps_the_longest_fitting_start(text in "\\PC{0,1000}") {
            let mut kept = text.repeat(4);
            let original = kept.clone();
            trim_after(&mut kept);
            prop_assert!(original.starts_with(&kept));
            prop_assert!(kept.len() <= MAX_AFTER);
            prop_assert!(kept.len() == original.len() || kept.len() + 3 >= MAX_AFTER);
        }
    }
}
