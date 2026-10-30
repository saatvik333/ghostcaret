//! The key gate shared by the desktop backends.
//!
//! A backend's key callback describes each key event as a [`KeyIn`] and asks
//! [`Gate::key`] what to do with it. The gate owns the overlay state the keys
//! act on and applies Take Word, Take All, Show More, typing-through,
//! dismissal and card capture to it in place, so text the gate inserts can
//! never be reordered with keys typed after it. What happened is queued as
//! reports; [`Gate::drain_events`] turns them into [`Event`]s once the
//! callback has returned.
//!
//! None of this allocates or blocks: taken text is copied into a buffer
//! reserved up front, and events are built only when drained.

use oft_engine::io::{
    Action, Bindings, CardKey, Chord, DismissReason, Event, FieldId, Key, Modifier, Mods, Overlay,
    Trigger,
};

/// Key codes must be below this to pair presses with releases.
pub const KEY_CODES: u32 = 1024;

/// A modifier released within this many milliseconds of its press, with no
/// other key in between, is a tap.
pub const TAP_MAX_MS: u32 = 400;

/// How the Reply & rewrite modifier tap treats the modifier's own press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapStyle {
    /// The press is withheld until another key shows it is used as a modifier
    /// ([`Verdict::Replay`]); a tap never reaches the app. For backends that
    /// forward keys themselves.
    #[cfg_attr(
        target_os = "windows",
        allow(dead_code, reason = "only the Linux backend forwards keys itself")
    )]
    Defer,
    /// The press reaches the app at once; the release that completes a tap
    /// returns [`Verdict::Tap`] so the backend can stop the app from acting on
    /// a lone modifier (the Windows menu bar).
    #[cfg_attr(
        target_os = "linux",
        allow(dead_code, reason = "only the Windows backend lets taps through")
    )]
    Pass,
}

/// What a key types into the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typed {
    /// Nothing: a named key, a modifier, or a shortcut.
    None,
    Char(char),
    /// Something the gate cannot predict: a dead key, a compose sequence or
    /// input-method composition.
    Unknown,
}

/// One key event as the gate sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyIn {
    /// Physical key code below [`KEY_CODES`] (evdev keycode, Windows virtual key).
    pub code: u32,
    pub down: bool,
    /// Event time in milliseconds from any wrapping clock.
    pub time_ms: u32,
    /// The key as bindings name it: a named key, or the character it produces
    /// with Shift and AltGr applied but not Ctrl, Alt or Meta.
    pub key: Option<Key>,
    /// What the key types; [`Typed::None`] while Ctrl, Alt or Meta is held.
    pub text: Typed,
    /// Set when this key is itself a modifier.
    pub modifier: Option<Modifier>,
    /// Modifiers held, not counting this key.
    pub mods: Mods,
}

/// What the backend does with a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The app gets the key.
    Pass,
    /// The app gets the withheld modifier press `code` first, then this key.
    Replay(u32),
    /// The app does not get the key.
    Consume,
    /// The app does not get the key; [`Gate::accepted`] is inserted before
    /// any later key reaches the app.
    Insert,
    /// The app gets this modifier release, which completed a Flow Draft tap
    /// ([`TapStyle::Pass`]).
    Tap,
}

#[derive(Clone, Debug, PartialEq)]
enum Report {
    /// The text is `taken[start..end]`.
    Accepted {
        field: FieldId,
        start: usize,
        end: usize,
        whole: bool,
    },
    TypedThrough {
        field: FieldId,
        ch: char,
    },
    Dismissed {
        field: FieldId,
        reason: DismissReason,
    },
    Action(Action),
    CardKey(CardKey),
}

/// A set of key codes below [`KEY_CODES`]; larger codes are never members.
#[derive(Clone, Copy, Debug, Default)]
struct KeySet([u64; (KEY_CODES / 64) as usize]);

impl KeySet {
    /// Returns false when `code` was already present.
    fn insert(&mut self, code: u32) -> bool {
        let Some(word) = self.0.get_mut((code / 64) as usize) else {
            return true;
        };
        let bit = 1u64 << (code % 64);
        let fresh = *word & bit == 0;
        *word |= bit;
        fresh
    }

    /// Returns whether `code` was present.
    fn remove(&mut self, code: u32) -> bool {
        let Some(word) = self.0.get_mut((code / 64) as usize) else {
            return false;
        };
        let bit = 1u64 << (code % 64);
        let present = *word & bit != 0;
        *word &= !bit;
        present
    }

    fn contains(&self, code: u32) -> bool {
        self.0
            .get((code / 64) as usize)
            .is_some_and(|w| w & (1u64 << (code % 64)) != 0)
    }

    fn is_empty(&self) -> bool {
        self.0.iter().all(|&w| w == 0)
    }

    fn clear(&mut self) {
        self.0 = [0; (KEY_CODES / 64) as usize];
    }
}

#[derive(Clone, Copy, Debug)]
struct ArmedTap {
    code: u32,
    since: u32,
}

/// Reports queued before the backend drains them; enough for any burst of
/// keys between two drains, so the queue does not grow on the key path.
const REPORTS: usize = 64;
/// Bytes of taken text buffered between two drains before the buffer grows.
const TAKEN: usize = 4096;

#[derive(Debug)]
pub struct Gate {
    taps: TapStyle,
    active: bool,
    bindings: Bindings,
    focus: Option<FieldId>,
    overlay: Overlay,
    display_changed: bool,
    /// Keys held down, to tell auto-repeat from a new press.
    held: KeySet,
    /// Keys whose press was consumed; their repeats and release are too.
    consumed: KeySet,
    tap: Option<ArmedTap>,
    /// A [`TapStyle::Defer`] modifier press not yet forwarded to the app.
    withheld: Option<u32>,
    reports: Vec<Report>,
    /// Text taken by Take Word and Take All since the last drain.
    taken: String,
}

impl Gate {
    pub fn new(taps: TapStyle) -> Gate {
        Gate {
            taps,
            active: true,
            bindings: Bindings::default(),
            focus: None,
            overlay: Overlay::Hidden,
            display_changed: false,
            held: KeySet::default(),
            consumed: KeySet::default(),
            tap: None,
            withheld: None,
            reports: Vec::with_capacity(REPORTS),
            taken: String::with_capacity(TAKEN),
        }
    }

    /// The focused field, or `None` when no editable field has focus. An
    /// overlay for any other field is dismissed and later ones are dropped,
    /// so a stale suggestion never acts on the next field.
    pub fn set_focus(&mut self, field: Option<FieldId>) {
        self.focus = field;
        if let Some(shown) = overlay_field(&self.overlay).filter(|&f| Some(f) != field) {
            self.dismiss(shown, DismissReason::FocusLost);
        }
    }

    pub fn set_overlay(&mut self, overlay: Overlay) {
        let overlay = match overlay {
            Overlay::Suggestion(v) if !v.is_valid() => {
                log::warn!("ignoring an invalid suggestion view");
                Overlay::Hidden
            }
            Overlay::Suggestion(v) if v.shown == 0 => Overlay::Hidden,
            o => o,
        };
        self.overlay = match overlay_field(&overlay) {
            Some(f) if Some(f) != self.focus => Overlay::Hidden,
            _ => overlay,
        };
        self.display_changed = true;
    }

    pub fn set_bindings(&mut self, bindings: Bindings) {
        self.bindings = bindings;
        self.tap = None;
    }

    /// While inactive every press passes and the overlay is hidden.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        self.tap = None;
        if !active {
            self.hide();
        }
    }

    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// True once after anything changed what the overlay shows.
    pub fn take_display_changed(&mut self) -> bool {
        std::mem::take(&mut self.display_changed)
    }

    /// The text to insert after [`Verdict::Insert`].
    pub fn accepted(&self) -> &str {
        match self.reports.last() {
            Some(&Report::Accepted { start, end, .. }) => &self.taken[start..end],
            _ => "",
        }
    }

    /// Forgets which keys are held, for when the backend loses sight of the
    /// keyboard (a grab ends, a hook is reinstalled).
    pub fn reset_keys(&mut self) {
        self.held.clear();
        self.consumed.clear();
        self.tap = None;
        self.withheld = None;
    }

    /// Sends the queued reports as events, in order. Consecutive typed-through
    /// characters become one [`Event::TypedThrough`].
    pub fn drain_events(&mut self, mut emit: impl FnMut(Event)) {
        let mut typed: Option<(FieldId, String)> = None;
        for report in self.reports.drain(..) {
            if let Report::TypedThrough { field, ch } = report {
                match &mut typed {
                    Some((f, text)) if *f == field => text.push(ch),
                    _ => {
                        if let Some((field, text)) = typed.replace((field, ch.to_string())) {
                            emit(Event::TypedThrough { field, text });
                        }
                    }
                }
                continue;
            }
            if let Some((field, text)) = typed.take() {
                emit(Event::TypedThrough { field, text });
            }
            emit(match report {
                Report::Accepted {
                    field,
                    start,
                    end,
                    whole,
                } => Event::Accepted {
                    field,
                    text: self.taken[start..end].to_owned(),
                    whole,
                },
                Report::Dismissed { field, reason } => Event::Dismissed { field, reason },
                Report::Action(a) => Event::Action(a),
                Report::CardKey(k) => Event::CardKey(k),
                Report::TypedThrough { .. } => unreachable!("handled above"),
            });
        }
        if let Some((field, text)) = typed {
            emit(Event::TypedThrough { field, text });
        }
        self.taken.clear();
    }

    pub fn key(&mut self, k: &KeyIn) -> Verdict {
        if !k.down {
            return self.release(k);
        }
        let repeat = !self.held.insert(k.code);
        if repeat && self.consumed.contains(k.code) {
            return Verdict::Consume;
        }
        if !self.active {
            return Verdict::Pass;
        }
        if let Some(m) = k.modifier {
            return self.modifier_press(k, m, repeat);
        }
        self.tap = None;
        let verdict = self.press(k, repeat);
        match verdict {
            Verdict::Consume | Verdict::Insert => {
                self.consumed.insert(k.code);
                verdict
            }
            Verdict::Pass => match self.withheld.take() {
                Some(code) => Verdict::Replay(code),
                None => Verdict::Pass,
            },
            Verdict::Replay(_) | Verdict::Tap => verdict,
        }
    }

    fn release(&mut self, k: &KeyIn) -> Verdict {
        self.held.remove(k.code);
        if let Some(tap) = self.tap.take_if(|t| t.code == k.code) {
            let tapped = k.time_ms.wrapping_sub(tap.since) <= TAP_MAX_MS;
            if tapped {
                self.reports.push(Report::Action(Action::FlowDraft));
            }
            return match self.taps {
                TapStyle::Defer => {
                    self.withheld = None;
                    Verdict::Consume
                }
                TapStyle::Pass if tapped => Verdict::Tap,
                TapStyle::Pass => Verdict::Pass,
            };
        }
        if self.withheld.take_if(|&mut c| c == k.code).is_some() || self.consumed.remove(k.code) {
            return Verdict::Consume;
        }
        Verdict::Pass
    }

    fn modifier_press(&mut self, k: &KeyIn, m: Modifier, repeat: bool) -> Verdict {
        if repeat {
            return if self.withheld == Some(k.code) {
                Verdict::Consume
            } else {
                Verdict::Pass
            };
        }
        let others_held = {
            let mut others = self.held;
            others.remove(k.code);
            !others.is_empty()
        };
        let trigger = self.bindings.flow_draft == Some(Trigger::ModifierTap(m));
        if !trigger || others_held {
            self.tap = None;
            return match self.withheld.take() {
                Some(code) => Verdict::Replay(code),
                None => Verdict::Pass,
            };
        }
        self.tap = Some(ArmedTap {
            code: k.code,
            since: k.time_ms,
        });
        match self.taps {
            TapStyle::Defer => {
                self.withheld = Some(k.code);
                Verdict::Consume
            }
            TapStyle::Pass => Verdict::Pass,
        }
    }

    fn press(&mut self, k: &KeyIn, repeat: bool) -> Verdict {
        let draft_chord = match self.bindings.flow_draft {
            Some(Trigger::Chord(c)) => Some(c),
            _ => None,
        };
        if !repeat && bound(draft_chord, k) {
            self.reports.push(Report::Action(Action::FlowDraft));
            return Verdict::Consume;
        }
        match &self.overlay {
            Overlay::Hidden => Verdict::Pass,
            Overlay::Card(card) => {
                let typing = card.input.is_some();
                if k.mods.ctrl || k.mods.alt || k.mods.meta {
                    return Verdict::Pass;
                }
                if let Some(ck) = card_key(k, typing) {
                    self.reports.push(Report::CardKey(ck));
                }
                Verdict::Consume
            }
            Overlay::Suggestion(_) => self.suggestion_key(k, repeat),
        }
    }

    fn suggestion_key(&mut self, k: &KeyIn, repeat: bool) -> Verdict {
        let Overlay::Suggestion(view) = &mut self.overlay else {
            return Verdict::Pass;
        };
        let field = view.field;
        let b = &self.bindings;
        if !repeat {
            if bound(b.take_word, k) || bound(b.take_all, k) {
                let whole = !bound(b.take_word, k);
                let end = if whole { view.shown } else { view.word_end() };
                let start = self.taken.len();
                self.taken.push_str(&view.text[..end]);
                view.advance(end);
                if view.shown == 0 {
                    self.overlay = Overlay::Hidden;
                }
                self.display_changed = true;
                self.reports.push(Report::Accepted {
                    field,
                    start,
                    end: start + end,
                    whole,
                });
                return Verdict::Insert;
            }
            if bound(b.show_more, k) {
                if view.show_more() {
                    self.display_changed = true;
                } else {
                    self.reports.push(Report::Action(Action::ShowMore));
                }
                return Verdict::Consume;
            }
            for (chord, action) in [
                (b.next_alternative, Action::NextAlternative),
                (b.prev_alternative, Action::PrevAlternative),
            ] {
                if bound(chord, k) {
                    self.reports.push(Report::Action(action));
                    return Verdict::Consume;
                }
            }
            if bound(b.dismiss, k) {
                self.dismiss(field, DismissReason::Escape);
                return Verdict::Consume;
            }
        }
        match k.text {
            Typed::Char(ch) => {
                let mut buf = [0u8; 4];
                if view.type_through(ch.encode_utf8(&mut buf)) {
                    if view.shown == 0 {
                        self.overlay = Overlay::Hidden;
                    }
                    self.display_changed = true;
                    self.reports.push(Report::TypedThrough { field, ch });
                } else {
                    self.dismiss(field, DismissReason::Mismatch);
                }
            }
            Typed::Unknown => self.dismiss(field, DismissReason::Mismatch),
            Typed::None => match k.key {
                Some(
                    Key::Left
                    | Key::Right
                    | Key::Up
                    | Key::Down
                    | Key::Home
                    | Key::End
                    | Key::PageUp
                    | Key::PageDown,
                ) => self.dismiss(field, DismissReason::CaretMoved),
                Some(Key::Escape) => self.dismiss(field, DismissReason::Escape),
                Some(_) => self.dismiss(field, DismissReason::Mismatch),
                // Keys with no meaning for the field (media, volume) leave it alone.
                None => {}
            },
        }
        Verdict::Pass
    }

    fn dismiss(&mut self, field: FieldId, reason: DismissReason) {
        self.hide();
        self.reports.push(Report::Dismissed { field, reason });
    }

    fn hide(&mut self) {
        if self.overlay != Overlay::Hidden {
            self.overlay = Overlay::Hidden;
            self.display_changed = true;
        }
    }
}

fn overlay_field(overlay: &Overlay) -> Option<FieldId> {
    match overlay {
        Overlay::Hidden => None,
        Overlay::Suggestion(v) => Some(v.field),
        Overlay::Card(c) => Some(c.field),
    }
}

/// Whether `k` is the bound chord. A character already includes Shift, so
/// character chords compare Ctrl, Alt and Meta only; named keys compare all
/// four modifiers.
fn bound(chord: Option<Chord>, k: &KeyIn) -> bool {
    let (Some(chord), Some(key)) = (chord, k.key) else {
        return false;
    };
    let (a, b) = (chord.mods, k.mods);
    let same_but_shift = a.ctrl == b.ctrl && a.alt == b.alt && a.meta == b.meta;
    match (chord.key, key) {
        (Key::Char(x), Key::Char(y)) => x == y && same_but_shift,
        (x, y) => x == y && same_but_shift && a.shift == b.shift,
    }
}

fn card_key(k: &KeyIn, typing: bool) -> Option<CardKey> {
    match (k.key, k.text) {
        (Some(Key::Up), _) => Some(CardKey::Up),
        (Some(Key::Down), _) => Some(CardKey::Down),
        (Some(Key::Enter), _) => Some(CardKey::Enter),
        (Some(Key::Escape), _) => Some(CardKey::Escape),
        (Some(Key::Tab), _) => Some(CardKey::Tab),
        (Some(Key::Backspace), _) => Some(CardKey::Backspace),
        (_, Typed::Char(c @ '1'..='9')) if !typing => Some(CardKey::Pick(c as u8 - b'0')),
        (_, Typed::Char(c)) => Some(CardKey::Char(c)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oft_engine::io::{CardView, SuggestionView};
    use proptest::prelude::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    /// Counts allocations made by the current thread, so tests running in
    /// parallel do not see each other's.
    struct Counting;

    thread_local! {
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
    }

    // SAFETY: forwards to the system allocator; the counter is a const-initialised
    // thread-local Cell, which never allocates.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCS.with(|a| a.set(a.get() + 1));
            // SAFETY: same contract as the caller's.
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: same contract as the caller's.
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            ALLOCS.with(|a| a.set(a.get() + 1));
            // SAFETY: same contract as the caller's.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    fn allocations(f: impl FnOnce()) -> usize {
        let start = ALLOCS.with(Cell::get);
        f();
        ALLOCS.with(Cell::get) - start
    }

    const FIELD: FieldId = FieldId(7);
    const ALT: Mods = Mods {
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    };
    const CTRL: Mods = Mods {
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    };
    const SHIFT: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: true,
        meta: false,
    };
    const RALT: u32 = 100;

    fn view(text: &str) -> SuggestionView {
        let word_ends = text
            .char_indices()
            .skip(1)
            .filter(|&(_, c)| c == ' ' || c == ',')
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        SuggestionView {
            field: FIELD,
            revision: 1,
            text: text.into(),
            shown: text.len(),
            word_ends,
            ..Default::default()
        }
    }

    fn gate_with(overlay: Overlay, taps: TapStyle) -> Gate {
        let mut g = Gate::new(taps);
        g.set_focus(Some(FIELD));
        g.set_overlay(overlay);
        g.take_display_changed();
        g
    }

    fn suggesting(text: &str) -> Gate {
        gate_with(Overlay::Suggestion(view(text)), TapStyle::Defer)
    }

    fn named(code: u32, key: Key, mods: Mods, down: bool) -> KeyIn {
        let text = if key == Key::Space {
            Typed::Char(' ')
        } else {
            Typed::None
        };
        KeyIn {
            code,
            down,
            time_ms: 0,
            key: Some(key),
            text,
            modifier: None,
            mods,
        }
    }

    fn ch(c: char, mods: Mods, down: bool) -> KeyIn {
        let text = if mods.ctrl || mods.alt || mods.meta {
            Typed::None
        } else {
            Typed::Char(c)
        };
        KeyIn {
            code: c as u32 % 512 + 200,
            down,
            time_ms: 0,
            key: Some(Key::Char(c)),
            text,
            modifier: None,
            mods,
        }
    }

    fn modifier(code: u32, m: Modifier, down: bool, time_ms: u32) -> KeyIn {
        KeyIn {
            code,
            down,
            time_ms,
            key: None,
            text: Typed::None,
            modifier: Some(m),
            mods: Mods::default(),
        }
    }

    fn events(g: &mut Gate) -> Vec<Event> {
        let mut out = Vec::new();
        g.drain_events(|e| out.push(e));
        out
    }

    fn shown(g: &Gate) -> Option<&str> {
        match g.overlay() {
            Overlay::Suggestion(v) => Some(v.visible()),
            _ => None,
        }
    }

    const TAB: u32 = 15;
    const RIGHT: u32 = 106;

    #[test]
    fn take_word_inserts_the_first_unit_and_swallows_its_release_and_repeats() {
        let mut g = suggesting(" world, how");
        assert_eq!(
            g.key(&named(TAB, Key::Tab, Mods::default(), true)),
            Verdict::Insert
        );
        assert_eq!(g.accepted(), " world");
        assert!(g.take_display_changed());
        assert_eq!(shown(&g), Some(", how"));
        assert_eq!(
            g.key(&named(TAB, Key::Tab, Mods::default(), true)),
            Verdict::Consume,
            "repeat"
        );
        assert_eq!(shown(&g), Some(", how"), "a repeat never acts");
        assert_eq!(
            g.key(&named(TAB, Key::Tab, Mods::default(), false)),
            Verdict::Consume
        );
        assert_eq!(
            events(&mut g),
            [Event::Accepted {
                field: FIELD,
                text: " world".into(),
                whole: false
            }]
        );
    }

    #[test]
    fn take_all_inserts_the_visible_part_and_hides() {
        let mut g = suggesting(" world, how");
        assert_eq!(g.key(&ch('`', Mods::default(), true)), Verdict::Insert);
        assert_eq!(g.accepted(), " world, how");
        assert_eq!(g.overlay(), &Overlay::Hidden);
        assert_eq!(
            events(&mut g),
            [Event::Accepted {
                field: FIELD,
                text: " world, how".into(),
                whole: true
            }]
        );
    }

    #[test]
    fn character_chords_ignore_shift_and_named_chords_do_not() {
        let mut g = suggesting(" world");
        assert_eq!(
            g.key(&named(TAB, Key::Tab, SHIFT, true)),
            Verdict::Pass,
            "Shift+Tab is not Tab"
        );
        assert_eq!(
            events(&mut g),
            [Event::Dismissed {
                field: FIELD,
                reason: DismissReason::Mismatch
            }]
        );

        // A layout where the backtick needs Shift still takes the whole suggestion.
        let mut g = suggesting(" world");
        assert_eq!(g.key(&ch('`', SHIFT, true)), Verdict::Insert);
        // Ctrl+backtick is a shortcut, not Take All.
        let mut g = suggesting(" world");
        assert_eq!(g.key(&ch('`', CTRL, true)), Verdict::Pass);
        assert_eq!(g.overlay(), &Overlay::Hidden);
    }

    #[test]
    fn show_more_reveals_the_next_step_or_asks_for_more() {
        let mut v = view(" world, how are you");
        v.shown = 6;
        v.step_ends = vec![11];
        let mut g = gate_with(Overlay::Suggestion(v), TapStyle::Defer);
        assert_eq!(
            g.key(&named(RIGHT, Key::Right, ALT, true)),
            Verdict::Consume
        );
        assert_eq!(shown(&g), Some(" world, how"));
        assert!(g.take_display_changed());
        assert!(events(&mut g).is_empty(), "served from the buffer");
        assert_eq!(
            g.key(&named(RIGHT, Key::Right, ALT, false)),
            Verdict::Consume
        );
        assert_eq!(
            g.key(&named(RIGHT, Key::Right, ALT, true)),
            Verdict::Consume
        );
        assert_eq!(events(&mut g), [Event::Action(Action::ShowMore)]);
    }

    #[test]
    fn typing_through_advances_and_a_mismatch_dismisses() {
        let mut g = suggesting(" wo");
        for c in [' ', 'w'] {
            assert_eq!(g.key(&ch(c, Mods::default(), true)), Verdict::Pass);
            assert_eq!(g.key(&ch(c, Mods::default(), false)), Verdict::Pass);
        }
        assert_eq!(shown(&g), Some("o"));
        assert_eq!(g.key(&ch('x', Mods::default(), true)), Verdict::Pass);
        assert_eq!(g.overlay(), &Overlay::Hidden);
        assert_eq!(
            events(&mut g),
            [
                Event::TypedThrough {
                    field: FIELD,
                    text: " w".into()
                },
                Event::Dismissed {
                    field: FIELD,
                    reason: DismissReason::Mismatch
                },
            ]
        );
    }

    #[test]
    fn typing_the_whole_suggestion_hides_it_without_a_dismissal() {
        let mut g = suggesting("ok");
        g.key(&ch('o', Mods::default(), true));
        g.key(&ch('k', Mods::default(), true));
        assert_eq!(g.overlay(), &Overlay::Hidden);
        assert_eq!(
            events(&mut g),
            [Event::TypedThrough {
                field: FIELD,
                text: "ok".into()
            }]
        );
    }

    #[test]
    fn navigation_dismisses_as_a_caret_move_and_modifiers_do_not_dismiss() {
        let mut g = suggesting(" world");
        assert_eq!(
            g.key(&modifier(42, Modifier::LeftShift, true, 0)),
            Verdict::Pass
        );
        assert_eq!(
            g.key(&modifier(42, Modifier::LeftShift, false, 10)),
            Verdict::Pass
        );
        assert!(shown(&g).is_some());
        assert_eq!(
            g.key(&named(105, Key::Left, Mods::default(), true)),
            Verdict::Pass
        );
        assert_eq!(
            events(&mut g),
            [Event::Dismissed {
                field: FIELD,
                reason: DismissReason::CaretMoved
            }]
        );
    }

    #[test]
    fn escape_dismisses_and_dead_keys_count_as_a_mismatch() {
        let mut g = suggesting(" world");
        assert_eq!(
            g.key(&named(1, Key::Escape, Mods::default(), true)),
            Verdict::Consume
        );
        assert_eq!(
            events(&mut g),
            [Event::Dismissed {
                field: FIELD,
                reason: DismissReason::Escape
            }]
        );

        let mut g = suggesting(" world");
        let dead = KeyIn {
            text: Typed::Unknown,
            ..ch('^', Mods::default(), true)
        };
        assert_eq!(g.key(&dead), Verdict::Pass);
        assert_eq!(
            events(&mut g),
            [Event::Dismissed {
                field: FIELD,
                reason: DismissReason::Mismatch
            }]
        );
    }

    #[test]
    fn a_release_whose_press_passed_still_passes_after_the_overlay_changes() {
        let mut g = Gate::new(TapStyle::Defer);
        g.set_focus(Some(FIELD));
        assert_eq!(
            g.key(&named(TAB, Key::Tab, Mods::default(), true)),
            Verdict::Pass
        );
        g.set_overlay(Overlay::Suggestion(view(" world")));
        assert_eq!(
            g.key(&named(TAB, Key::Tab, Mods::default(), false)),
            Verdict::Pass
        );
        assert_eq!(shown(&g), Some(" world"));
    }

    #[test]
    fn repeats_of_a_passed_key_never_trigger_bindings() {
        let mut g = Gate::new(TapStyle::Defer);
        g.set_focus(Some(FIELD));
        g.key(&named(TAB, Key::Tab, Mods::default(), true));
        g.set_overlay(Overlay::Suggestion(view(" world")));
        assert_eq!(
            g.key(&named(TAB, Key::Tab, Mods::default(), true)),
            Verdict::Pass
        );
        assert_eq!(
            events(&mut g),
            [Event::Dismissed {
                field: FIELD,
                reason: DismissReason::Mismatch
            }]
        );
    }

    #[test]
    fn overlays_for_another_field_are_dropped() {
        let mut g = suggesting(" world");
        g.set_focus(Some(FieldId(8)));
        assert_eq!(g.overlay(), &Overlay::Hidden);
        assert!(g.take_display_changed());
        g.set_overlay(Overlay::Suggestion(view(" world")));
        assert_eq!(
            g.overlay(),
            &Overlay::Hidden,
            "stale suggestion for field 7"
        );
        assert_eq!(
            g.key(&named(TAB, Key::Tab, Mods::default(), true)),
            Verdict::Pass
        );
    }

    #[test]
    fn invalid_or_empty_views_hide_the_overlay() {
        let mut g = suggesting(" world");
        let mut bad = view(" world");
        bad.shown = 99;
        g.set_overlay(Overlay::Suggestion(bad));
        assert_eq!(g.overlay(), &Overlay::Hidden);
        let mut empty = view(" world");
        empty.shown = 0;
        g.set_overlay(Overlay::Suggestion(empty));
        assert_eq!(g.overlay(), &Overlay::Hidden);
    }

    fn card(input: Option<&str>) -> Overlay {
        Overlay::Card(CardView {
            field: FIELD,
            items: vec!["a".into(), "b".into()],
            input: input.map(Into::into),
            ..Default::default()
        })
    }

    #[test]
    fn a_card_captures_plain_keys_and_lets_shortcuts_through() {
        let mut g = gate_with(card(None), TapStyle::Defer);
        assert_eq!(
            g.key(&named(108, Key::Down, Mods::default(), true)),
            Verdict::Consume
        );
        assert_eq!(g.key(&ch('2', Mods::default(), true)), Verdict::Consume);
        assert_eq!(g.key(&ch('x', SHIFT, true)), Verdict::Consume);
        assert_eq!(
            g.key(&named(105, Key::Left, Mods::default(), true)),
            Verdict::Consume,
            "no card meaning, still swallowed"
        );
        assert_eq!(
            g.key(&named(TAB, Key::Tab, ALT, true)),
            Verdict::Pass,
            "Alt+Tab switches apps"
        );
        assert_eq!(g.key(&ch('c', CTRL, true)), Verdict::Pass);
        assert_eq!(
            g.key(&named(108, Key::Down, Mods::default(), false)),
            Verdict::Consume
        );
        assert_eq!(
            events(&mut g),
            [
                Event::CardKey(CardKey::Down),
                Event::CardKey(CardKey::Pick(2)),
                Event::CardKey(CardKey::Char('x'))
            ]
        );
    }

    #[test]
    fn digits_are_characters_while_typing_a_direction() {
        let mut g = gate_with(card(Some("by ")), TapStyle::Defer);
        g.key(&ch('3', Mods::default(), true));
        assert_eq!(events(&mut g), [Event::CardKey(CardKey::Char('3'))]);
    }

    #[test]
    fn a_deferred_tap_never_reaches_the_app() {
        let mut g = suggesting(" world");
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, true, 1000)),
            Verdict::Consume
        );
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, true, 1030)),
            Verdict::Consume,
            "repeat"
        );
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, false, 1100)),
            Verdict::Consume
        );
        assert_eq!(events(&mut g), [Event::Action(Action::FlowDraft)]);
        assert!(shown(&g).is_some(), "a tap is not a dismissal");
    }

    #[test]
    fn a_long_lone_hold_is_not_a_tap() {
        let mut g = Gate::new(TapStyle::Defer);
        g.key(&modifier(RALT, Modifier::RightAlt, true, u32::MAX - 10));
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, false, TAP_MAX_MS)),
            Verdict::Consume
        );
        assert!(events(&mut g).is_empty());
    }

    #[test]
    fn a_withheld_modifier_is_replayed_before_the_first_key_that_passes() {
        let mut g = Gate::new(TapStyle::Defer);
        g.key(&modifier(RALT, Modifier::RightAlt, true, 0));
        let altgr_q = KeyIn {
            text: Typed::Char('@'),
            ..ch('@', Mods::default(), true)
        };
        assert_eq!(g.key(&altgr_q), Verdict::Replay(RALT));
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, false, 50)),
            Verdict::Pass
        );
        assert!(events(&mut g).is_empty());
    }

    #[test]
    fn a_withheld_modifier_stays_hidden_when_its_chord_is_consumed() {
        let mut g = suggesting(" world");
        g.key(&modifier(RALT, Modifier::RightAlt, true, 0));
        assert_eq!(
            g.key(&named(RIGHT, Key::Right, ALT, true)),
            Verdict::Consume
        );
        assert_eq!(
            g.key(&named(RIGHT, Key::Right, ALT, false)),
            Verdict::Consume
        );
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, false, 50)),
            Verdict::Consume
        );
        assert_eq!(events(&mut g), [Event::Action(Action::ShowMore)]);
    }

    #[test]
    fn a_passed_tap_reports_on_release() {
        let mut g = Gate::new(TapStyle::Pass);
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, true, 0)),
            Verdict::Pass
        );
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, false, 90)),
            Verdict::Tap
        );
        assert_eq!(events(&mut g), [Event::Action(Action::FlowDraft)]);

        // Pressed with another key held, it is a modifier, not a tap.
        g.key(&modifier(42, Modifier::LeftShift, true, 100));
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, true, 110)),
            Verdict::Pass
        );
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, false, 120)),
            Verdict::Pass
        );
        assert!(events(&mut g).is_empty());
    }

    #[test]
    fn inactive_gates_pass_every_press() {
        let mut g = suggesting(" world");
        g.set_active(false);
        assert_eq!(g.overlay(), &Overlay::Hidden);
        assert_eq!(
            g.key(&named(TAB, Key::Tab, Mods::default(), true)),
            Verdict::Pass
        );
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, true, 0)),
            Verdict::Pass
        );
        assert_eq!(
            g.key(&modifier(RALT, Modifier::RightAlt, false, 10)),
            Verdict::Pass
        );
        assert!(events(&mut g).is_empty());
    }

    #[test]
    fn a_flow_draft_chord_works_with_nothing_shown() {
        let mut g = Gate::new(TapStyle::Pass);
        let chord = Chord {
            key: Key::Char('d'),
            mods: CTRL,
        };
        g.set_bindings(Bindings {
            flow_draft: Some(Trigger::Chord(chord)),
            ..Bindings::default()
        });
        assert_eq!(g.key(&ch('d', CTRL, true)), Verdict::Consume);
        assert_eq!(g.key(&ch('d', CTRL, false)), Verdict::Consume);
        assert_eq!(events(&mut g), [Event::Action(Action::FlowDraft)]);
    }

    #[test]
    fn the_key_path_does_not_allocate() {
        assert_eq!(
            allocations(|| drop(std::hint::black_box(String::from("x")))),
            1,
            "the counter works"
        );
        let mut g = suggesting(" world, how are you today");
        let n = allocations(|| {
            // Typing through, taking words and the rest, modifiers, releases.
            for c in [' ', 'w', 'o'] {
                assert_eq!(g.key(&ch(c, Mods::default(), true)), Verdict::Pass);
                assert_eq!(g.key(&ch(c, Mods::default(), false)), Verdict::Pass);
            }
            assert_eq!(
                g.key(&named(TAB, Key::Tab, Mods::default(), true)),
                Verdict::Insert
            );
            assert_eq!(g.accepted(), "rld");
            g.key(&named(TAB, Key::Tab, Mods::default(), false));
            g.key(&modifier(42, Modifier::LeftShift, true, 0));
            g.key(&modifier(42, Modifier::LeftShift, false, 1));
            assert_eq!(g.key(&ch('`', Mods::default(), true)), Verdict::Insert);
            g.key(&ch('`', Mods::default(), false));
            // With nothing shown, keys pass.
            for c in "hello".chars() {
                g.key(&ch(c, Mods::default(), true));
                g.key(&ch(c, Mods::default(), false));
            }
        });
        assert_eq!(n, 0);

        let mut g = suggesting(" world");
        assert_eq!(
            allocations(|| {
                g.key(&named(105, Key::Left, Mods::default(), true));
                g.key(&named(105, Key::Left, Mods::default(), false));
            }),
            0,
            "dismissal"
        );

        let mut g = gate_with(card(None), TapStyle::Defer);
        assert_eq!(
            allocations(|| {
                g.key(&named(108, Key::Down, Mods::default(), true));
                g.key(&ch('x', Mods::default(), true));
            }),
            0,
            "card keys"
        );
    }

    fn arb_key() -> impl Strategy<Value = KeyIn> {
        let mods =
            (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(|(ctrl, alt, shift)| Mods {
                ctrl,
                alt,
                shift,
                meta: false,
            });
        let key = prop_oneof![
            Just(Key::Tab),
            Just(Key::Escape),
            Just(Key::Right),
            Just(Key::Left),
            Just(Key::Backspace),
            "[ a-z`,]".prop_map(|s| Key::Char(s.chars().next().unwrap())),
        ];
        (key, mods, any::<bool>()).prop_map(|(key, mods, down)| match key {
            Key::Char(c) => ch(c, mods, down),
            k => named(code_of(k), k, mods, down),
        })
    }

    fn code_of(key: Key) -> u32 {
        match key {
            Key::Escape => 1,
            Key::Backspace => 14,
            Key::Tab => TAB,
            Key::Left => 105,
            Key::Right => RIGHT,
            _ => 50,
        }
    }

    proptest! {
        /// Whatever is typed, accepted text and typed-through text together are
        /// always a prefix of the original suggestion, in order, and the view
        /// stays valid.
        #[test]
        fn taken_and_typed_text_is_always_a_prefix(keys in proptest::collection::vec(arb_key(), 0..40)) {
            let original = " world, how are you today";
            let mut g = suggesting(original);
            let mut consumed = String::new();
            for k in &keys {
                g.key(k);
                if let Overlay::Suggestion(v) = g.overlay() {
                    prop_assert!(v.is_valid());
                }
                g.drain_events(|e| match e {
                    Event::Accepted { text, .. } | Event::TypedThrough { text, .. } => consumed.push_str(&text),
                    _ => {}
                });
            }
            prop_assert!(original.starts_with(&consumed), "{consumed:?}");
        }
    }
}
