//! One replay: a sample typed into a simulated field, with the product's
//! controller deciding when to ask for a suggestion and what to show.
//!
//! The replay plays the backend and the user around a real
//! [`Controller`]: it sends the field snapshots, key-gate events and timers a
//! backend would send, runs every [`Output::Complete`] on the provider, and
//! performs Take Word, Take All and typing-through with the same
//! [`SuggestionView`] methods the key gate uses. So the request (prompt
//! segments from `prompt::build`, stop rule from the settings and the app's
//! category), the cut into a suggestion (`stop::cut`), the silence rules and
//! the decision to keep a suggestion while it is typed through are all the
//! product's own code.

use std::collections::VecDeque;

use oft_engine::complete::{Caps, CompleteError, Completion, CompletionRequest, StopParams};
use oft_engine::controller::{Controller, Feature, Input, Output, TimerId};
use oft_engine::io::{
    AppInfo, Conversation, DismissReason, Event, FieldId, FieldKind, FieldSnapshot, Message,
    Overlay, SuggestionView,
};
use oft_engine::settings::Settings;
use oft_engine::stop::{self, CutContext};
use unicode_segmentation::UnicodeSegmentation;

use crate::metrics::Tally;
use crate::pause::{PauseModel, sample_key};
use crate::user::{self, Decision};

/// One piece of writing to replay.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Stable id; also seeds the pause draws.
    pub id: String,
    /// The window title the backend reports (an email subject, a document
    /// name); may be empty.
    pub title: String,
    /// The conversation visible around the field (chat and email), oldest
    /// first.
    pub conversation: Vec<Message>,
    /// Text already in the field before the user starts.
    pub before: String,
    /// What the user types.
    pub text: String,
}

/// A provider's answer to one request.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub completion: Completion,
    /// Time from the request to the finished completion.
    pub latency_ms: f64,
}

/// Runs completion requests the way the product's runtime does: on the model,
/// with a [`stop::Stopper`] built from `request.stop`.
pub trait Provider {
    fn caps(&self) -> Caps;
    /// `at` is the caret's byte offset in the sample's text, so a shared
    /// provider can serve requests in typing order.
    fn complete(&mut self, request: &CompletionRequest, at: usize)
    -> Result<Answer, CompleteError>;
}

const FIELD: FieldId = FieldId(1);

/// Replays `sample` typed into a field of `app` with these settings.
pub fn replay(
    sample: &Sample,
    app: &AppInfo,
    settings: &Settings,
    pauses: &PauseModel,
    provider: &mut dyn Provider,
) -> Tally {
    let caps = provider.caps();
    let mut sim = Sim {
        ctrl: controller(settings, caps),
        provider,
        sample,
        key: sample_key(&sample.id),
        pauses,
        snap: snapshot(
            &AppInfo {
                title: sample.title.clone(),
                ..app.clone()
            },
            sample.before.clone(),
        ),
        pos: 0,
        overlay: None,
        used: false,
        main_silent: false,
        tally: Tally {
            samples: 1,
            words: sample.text.unicode_words().count() as u64,
            chars: sample.text.chars().count() as u64,
            ..Tally::default()
        },
    };
    sim.run();
    assert_eq!(
        sim.tally.typed + sim.tally.accepted_chars,
        sim.tally.chars,
        "every character is typed or accepted exactly once"
    );
    sim.tally
}

/// The stop rule and token cap the product puts on requests in `app` with
/// these settings, read from a request the controller makes.
pub fn request_stop(settings: &Settings, app: &AppInfo, caps: Caps) -> (StopParams, u32) {
    let mut ctrl = controller(settings, caps);
    let mut snap = snapshot(app, String::new());
    ctrl.handle(Input::Event(Event::Focus(snap.clone())));
    snap.revision += 1;
    snap.before = "Thanks for the ".into();
    ctrl.handle(Input::Event(Event::Changed(snap)));
    ctrl.handle(Input::Timer(TimerId::Pause))
        .into_iter()
        .find_map(|o| match o {
            Output::Complete(r) => Some((r.stop, r.max_tokens)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the controller makes no request in {}", app.id))
}

fn controller(settings: &Settings, caps: Caps) -> Controller {
    let mut ctrl = Controller::new(settings.clone());
    ctrl.handle(Input::Settings(Box::new(settings.clone())));
    ctrl.handle(Input::Caps {
        feature: Feature::Autocomplete,
        caps: Some(caps),
    });
    ctrl
}

fn snapshot(app: &AppInfo, before: String) -> FieldSnapshot {
    FieldSnapshot {
        field: FIELD,
        revision: 1,
        app: app.clone(),
        kind: FieldKind::Text,
        multiline: true,
        before,
        ..FieldSnapshot::default()
    }
}

struct Sim<'a> {
    ctrl: Controller,
    provider: &'a mut dyn Provider,
    sample: &'a Sample,
    key: u64,
    pauses: &'a PauseModel,
    snap: FieldSnapshot,
    /// Bytes of `sample.text` in the field.
    pos: usize,
    /// What the key gate holds.
    overlay: Option<SuggestionView>,
    /// The suggestion on screen was accepted from at least once.
    used: bool,
    /// The main candidate of the last completion cut to nothing.
    main_silent: bool,
    tally: Tally,
}

impl Sim<'_> {
    fn run(&mut self) {
        self.send(Input::Event(Event::Focus(self.snap.clone())));
        // The caret got here by typing (Enter for a new paragraph), so the
        // controller sees an edit before the first keystroke of the sample.
        self.changed();
        self.pause(false);
        loop {
            if let Some(view) = &self.overlay {
                match user::decide(view, self.rest(), &self.snap.before) {
                    Decision::TakeAll => {
                        self.take(true);
                        self.pause(true);
                        continue;
                    }
                    Decision::TakeWord => {
                        self.take(false);
                        self.pause(true);
                        continue;
                    }
                    Decision::Type => {}
                }
            }
            let Some(c) = self.rest().chars().next() else {
                break;
            };
            self.type_char(c);
            self.pause(false);
        }
    }

    fn rest(&self) -> &str {
        &self.sample.text[self.pos..]
    }

    /// Lets the pause timer elapse when the typist pauses here.
    fn pause(&mut self, after_accept: bool) {
        if after_accept || self.pauses.pauses(self.key, self.pos, &self.snap.before) {
            self.send(Input::Timer(TimerId::Pause));
        }
    }

    fn changed(&mut self) {
        self.snap.revision += 1;
        self.send(Input::Event(Event::Changed(self.snap.clone())));
    }

    /// Take Word or Take All, as the key gate performs it.
    fn take(&mut self, whole: bool) {
        let view = self
            .overlay
            .as_mut()
            .expect("the user decided on a suggestion");
        let end = if whole { view.shown } else { view.word_end() };
        let taken = view.text[..end].to_owned();
        view.advance(end);
        if view.shown == 0 {
            self.overlay = None;
        }
        if whole {
            self.tally.take_all += 1;
        } else {
            self.tally.take_word += 1;
        }
        self.tally.accepted_chars += taken.chars().count() as u64;
        if !self.used {
            self.used = true;
            self.tally.accepted += 1;
        }
        self.pos += taken.len();
        self.snap.before.push_str(&taken);
        self.send(Input::Event(Event::Accepted {
            field: FIELD,
            text: taken,
            whole,
        }));
        self.changed();
    }

    /// One typed character, through the key gate.
    fn type_char(&mut self, c: char) {
        if let Some(view) = &mut self.overlay {
            let mut buf = [0u8; 4];
            if view.type_through(c.encode_utf8(&mut buf)) {
                if view.shown == 0 {
                    self.overlay = None;
                }
                self.send(Input::Event(Event::TypedThrough {
                    field: FIELD,
                    text: c.to_string(),
                }));
            } else {
                self.overlay = None;
                self.send(Input::Event(Event::Dismissed {
                    field: FIELD,
                    reason: DismissReason::Mismatch,
                }));
            }
        }
        self.tally.typed += 1;
        self.pos += c.len_utf8();
        self.snap.before.push(c);
        self.changed();
    }

    /// Feeds `input` and everything it leads to (conversation reads and
    /// completions answered at once) through the controller.
    fn send(&mut self, input: Input) {
        let mut queue = VecDeque::from([input]);
        while let Some(input) = queue.pop_front() {
            for out in self.ctrl.handle(input) {
                match out {
                    Output::Overlay(Overlay::Suggestion(view)) => self.show(view),
                    Output::Overlay(_) => self.overlay = None,
                    Output::Complete(request) => queue.push_back(self.complete(request)),
                    Output::ReadConversation(field) => queue.push_back(Input::Conversation {
                        field,
                        conversation: (!self.sample.conversation.is_empty()).then(|| {
                            Conversation {
                                messages: self.sample.conversation.clone(),
                            }
                        }),
                    }),
                    _ => {}
                }
            }
        }
    }

    fn complete(&mut self, request: CompletionRequest) -> Input {
        self.tally.requests += 1;
        let id = request.id;
        let result = match self.provider.complete(&request, self.pos) {
            Ok(answer) => {
                self.tally.latency_ms.push(answer.latency_ms);
                let ctx = CutContext {
                    before: &self.snap.before,
                    after: &self.snap.after,
                };
                self.main_silent = answer
                    .completion
                    .candidates
                    .first()
                    .is_none_or(|c| stop::cut(c, &request.stop, &ctx).is_none());
                Ok(answer.completion)
            }
            Err(e) => {
                self.tally.errors += 1;
                Err(e)
            }
        };
        Input::Completed { id, result }
    }

    /// A new suggestion appeared.
    fn show(&mut self, view: SuggestionView) {
        self.tally.shown += 1;
        if !user::matches(self.rest(), user::first_unit(&view), &self.snap.before) {
            self.tally.wrong += 1;
        }
        if self.main_silent {
            self.tally.from_alternative += 1;
        }
        self.tally.shown_words += view.visible().unicode_words().count() as u64;
        self.used = false;
        self.overlay = Some(view);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use oft_engine::complete::{Candidate, LengthMode};

    use super::*;

    /// Answers from a table of `before` → continuation; silent elsewhere.
    struct Scripted {
        by_before: HashMap<String, &'static str>,
        requests: Vec<String>,
    }

    impl Scripted {
        fn new(table: &[(&str, &'static str)]) -> Scripted {
            Scripted {
                by_before: table.iter().map(|(b, t)| (b.to_string(), *t)).collect(),
                requests: Vec::new(),
            }
        }
    }

    impl Provider for Scripted {
        fn caps(&self) -> Caps {
            Caps {
                token_probs: false,
                cheap_alternatives: false,
                prefill: false,
                local: true,
            }
        }

        fn complete(
            &mut self,
            request: &CompletionRequest,
            _: usize,
        ) -> Result<Answer, CompleteError> {
            let before = request.segments.before.clone();
            let text = self.by_before.get(&before).copied().unwrap_or("");
            self.requests.push(before);
            Ok(Answer {
                completion: Completion {
                    id: request.id,
                    candidates: vec![Candidate {
                        text: text.into(),
                        ..Candidate::default()
                    }],
                },
                latency_ms: 1.0,
            })
        }
    }

    fn app() -> AppInfo {
        AppInfo {
            id: "slack".into(),
            name: "Slack".into(),
            title: String::new(),
        }
    }

    fn sample(text: &str) -> Sample {
        Sample {
            id: "t".into(),
            title: String::new(),
            conversation: Vec::new(),
            before: String::new(),
            text: text.into(),
        }
    }

    fn settings(mode: LengthMode) -> Settings {
        Settings {
            length: mode,
            ..Settings::default()
        }
    }

    /// Pauses only at word boundaries and after accepts.
    fn boundaries_only() -> PauseModel {
        PauseModel {
            visible_ms: f64::INFINITY,
            ..PauseModel::new(1, 30)
        }
    }

    #[test]
    fn exact_suggestion_is_taken_whole() {
        // "see you soon.": typed "see " (4 keys); at the boundary the model
        // offers "you soon.", taken with one backtick.
        let mut p = Scripted::new(&[("see ", "you soon.")]);
        let t = replay(
            &sample("see you soon."),
            &app(),
            &settings(LengthMode::Sentence),
            &boundaries_only(),
            &mut p,
        );
        assert_eq!(t.chars, 13);
        assert_eq!(t.typed, 4);
        assert_eq!(t.take_all, 1);
        assert_eq!(t.take_word, 0);
        assert_eq!(t.accepted_chars, 9);
        assert_eq!(t.keystrokes(), 5);
        assert_eq!((t.shown, t.wrong, t.accepted), (1, 0, 1));
        assert!((t.metrics().keystroke_savings - 8.0 / 13.0).abs() < 1e-12);
        // Empty field: no request before the first keystroke; the request
        // after the accept finds the text complete.
        assert_eq!(p.requests, ["see ", "see you soon."]);
    }

    #[test]
    fn unfinished_last_word_is_not_shown() {
        // The stop rule's end-of-text rule: "soon" may have been cut off by
        // the token limit, so Sentence shows only "you"; the user takes it
        // and types the rest.
        let mut p = Scripted::new(&[("see ", "you soon")]);
        let t = replay(
            &sample("see you soon"),
            &app(),
            &settings(LengthMode::Sentence),
            &boundaries_only(),
            &mut p,
        );
        assert_eq!((t.typed, t.take_all, t.accepted_chars), (9, 1, 3));
    }

    #[test]
    fn partial_match_takes_words_then_types_on() {
        // The model says "you tomorrow."; the user writes "you later".
        let mut p = Scripted::new(&[("see ", "you tomorrow.")]);
        let t = replay(
            &sample("see you later"),
            &app(),
            &settings(LengthMode::Sentence),
            &boundaries_only(),
            &mut p,
        );
        // "see " typed (4), Tab takes "you " (1), "later" typed (5).
        assert_eq!((t.typed, t.take_word, t.take_all), (9, 1, 0));
        assert_eq!(t.accepted_chars, 4);
        assert_eq!((t.shown, t.wrong, t.accepted), (1, 0, 1));
        assert_eq!(t.keystrokes(), 10);
    }

    #[test]
    fn wrong_suggestion_is_counted_and_typed_over() {
        let mut p = Scripted::new(&[("see ", "them ")]);
        let t = replay(
            &sample("see you"),
            &app(),
            &settings(LengthMode::Word),
            &boundaries_only(),
            &mut p,
        );
        assert_eq!((t.shown, t.wrong, t.accepted), (1, 1, 0));
        assert_eq!(t.typed, 7);
        assert_eq!(t.metrics().keystroke_savings, 0.0);
    }

    /// Every position inside a word is a pause point too.
    fn every_position() -> PauseModel {
        PauseModel {
            visible_ms: 0.0,
            ..PauseModel::new(1, 30)
        }
    }

    #[test]
    fn mid_word_completion_is_taken_whole() {
        let mut p = Scripted::new(&[("s", "ee you.")]);
        let t = replay(
            &sample("see you."),
            &app(),
            &settings(LengthMode::Sentence),
            &every_position(),
            &mut p,
        );
        assert_eq!((t.shown, t.wrong), (1, 0));
        assert_eq!((t.typed, t.take_all, t.accepted_chars), (1, 1, 7));
        assert_eq!(p.requests, ["s", "see you."]);
    }

    #[test]
    fn typing_through_keeps_the_suggestion_without_new_requests() {
        // At "s" the model offers "ell it." (wrong: the user writes "see").
        // Typing "e" types through it, so no request at "se"; typing the
        // second "e" mismatches, and requests resume.
        let mut p = Scripted::new(&[("s", "ell it.")]);
        let t = replay(
            &sample("see you."),
            &app(),
            &settings(LengthMode::Sentence),
            &every_position(),
            &mut p,
        );
        assert_eq!((t.shown, t.wrong, t.accepted), (1, 1, 0));
        assert_eq!(t.typed, 8);
        assert_eq!(
            p.requests,
            ["s", "see", "see ", "see y", "see yo", "see you", "see you."]
        );
    }

    #[test]
    fn question_mark_silences_and_counts_no_request() {
        let mut p = Scripted::new(&[]);
        let t = replay(
            &sample("ok? "),
            &app(),
            &settings(LengthMode::Word),
            &boundaries_only(),
            &mut p,
        );
        // Only "ok? " ends in whitespace, and it ends with a question mark.
        assert_eq!(t.requests, 0);
        assert_eq!(t.typed, 4);
    }

    #[test]
    fn conversation_reaches_the_prompt() {
        struct Spy(Vec<usize>);
        impl Provider for Spy {
            fn caps(&self) -> Caps {
                Caps::default()
            }
            fn complete(
                &mut self,
                r: &CompletionRequest,
                _: usize,
            ) -> Result<Answer, CompleteError> {
                self.0.push(r.segments.conversation.len());
                Ok(Answer {
                    completion: Completion {
                        id: r.id,
                        candidates: vec![Candidate::default()],
                    },
                    latency_ms: 0.0,
                })
            }
        }
        let mut s = sample("sure ");
        s.conversation = vec![Message {
            author: oft_engine::io::Author::Other("Ann".into()),
            text: "Lunch?".into(),
        }];
        let mut spy = Spy(Vec::new());
        replay(
            &s,
            &app(),
            &settings(LengthMode::Word),
            &boundaries_only(),
            &mut spy,
        );
        assert_eq!(spy.0, [1]);
    }

    #[test]
    fn request_stop_reads_the_category_ceiling() {
        let caps = Caps::default();
        let (stop, max) = request_stop(&settings(LengthMode::Auto), &app(), caps);
        assert_eq!(stop.mode, LengthMode::Auto);
        assert_eq!(stop.ceiling, Settings::default().ceilings.chat);
        assert_eq!(max, stop::max_tokens(&stop));
    }
}
