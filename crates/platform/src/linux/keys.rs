//! Keyboard state from the grab's keymap, and the translation of each key
//! event into the gate's [`KeyIn`].

use std::os::fd::OwnedFd;

use ghostcaret_engine::io::{Key, Modifier, Mods};
use xkbcommon::xkb;

use crate::gate::{KeyIn, Typed};

/// The modifier a keysym names. The keysym, not the physical key, decides:
/// with `altwin:swap_alt_win` the key labelled Win is Alt. AltGr counts as
/// the right Alt.
fn modifier(sym: u32) -> Option<Modifier> {
    use xkb::keysyms as k;
    Some(match sym {
        k::KEY_Control_L => Modifier::LeftCtrl,
        k::KEY_Control_R => Modifier::RightCtrl,
        k::KEY_Shift_L => Modifier::LeftShift,
        k::KEY_Shift_R => Modifier::RightShift,
        k::KEY_Alt_L | k::KEY_Meta_L => Modifier::LeftAlt,
        k::KEY_Alt_R | k::KEY_Meta_R | k::KEY_ISO_Level3_Shift => Modifier::RightAlt,
        k::KEY_Super_L | k::KEY_Hyper_L => Modifier::LeftMeta,
        k::KEY_Super_R | k::KEY_Hyper_R => Modifier::RightMeta,
        _ => return None,
    })
}

/// The bindable key a keysym names, if it is not a character.
fn named(sym: u32) -> Option<Key> {
    use xkb::keysyms as k;
    Some(match sym {
        k::KEY_Tab | k::KEY_ISO_Left_Tab | k::KEY_KP_Tab => Key::Tab,
        k::KEY_Escape => Key::Escape,
        k::KEY_Return | k::KEY_KP_Enter | k::KEY_ISO_Enter => Key::Enter,
        k::KEY_BackSpace => Key::Backspace,
        k::KEY_space | k::KEY_KP_Space => Key::Space,
        k::KEY_Left | k::KEY_KP_Left => Key::Left,
        k::KEY_Right | k::KEY_KP_Right => Key::Right,
        k::KEY_Up | k::KEY_KP_Up => Key::Up,
        k::KEY_Down | k::KEY_KP_Down => Key::Down,
        k::KEY_Home | k::KEY_KP_Home => Key::Home,
        k::KEY_End | k::KEY_KP_End => Key::End,
        k::KEY_Page_Up | k::KEY_KP_Page_Up => Key::PageUp,
        k::KEY_Page_Down | k::KEY_KP_Page_Down => Key::PageDown,
        k::KEY_F1..=k::KEY_F35 => Key::F((sym - k::KEY_F1 + 1) as u8),
        _ => return None,
    })
}

/// Keysyms that start a dead-key or compose sequence, whose result the gate
/// cannot know.
fn composes(sym: u32) -> bool {
    use xkb::keysyms as k;
    matches!(
        sym,
        k::KEY_dead_grave..=k::KEY_dead_greek | k::KEY_Multi_key
    )
}

/// Translates a key into what the gate needs to know.
pub(crate) fn key_in(code: u32, down: bool, time_ms: u32, sym: u32, ch: u32, mods: Mods) -> KeyIn {
    let modifier = modifier(sym);
    let shortcut = mods.ctrl || mods.alt || mods.meta;
    let (key, text) = if let Some(key) = named(sym) {
        let text = if key == Key::Space && !shortcut {
            Typed::Char(' ')
        } else {
            Typed::None
        };
        (Some(key), text)
    } else if let Some(c) = char::from_u32(ch).filter(|c| !c.is_control()) {
        (
            Some(Key::Char(c)),
            if shortcut {
                Typed::None
            } else {
                Typed::Char(c)
            },
        )
    } else if composes(sym) && !shortcut {
        (None, Typed::Unknown)
    } else {
        (None, Typed::None)
    };
    KeyIn {
        code,
        down,
        time_ms,
        key,
        text,
        modifier,
        mods,
    }
}

/// The grab's keymap and the modifier state the compositor reports.
pub(crate) struct Keyboard {
    state: xkb::State,
    ctrl: xkb::ModIndex,
    alt: xkb::ModIndex,
    shift: xkb::ModIndex,
    meta: xkb::ModIndex,
    /// The keymap text, to skip recompiling an identical keymap.
    source: Vec<u8>,
}

impl Keyboard {
    /// Compiles an xkb v1 keymap from the grab's file descriptor.
    pub fn new(fd: OwnedFd, size: u32, previous: Option<Keyboard>) -> Option<Keyboard> {
        let file = std::fs::File::from(fd);
        // SAFETY: the compositor hands over a sealed or private keymap file of
        // `size` bytes; a private read-only copy is mapped, so later writes by
        // the compositor cannot change what xkb reads.
        let map = unsafe {
            memmap2::MmapOptions::new()
                .len(size as usize)
                .map_copy_read_only(&file)
        }
        .ok()?;
        // The text is NUL-terminated.
        let source = map.split(|&b| b == 0).next().unwrap_or(&map);
        if let Some(previous) = previous.filter(|p| p.source == source) {
            return Some(previous);
        }
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let text = std::str::from_utf8(source).ok()?;
        let keymap = xkb::Keymap::new_from_string(
            &context,
            text.to_owned(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::COMPILE_NO_FLAGS,
        )?;
        let index = |name| keymap.mod_get_index(name);
        Some(Keyboard {
            ctrl: index(xkb::MOD_NAME_CTRL),
            alt: index(xkb::MOD_NAME_ALT),
            shift: index(xkb::MOD_NAME_SHIFT),
            meta: index(xkb::MOD_NAME_LOGO),
            state: xkb::State::new(&keymap),
            source: source.to_vec(),
        })
    }

    pub fn update_mask(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        self.state
            .update_mask(depressed, latched, locked, 0, 0, group);
    }

    /// The modifiers in effect.
    pub fn mods(&self) -> Mods {
        let on = |i| self.state.mod_index_is_active(i, xkb::STATE_MODS_EFFECTIVE);
        Mods {
            ctrl: on(self.ctrl),
            alt: on(self.alt),
            shift: on(self.shift),
            meta: on(self.meta),
        }
    }

    /// The keysym of an evdev key at the current level, and the character
    /// that keysym stands for (Shift and AltGr applied, Ctrl not), or 0.
    pub fn sym(&self, code: u32) -> (u32, u32) {
        let sym = self.state.key_get_one_sym(xkb::Keycode::new(code + 8));
        (sym.raw(), xkb::keysym_to_utf32(sym))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xkb::keysyms as k;

    const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
    };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };

    #[test]
    fn characters_type_unless_a_shortcut_modifier_is_held() {
        let k = key_in(41, true, 0, k::KEY_grave, '`' as u32, NONE);
        assert_eq!((k.key, k.text), (Some(Key::Char('`')), Typed::Char('`')));
        let k = key_in(30, true, 0, k::KEY_a, 'a' as u32, CTRL);
        assert_eq!((k.key, k.text), (Some(Key::Char('a')), Typed::None));
    }

    #[test]
    fn named_keys_type_nothing_except_space() {
        let tab = key_in(15, true, 0, k::KEY_Tab, '\t' as u32, NONE);
        assert_eq!((tab.key, tab.text), (Some(Key::Tab), Typed::None));
        let space = key_in(57, true, 0, k::KEY_space, ' ' as u32, NONE);
        assert_eq!(
            (space.key, space.text),
            (Some(Key::Space), Typed::Char(' '))
        );
        let f5 = key_in(63, true, 0, k::KEY_F5, 0, NONE);
        assert_eq!(f5.key, Some(Key::F(5)));
    }

    #[test]
    fn dead_keys_are_unpredictable_and_modifiers_are_named() {
        let dead = key_in(40, true, 0, k::KEY_dead_acute, 0, NONE);
        assert_eq!((dead.key, dead.text), (None, Typed::Unknown));
        let ralt = key_in(100, true, 0, k::KEY_Alt_R, 0, NONE);
        assert_eq!(
            (ralt.modifier, ralt.key, ralt.text),
            (Some(Modifier::RightAlt), None, Typed::None)
        );
    }

    /// Regression: with `altwin:swap_alt_win` the physical Alt key sends
    /// Super, so naming modifiers by evdev code made Alt+→ a Super chord and
    /// the Win key's Alt a Meta tap.
    #[test]
    fn modifiers_follow_the_keysym_not_the_key_position() {
        let physical_alt = key_in(56, true, 0, k::KEY_Super_L, 0, NONE);
        assert_eq!(physical_alt.modifier, Some(Modifier::LeftMeta));
        let physical_win = key_in(125, true, 0, k::KEY_Alt_L, 0, NONE);
        assert_eq!(physical_win.modifier, Some(Modifier::LeftAlt));
        let altgr = key_in(100, true, 0, k::KEY_ISO_Level3_Shift, 0, NONE);
        assert_eq!(altgr.modifier, Some(Modifier::RightAlt));
    }
}
