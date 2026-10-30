//! Contract between the engine and the OS backends (`ghostcaret-platform`).
//!
//! A backend reports what the user is typing ([`Event`]) and executes what the
//! controller decides ([`Backend`]). The backend owns the keystroke path: it
//! applies Take Word, Take All, Show More, typing-through and dismissal to the
//! current [`SuggestionView`] locally and synchronously, so inserted text can
//! never be reordered with keys the user types next. The controller is told
//! afterwards through events.
//!
//! All text offsets in this module are UTF-8 byte offsets on `char` boundaries.
//! Backends convert from their platform's units (UTF-16 on Windows and
//! macOS) once, at the boundary.

use serde::{Deserialize, Serialize};

use crate::settings::Appearance;

/// Most bytes of text before the caret a backend reports.
pub const MAX_BEFORE: usize = 8 * 1024;
/// Most bytes of text after the caret a backend reports.
pub const MAX_AFTER: usize = 2 * 1024;

/// Identifies one focused text field. Backends allocate a fresh id each time a
/// field gains focus, so a stale suggestion can never land in another field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldId(pub u64);

/// The application that owns the focused field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppInfo {
    /// Stable key for per-app rules: the Wayland app_id or the lower-case
    /// Windows executable name (`slack.exe`).
    pub id: String,
    /// Human-readable name for the settings list.
    pub name: String,
    /// Window title; may be empty. Used for hints such as "Gmail" in a browser.
    pub title: String,
}

/// What kind of field has focus, from the platform's content-type hints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    #[default]
    Text,
    Password,
    Pin,
    Search,
    Url,
    Email,
    Number,
    Phone,
    Terminal,
    Code,
    Unknown,
}

impl FieldKind {
    /// Fields where ghostcaret never reads, predicts or shows anything.
    pub fn is_secure_or_structured(self) -> bool {
        matches!(
            self,
            FieldKind::Password
                | FieldKind::Pin
                | FieldKind::Url
                | FieldKind::Number
                | FieldKind::Phone
        )
    }
}

/// A rectangle in global logical screen coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The state of the focused field after a change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldSnapshot {
    pub field: FieldId,
    /// Increases on every change the backend observes in this field.
    pub revision: u64,
    pub app: AppInfo,
    pub kind: FieldKind,
    pub multiline: bool,
    /// Text before the selection start (the caret when nothing is selected).
    /// At most [`MAX_BEFORE`] bytes, trimmed from the left on a char boundary.
    pub before: String,
    /// True when `before` was trimmed or the backend only sees part of the field.
    pub before_truncated: bool,
    /// Selected text; empty when nothing is selected.
    pub selection: String,
    /// Text after the selection end, at most [`MAX_AFTER`] bytes.
    pub after: String,
    /// Caret rectangle, when the platform reports it.
    pub caret: Option<Rect>,
}

/// What the overlay shows. Replacing the overlay replaces the key gate state.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Overlay {
    #[default]
    Hidden,
    Suggestion(SuggestionView),
    Card(CardView),
}

/// A suggestion as the backend displays it and acts on it.
///
/// Invariants (checked by [`SuggestionView::is_valid`]): `shown <= text.len()`,
/// every offset is a char boundary of `text`, `word_ends` and `step_ends` are
/// strictly ascending, and every `step_ends` entry is greater than `shown`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SuggestionView {
    pub field: FieldId,
    /// The snapshot revision this suggestion continues.
    pub revision: u64,
    /// The whole buffered continuation, which can be longer than what is shown.
    pub text: String,
    /// Bytes of `text` currently visible.
    pub shown: usize,
    /// Byte offsets in `text` where a Take Word unit ends.
    pub word_ends: Vec<usize>,
    /// Byte offsets `shown` moves to on each Show More.
    pub step_ends: Vec<usize>,
    /// `(index, count)` with a 1-based index when alternatives exist.
    pub alternative: Option<(u8, u8)>,
}

impl SuggestionView {
    pub fn is_valid(&self) -> bool {
        let t = &self.text;
        let ascending = |v: &[usize]| v.windows(2).all(|w| w[0] < w[1]);
        self.shown <= t.len()
            && t.is_char_boundary(self.shown)
            && ascending(&self.word_ends)
            && ascending(&self.step_ends)
            && self
                .word_ends
                .iter()
                .all(|&e| e > 0 && e <= t.len() && t.is_char_boundary(e))
            && self
                .step_ends
                .iter()
                .all(|&e| e > self.shown && e <= t.len() && t.is_char_boundary(e))
    }

    /// The visible part of the suggestion.
    pub fn visible(&self) -> &str {
        &self.text[..self.shown]
    }

    /// End of the next Take Word unit within the visible part: the first unit
    /// end, clamped to `shown`. 0 when nothing is visible. Lets a key gate copy
    /// the unit into its own buffer before calling [`SuggestionView::advance`].
    pub fn word_end(&self) -> usize {
        match self.word_ends.first() {
            Some(&e) if e <= self.shown => e,
            _ => self.shown,
        }
    }

    /// Take Word: removes and returns the first word unit within the visible
    /// part. Returns `None` when nothing is visible. Afterwards `shown == 0`
    /// means the suggestion is used up and should be hidden.
    pub fn take_word(&mut self) -> Option<String> {
        let end = self.word_end();
        if end == 0 {
            return None;
        }
        let taken = self.text[..end].to_owned();
        self.advance(end);
        Some(taken)
    }

    /// Take All: removes and returns the whole visible part.
    pub fn take_all(&mut self) -> Option<String> {
        if self.shown == 0 {
            return None;
        }
        let taken = self.text[..self.shown].to_owned();
        self.advance(self.shown);
        Some(taken)
    }

    /// Show More: reveals the next step. Returns false when the buffer has no
    /// further step, in which case the backend reports [`Action::ShowMore`] so
    /// the controller can fetch more.
    pub fn show_more(&mut self) -> bool {
        match self.step_ends.iter().position(|&e| e > self.shown) {
            Some(i) => {
                self.shown = self.step_ends[i];
                self.step_ends.drain(..=i);
                true
            }
            None => false,
        }
    }

    /// The user typed `typed`. When the visible text starts with it, the
    /// suggestion advances past it and this returns true. Otherwise the
    /// suggestion no longer applies and this returns false. Never allocates.
    pub fn type_through(&mut self, typed: &str) -> bool {
        if typed.is_empty() || !self.visible().starts_with(typed) {
            return false;
        }
        self.advance(typed.len());
        true
    }

    /// Drops the first `n` bytes, which must be visible and end on a char
    /// boundary, shifting every offset. Never allocates.
    ///
    /// # Panics
    /// When `n > shown` or `n` is not a char boundary.
    pub fn advance(&mut self, n: usize) {
        assert!(
            n <= self.shown && self.text.is_char_boundary(n),
            "advance past the visible text"
        );
        self.text.drain(..n);
        self.shown -= n;
        let shift = |v: &mut Vec<usize>| {
            v.retain(|&e| e > n);
            v.iter_mut().for_each(|e| *e -= n);
        };
        shift(&mut self.word_ends);
        shift(&mut self.step_ends);
    }
}

/// The Reply & rewrite card. While a card is shown the backend sends every key to
/// the controller as [`CardKey`] and none to the app.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardView {
    pub field: FieldId,
    pub title: String,
    /// Reply directions or rewrite actions, numbered from 1 on screen.
    pub items: Vec<String>,
    pub selected: Option<usize>,
    /// `Some` while the user types a custom direction; rendered with a caret.
    pub input: Option<String>,
    /// Draft text; grows while it streams.
    pub preview: Option<String>,
    /// Progress or error line.
    pub status: Option<String>,
    /// Key hints, such as "Enter insert · Esc close".
    pub hint: String,
}

/// Actions bound to keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    TakeWord,
    TakeAll,
    ShowMore,
    NextAlternative,
    PrevAlternative,
    Dismiss,
    Reply,
}

/// Keys that can be bound. `Char` is the character the key produces in the
/// current layout with the given modifiers, so "`" works on any layout that
/// has a backtick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Tab,
    Escape,
    Enter,
    Backspace,
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    Char(char),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mods {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    /// Super, Windows or Command.
    #[serde(default)]
    pub meta: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chord {
    pub key: Key,
    #[serde(default)]
    pub mods: Mods,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    LeftAlt,
    RightAlt,
    LeftCtrl,
    RightCtrl,
    LeftShift,
    RightShift,
    LeftMeta,
    RightMeta,
}

/// The Reply & rewrite trigger: a chord, or a modifier pressed and released on its
/// own. A modifier held while another key is pressed keeps its normal meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Chord(Chord),
    ModifierTap(Modifier),
}

/// Key bindings. `None` unbinds an action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bindings {
    pub take_word: Option<Chord>,
    pub take_all: Option<Chord>,
    pub show_more: Option<Chord>,
    pub next_alternative: Option<Chord>,
    pub prev_alternative: Option<Chord>,
    pub dismiss: Option<Chord>,
    pub reply: Option<Trigger>,
}

impl Default for Bindings {
    fn default() -> Self {
        let plain = |key| {
            Some(Chord {
                key,
                mods: Mods::default(),
            })
        };
        let alt = |key| {
            Some(Chord {
                key,
                mods: Mods {
                    alt: true,
                    ..Mods::default()
                },
            })
        };
        Bindings {
            take_word: plain(Key::Tab),
            take_all: plain(Key::Char('`')),
            show_more: alt(Key::Right),
            next_alternative: alt(Key::Down),
            prev_alternative: alt(Key::Up),
            dismiss: plain(Key::Escape),
            reply: Some(Trigger::ModifierTap(Modifier::RightAlt)),
        }
    }
}

/// How to insert text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Replace {
    /// Insert at the caret.
    #[default]
    Nothing,
    /// Replace the current selection.
    Selection,
    /// Delete this many bytes before and after the caret first.
    Around { before: usize, after: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertRequest {
    pub id: u64,
    pub field: FieldId,
    pub text: String,
    pub replace: Replace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    Failed(String),
    /// The platform gave no confirmation either way.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DismissReason {
    Escape,
    /// The user typed something the suggestion did not predict.
    Mismatch,
    CaretMoved,
    FocusLost,
    Clicked,
}

/// Keys while a card is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKey {
    Up,
    Down,
    /// Digit 1–9, as a 1-based item number.
    Pick(u8),
    Enter,
    Escape,
    Tab,
    Backspace,
    Char(char),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendStatus {
    Ready,
    /// A required permission is missing; the text says which.
    PermissionMissing(String),
    /// Another app holds secure keyboard input.
    SecureInput {
        app: Option<String>,
    },
    /// The platform cannot provide the service; the text says why.
    Unavailable(String),
}

/// What backends report to the controller.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A text field gained focus.
    Focus(FieldSnapshot),
    /// No editable field has focus.
    Blur,
    /// The focused field's text, caret or selection changed.
    Changed(FieldSnapshot),
    /// The key gate inserted suggestion text (Take Word or Take All).
    Accepted {
        field: FieldId,
        text: String,
        whole: bool,
    },
    /// The user typed characters the suggestion predicted; the gate advanced it.
    TypedThrough {
        field: FieldId,
        text: String,
    },
    /// The gate hid the suggestion.
    Dismissed {
        field: FieldId,
        reason: DismissReason,
    },
    /// A bound action the controller must handle (Show More past the buffer,
    /// alternatives, Reply & rewrite).
    Action(Action),
    CardKey(CardKey),
    InsertDone {
        id: u64,
        outcome: InsertOutcome,
    },
    Status(BackendStatus),
}

/// Who wrote a message in a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Me,
    Other(String),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub author: Author,
    pub text: String,
}

/// The conversation visible around the focused field, oldest message first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conversation {
    pub messages: Vec<Message>,
}

/// An OS backend. Every method returns promptly; none blocks the key path.
pub trait Backend: Send + Sync {
    /// Replaces what the overlay shows and the state the key gate acts on.
    fn set_overlay(&self, overlay: Overlay);
    fn set_bindings(&self, bindings: Bindings);
    /// Applies the user's appearance settings: the suggestion style (faint or
    /// high contrast) and whether suggestions are announced through the
    /// platform's accessibility API.
    fn set_appearance(&self, appearance: Appearance);
    /// `false` pauses everything: keys pass straight through, the overlay is
    /// hidden, and where possible the backend stops intercepting input at all.
    fn set_active(&self, active: bool);
    /// Inserts text; the outcome arrives as [`Event::InsertDone`].
    fn insert(&self, request: InsertRequest);
    /// Reads the visible conversation for Reply & rewrite. May block for a while;
    /// the caller runs it off the key path. `None` when unavailable.
    fn conversation(&self, field: FieldId) -> Option<Conversation>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(text: &str, shown: usize, words: &[usize], steps: &[usize]) -> SuggestionView {
        SuggestionView {
            text: text.into(),
            shown,
            word_ends: words.into(),
            step_ends: steps.into(),
            ..Default::default()
        }
    }

    #[test]
    fn take_word_then_all() {
        let mut v = view("see you tomorrow", 7, &[4, 8, 16], &[16]);
        assert!(v.is_valid());
        assert_eq!(v.take_word().as_deref(), Some("see "));
        assert_eq!(v.visible(), "you");
        assert!(v.is_valid());
        assert_eq!(v.take_all().as_deref(), Some("you"));
        assert_eq!(v.shown, 0);
        assert!(v.show_more());
        assert_eq!(v.visible(), " tomorrow");
    }

    #[test]
    fn type_through_shrinks_and_mismatch_rejects() {
        let mut v = view("héllo there", 6, &[6, 12], &[]);
        assert!(v.type_through("h"));
        assert_eq!(v.visible(), "éllo");
        assert!(v.is_valid());
        assert!(!v.type_through("x"));
    }

    #[test]
    fn take_word_without_unit_takes_visible() {
        let mut v = view("ok", 2, &[], &[]);
        assert_eq!(v.take_word().as_deref(), Some("ok"));
        assert_eq!(v.take_word(), None);
    }
}
