//! Windows virtual keys as the gate sees them: key names, modifier tracking
//! (including AltGr's synthetic Ctrl), and the per-layout character table
//! the hook reads instead of asking the layout on the key path.
//!
//! Pure; the table is filled by the hook thread through `ToUnicodeEx`.

use oft_engine::io::{Key, Modifier, Mods};

use crate::gate::{KeyIn, Typed};

// Virtual-key codes from WinUser.h, kept here so the mapping builds and is
// tested on every host.
pub const VK_BACK: u8 = 0x08;
pub const VK_TAB: u8 = 0x09;
pub const VK_RETURN: u8 = 0x0D;
pub const VK_SHIFT: u8 = 0x10;
pub const VK_CONTROL: u8 = 0x11;
pub const VK_MENU: u8 = 0x12;
pub const VK_CAPITAL: u8 = 0x14;
pub const VK_ESCAPE: u8 = 0x1B;
pub const VK_SPACE: u8 = 0x20;
pub const VK_PRIOR: u8 = 0x21;
pub const VK_NEXT: u8 = 0x22;
pub const VK_END: u8 = 0x23;
pub const VK_HOME: u8 = 0x24;
pub const VK_LEFT: u8 = 0x25;
pub const VK_UP: u8 = 0x26;
pub const VK_RIGHT: u8 = 0x27;
pub const VK_DOWN: u8 = 0x28;
pub const VK_LWIN: u8 = 0x5B;
pub const VK_RWIN: u8 = 0x5C;
pub const VK_F1: u8 = 0x70;
pub const VK_F24: u8 = 0x87;
pub const VK_LSHIFT: u8 = 0xA0;
pub const VK_RSHIFT: u8 = 0xA1;
pub const VK_LCONTROL: u8 = 0xA2;
pub const VK_RCONTROL: u8 = 0xA3;
pub const VK_LMENU: u8 = 0xA4;
pub const VK_RMENU: u8 = 0xA5;
/// A Unicode character injected with `KEYEVENTF_UNICODE`; the scan code holds
/// one UTF-16 unit.
pub const VK_PACKET: u8 = 0xE7;
/// Unassigned. Injected while Alt or Win is held so that releasing it does not
/// open the menu bar or the Start menu (the "menu mask" convention).
pub const VK_MASK: u8 = 0xE8;

/// AltGr sends a synthetic left Ctrl whose scan code has this bit set.
const SC_ALTGR_CTRL: u32 = 0x200;

/// The named key a virtual key is, if any.
pub fn named_key(vk: u8) -> Option<Key> {
    Some(match vk {
        VK_TAB => Key::Tab,
        VK_ESCAPE => Key::Escape,
        VK_RETURN => Key::Enter,
        VK_BACK => Key::Backspace,
        VK_SPACE => Key::Space,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_F1..=VK_F24 => Key::F(vk - VK_F1 + 1),
        _ => return None,
    })
}

/// The modifier a virtual key is. Low-level hooks report the sided codes.
pub fn modifier(vk: u8) -> Option<Modifier> {
    Some(match vk {
        VK_LSHIFT | VK_SHIFT => Modifier::LeftShift,
        VK_RSHIFT => Modifier::RightShift,
        VK_LCONTROL | VK_CONTROL => Modifier::LeftCtrl,
        VK_RCONTROL => Modifier::RightCtrl,
        VK_LMENU | VK_MENU => Modifier::LeftAlt,
        VK_RMENU => Modifier::RightAlt,
        VK_LWIN => Modifier::LeftMeta,
        VK_RWIN => Modifier::RightMeta,
        _ => return None,
    })
}

/// True for the left Ctrl that the system synthesises before AltGr.
pub fn is_altgr_ctrl(vk: u8, scan: u32) -> bool {
    vk == VK_LCONTROL && scan & SC_ALTGR_CTRL != 0
}

/// Which modifiers are held, from the hook's own key events (the hook runs
/// before the system updates its key state, so it cannot ask).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModState {
    held: u8,
    /// Right Alt is acting as AltGr (the layout synthesised a Ctrl for it).
    altgr: bool,
    altgr_ctrl_seen: bool,
    caps_lock: bool,
    caps_down: bool,
}

fn bit(m: Modifier) -> u8 {
    1 << m as u8
}

impl ModState {
    pub fn new(caps_lock: bool) -> ModState {
        ModState {
            caps_lock,
            ..ModState::default()
        }
    }

    /// Records a key event. Returns false for AltGr's synthetic Ctrl, which
    /// the gate must not see.
    pub fn record(&mut self, vk: u8, scan: u32, down: bool) -> bool {
        if is_altgr_ctrl(vk, scan) {
            self.altgr_ctrl_seen = down;
            return false;
        }
        if vk == VK_CAPITAL {
            if down && !self.caps_down {
                self.caps_lock = !self.caps_lock;
            }
            self.caps_down = down;
        }
        if let Some(m) = modifier(vk) {
            if down {
                self.held |= bit(m);
            } else {
                self.held &= !bit(m);
            }
            if m == Modifier::RightAlt {
                self.altgr = down && self.altgr_ctrl_seen;
            }
        }
        true
    }

    fn has(&self, m: Modifier) -> bool {
        self.held & bit(m) != 0
    }

    pub fn shift(&self) -> bool {
        self.has(Modifier::LeftShift) || self.has(Modifier::RightShift)
    }

    pub fn altgr(&self) -> bool {
        self.altgr
    }

    pub fn caps_lock(&self) -> bool {
        self.caps_lock
    }

    /// Modifiers in the gate's sense: AltGr is part of typing a character,
    /// not Ctrl+Alt.
    pub fn mods(&self) -> Mods {
        Mods {
            ctrl: self.has(Modifier::LeftCtrl) || self.has(Modifier::RightCtrl),
            alt: self.has(Modifier::LeftAlt) || (self.has(Modifier::RightAlt) && !self.altgr),
            shift: self.shift(),
            meta: self.has(Modifier::LeftMeta) || self.has(Modifier::RightMeta),
        }
    }
}

/// What a key produces under one layout and modifier state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Produced {
    None,
    Char(char),
    /// A dead key: it shows this character and changes the next one.
    Dead(char),
}

const STATES: usize = 8;
const DEAD: u32 = 1 << 31;

/// The characters every virtual key produces under one keyboard layout, for
/// each combination of Shift, AltGr and Caps Lock.
#[derive(Clone)]
pub struct CharTable {
    /// The layout handle the table was built for.
    pub layout: usize,
    cells: Box<[u32; 256 * STATES]>,
}

impl std::fmt::Debug for CharTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CharTable({:#x})", self.layout)
    }
}

fn state(shift: bool, altgr: bool, caps: bool) -> usize {
    usize::from(shift) | usize::from(altgr) << 1 | usize::from(caps) << 2
}

impl CharTable {
    /// An empty table: every key produces nothing until the layout is read.
    pub fn empty() -> CharTable {
        CharTable {
            layout: 0,
            cells: Box::new([0; 256 * STATES]),
        }
    }

    /// Builds the table by asking `produce(vk, shift, altgr, caps)` for every
    /// key and state.
    pub fn build(
        layout: usize,
        mut produce: impl FnMut(u8, bool, bool, bool) -> Produced,
    ) -> CharTable {
        let mut t = CharTable::empty();
        t.layout = layout;
        for vk in 0..=255u8 {
            if named_key(vk).is_some() || modifier(vk).is_some() || vk == VK_PACKET || vk == VK_MASK
            {
                continue;
            }
            for s in 0..STATES {
                let (shift, altgr, caps) = (s & 1 != 0, s & 2 != 0, s & 4 != 0);
                t.cells[usize::from(vk) * STATES + s] = match produce(vk, shift, altgr, caps) {
                    Produced::None => 0,
                    Produced::Char(c) => c as u32,
                    Produced::Dead(c) => c as u32 | DEAD,
                };
            }
        }
        t
    }

    pub fn get(&self, vk: u8, shift: bool, altgr: bool, caps: bool) -> Produced {
        let cell = self.cells[usize::from(vk) * STATES + state(shift, altgr, caps)];
        match char::from_u32(cell & !DEAD) {
            Some('\0') | None => Produced::None,
            Some(c) if cell & DEAD != 0 => Produced::Dead(c),
            Some(c) => Produced::Char(c),
        }
    }
}

/// The gate's code for a key: the virtual key, with extended keys (the
/// numpad Enter, the right-hand block) kept apart from their twins.
pub fn code(vk: u8, extended: bool) -> u32 {
    u32::from(vk) | u32::from(extended) << 8
}

/// Describes a physical (or foreign injected) key event for the gate.
/// `VK_PACKET` events are handled by the caller, which knows the character.
pub fn key_in(
    vk: u8,
    extended: bool,
    down: bool,
    time_ms: u32,
    mods: &ModState,
    table: &CharTable,
) -> KeyIn {
    let m = mods.mods();
    let shortcut = m.ctrl || m.alt || m.meta;
    let modifier = modifier(vk);
    let (key, text) = match named_key(vk) {
        Some(Key::Space) => (
            Some(Key::Space),
            if shortcut {
                Typed::None
            } else {
                Typed::Char(' ')
            },
        ),
        Some(k) => (Some(k), Typed::None),
        None if modifier.is_some() => (None, Typed::None),
        None => match table.get(vk, mods.shift(), mods.altgr(), mods.caps_lock()) {
            Produced::None => (None, Typed::None),
            Produced::Char(c) => (
                Some(Key::Char(c)),
                if shortcut || c.is_control() {
                    Typed::None
                } else {
                    Typed::Char(c)
                },
            ),
            Produced::Dead(c) => (
                Some(Key::Char(c)),
                if shortcut {
                    Typed::None
                } else {
                    Typed::Unknown
                },
            ),
        },
    };
    KeyIn {
        code: code(vk, extended),
        down,
        time_ms,
        key,
        text,
        modifier,
        mods: m,
    }
}

/// Pairs the UTF-16 units of injected `VK_PACKET` key-downs into characters.
#[derive(Clone, Copy, Debug, Default)]
pub struct PacketChars {
    high: Option<u16>,
}

impl PacketChars {
    /// Feeds one unit; returns a character once complete. A lone surrogate
    /// yields U+FFFD so the gate treats it as a mismatch.
    pub fn push(&mut self, unit: u16) -> Option<char> {
        match (self.high.take(), unit) {
            (None, 0xD800..=0xDBFF) => {
                self.high = Some(unit);
                None
            }
            (Some(high), 0xDC00..=0xDFFF) => {
                char::decode_utf16([high, unit]).next().and_then(Result::ok)
            }
            (Some(_), _) | (None, 0xDC00..=0xDFFF) => Some(char::REPLACEMENT_CHARACTER),
            (None, _) => char::from_u32(u32::from(unit)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A US-like layout with a German-style dead backtick on the `^` key
    /// under Shift, and AltGr+Q producing '@'.
    fn table() -> CharTable {
        CharTable::build(0x0409_0409, |vk, shift, altgr, caps| match (vk, altgr) {
            (b'A'..=b'Z', false) => {
                let c = char::from(vk);
                Produced::Char(if shift != caps {
                    c
                } else {
                    c.to_ascii_lowercase()
                })
            }
            (b'Q', true) => Produced::Char('@'),
            (0xC0, false) => Produced::Char(if shift { '~' } else { '`' }),
            (0xDC, false) if shift => Produced::Dead('`'),
            (0xDC, false) => Produced::Dead('^'),
            (b'1', false) => Produced::Char(if shift { '!' } else { '1' }),
            _ => Produced::None,
        })
    }

    fn press(mods: &mut ModState, vk: u8) {
        assert!(mods.record(vk, 0, true));
    }

    #[test]
    fn letters_follow_shift_and_caps_lock() {
        let t = table();
        let mut m = ModState::new(false);
        assert_eq!(key_in(b'A', false, true, 0, &m, &t).text, Typed::Char('a'));
        press(&mut m, VK_LSHIFT);
        let k = key_in(b'A', false, true, 0, &m, &t);
        assert_eq!(
            (k.key, k.text, k.mods.shift),
            (Some(Key::Char('A')), Typed::Char('A'), true)
        );
        m.record(VK_LSHIFT, 0, false);
        m.record(VK_CAPITAL, 0, true);
        m.record(VK_CAPITAL, 0, true); // auto-repeat does not toggle again
        m.record(VK_CAPITAL, 0, false);
        assert_eq!(key_in(b'A', false, true, 0, &m, &t).text, Typed::Char('A'));
    }

    #[test]
    fn ctrl_makes_a_shortcut_that_still_names_its_character() {
        let t = table();
        let mut m = ModState::default();
        press(&mut m, VK_LCONTROL);
        let k = key_in(b'C', false, true, 0, &m, &t);
        assert_eq!(
            (k.key, k.text, k.mods.ctrl),
            (Some(Key::Char('c')), Typed::None, true)
        );
    }

    #[test]
    fn altgr_types_characters_instead_of_counting_as_ctrl_alt() {
        let t = table();
        let mut m = ModState::default();
        assert!(
            !m.record(VK_LCONTROL, 0x21D, true),
            "synthetic Ctrl is hidden from the gate"
        );
        press(&mut m, VK_RMENU);
        let k = key_in(b'Q', false, true, 0, &m, &t);
        assert_eq!((k.text, k.mods), (Typed::Char('@'), Mods::default()));
        m.record(VK_RMENU, 0, false);
        m.record(VK_LCONTROL, 0x21D, false);
        // Right Alt on a layout without AltGr is Alt.
        press(&mut m, VK_RMENU);
        assert!(m.mods().alt);
    }

    #[test]
    fn dead_keys_are_unpredictable_but_still_bindable() {
        let t = table();
        let mut m = ModState::default();
        press(&mut m, VK_LSHIFT);
        let k = key_in(0xDC, false, true, 0, &m, &t);
        assert_eq!((k.key, k.text), (Some(Key::Char('`')), Typed::Unknown));
    }

    #[test]
    fn named_and_modifier_keys() {
        let t = table();
        let m = ModState::default();
        let space = key_in(VK_SPACE, false, true, 0, &m, &t);
        assert_eq!(
            (space.key, space.text),
            (Some(Key::Space), Typed::Char(' '))
        );
        let enter = key_in(VK_RETURN, true, true, 0, &m, &t);
        assert_eq!((enter.key, enter.text), (Some(Key::Enter), Typed::None));
        assert_ne!(
            enter.code,
            key_in(VK_RETURN, false, true, 0, &m, &t).code,
            "numpad Enter is its own key"
        );
        assert_eq!(named_key(0x7B), Some(Key::F(12)));
        let ralt = key_in(VK_RMENU, true, true, 0, &m, &t);
        assert_eq!((ralt.modifier, ralt.key), (Some(Modifier::RightAlt), None));
        assert!(code(0xFF, true) < crate::gate::KEY_CODES);
    }

    #[test]
    fn packets_pair_surrogates() {
        let mut p = PacketChars::default();
        assert_eq!(p.push(u16::from(b'h')), Some('h'));
        let mut units = [0u16; 2];
        '😀'.encode_utf16(&mut units);
        assert_eq!(p.push(units[0]), None);
        assert_eq!(p.push(units[1]), Some('😀'));
        assert_eq!(p.push(units[1]), Some(char::REPLACEMENT_CHARACTER));
    }
}
