//! The controller: a pure state machine that turns backend events, model
//! results and timers into overlay updates, insertions and model requests.
//!
//! It performs no I/O and reads no clock. The app runtime feeds it [`Input`]s
//! and executes the [`Output`]s it returns, which keeps every product rule in
//! one place and testable without an OS or a model. After constructing it,
//! send [`Input::Settings`] once to receive the initial bindings, appearance,
//! active state and status.
//!
//! # Autocomplete
//!
//! An edit restarts the pause timer (and, when the provider can prefill and
//! the pause is longer than [`PREFILL_AFTER`], a shorter prefill timer); the
//! pause ends in one [`Output::Complete`]. At most one completion is in
//! flight: a new edit cancels it, and a result whose id, field or revision no
//! longer matches is dropped. The result is cut ([`crate::stop::cut`]) into
//! the suggestion shown, with alternatives when the provider produced them.
//!
//! The backend advances the suggestion itself on Take Word, Take All, Show
//! More and typing-through. The controller mirrors those steps, so a
//! following snapshot that equals the old text plus what was taken or typed
//! keeps the suggestion as it is: no new request, no redraw. Snapshots that
//! confirm only part of it, as when key events overtake field
//! notifications, are matched step by step. Anything else hides it.
//!
//! It stays silent in secure and structured fields, in apps turned off by a
//! rule or by default (code editors, password managers), while paused or off,
//! without a model, in an empty field, at a plain shell prompt in a terminal,
//! right after a question mark, with text selected, and with the caret inside
//! a word.
//!
//! # Reply & rewrite
//!
//! [`Action::Reply`] opens a rewrite card when text is selected and a
//! reply card otherwise. A reply card reads the conversation, asks for three
//! directions (retrying once when they come back invalid), and drafts the
//! chosen or typed direction into a streamed preview; a rewrite card offers
//! the built-in and custom actions. Enter inserts the finished preview,
//! replacing the selection for rewrites. Nothing is ever sent.
//!
//! # Learn my length
//!
//! When `learn_length` is on, every whole suggestion taken counts +1 for the
//! app and every suggestion dismissed with Esc or typed over counts −1. At
//! ±[`LEARN_STEP`] the app's Auto ceiling moves one level longer or shorter
//! (Word to Paragraph) and the count restarts. An explicit per-app ceiling
//! always wins. The learned steps live for the session.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::apps;
use crate::complete::{
    Caps, CompleteError, Completion, CompletionRequest, GenerateRequest, LengthMode, StopParams,
};
use crate::draft::{self, DraftContext, REWRITE_ACTIONS, Rewrite};
use crate::filter;
use crate::io::{
    Action, AppInfo, BackendStatus, Bindings, CardKey, CardView, Conversation, DismissReason,
    Event, FieldId, FieldKind, FieldSnapshot, InsertOutcome, InsertRequest, Message, Overlay,
    Replace, SuggestionView,
};
use crate::prompt;
use crate::settings::{AppCategory, Appearance, Ceilings, Settings};
use crate::stop::{self, CutContext};
use crate::text::is_newline;

/// Timers the controller asks the runtime to run. Starting a timer with an id
/// that is already running restarts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerId {
    /// The pause after typing before a suggestion is requested.
    Pause,
    /// Prefill debounce while the user is still typing.
    Prefill,
    /// The pause in an empty reply box before offering a draft.
    DraftOffer,
}

/// Which model serves a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Suggestions while typing.
    Autocomplete,
    /// Reply & rewrite.
    Reply,
}

/// Progress of a [`Output::Generate`] request.
#[derive(Clone, Debug, PartialEq)]
pub enum GenerateUpdate {
    /// More text, appended to what came before.
    Delta(String),
    /// The whole text, or why it failed.
    Done(Result<String, CompleteError>),
}

/// What is paused from the tray. The runtime owns the resume times and sends
/// the new state whenever it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pause {
    /// Everything is paused.
    pub all: bool,
    /// Apps paused on their own, by `AppInfo::id`.
    pub apps: Vec<String>,
}

/// Everything the controller reacts to.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// Something the backend observed.
    Event(Event),
    /// The result of [`Output::Complete`] with this id.
    Completed {
        id: u64,
        result: Result<Completion, CompleteError>,
    },
    /// Progress of [`Output::Generate`] with this id.
    Generated { id: u64, update: GenerateUpdate },
    /// A timer started with [`Output::StartTimer`] elapsed.
    Timer(TimerId),
    /// The answer to [`Output::ReadConversation`].
    Conversation {
        field: FieldId,
        conversation: Option<Conversation>,
    },
    /// New settings, already validated.
    Settings(Box<Settings>),
    /// Capabilities of the current model for a feature; `None` when no model
    /// is configured or loaded.
    Caps {
        feature: Feature,
        caps: Option<Caps>,
    },
    /// New clipboard text, already checked against password-manager markers.
    Clipboard(String),
    /// The tray's pause state.
    Pause(Pause),
    /// Recent messages the user sent in the focused app, oldest first, from
    /// style memory. Sent after each focus change while style memory is on.
    Style(Vec<String>),
}

/// Usage counters; never contain typed text. `app` is `AppInfo::id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stat {
    /// A suggestion appeared.
    Shown { app: String },
    /// The user took `chars` characters of a suggestion; `whole` for Take
    /// All.
    Accepted {
        app: String,
        chars: u32,
        whole: bool,
    },
    /// The user dismissed a suggestion or typed something else.
    Dismissed { app: String },
    /// A reply or rewrite was inserted.
    DraftInserted { app: String },
}

/// Tray status the controller derives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Suggestions are on.
    Active,
    /// Paused or turned off globally, or paused for the focused app.
    Paused,
    /// Turned off in the focused app, by a rule or by default.
    OffInApp(AppInfo),
    /// No autocomplete model is configured or loaded.
    NoModel,
    /// The cloud provider's last request failed for this reason.
    CloudUnavailable(String),
    /// The backend reported a problem.
    Backend(crate::io::BackendStatus),
}

/// Everything the controller asks the runtime to do, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Pass to [`crate::io::Backend::set_overlay`].
    Overlay(Overlay),
    /// Pass to [`crate::io::Backend::insert`].
    Insert(InsertRequest),
    /// Pass to [`crate::io::Backend::set_bindings`].
    Bindings(Bindings),
    /// Emitted at start and whenever the appearance settings change.
    Appearance(Appearance),
    /// Pass to [`crate::io::Backend::set_active`].
    Active(bool),
    /// Run on the autocomplete model with a
    /// [`Stopper`](crate::stop::Stopper) built from `stop`, and answer with
    /// [`Input::Completed`].
    Complete(CompletionRequest),
    /// Warm the autocomplete model's cache for this context; no answer.
    Prefill(CompletionRequest),
    /// Cancel the completion or prefill with this id.
    Cancel(u64),
    /// Run on the reply & rewrite model, streaming [`Input::Generated`].
    Generate(GenerateRequest),
    /// Cancel the generation with this id.
    CancelGenerate(u64),
    /// Start or restart a timer; answer with [`Input::Timer`] when it elapses.
    StartTimer { id: TimerId, after: Duration },
    /// Stop a timer if it is running.
    StopTimer(TimerId),
    /// Call [`crate::io::Backend::conversation`] off the key path and answer
    /// with [`Input::Conversation`].
    ReadConversation(FieldId),
    /// Count this in the usage stats.
    Record(Stat),
    /// Show this in the tray.
    Status(Status),
    /// A message the user sent, for style memory: already secret-filtered.
    /// Emitted only while style memory is on, in chat and email apps, when a
    /// field with text is emptied in one change.
    Remember { app: String, text: String },
}

/// Delay before prefilling after an edit; used when the pause is longer.
pub const PREFILL_AFTER: Duration = Duration::from_millis(10);
/// Pause in an empty reply box before a draft is offered, when enabled.
pub const DRAFT_OFFER_AFTER: Duration = Duration::from_millis(1500);
/// Net whole suggestions taken (or dismissed) that lengthen (or shorten) an
/// app's Auto ceiling by one level.
pub const LEARN_STEP: i8 = 4;

/// The focused field and the rules that apply to it.
#[derive(Debug)]
struct Field {
    snap: FieldSnapshot,
    category: AppCategory,
    /// Not turned off by a rule, by default or by a per-app pause.
    on: bool,
    stop: StopParams,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    Main,
    /// Show More ran past the buffer.
    More,
    /// Alternatives fetched on demand.
    Alternatives,
}

#[derive(Debug)]
struct Request {
    id: u64,
    field: FieldId,
    revision: u64,
    purpose: Purpose,
    stop: StopParams,
}

/// The backend's suggestion as the controller mirrors it. `view.shown` is a
/// lower bound, since a successful Show More is not reported.
#[derive(Debug)]
struct Shown {
    view: SuggestionView,
    /// Text taken or typed through that no snapshot has confirmed yet, with
    /// the end of each step in it.
    pending: String,
    steps: Vec<usize>,
    /// Every distinct candidate, the one first shown at 0. Dropped on the
    /// first step through the suggestion.
    alternatives: Vec<SuggestionView>,
    index: usize,
    /// Alternatives were produced with the suggestion or fetched already.
    fetched: bool,
}

enum Confirm {
    Keep,
    UsedUp,
    Mismatch,
}

impl Shown {
    fn new(views: Vec<SuggestionView>, fetched: bool) -> Shown {
        Shown {
            view: views[0].clone(),
            pending: String::new(),
            steps: Vec::new(),
            alternatives: views,
            index: 0,
            fetched,
        }
    }

    /// Applies a step the backend took: `taken` left the front of the view.
    fn step(&mut self, taken: &str, whole: bool) -> bool {
        let v = &mut self.view;
        if taken.is_empty() || !v.text.starts_with(taken) {
            return false;
        }
        let n = taken.len();
        v.shown = v.shown.max(n);
        v.step_ends.retain(|&e| e > v.shown);
        v.advance(n);
        if whole {
            v.shown = 0;
        }
        self.pending.push_str(taken);
        self.steps.push(self.pending.len());
        self.alternatives.clear();
        self.index = 0;
        true
    }

    /// Whether `new` is `old` plus a prefix of the pending steps.
    fn confirm(&mut self, old: &FieldSnapshot, new: &FieldSnapshot) -> Confirm {
        if !new.selection.is_empty() || new.after != old.after {
            return Confirm::Mismatch;
        }
        let matched = self.steps.iter().rev().copied().chain([0]).find(|&k| {
            let typed = &self.pending[..k];
            new.before.strip_suffix(typed).is_some_and(|head| {
                head == old.before || (new.before_truncated && old.before.ends_with(head))
            })
        });
        let Some(k) = matched else {
            return Confirm::Mismatch;
        };
        self.pending.drain(..k);
        self.steps.retain(|&e| e > k);
        self.steps.iter_mut().for_each(|e| *e -= k);
        self.view.revision = new.revision;
        for a in &mut self.alternatives {
            a.revision = new.revision;
        }
        if self.pending.is_empty() && self.view.shown == 0 {
            Confirm::UsedUp
        } else {
            Confirm::Keep
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    Language,
    Tone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    Directions,
    Draft,
}

/// The reply & rewrite card.
#[derive(Debug)]
struct Card {
    field: FieldId,
    app: String,
    /// The selected text, for a rewrite.
    selection: Option<String>,
    items: Vec<String>,
    selected: Option<usize>,
    input: Option<String>,
    asking: Option<Ask>,
    preview: Option<String>,
    /// The preview is complete and can be inserted.
    ready: bool,
    status: Option<String>,
    job: Option<(u64, Job)>,
    inserting: Option<u64>,
    reading: bool,
    retried: bool,
    direction: Option<String>,
    conversation: Vec<Message>,
}

impl Card {
    fn new(field: &FieldSnapshot) -> Card {
        Card {
            field: field.field,
            app: field.app.id.clone(),
            selection: None,
            items: Vec::new(),
            selected: None,
            input: None,
            asking: None,
            preview: None,
            ready: false,
            status: None,
            job: None,
            inserting: None,
            reading: false,
            retried: false,
            direction: None,
            conversation: Vec::new(),
        }
    }

    fn view(&self) -> CardView {
        let hint = if self.inserting.is_some() {
            String::new()
        } else if self.input.is_some() {
            "Enter write · Esc close".to_owned()
        } else if self.ready {
            "Enter insert · Tab edit · Esc close".to_owned()
        } else if !self.items.is_empty() && self.job.is_none() {
            format!(
                "1–{} pick · ↑↓ choose · type your own · Esc close",
                self.items.len().min(9)
            )
        } else {
            "Esc close".to_owned()
        };
        CardView {
            field: self.field,
            title: if self.selection.is_some() {
                "Rewrite"
            } else {
                "Reply"
            }
            .to_owned(),
            items: self.items.clone(),
            selected: self.selected,
            input: self.input.clone(),
            preview: self.preview.clone(),
            status: self.status.clone(),
            hint,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Learned {
    score: i8,
    shift: i8,
}

#[derive(Clone, Copy, Debug, Default)]
struct Timers {
    pause: bool,
    prefill: bool,
    offer: bool,
}

/// See the module documentation.
#[derive(Debug)]
pub struct Controller {
    settings: Settings,
    caps: Option<Caps>,
    draft_caps: Option<Caps>,
    pause: Pause,
    backend: Option<BackendStatus>,
    cloud_error: Option<String>,
    status: Option<Status>,
    field: Option<Field>,
    conversation: Option<Conversation>,
    clipboard: Option<String>,
    style: Vec<String>,
    request: Option<Request>,
    /// Id and revision of the last prefill.
    prefill: Option<(u64, u64)>,
    timers: Timers,
    shown: Option<Shown>,
    card: Option<Card>,
    learned: BTreeMap<String, Learned>,
    next_id: u64,
}

const LEVELS: [LengthMode; 4] = [
    LengthMode::Word,
    LengthMode::Phrase,
    LengthMode::Sentence,
    LengthMode::Paragraph,
];

/// `mode` moved `by` levels along Word..Paragraph.
fn shift_level(mode: LengthMode, by: i8) -> LengthMode {
    let Some(i) = LEVELS.iter().position(|&m| m == mode) else {
        return mode;
    };
    LEVELS[(i as i64 + i64::from(by)).clamp(0, 3) as usize]
}

/// True when the caret's line in a terminal is a plain shell prompt: a
/// prompt marker (`$`, `#`, `%`, `❯`, `➜`, `λ`, `»`) followed by a space, or
/// `>` after a path as fish, PowerShell and cmd print it. A bare `> ` and
/// lines inside box-drawing characters belong to programs running in the
/// terminal, such as chat tools, where suggestions are wanted.
fn at_shell_prompt(before: &str) -> bool {
    let line = before.rsplit(is_newline).next().unwrap_or("");
    let first = line.trim_start().chars().next();
    if first.is_some_and(|c| ('\u{2500}'..='\u{259F}').contains(&c)) {
        return false;
    }
    let cmd =
        line.len() > 3 && line.as_bytes()[0].is_ascii_alphabetic() && line[1..].starts_with(":\\");
    for (i, m) in line.match_indices(['$', '#', '%', '❯', '➜', 'λ', '»', '>']) {
        let rest = &line[i + m.len()..];
        let spaced = rest.is_empty() || rest.starts_with(' ');
        if m == ">" {
            let prefix = line[..i].trim();
            if cmd || (spaced && (prefix.contains(['/', '~', '\\']) || prefix.starts_with("PS "))) {
                return true;
            }
            if spaced {
                return false;
            }
        } else if spaced {
            return true;
        }
    }
    false
}

/// Silence rules that depend on the field's content.
fn content_silent(snap: &FieldSnapshot, category: AppCategory) -> bool {
    let before = snap.before.as_str();
    !snap.selection.is_empty()
        || before.trim().is_empty()
        || before.trim_end().ends_with(['?', '？', '¿'])
        || (before.ends_with(char::is_alphanumeric)
            && snap.after.starts_with(char::is_alphanumeric))
        || ((category == AppCategory::Terminal || snap.kind == FieldKind::Terminal)
            && at_shell_prompt(before))
}

fn field_text(s: &FieldSnapshot) -> String {
    [s.before.as_str(), &s.selection, &s.after].concat()
}

impl Controller {
    /// A controller with these settings and no model yet.
    pub fn new(settings: Settings) -> Controller {
        Controller {
            settings,
            caps: None,
            draft_caps: None,
            pause: Pause::default(),
            backend: None,
            cloud_error: None,
            status: None,
            field: None,
            conversation: None,
            clipboard: None,
            style: Vec::new(),
            request: None,
            prefill: None,
            timers: Timers::default(),
            shown: None,
            card: None,
            learned: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Handles one input and returns what to do, in order.
    pub fn handle(&mut self, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        match input {
            Input::Event(event) => self.event(event, &mut out),
            Input::Completed { id, result } => self.completed(id, result, &mut out),
            Input::Generated { id, update } => self.generated(id, update, &mut out),
            Input::Timer(timer) => self.timer(timer, &mut out),
            Input::Conversation {
                field,
                conversation,
            } => self.conversation(field, conversation, &mut out),
            Input::Settings(settings) => self.set_settings(*settings, &mut out),
            Input::Caps {
                feature: Feature::Autocomplete,
                caps,
            } => {
                self.caps = caps;
                if caps.is_none() {
                    self.quiet(&mut out);
                }
                self.update_status(&mut out);
            }
            Input::Caps {
                feature: Feature::Reply,
                caps,
            } => self.draft_caps = caps,
            Input::Clipboard(text) => self.clipboard = (!text.is_empty()).then_some(text),
            Input::Pause(pause) => {
                let was = self.active();
                self.pause = pause;
                if self.active() != was {
                    out.push(Output::Active(self.active()));
                }
                self.refresh_policy(&mut out);
            }
            Input::Style(samples) => self.style = samples,
        }
        out
    }

    fn active(&self) -> bool {
        self.settings.enabled && !self.pause.all
    }

    fn id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn event(&mut self, event: Event, out: &mut Vec<Output>) {
        match event {
            Event::Focus(snap) => self.focus(snap, out),
            Event::Blur => {
                self.leave_field(out);
                self.field = None;
                self.update_status(out);
            }
            Event::Changed(snap) => self.changed(snap, out),
            Event::Accepted { field, text, whole } => {
                let Some(app) = self.app_of(field) else {
                    return;
                };
                out.push(Output::Record(Stat::Accepted {
                    app: app.clone(),
                    chars: u32::try_from(text.chars().count()).unwrap_or(u32::MAX),
                    whole,
                }));
                if whole {
                    self.learn(&app, 1);
                }
                self.stepped(&text, whole, out);
            }
            Event::TypedThrough { field, text } => {
                if self.app_of(field).is_some() {
                    self.stepped(&text, false, out);
                }
            }
            Event::Dismissed { field, reason } => {
                let Some(app) = self.app_of(field) else {
                    return;
                };
                self.shown = None;
                if self
                    .request
                    .as_ref()
                    .is_some_and(|r| r.purpose != Purpose::Main)
                {
                    self.cancel_request(out);
                }
                if matches!(reason, DismissReason::Escape | DismissReason::Mismatch) {
                    out.push(Output::Record(Stat::Dismissed { app: app.clone() }));
                    self.learn(&app, -1);
                }
            }
            Event::Action(Action::ShowMore) => self.more(out),
            Event::Action(Action::NextAlternative) => self.cycle(1, out),
            Event::Action(Action::PrevAlternative) => self.cycle(-1, out),
            Event::Action(Action::Reply) => self.open_card(out),
            Event::Action(Action::TakeWord | Action::TakeAll | Action::Dismiss) => {}
            Event::CardKey(key) => self.card_key(key, out),
            Event::InsertDone { id, outcome } => self.insert_done(id, outcome, out),
            Event::Status(status) => {
                self.backend = (status != BackendStatus::Ready).then_some(status);
                self.update_status(out);
            }
        }
    }

    /// The app id of the focused field, when `field` is it.
    fn app_of(&self, field: FieldId) -> Option<String> {
        self.field
            .as_ref()
            .filter(|f| f.snap.field == field)
            .map(|f| f.snap.app.id.clone())
    }

    /// Category, on/off and stop rule for an app and field kind.
    fn policy(&self, app: &AppInfo, kind: FieldKind) -> (AppCategory, bool, StopParams) {
        let s = &self.settings;
        let rule = s.apps.get(&app.id);
        let own = rule
            .and_then(|r| r.category)
            .unwrap_or_else(|| apps::category(app));
        let category = match kind {
            FieldKind::Search => AppCategory::Search,
            FieldKind::Terminal => AppCategory::Terminal,
            FieldKind::Code => AppCategory::Code,
            _ => own,
        };
        let default_off = category == AppCategory::Code
            || (rule.is_none_or(|r| r.category.is_none()) && apps::off_by_default(app));
        let on = !self.pause.apps.contains(&app.id)
            && rule.and_then(|r| r.enabled).unwrap_or(!default_off);
        let (mode, ceiling) = if kind == FieldKind::Search {
            (LengthMode::Auto, s.ceilings.search)
        } else {
            let learned = if s.learn_length {
                self.learned.get(&app.id).map_or(0, |l| l.shift)
            } else {
                0
            };
            let ceiling = rule
                .and_then(|r| r.ceiling)
                .unwrap_or_else(|| shift_level(s.ceilings.for_category(category), learned));
            (rule.and_then(|r| r.length).unwrap_or(s.length), ceiling)
        };
        let ceiling = if ceiling == LengthMode::Auto {
            Ceilings::default().for_category(category)
        } else {
            ceiling
        };
        let stop = StopParams {
            mode,
            ceiling: if mode == LengthMode::Auto {
                ceiling
            } else {
                mode
            },
            boldness: s.boldness,
            paragraph_words: s.paragraph_words,
        };
        (category, on, stop)
    }

    fn make_field(&self, mut snap: FieldSnapshot) -> Field {
        if snap.kind.is_secure_or_structured() {
            // Never keep what a secure field holds.
            snap.before.clear();
            snap.selection.clear();
            snap.after.clear();
        }
        let (category, on, stop) = self.policy(&snap.app, snap.kind);
        Field {
            snap,
            category,
            on,
            stop,
        }
    }

    /// True when a suggestion may be requested and shown now.
    fn can_suggest(&self) -> bool {
        let Some(f) = &self.field else { return false };
        self.active()
            && self.caps.is_some()
            && self.card.is_none()
            && f.on
            && !f.snap.kind.is_secure_or_structured()
            && !content_silent(&f.snap, f.category)
    }

    /// True when the focused field may use reply & rewrite at all.
    fn draft_allowed(&self) -> bool {
        self.settings.reply.enabled
            && self.active()
            && self
                .field
                .as_ref()
                .is_some_and(|f| f.on && !f.snap.kind.is_secure_or_structured())
    }

    fn focus(&mut self, snap: FieldSnapshot, out: &mut Vec<Output>) {
        self.leave_field(out);
        self.conversation = None;
        self.style.clear();
        let field = self.make_field(snap);
        let read = self.settings.context.conversation
            && field.on
            && !field.snap.kind.is_secure_or_structured()
            && matches!(field.category, AppCategory::Chat | AppCategory::Email);
        let id = field.snap.field;
        self.field = Some(field);
        self.update_status(out);
        if read {
            out.push(Output::ReadConversation(id));
        }
        if self.can_suggest() {
            self.arm_prefill(out);
        }
        self.arm_offer(out);
    }

    /// Hides and cancels everything tied to the focused field.
    fn leave_field(&mut self, out: &mut Vec<Output>) {
        self.close_card(out);
        self.quiet(out);
        if self.timers.offer {
            self.timers.offer = false;
            out.push(Output::StopTimer(TimerId::DraftOffer));
        }
    }

    /// Hides the suggestion, cancels the request and stops the timers.
    fn quiet(&mut self, out: &mut Vec<Output>) {
        if self.shown.take().is_some() {
            out.push(Output::Overlay(Overlay::Hidden));
        }
        self.cancel_request(out);
        self.cancel_prefill(out);
        if self.timers.pause {
            self.timers.pause = false;
            out.push(Output::StopTimer(TimerId::Pause));
        }
        if self.timers.prefill {
            self.timers.prefill = false;
            out.push(Output::StopTimer(TimerId::Prefill));
        }
    }

    fn cancel_request(&mut self, out: &mut Vec<Output>) {
        if let Some(r) = self.request.take() {
            out.push(Output::Cancel(r.id));
        }
    }

    fn cancel_prefill(&mut self, out: &mut Vec<Output>) {
        if let Some((id, _)) = self.prefill.take() {
            out.push(Output::Cancel(id));
        }
    }

    fn arm_prefill(&mut self, out: &mut Vec<Output>) {
        if self.caps.is_some_and(|c| c.prefill)
            && u64::from(self.settings.pause_ms) > PREFILL_AFTER.as_millis() as u64
        {
            self.timers.prefill = true;
            out.push(Output::StartTimer {
                id: TimerId::Prefill,
                after: PREFILL_AFTER,
            });
        }
    }

    /// Starts or stops the draft-offer timer for the field's current state.
    fn arm_offer(&mut self, out: &mut Vec<Output>) {
        let want = self.settings.reply.auto_offer
            && self.draft_allowed()
            && self.card.is_none()
            && self.field.as_ref().is_some_and(|f| {
                matches!(f.category, AppCategory::Chat | AppCategory::Email)
                    && field_text(&f.snap).trim().is_empty()
            });
        if want {
            self.timers.offer = true;
            out.push(Output::StartTimer {
                id: TimerId::DraftOffer,
                after: DRAFT_OFFER_AFTER,
            });
        } else if self.timers.offer {
            self.timers.offer = false;
            out.push(Output::StopTimer(TimerId::DraftOffer));
        }
    }

    fn changed(&mut self, snap: FieldSnapshot, out: &mut Vec<Output>) {
        let relabel = self.field.as_ref().is_some_and(|f| {
            f.snap.field == snap.field && (f.snap.kind != snap.kind || f.snap.app != snap.app)
        });
        if relabel {
            let (category, on, stop) = self.policy(&snap.app, snap.kind);
            if let Some(f) = &mut self.field {
                (f.category, f.on, f.stop) = (category, on, stop);
            }
        }
        let Some(field) = self.field.as_mut().filter(|f| f.snap.field == snap.field) else {
            return self.focus(snap, out);
        };
        let mut snap = snap;
        if snap.kind.is_secure_or_structured() {
            snap.before.clear();
            snap.selection.clear();
            snap.after.clear();
        }
        let old = std::mem::replace(&mut field.snap, snap);
        let new = &field.snap;
        let chatty = matches!(field.category, AppCategory::Chat | AppCategory::Email);
        let old_text = !old.before.trim().is_empty() || !old.after.trim().is_empty();
        let emptied =
            old_text && new.before.is_empty() && new.after.is_empty() && new.selection.is_empty();
        if emptied && chatty && field.on {
            if self.settings.context.style_memory {
                let text = field_text(&old);
                if !filter::contains_secret(&text) && !text.trim().is_empty() {
                    out.push(Output::Remember {
                        app: old.app.id.clone(),
                        text: text.trim().to_owned(),
                    });
                }
            }
            if self.settings.context.conversation && self.card.is_none() {
                out.push(Output::ReadConversation(new.field));
            }
        }
        if self.card.is_some() {
            return;
        }
        if let Some(shown) = &mut self.shown {
            match shown.confirm(&old, new) {
                Confirm::Keep => return,
                Confirm::UsedUp => self.shown = None,
                Confirm::Mismatch => {
                    self.shown = None;
                    out.push(Output::Overlay(Overlay::Hidden));
                }
            }
        }
        self.cancel_request(out);
        self.cancel_prefill(out);
        if self.can_suggest() {
            self.timers.pause = true;
            out.push(Output::StartTimer {
                id: TimerId::Pause,
                after: Duration::from_millis(self.settings.pause_ms.into()),
            });
            self.arm_prefill(out);
        } else {
            self.quiet(out);
        }
        self.arm_offer(out);
    }

    /// The backend took `taken` from the front of its suggestion.
    fn stepped(&mut self, taken: &str, whole: bool, out: &mut Vec<Output>) {
        if let Some(shown) = &mut self.shown
            && !shown.step(taken, whole)
        {
            self.shown = None;
            out.push(Output::Overlay(Overlay::Hidden));
        }
    }

    fn request(&mut self, purpose: Purpose, out: &mut Vec<Output>) {
        let (Some(field), Some(caps)) = (&self.field, self.caps) else {
            return;
        };
        let pending = match (&self.shown, purpose) {
            (Some(s), Purpose::More) => [s.pending.as_str(), &s.view.text].concat(),
            (Some(s), _) => s.pending.clone(),
            (None, _) => String::new(),
        };
        let segments = prompt::build(
            &field.snap,
            &self.settings,
            &prompt::Context {
                conversation: self.conversation.as_ref(),
                clipboard: self.clipboard.as_deref(),
                style: &self.style,
                pending: &pending,
            },
        );
        let stop = match purpose {
            // The user asked for more: only the first word must clear the
            // boldest threshold.
            Purpose::More => StopParams {
                boldness: 1.0,
                ..field.stop
            },
            _ => field.stop,
        };
        let alternatives = match purpose {
            Purpose::Alternatives => 2,
            Purpose::Main if caps.cheap_alternatives => 2,
            _ => 0,
        };
        let (field_id, revision) = (field.snap.field, field.snap.revision);
        self.cancel_request(out);
        let id = self.id();
        self.request = Some(Request {
            id,
            field: field_id,
            revision,
            purpose,
            stop,
        });
        out.push(Output::Complete(CompletionRequest {
            id,
            field: field_id,
            revision,
            segments,
            stop,
            max_tokens: stop::max_tokens(&stop),
            alternatives,
        }));
    }

    fn timer(&mut self, timer: TimerId, out: &mut Vec<Output>) {
        match timer {
            TimerId::Pause => {
                if !std::mem::take(&mut self.timers.pause) {
                    return;
                }
                let fresh = self.request.is_none() && self.shown.is_none();
                if fresh && self.can_suggest() {
                    self.timers.prefill = false;
                    self.request(Purpose::Main, out);
                }
            }
            TimerId::Prefill => {
                if !std::mem::take(&mut self.timers.prefill) {
                    return;
                }
                let Some(f) = &self.field else { return };
                let revision = f.snap.revision;
                let done = self.prefill.is_some_and(|(_, r)| r == revision);
                if self.request.is_none()
                    && !done
                    && self.caps.is_some_and(|c| c.prefill)
                    && self.can_suggest()
                {
                    let segments = prompt::build(
                        &f.snap,
                        &self.settings,
                        &prompt::Context {
                            conversation: self.conversation.as_ref(),
                            clipboard: self.clipboard.as_deref(),
                            style: &self.style,
                            pending: "",
                        },
                    );
                    let (field, stop) = (f.snap.field, f.stop);
                    let id = self.id();
                    self.prefill = Some((id, revision));
                    out.push(Output::Prefill(CompletionRequest {
                        id,
                        field,
                        revision,
                        segments,
                        stop,
                        max_tokens: stop::max_tokens(&stop),
                        alternatives: 0,
                    }));
                }
            }
            TimerId::DraftOffer => {
                if !std::mem::take(&mut self.timers.offer) {
                    return;
                }
                let empty = self
                    .field
                    .as_ref()
                    .is_some_and(|f| field_text(&f.snap).trim().is_empty());
                if self.settings.reply.auto_offer && empty && self.card.is_none() {
                    self.open_card(out);
                }
            }
        }
    }

    fn completed(
        &mut self,
        id: u64,
        result: Result<Completion, CompleteError>,
        out: &mut Vec<Output>,
    ) {
        let Some(request) = self.request.take_if(|r| r.id == id) else {
            return;
        };
        let Some(field) = &self.field else { return };
        if field.snap.field != request.field || field.snap.revision != request.revision {
            return;
        }
        let completion = match result {
            Ok(c) => {
                if self.cloud_error.take().is_some() {
                    self.update_status(out);
                }
                c
            }
            Err(CompleteError::Cancelled) => return,
            Err(e) => {
                if self.caps.is_some_and(|c| !c.local) {
                    self.cloud_error = Some(e.to_string());
                    self.update_status(out);
                } else {
                    log::warn!("local completion failed: {e}");
                }
                return;
            }
        };
        if self.card.is_some() {
            return;
        }
        match request.purpose {
            Purpose::Main => self.show(completion, &request.stop, out),
            Purpose::More => self.append(completion, &request.stop, out),
            Purpose::Alternatives => self.add_alternatives(completion, &request.stop, out),
        }
    }

    /// Distinct suggestions cut from the candidates, in order.
    fn views(
        &self,
        completion: &Completion,
        stop: &StopParams,
        pending: &str,
    ) -> Vec<SuggestionView> {
        let Some(f) = &self.field else {
            return Vec::new();
        };
        let before = if pending.is_empty() {
            None
        } else {
            Some([f.snap.before.as_str(), pending].concat())
        };
        let ctx = CutContext {
            before: before.as_deref().unwrap_or(&f.snap.before),
            after: &f.snap.after,
        };
        let mut views: Vec<SuggestionView> = Vec::new();
        for c in &completion.candidates {
            if let Some(mut v) = stop::cut(c, stop, &ctx)
                && !views.iter().any(|w| w.visible() == v.visible())
            {
                v.field = f.snap.field;
                v.revision = f.snap.revision;
                views.push(v);
            }
        }
        views
    }

    fn index_alternatives(views: &mut [SuggestionView]) {
        let n = views.len();
        for (i, v) in views.iter_mut().enumerate() {
            v.alternative = (n > 1).then(|| (i as u8 + 1, n as u8));
        }
    }

    fn show(&mut self, completion: Completion, stop: &StopParams, out: &mut Vec<Output>) {
        if !self.can_suggest() || self.shown.is_some() {
            return;
        }
        let mut views = self.views(&completion, stop, "");
        views.truncate(3);
        if views.is_empty() {
            return;
        }
        Self::index_alternatives(&mut views);
        let fetched = self.caps.is_some_and(|c| c.cheap_alternatives) || views.len() > 1;
        let shown = Shown::new(views, fetched);
        out.push(Output::Overlay(Overlay::Suggestion(shown.view.clone())));
        let app = self
            .field
            .as_ref()
            .map(|f| f.snap.app.id.clone())
            .unwrap_or_default();
        out.push(Output::Record(Stat::Shown { app }));
        self.shown = Some(shown);
    }

    /// Show More past the buffer: extends the suggestion with fresh text.
    fn more(&mut self, out: &mut Vec<Output>) {
        let Some(shown) = &mut self.shown else { return };
        // The backend reveals every buffered step before it asks.
        shown.view.shown = shown.view.text.len();
        shown.view.step_ends.clear();
        if self.request.is_none() {
            self.request(Purpose::More, out);
        }
    }

    fn append(&mut self, completion: Completion, stop: &StopParams, out: &mut Vec<Output>) {
        let Some(shown) = &self.shown else { return };
        let pending = [shown.pending.as_str(), &shown.view.text].concat();
        let Some(more) = self.views(&completion, stop, &pending).into_iter().next() else {
            return;
        };
        let Some(shown) = &mut self.shown else { return };
        let v = &mut shown.view;
        let base = v.text.len();
        v.text.push_str(&more.text);
        v.word_ends.extend(
            more.word_ends
                .iter()
                .map(|e| base + e)
                .filter(|&e| e > base),
        );
        v.shown = base + more.shown;
        v.step_ends = more.step_ends.iter().map(|e| base + e).collect();
        v.alternative = None;
        shown.alternatives.clear();
        out.push(Output::Overlay(Overlay::Suggestion(v.clone())));
    }

    fn cycle(&mut self, by: isize, out: &mut Vec<Output>) {
        let Some(shown) = &mut self.shown else { return };
        if !shown.pending.is_empty() {
            return;
        }
        let n = shown.alternatives.len();
        if n > 1 {
            shown.index = (shown.index as isize + by).rem_euclid(n as isize) as usize;
            shown.view = shown.alternatives[shown.index].clone();
            out.push(Output::Overlay(Overlay::Suggestion(shown.view.clone())));
        } else if !shown.fetched {
            shown.fetched = true;
            self.request(Purpose::Alternatives, out);
        }
    }

    fn add_alternatives(
        &mut self,
        completion: Completion,
        stop: &StopParams,
        out: &mut Vec<Output>,
    ) {
        let views = self.views(&completion, stop, "");
        let Some(shown) = &mut self.shown else { return };
        if !shown.pending.is_empty() || shown.alternatives.is_empty() {
            return;
        }
        for v in views {
            if shown.alternatives.len() < 3
                && !shown
                    .alternatives
                    .iter()
                    .any(|w| w.visible() == v.visible())
            {
                shown.alternatives.push(v);
            }
        }
        if shown.alternatives.len() > 1 {
            Self::index_alternatives(&mut shown.alternatives);
            shown.index = 1;
            shown.view = shown.alternatives[1].clone();
            out.push(Output::Overlay(Overlay::Suggestion(shown.view.clone())));
        }
    }

    fn conversation(
        &mut self,
        field: FieldId,
        conversation: Option<Conversation>,
        out: &mut Vec<Output>,
    ) {
        if self.field.as_ref().is_none_or(|f| f.snap.field != field) {
            return;
        }
        self.conversation = conversation;
        if self
            .card
            .as_ref()
            .is_some_and(|c| c.reading && c.field == field)
        {
            if let Some(card) = &mut self.card {
                card.reading = false;
                card.conversation = self
                    .conversation
                    .as_ref()
                    .map(prompt::conversation)
                    .unwrap_or_default();
            }
            self.start_directions(out);
            self.emit_card(out);
        }
    }

    fn set_settings(&mut self, settings: Settings, out: &mut Vec<Output>) {
        let appearance = settings.appearance;
        self.settings = settings;
        out.push(Output::Bindings(self.settings.keys.clone()));
        out.push(Output::Appearance(appearance));
        out.push(Output::Active(self.active()));
        self.refresh_policy(out);
    }

    /// Recomputes the focused field's rules after settings, pause or learning
    /// changed, and goes quiet when it may no longer suggest.
    fn refresh_policy(&mut self, out: &mut Vec<Output>) {
        if let Some(f) = &self.field {
            let (category, on, stop) = self.policy(&f.snap.app, f.snap.kind);
            if let Some(f) = &mut self.field {
                (f.category, f.on, f.stop) = (category, on, stop);
            }
        }
        if !self.can_suggest() {
            self.quiet(out);
        }
        if self.card.is_some() && !self.draft_allowed() {
            self.close_card(out);
        }
        self.update_status(out);
    }

    fn learn(&mut self, app: &str, by: i8) {
        if !self.settings.learn_length {
            return;
        }
        let l = self.learned.entry(app.to_owned()).or_default();
        l.score += by;
        if l.score.abs() < LEARN_STEP {
            return;
        }
        l.shift = (l.shift + l.score.signum()).clamp(-3, 3);
        l.score = 0;
        if let Some(f) = &self.field {
            let (_, _, stop) = self.policy(&f.snap.app, f.snap.kind);
            if let Some(f) = &mut self.field {
                f.stop = stop;
            }
        }
    }

    fn status(&self) -> Status {
        if let Some(b) = &self.backend {
            return Status::Backend(b.clone());
        }
        if self.caps.is_none() {
            return Status::NoModel;
        }
        if !self.active() {
            return Status::Paused;
        }
        if let Some(f) = &self.field {
            if self.pause.apps.contains(&f.snap.app.id) {
                return Status::Paused;
            }
            if !f.on {
                return Status::OffInApp(f.snap.app.clone());
            }
        }
        match &self.cloud_error {
            Some(e) => Status::CloudUnavailable(e.clone()),
            None => Status::Active,
        }
    }

    fn update_status(&mut self, out: &mut Vec<Output>) {
        let status = self.status();
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status.clone());
            out.push(Output::Status(status));
        }
    }

    // Reply & rewrite.

    fn emit_card(&self, out: &mut Vec<Output>) {
        if let Some(card) = &self.card {
            out.push(Output::Overlay(Overlay::Card(card.view())));
        }
    }

    fn close_card(&mut self, out: &mut Vec<Output>) {
        if let Some(card) = self.card.take() {
            if let Some((id, _)) = card.job {
                out.push(Output::CancelGenerate(id));
            }
            out.push(Output::Overlay(Overlay::Hidden));
        }
    }

    fn open_card(&mut self, out: &mut Vec<Output>) {
        if !self.draft_allowed() {
            return;
        }
        self.close_card(out);
        self.shown = None;
        self.quiet(out);
        let Some(f) = &self.field else { return };
        let mut card = Card::new(&f.snap);
        let rewrite = !f.snap.selection.is_empty();
        if rewrite {
            card.selection = Some(f.snap.selection.clone());
            card.items = REWRITE_ACTIONS.iter().map(|s| (*s).to_owned()).collect();
            card.items.extend(
                self.settings
                    .reply
                    .custom_actions
                    .iter()
                    .map(|a| a.name.clone()),
            );
            card.selected = Some(0);
        }
        let field = f.snap.field;
        let reading = !rewrite && self.settings.context.conversation;
        if self.draft_caps.is_none() {
            card.status =
                Some("No model is set up for Reply & rewrite. Choose one in Settings.".to_owned());
        } else if reading {
            card.reading = true;
            card.status = Some("Reading the conversation…".to_owned());
        }
        self.card = Some(card);
        if self.draft_caps.is_some() && !rewrite {
            if reading {
                out.push(Output::ReadConversation(field));
            } else {
                self.start_directions(out);
            }
        }
        self.emit_card(out);
    }

    /// The clipboard, when the user allowed it as context and it holds no
    /// secret.
    fn usable_clipboard(&self) -> Option<&str> {
        self.clipboard
            .as_deref()
            .filter(|c| self.settings.context.clipboard && !filter::contains_secret(c))
    }

    fn draft_context<'a>(&'a self, card: &'a Card) -> Option<DraftContext<'a>> {
        let f = self.field.as_ref()?;
        Some(DraftContext {
            app: Some(&f.snap.app),
            conversation: &card.conversation,
            before: &f.snap.before,
            after: &f.snap.after,
            clipboard: self.usable_clipboard(),
            about_me: &self.settings.context.about_me,
            style: if self.settings.context.style_memory {
                &self.style
            } else {
                &[]
            },
        })
    }

    /// Asks for directions from whatever there is to reply to, or asks the
    /// user to type one.
    fn start_directions(&mut self, out: &mut Vec<Output>) {
        let Some(card) = &self.card else { return };
        let has_text = self
            .field
            .as_ref()
            .is_some_and(|f| !field_text(&f.snap).trim().is_empty());
        let material =
            !card.conversation.is_empty() || has_text || self.usable_clipboard().is_some();
        if !material {
            let hint = if self.settings.context.screen_ocr {
                ""
            } else {
                ", or turn on screen reading in Settings"
            };
            let message = format!("Can't read this conversation. Type a direction{hint}.");
            if let Some(card) = &mut self.card {
                card.status = Some(message);
                card.input = Some(String::new());
            }
            return;
        }
        let id = self.id();
        let Some(card) = &self.card else { return };
        let Some(ctx) = self.draft_context(card) else {
            return;
        };
        let request = draft::directions(&ctx, id, card.retried);
        if let Some(card) = &mut self.card {
            card.job = Some((id, Job::Directions));
            card.status = Some("Thinking of directions…".to_owned());
        }
        out.push(Output::Generate(request));
    }

    fn card_key(&mut self, key: CardKey, out: &mut Vec<Output>) {
        let Some(card) = &mut self.card else { return };
        if card.inserting.is_some() && key != CardKey::Escape {
            return;
        }
        match key {
            CardKey::Escape => return self.close_card(out),
            CardKey::Up | CardKey::Down if !card.items.is_empty() && card.input.is_none() => {
                let n = card.items.len();
                let next = match (card.selected, key) {
                    (None, CardKey::Down) => 0,
                    (None, _) => n - 1,
                    (Some(i), CardKey::Down) => (i + 1) % n,
                    (Some(i), _) => (i + n - 1) % n,
                };
                card.selected = Some(next);
            }
            CardKey::Up | CardKey::Down => {}
            CardKey::Pick(n) if card.input.is_some() => {
                if let (Some(input), Some(d)) =
                    (&mut card.input, char::from_digit(u32::from(n), 10))
                {
                    input.push(d);
                }
            }
            CardKey::Pick(n) => {
                let i = usize::from(n).wrapping_sub(1);
                if i < card.items.len() && card.job.is_none_or(|(_, j)| j != Job::Directions) {
                    card.selected = Some(i);
                    self.choose(i, out);
                }
            }
            CardKey::Char(c) => card.input.get_or_insert_with(String::new).push(c),
            CardKey::Backspace => {
                if let Some(input) = &mut card.input
                    && input.pop().is_none()
                {
                    card.input = None;
                    card.asking = None;
                }
            }
            CardKey::Tab => {
                if card.preview.is_some() {
                    if let Some((id, _)) = card.job.take() {
                        out.push(Output::CancelGenerate(id));
                    }
                    card.input = Some(card.direction.clone().unwrap_or_default());
                    card.preview = None;
                    card.ready = false;
                    card.status = None;
                }
            }
            CardKey::Enter => {
                if let Some(text) = card
                    .input
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    let text = text.to_owned();
                    card.input = None;
                    self.submit(text, out);
                } else if card.ready && card.preview.as_deref().is_some_and(|p| !p.is_empty()) {
                    self.insert(out);
                } else if let Some(i) = card.selected
                    && card.job.is_none()
                    && card.preview.is_none()
                    && card.asking.is_none()
                    && i < card.items.len()
                {
                    self.choose(i, out);
                }
            }
        }
        self.emit_card(out);
    }

    /// The user picked item `i`.
    fn choose(&mut self, i: usize, out: &mut Vec<Output>) {
        let Some(card) = &mut self.card else { return };
        if card.selection.is_none() {
            let direction = card.items[i].clone();
            return self.run_reply(direction, out);
        }
        let action = match Rewrite::builtin(i) {
            Some(action) => action,
            None if i < REWRITE_ACTIONS.len() => {
                let (ask, question) = if i == 4 {
                    (Ask::Language, "Translate into which language?")
                } else {
                    (
                        Ask::Tone,
                        "Which tone? For example: friendly, formal, direct",
                    )
                };
                card.asking = Some(ask);
                card.input = Some(String::new());
                card.status = Some(question.to_owned());
                return;
            }
            None => match self
                .settings
                .reply
                .custom_actions
                .get(i - REWRITE_ACTIONS.len())
            {
                Some(a) => Rewrite::Custom {
                    instruction: a.instruction.clone(),
                },
                None => return,
            },
        };
        self.run_rewrite(action, out);
    }

    /// The user typed a direction, instruction, language or tone.
    fn submit(&mut self, text: String, out: &mut Vec<Output>) {
        let Some(card) = &mut self.card else { return };
        if card.selection.is_none() {
            return self.run_reply(text, out);
        }
        let action = match card.asking.take() {
            Some(Ask::Language) => Rewrite::Translate { language: text },
            Some(Ask::Tone) => Rewrite::Tone { tone: text },
            None => Rewrite::Custom { instruction: text },
        };
        self.run_rewrite(action, out);
    }

    /// Starts a draft; `build` makes the request from the card's context.
    fn start_draft(
        &mut self,
        direction: Option<String>,
        out: &mut Vec<Output>,
        build: impl FnOnce(&DraftContext<'_>, &Card, u64) -> GenerateRequest,
    ) {
        if self.draft_caps.is_none() {
            if let Some(card) = &mut self.card {
                card.status = Some(
                    "No model is set up for Reply & rewrite. Choose one in Settings.".to_owned(),
                );
            }
            return;
        }
        if let Some((id, _)) = self.card.as_mut().and_then(|c| c.job.take()) {
            out.push(Output::CancelGenerate(id));
        }
        let id = self.id();
        let Some(card) = &self.card else { return };
        let Some(ctx) = self.draft_context(card) else {
            return;
        };
        let request = build(&ctx, card, id);
        let Some(card) = &mut self.card else { return };
        if direction.is_some() {
            card.direction = direction;
        }
        card.job = Some((id, Job::Draft));
        card.preview = Some(String::new());
        card.ready = false;
        card.status = Some("Writing…".to_owned());
        out.push(Output::Generate(request));
    }

    fn run_reply(&mut self, direction: String, out: &mut Vec<Output>) {
        let d = direction.clone();
        self.start_draft(Some(direction), out, |ctx, _, id| draft::reply(ctx, &d, id));
    }

    fn run_rewrite(&mut self, action: Rewrite, out: &mut Vec<Output>) {
        self.start_draft(None, out, |ctx, card, id| {
            draft::rewrite(
                ctx,
                card.selection.as_deref().unwrap_or_default(),
                &action,
                id,
            )
        });
    }

    fn generated(&mut self, id: u64, update: GenerateUpdate, out: &mut Vec<Output>) {
        let Some(card) = &mut self.card else { return };
        let Some((job_id, job)) = card.job else {
            return;
        };
        if job_id != id {
            return;
        }
        match (job, update) {
            (Job::Directions, GenerateUpdate::Delta(_)) => return,
            (Job::Directions, GenerateUpdate::Done(result)) => {
                card.job = None;
                match result.as_deref().ok().and_then(draft::parse_directions) {
                    Some(directions) => {
                        card.items = directions;
                        card.selected = Some(0);
                        card.status = None;
                    }
                    None if result.is_ok() && !card.retried => {
                        card.retried = true;
                        self.start_directions(out);
                    }
                    None => {
                        if result != Err(CompleteError::Cancelled) {
                            let reason = match &result {
                                Err(e) => format!("Couldn't suggest directions ({e})."),
                                Ok(_) => "Couldn't suggest directions.".to_owned(),
                            };
                            card.status = Some(format!("{reason} Type one."));
                            card.input = Some(String::new());
                        }
                    }
                }
            }
            (Job::Draft, GenerateUpdate::Delta(delta)) => {
                card.preview
                    .get_or_insert_with(String::new)
                    .push_str(&delta);
            }
            (Job::Draft, GenerateUpdate::Done(Ok(text))) => {
                card.job = None;
                let text = draft::clean_output(&text);
                if text.is_empty() {
                    card.preview = None;
                    card.status = Some("The model returned nothing. Try again.".to_owned());
                } else {
                    card.preview = Some(text);
                    card.ready = true;
                    card.status = None;
                }
            }
            (Job::Draft, GenerateUpdate::Done(Err(CompleteError::Cancelled))) => card.job = None,
            (Job::Draft, GenerateUpdate::Done(Err(e))) => {
                card.job = None;
                card.preview = None;
                card.ready = false;
                card.status = Some(format!(
                    "Couldn't write it ({e}). Pick again or type a direction."
                ));
            }
        }
        self.emit_card(out);
    }

    fn insert(&mut self, out: &mut Vec<Output>) {
        let id = self.id();
        let Some(f) = &self.field else { return };
        let Some(card) = &mut self.card else { return };
        let Some(preview) = card.preview.as_deref() else {
            return;
        };
        let replace = if card.selection.is_some() {
            Replace::Selection
        } else {
            Replace::Nothing
        };
        let before = f.snap.before.as_str();
        let text = if replace == Replace::Selection {
            preview.to_owned()
        } else if before.is_empty() || before.ends_with(char::is_whitespace) {
            preview.trim_start().to_owned()
        } else if preview.starts_with(char::is_alphanumeric) {
            format!(" {preview}")
        } else {
            preview.to_owned()
        };
        card.inserting = Some(id);
        card.status = Some("Inserting…".to_owned());
        out.push(Output::Insert(InsertRequest {
            id,
            field: card.field,
            text,
            replace,
        }));
    }

    fn insert_done(&mut self, id: u64, outcome: InsertOutcome, out: &mut Vec<Output>) {
        let Some(card) = &mut self.card else { return };
        if card.inserting != Some(id) {
            return;
        }
        card.inserting = None;
        match outcome {
            InsertOutcome::Inserted => {
                let app = card.app.clone();
                out.push(Output::Record(Stat::DraftInserted { app }));
                return self.close_card(out);
            }
            InsertOutcome::Failed(reason) => {
                card.status = Some(format!(
                    "Couldn't insert ({reason}). The text is still here."
                ));
            }
            InsertOutcome::Unknown => {
                card.status = Some(
                    "The app didn't confirm the insert. Check the field; the text is still here."
                        .to_owned(),
                );
            }
        }
        self.emit_card(out);
    }
}
