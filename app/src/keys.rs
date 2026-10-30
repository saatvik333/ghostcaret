//! Key binding checks: a binding may not clash with another action, with
//! ordinary typing, or with a shortcut the system reserves.

use ghostcaret_engine::io::{Bindings, Chord, Key, Mods, Trigger};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    TakeWord,
    TakeAll,
    ShowMore,
    NextAlternative,
    PrevAlternative,
    Dismiss,
    Reply,
}

impl Slot {
    fn label(self) -> &'static str {
        match self {
            Slot::TakeWord => "Take next word",
            Slot::TakeAll => "Take whole suggestion",
            Slot::ShowMore => "Show more",
            Slot::NextAlternative => "Next alternative",
            Slot::PrevAlternative => "Previous alternative",
            Slot::Dismiss => "Dismiss",
            Slot::Reply => "Reply & rewrite",
        }
    }
}

/// Why a binding was refused, attached to the slot that has to change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KeyProblem {
    pub slot: Slot,
    pub message: String,
}

fn slots(b: &Bindings) -> [(Slot, Option<Trigger>); 7] {
    let chord = |c: Option<Chord>| c.map(Trigger::Chord);
    [
        (Slot::TakeWord, chord(b.take_word)),
        (Slot::TakeAll, chord(b.take_all)),
        (Slot::ShowMore, chord(b.show_more)),
        (Slot::NextAlternative, chord(b.next_alternative)),
        (Slot::PrevAlternative, chord(b.prev_alternative)),
        (Slot::Dismiss, chord(b.dismiss)),
        (Slot::Reply, b.reply),
    ]
}

/// Every problem in `bindings`, in slot order. Empty when all are usable.
pub fn check(bindings: &Bindings) -> Vec<KeyProblem> {
    let all = slots(bindings);
    let mut problems = Vec::new();
    for (i, &(slot, trigger)) in all.iter().enumerate() {
        let Some(trigger) = trigger else { continue };
        if let Trigger::Chord(chord) = trigger
            && let Some(why) = reserved(chord)
        {
            problems.push(KeyProblem {
                slot,
                message: why.to_owned(),
            });
            continue;
        }
        if let Some(&(other, _)) = all[..i].iter().find(|(_, t)| *t == Some(trigger)) {
            problems.push(KeyProblem {
                slot,
                message: format!("Already used by “{}”.", other.label()),
            });
        }
    }
    problems
}

const NONE: Mods = Mods {
    ctrl: false,
    alt: false,
    shift: false,
    meta: false,
};

/// Why `chord` cannot be bound, if it cannot.
fn reserved(chord: Chord) -> Option<&'static str> {
    let Chord { key, mods } = chord;
    let typing = mods == NONE
        || mods
            == Mods {
                shift: true,
                ..NONE
            };
    if mods.meta {
        return Some("Shortcuts with the Super/Windows/Command key belong to the system.");
    }
    match key {
        Key::Char(c) if typing && (c.is_alphanumeric() || c.is_whitespace()) => {
            Some("This key types text; choose a symbol key or add Ctrl or Alt.")
        }
        Key::Space | Key::Enter | Key::Backspace if typing => {
            Some("This key is needed for typing.")
        }
        Key::Tab
            if mods == Mods { alt: true, ..NONE }
                || mods
                    == Mods {
                        alt: true,
                        shift: true,
                        ..NONE
                    } =>
        {
            Some("Alt+Tab switches windows.")
        }
        Key::F(4) if mods.alt => Some("Alt+F4 closes windows."),
        Key::Space if mods == Mods { alt: true, ..NONE } => {
            Some("Alt+Space opens the window menu.")
        }
        Key::Escape if mods.ctrl => Some("Ctrl+Esc belongs to the system."),
        Key::Char(c)
            if mods == Mods { ctrl: true, ..NONE }
                && "acfnpstvwxyz".contains(c.to_ascii_lowercase()) =>
        {
            Some("Apps use this shortcut (copy, paste, undo, save and similar).")
        }
        Key::Char(c)
            if mods
                == Mods {
                    ctrl: true,
                    shift: true,
                    ..NONE
                }
                && "tvz".contains(c.to_ascii_lowercase()) =>
        {
            Some("Apps use this shortcut (paste, redo, reopen tab).")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostcaret_engine::io::Modifier;

    fn chord(key: Key, mods: Mods) -> Option<Chord> {
        Some(Chord { key, mods })
    }

    #[test]
    fn defaults_are_accepted() {
        assert_eq!(check(&Bindings::default()), vec![]);
    }

    #[test]
    fn duplicate_is_reported_on_the_later_slot() {
        let b = Bindings {
            show_more: chord(Key::Tab, NONE),
            ..Bindings::default()
        };
        let problems = check(&b);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].slot, Slot::ShowMore);
        assert!(problems[0].message.contains("Take next word"));
    }

    #[test]
    fn reply_chord_conflicts_with_chords() {
        let b = Bindings {
            reply: Some(Trigger::Chord(Chord {
                key: Key::Escape,
                mods: NONE,
            })),
            ..Bindings::default()
        };
        assert_eq!(check(&b)[0].slot, Slot::Reply);
    }

    #[test]
    fn typing_and_system_keys_are_refused() {
        let alt = Mods { alt: true, ..NONE };
        let ctrl = Mods { ctrl: true, ..NONE };
        for c in [
            chord(Key::Char('a'), NONE),
            chord(
                Key::Char('A'),
                Mods {
                    shift: true,
                    ..NONE
                },
            ),
            chord(Key::Char('7'), NONE),
            chord(Key::Space, NONE),
            chord(Key::Enter, NONE),
            chord(Key::Tab, alt),
            chord(Key::F(4), alt),
            chord(Key::Char('c'), ctrl),
            chord(Key::Char('V'), ctrl),
            chord(Key::Right, Mods { meta: true, ..NONE }),
        ] {
            let b = Bindings {
                take_all: c,
                ..Bindings::default()
            };
            assert_eq!(check(&b).len(), 1, "{c:?} should be refused");
        }
    }

    #[test]
    fn useful_alternatives_are_allowed() {
        for c in [
            chord(Key::Right, NONE),
            chord(Key::Char(';'), NONE),
            chord(Key::Enter, Mods { ctrl: true, ..NONE }),
            chord(Key::Char('j'), Mods { alt: true, ..NONE }),
            chord(Key::F(8), NONE),
        ] {
            let b = Bindings {
                take_all: c,
                ..Bindings::default()
            };
            assert_eq!(check(&b), vec![], "{c:?} should be allowed");
        }
    }

    #[test]
    fn modifier_taps_are_allowed_and_unbound_slots_ignored() {
        let b = Bindings {
            reply: Some(Trigger::ModifierTap(Modifier::RightCtrl)),
            next_alternative: None,
            prev_alternative: None,
            ..Bindings::default()
        };
        assert_eq!(check(&b), vec![]);
    }
}
