//! Scenario tests for the controller, driven through its public inputs and
//! outputs only.

use std::time::Duration;

use ghostcaret_engine::complete::{
    Candidate, Caps, CompleteError, Completion, CompletionRequest, LengthMode,
};
use ghostcaret_engine::controller::{
    Controller, Feature, GenerateUpdate, Input, Output, Pause, Stat, Status, TimerId,
};
use ghostcaret_engine::io::{
    Action, AppInfo, Author, CardKey, CardView, Conversation, DismissReason, Event, FieldId,
    FieldKind, FieldSnapshot, InsertOutcome, Message, Overlay, Replace, SuggestionView,
};
use ghostcaret_engine::settings::{AppRule, Settings};

const LOCAL: Caps = Caps {
    token_probs: false,
    cheap_alternatives: false,
    prefill: false,
    local: true,
};
const CHAT: &str = "org.telegram.desktop";

fn controller_with(settings: Settings, caps: Caps) -> Controller {
    let mut c = Controller::new(settings.clone());
    c.handle(Input::Settings(Box::new(settings)));
    c.handle(Input::Caps {
        feature: Feature::Autocomplete,
        caps: Some(caps),
    });
    c.handle(Input::Caps {
        feature: Feature::Reply,
        caps: Some(caps),
    });
    c
}

fn controller() -> Controller {
    controller_with(Settings::default(), LOCAL)
}

fn snap(field: u64, revision: u64, app: &str, before: &str) -> FieldSnapshot {
    FieldSnapshot {
        field: FieldId(field),
        revision,
        app: AppInfo {
            id: app.into(),
            name: app.into(),
            title: String::new(),
        },
        kind: FieldKind::Text,
        multiline: true,
        before: before.into(),
        ..FieldSnapshot::default()
    }
}

fn ev(c: &mut Controller, event: Event) -> Vec<Output> {
    c.handle(Input::Event(event))
}

fn completes(out: &[Output]) -> Vec<&CompletionRequest> {
    out.iter()
        .filter_map(|o| {
            if let Output::Complete(r) = o {
                Some(r)
            } else {
                None
            }
        })
        .collect()
}

fn one_complete(out: &[Output]) -> CompletionRequest {
    let r = completes(out);
    assert_eq!(r.len(), 1, "{out:?}");
    r[0].clone()
}

fn suggestion(out: &[Output]) -> Option<&SuggestionView> {
    out.iter().rev().find_map(|o| {
        if let Output::Overlay(Overlay::Suggestion(v)) = o {
            Some(v)
        } else {
            None
        }
    })
}

fn card(out: &[Output]) -> &CardView {
    out.iter()
        .rev()
        .find_map(|o| {
            if let Output::Overlay(Overlay::Card(v)) = o {
                Some(v)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no card in {out:?}"))
}

fn hidden(out: &[Output]) -> bool {
    out.contains(&Output::Overlay(Overlay::Hidden))
}

fn plain(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        ..Candidate::default()
    }
}

fn complete(c: &mut Controller, id: u64, candidates: &[&str]) -> Vec<Output> {
    let candidates = candidates.iter().map(|t| plain(t)).collect();
    c.handle(Input::Completed {
        id,
        result: Ok(Completion { id, candidates }),
    })
}

/// Focuses `app`, types `before` and runs a completion that returns `texts`.
fn show(c: &mut Controller, app: &str, before: &str, texts: &[&str]) -> SuggestionView {
    ev(c, Event::Focus(snap(1, 1, app, "")));
    let out = ev(c, Event::Changed(snap(1, 2, app, before)));
    assert!(
        out.contains(&Output::StartTimer {
            id: TimerId::Pause,
            after: Duration::from_millis(30)
        }),
        "{out:?}"
    );
    let request = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
    let out = complete(c, request.id, texts);
    suggestion(&out)
        .cloned()
        .unwrap_or_else(|| panic!("no suggestion in {out:?}"))
}

fn generates(out: &[Output]) -> Vec<&ghostcaret_engine::complete::GenerateRequest> {
    out.iter()
        .filter_map(|o| {
            if let Output::Generate(r) = o {
                Some(r)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn settings_emit_bindings_appearance_and_active() {
    let mut c = Controller::new(Settings::default());
    let out = c.handle(Input::Settings(Box::default()));
    assert!(out.iter().any(|o| matches!(o, Output::Bindings(_))));
    assert!(out.iter().any(|o| matches!(o, Output::Appearance(_))));
    assert!(out.contains(&Output::Active(true)));
    assert!(out.contains(&Output::Status(Status::NoModel)));
}

#[test]
fn type_pause_complete_shows_the_suggestion() {
    let mut c = controller();
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Changed(snap(1, 2, CHAT, "I will")));
    let request = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
    assert_eq!((request.field, request.revision), (FieldId(1), 2));
    assert_eq!(request.segments.before, "I will");
    assert_eq!(request.stop.mode, LengthMode::Auto);
    assert_eq!(request.stop.ceiling, LengthMode::Sentence);
    let out = complete(&mut c, request.id, &[" see you tomorrow. Bye "]);
    let view = suggestion(&out).expect("suggestion");
    assert_eq!(view.visible(), " see you tomorrow.");
    assert_eq!((view.field, view.revision), (FieldId(1), 2));
    assert!(view.is_valid());
    assert!(out.contains(&Output::Record(Stat::Shown { app: CHAT.into() })));
}

#[test]
fn a_new_keystroke_cancels_and_a_stale_result_is_dropped() {
    let mut c = controller();
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Changed(snap(1, 2, CHAT, "I will")));
    let request = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
    let out = ev(&mut c, Event::Changed(snap(1, 3, CHAT, "I will s")));
    assert!(out.contains(&Output::Cancel(request.id)), "{out:?}");
    let out = complete(&mut c, request.id, &[" see you."]);
    assert!(suggestion(&out).is_none() && out.is_empty(), "{out:?}");
    let out = complete(&mut c, 999, &[" see you."]);
    assert!(out.is_empty());
}

#[test]
fn typing_through_keeps_the_suggestion_without_a_request() {
    let mut c = controller();
    let view = show(&mut c, CHAT, "I will", &[" see you tomorrow. Bye "]);
    // Key events can overtake field notifications.
    assert!(
        ev(
            &mut c,
            Event::TypedThrough {
                field: FieldId(1),
                text: " ".into()
            }
        )
        .is_empty()
    );
    assert!(
        ev(
            &mut c,
            Event::TypedThrough {
                field: FieldId(1),
                text: "s".into()
            }
        )
        .is_empty()
    );
    assert!(ev(&mut c, Event::Changed(snap(1, 3, CHAT, "I will "))).is_empty());
    assert!(ev(&mut c, Event::Changed(snap(1, 4, CHAT, "I will s"))).is_empty());
    let out = ev(
        &mut c,
        Event::Accepted {
            field: FieldId(1),
            text: "ee ".into(),
            whole: false,
        },
    );
    assert_eq!(
        out,
        [Output::Record(Stat::Accepted {
            app: CHAT.into(),
            chars: 3,
            whole: false
        })]
    );
    assert!(ev(&mut c, Event::Changed(snap(1, 5, CHAT, "I will see "))).is_empty());
    assert!(c.handle(Input::Timer(TimerId::Pause)).is_empty());
    // A change that is not the suggestion hides it and starts over.
    let out = ev(&mut c, Event::Changed(snap(1, 6, CHAT, "I will see x")));
    assert!(hidden(&out));
    assert!(out.iter().any(|o| matches!(
        o,
        Output::StartTimer {
            id: TimerId::Pause,
            ..
        }
    )));
    assert!(view.is_valid());
}

#[test]
fn taking_everything_then_confirming_starts_a_fresh_request() {
    let mut c = controller();
    show(&mut c, CHAT, "I will", &[" see you. Bye "]);
    ev(
        &mut c,
        Event::Accepted {
            field: FieldId(1),
            text: " see you.".into(),
            whole: true,
        },
    );
    let out = ev(&mut c, Event::Changed(snap(1, 3, CHAT, "I will see you.")));
    assert!(!hidden(&out), "the backend already hid it: {out:?}");
    assert!(out.iter().any(|o| matches!(
        o,
        Output::StartTimer {
            id: TimerId::Pause,
            ..
        }
    )));
}

#[test]
fn password_fields_stay_silent() {
    let mut c = controller();
    let mut s = snap(1, 1, CHAT, "hunter2");
    s.kind = FieldKind::Password;
    let out = ev(&mut c, Event::Focus(s.clone()));
    assert!(
        !out.iter()
            .any(|o| matches!(o, Output::StartTimer { .. } | Output::ReadConversation(_))),
        "{out:?}"
    );
    s.revision = 2;
    s.before = "hunter22".into();
    let out = ev(&mut c, Event::Changed(s));
    assert!(
        completes(&out).is_empty() && !out.iter().any(|o| matches!(o, Output::StartTimer { .. })),
        "{out:?}"
    );
    assert!(c.handle(Input::Timer(TimerId::Pause)).is_empty());
    assert!(ev(&mut c, Event::Action(Action::Reply)).is_empty());
}

#[test]
fn focus_change_hides_and_cancels() {
    let mut c = controller();
    show(&mut c, CHAT, "I will", &[" see you. Bye "]);
    let out = ev(&mut c, Event::Focus(snap(2, 1, CHAT, "")));
    assert!(hidden(&out));
    let out = ev(&mut c, Event::Blur);
    assert!(!hidden(&out));
}

#[test]
fn silence_rules() {
    let silent = |app: &str, kind: FieldKind, before: &str, after: &str| {
        let mut c = controller();
        let mut s = snap(1, 2, app, before);
        s.kind = kind;
        s.after = after.into();
        ev(&mut c, Event::Focus(snap(1, 1, app, "")));
        let out = ev(&mut c, Event::Changed(s));
        !out.iter().any(|o| {
            matches!(
                o,
                Output::StartTimer {
                    id: TimerId::Pause,
                    ..
                }
            )
        })
    };
    assert!(silent(CHAT, FieldKind::Text, "   ", ""), "empty field");
    assert!(
        silent(CHAT, FieldKind::Text, "How are you? ", ""),
        "after a question mark"
    );
    assert!(silent(CHAT, FieldKind::Text, "hel", "lo"), "inside a word");
    assert!(
        silent(CHAT, FieldKind::Url, "https://exa", ""),
        "address field"
    );
    assert!(
        silent("code", FieldKind::Text, "fn main", ""),
        "code editor"
    );
    assert!(
        silent("org.keepassxc.keepassxc", FieldKind::Text, "my notes", ""),
        "password manager"
    );
    assert!(
        silent(
            "com.mitchellh.ghostty",
            FieldKind::Terminal,
            "me@host:~/src$ git sta",
            ""
        ),
        "bash prompt"
    );
    assert!(
        silent("kitty", FieldKind::Text, "~/src ❯ cargo te", ""),
        "starship prompt"
    );
    assert!(
        silent("foot", FieldKind::Text, "me@host ~/src> ls -l", ""),
        "fish prompt"
    );
    assert!(
        !silent(
            "com.mitchellh.ghostty",
            FieldKind::Terminal,
            "│ > please refactor the",
            ""
        ),
        "chat tool in a terminal"
    );
    assert!(
        !silent(
            "com.mitchellh.ghostty",
            FieldKind::Terminal,
            "> please refactor the",
            ""
        ),
        "chat tool in a terminal"
    );
    assert!(
        !silent(CHAT, FieldKind::Text, "See you", " later"),
        "caret before a space"
    );
}

#[test]
fn app_rules_and_field_kinds_shape_the_stop_rule() {
    let mut settings = Settings::default();
    settings.apps.insert(
        CHAT.into(),
        AppRule {
            length: Some(LengthMode::Word),
            ..AppRule::default()
        },
    );
    settings.apps.insert(
        "code".into(),
        AppRule {
            enabled: Some(true),
            ..AppRule::default()
        },
    );
    let stop = |settings: &Settings, app: &str, kind: FieldKind| {
        let mut c = controller_with(settings.clone(), LOCAL);
        let mut s = snap(1, 2, app, "Hello");
        s.kind = kind;
        ev(&mut c, Event::Focus(snap(1, 1, app, "")));
        ev(&mut c, Event::Changed(s));
        let r = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
        (r.stop.mode, r.stop.ceiling)
    };
    assert_eq!(
        stop(&settings, CHAT, FieldKind::Text),
        (LengthMode::Word, LengthMode::Word)
    );
    assert_eq!(
        stop(&settings, "thunderbird", FieldKind::Text),
        (LengthMode::Auto, LengthMode::Paragraph)
    );
    assert_eq!(
        stop(&settings, "thunderbird", FieldKind::Search),
        (LengthMode::Auto, LengthMode::Word)
    );
    assert_eq!(
        stop(&settings, "code", FieldKind::Text),
        (LengthMode::Auto, LengthMode::Sentence)
    );
}

#[test]
fn without_a_model_nothing_is_requested() {
    let mut c = controller();
    let out = c.handle(Input::Caps {
        feature: Feature::Autocomplete,
        caps: None,
    });
    assert!(out.contains(&Output::Status(Status::NoModel)));
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    let out = ev(&mut c, Event::Changed(snap(1, 2, CHAT, "Hello")));
    assert!(!out.iter().any(|o| matches!(o, Output::StartTimer { .. })));
    assert!(c.handle(Input::Timer(TimerId::Pause)).is_empty());
}

#[test]
fn pause_and_off_apps_report_status() {
    let mut c = controller();
    let out = c.handle(Input::Pause(Pause {
        all: true,
        apps: Vec::new(),
    }));
    assert!(out.contains(&Output::Active(false)) && out.contains(&Output::Status(Status::Paused)));
    let out = c.handle(Input::Pause(Pause::default()));
    assert!(out.contains(&Output::Active(true)) && out.contains(&Output::Status(Status::Active)));
    let out = ev(&mut c, Event::Focus(snap(1, 1, "code", "")));
    assert!(
        out.iter()
            .any(|o| matches!(o, Output::Status(Status::OffInApp(a)) if a.id == "code"))
    );
    ev(&mut c, Event::Focus(snap(2, 1, CHAT, "")));
    let out = c.handle(Input::Pause(Pause {
        all: false,
        apps: vec![CHAT.into()],
    }));
    assert!(out.contains(&Output::Status(Status::Paused)));
}

#[test]
fn cloud_errors_show_and_clear() {
    let cloud = Caps {
        local: false,
        ..LOCAL
    };
    let mut c = controller_with(Settings::default(), cloud);
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Changed(snap(1, 2, CHAT, "Hi")));
    let r = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
    let out = c.handle(Input::Completed {
        id: r.id,
        result: Err(CompleteError::RateLimited),
    });
    assert!(
        out.iter()
            .any(|o| matches!(o, Output::Status(Status::CloudUnavailable(_))))
    );
    ev(&mut c, Event::Changed(snap(1, 3, CHAT, "Hi t")));
    let r = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
    let out = complete(&mut c, r.id, &["here you go. "]);
    assert!(out.contains(&Output::Status(Status::Active)));
}

#[test]
fn show_more_past_the_buffer_fetches_and_extends() {
    let settings = Settings {
        length: LengthMode::Word,
        ..Settings::default()
    };
    let mut c = controller_with(settings, LOCAL);
    let view = show(&mut c, CHAT, "I will", &[" see you "]);
    assert_eq!(view.visible(), " see");
    let request = one_complete(&ev(&mut c, Event::Action(Action::ShowMore)));
    assert_eq!(request.segments.before, "I will see you");
    let out = complete(&mut c, request.id, &[" tomorrow at noon "]);
    let more = suggestion(&out).expect("extended suggestion");
    assert_eq!(more.text, " see you tomorrow at noon");
    assert_eq!(more.visible(), " see you tomorrow");
    assert!(more.is_valid());
}

#[test]
fn alternatives_cycle_when_produced_with_the_suggestion() {
    let caps = Caps {
        cheap_alternatives: true,
        ..LOCAL
    };
    let mut c = controller_with(Settings::default(), caps);
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Changed(snap(1, 2, CHAT, "I will")));
    let r = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
    assert_eq!(r.alternatives, 2);
    let out = complete(
        &mut c,
        r.id,
        &[" call you. ", " call you. ", " be late. ", " see you. "],
    );
    let v = suggestion(&out).unwrap();
    assert_eq!((v.visible(), v.alternative), (" call you.", Some((1, 3))));
    let out = ev(&mut c, Event::Action(Action::NextAlternative));
    let v = suggestion(&out).unwrap();
    assert_eq!((v.visible(), v.alternative), (" be late.", Some((2, 3))));
    ev(&mut c, Event::Action(Action::PrevAlternative));
    let out = ev(&mut c, Event::Action(Action::PrevAlternative));
    assert_eq!(suggestion(&out).unwrap().alternative, Some((3, 3)));
    assert!(completes(&out).is_empty());
}

#[test]
fn alternatives_are_fetched_on_demand_once() {
    let mut c = controller();
    show(&mut c, CHAT, "I will", &[" call you. "]);
    let r = one_complete(&ev(&mut c, Event::Action(Action::NextAlternative)));
    assert_eq!(r.alternatives, 2);
    let out = complete(&mut c, r.id, &[" call you. ", " be late. "]);
    let v = suggestion(&out).unwrap();
    assert_eq!((v.visible(), v.alternative), (" be late.", Some((2, 2))));
    let out = ev(&mut c, Event::Action(Action::NextAlternative));
    assert!(completes(&out).is_empty());
    assert_eq!(suggestion(&out).unwrap().alternative, Some((1, 2)));
}

#[test]
fn dismissing_records_and_requests_nothing() {
    let mut c = controller();
    show(&mut c, CHAT, "I will", &[" call you. "]);
    let out = ev(
        &mut c,
        Event::Dismissed {
            field: FieldId(1),
            reason: DismissReason::Escape,
        },
    );
    assert_eq!(out, [Output::Record(Stat::Dismissed { app: CHAT.into() })]);
    assert!(c.handle(Input::Timer(TimerId::Pause)).is_empty());
}

#[test]
fn learn_my_length_lengthens_after_whole_takes() {
    let settings = Settings {
        learn_length: true,
        ..Settings::default()
    };
    let mut c = controller_with(settings, LOCAL);
    for round in 0..4u64 {
        let before = format!("Round {round}");
        ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
        ev(&mut c, Event::Changed(snap(1, 2, CHAT, &before)));
        let r = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
        assert_eq!(r.stop.ceiling, LengthMode::Sentence);
        complete(&mut c, r.id, &[" done. "]);
        ev(
            &mut c,
            Event::Accepted {
                field: FieldId(1),
                text: " done.".into(),
                whole: true,
            },
        );
    }
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Changed(snap(1, 2, CHAT, "Next")));
    let r = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
    assert_eq!(r.stop.ceiling, LengthMode::Paragraph);
}

#[test]
fn sent_messages_are_remembered_when_style_memory_is_on() {
    let mut settings = Settings::default();
    settings.context.style_memory = true;
    let mut c = controller_with(settings, LOCAL);
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Changed(snap(1, 2, CHAT, "see you at 5")));
    let out = ev(&mut c, Event::Changed(snap(1, 3, CHAT, "")));
    assert!(
        out.contains(&Output::Remember {
            app: CHAT.into(),
            text: "see you at 5".into()
        }),
        "{out:?}"
    );
    let secret = ["pass", "word=hunter2hunter2"].concat();
    ev(&mut c, Event::Changed(snap(1, 4, CHAT, &secret)));
    let out = ev(&mut c, Event::Changed(snap(1, 5, CHAT, "")));
    assert!(!out.iter().any(|o| matches!(o, Output::Remember { .. })));
}

#[test]
fn prefill_runs_before_the_pause_when_supported() {
    let settings = Settings {
        pause_ms: 120,
        ..Settings::default()
    };
    let caps = Caps {
        prefill: true,
        ..LOCAL
    };
    let mut c = controller_with(settings, caps);
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    let out = ev(&mut c, Event::Changed(snap(1, 2, CHAT, "Hello")));
    assert!(out.iter().any(|o| matches!(
        o,
        Output::StartTimer {
            id: TimerId::Prefill,
            ..
        }
    )));
    let out = c.handle(Input::Timer(TimerId::Prefill));
    let prefill = out
        .iter()
        .find_map(|o| {
            if let Output::Prefill(r) = o {
                Some(r.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(prefill.segments.before, "Hello");
    let out = ev(&mut c, Event::Changed(snap(1, 3, CHAT, "Hello t")));
    assert!(out.contains(&Output::Cancel(prefill.id)));
}

fn chat_messages() -> Conversation {
    Conversation {
        messages: vec![
            Message {
                author: Author::Other("Sam".into()),
                text: "Lunch on Tuesday?".into(),
            },
            Message {
                author: Author::Me,
                text: "Maybe".into(),
            },
        ],
    }
}

#[test]
fn reply_flow_through_insert() {
    let mut c = controller();
    let out = ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    assert!(out.contains(&Output::ReadConversation(FieldId(1))));
    let out = ev(&mut c, Event::Action(Action::Reply));
    assert!(out.contains(&Output::ReadConversation(FieldId(1))));
    assert_eq!(card(&out).title, "Reply");
    let out = c.handle(Input::Conversation {
        field: FieldId(1),
        conversation: Some(chat_messages()),
    });
    let directions = generates(&out)[0].clone();
    assert!(
        directions
            .messages
            .iter()
            .any(|m| m.content.contains("Lunch on Tuesday?"))
    );
    let out = c.handle(Input::Generated {
        id: directions.id,
        update: GenerateUpdate::Done(Ok(
            "1. Accept and suggest noon\n2. Decline politely\n3. Ask about Wednesday".into(),
        )),
    });
    assert_eq!(card(&out).items.len(), 3);
    let out = ev(&mut c, Event::CardKey(CardKey::Pick(1)));
    let draft = generates(&out)[0].clone();
    assert!(
        draft.messages[1]
            .content
            .contains("Accept and suggest noon")
    );
    c.handle(Input::Generated {
        id: draft.id,
        update: GenerateUpdate::Delta("Sure, ".into()),
    });
    let out = c.handle(Input::Generated {
        id: draft.id,
        update: GenerateUpdate::Delta("noon works!".into()),
    });
    assert_eq!(card(&out).preview.as_deref(), Some("Sure, noon works!"));
    // Enter does nothing while the draft is still streaming.
    assert!(
        !ev(&mut c, Event::CardKey(CardKey::Enter))
            .iter()
            .any(|o| matches!(o, Output::Insert(_)))
    );
    c.handle(Input::Generated {
        id: draft.id,
        update: GenerateUpdate::Done(Ok("\"Sure, noon works!\"".into())),
    });
    let out = ev(&mut c, Event::CardKey(CardKey::Enter));
    let insert = out
        .iter()
        .find_map(|o| {
            if let Output::Insert(r) = o {
                Some(r.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        (insert.text.as_str(), insert.replace, insert.field),
        ("Sure, noon works!", Replace::Nothing, FieldId(1))
    );
    let out = ev(
        &mut c,
        Event::InsertDone {
            id: insert.id,
            outcome: InsertOutcome::Inserted,
        },
    );
    assert!(out.contains(&Output::Record(Stat::DraftInserted { app: CHAT.into() })));
    assert!(hidden(&out));
}

#[test]
fn invalid_directions_retry_once_then_ask() {
    let mut c = controller();
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Action(Action::Reply));
    let out = c.handle(Input::Conversation {
        field: FieldId(1),
        conversation: Some(chat_messages()),
    });
    let first = generates(&out)[0].id;
    let out = c.handle(Input::Generated {
        id: first,
        update: GenerateUpdate::Done(Ok("Sure!".into())),
    });
    let retry = generates(&out)[0].id;
    let out = c.handle(Input::Generated {
        id: retry,
        update: GenerateUpdate::Done(Ok("Sure!".into())),
    });
    assert!(generates(&out).is_empty());
    let view = card(&out);
    assert!(view.input.is_some() && view.status.is_some());
    for ch in "say yes".chars() {
        ev(&mut c, Event::CardKey(CardKey::Char(ch)));
    }
    let out = ev(&mut c, Event::CardKey(CardKey::Enter));
    assert!(generates(&out)[0].messages[1].content.contains("say yes"));
}

#[test]
fn without_a_conversation_directions_use_the_field_text() {
    let mut c = controller();
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "Thanks for")));
    ev(&mut c, Event::Action(Action::Reply));
    let out = c.handle(Input::Conversation {
        field: FieldId(1),
        conversation: None,
    });
    let request = generates(&out)[0];
    assert!(request.messages[1].content.contains("Thanks for"));
}

#[test]
fn without_anything_to_reply_to_the_card_asks_for_a_direction() {
    let mut c = controller();
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Action(Action::Reply));
    let out = c.handle(Input::Conversation {
        field: FieldId(1),
        conversation: None,
    });
    assert!(generates(&out).is_empty());
    let view = card(&out);
    assert_eq!(view.input.as_deref(), Some(""));
    assert!(view.status.is_some());
}

#[test]
fn rewrite_flow_with_translate_and_unknown_insert() {
    let mut c = controller();
    let mut s = snap(1, 1, CHAT, "Hi ");
    s.selection = "teh cat".into();
    ev(&mut c, Event::Focus(s));
    let out = ev(&mut c, Event::Action(Action::Reply));
    assert!(!out.iter().any(|o| matches!(o, Output::ReadConversation(_))));
    let view = card(&out);
    assert_eq!(view.title, "Rewrite");
    assert_eq!(view.items.len(), 6);
    let out = ev(&mut c, Event::CardKey(CardKey::Pick(5)));
    assert!(generates(&out).is_empty());
    assert_eq!(card(&out).input.as_deref(), Some(""));
    for ch in "German".chars() {
        ev(&mut c, Event::CardKey(CardKey::Char(ch)));
    }
    let out = ev(&mut c, Event::CardKey(CardKey::Enter));
    let request = generates(&out)[0].clone();
    assert!(
        request.messages[1]
            .content
            .contains("Translate it into German.")
    );
    assert!(request.messages[1].content.contains("teh cat"));
    c.handle(Input::Generated {
        id: request.id,
        update: GenerateUpdate::Done(Ok("die Katze".into())),
    });
    let out = ev(&mut c, Event::CardKey(CardKey::Enter));
    let insert = out
        .iter()
        .find_map(|o| {
            if let Output::Insert(r) = o {
                Some(r.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        (insert.text.as_str(), insert.replace),
        ("die Katze", Replace::Selection)
    );
    let out = ev(
        &mut c,
        Event::InsertDone {
            id: insert.id,
            outcome: InsertOutcome::Unknown,
        },
    );
    let view = card(&out);
    assert_eq!(view.preview.as_deref(), Some("die Katze"));
    assert!(view.status.is_some());
    let out = ev(&mut c, Event::CardKey(CardKey::Escape));
    assert!(hidden(&out));
}

#[test]
fn card_escape_cancels_generation_and_focus_closes_it() {
    let mut c = controller();
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Action(Action::Reply));
    let out = c.handle(Input::Conversation {
        field: FieldId(1),
        conversation: Some(chat_messages()),
    });
    let id = generates(&out)[0].id;
    let out = ev(&mut c, Event::CardKey(CardKey::Escape));
    assert!(out.contains(&Output::CancelGenerate(id)) && hidden(&out));
    ev(&mut c, Event::Action(Action::Reply));
    let out = ev(&mut c, Event::Focus(snap(2, 1, CHAT, "")));
    assert!(hidden(&out));
}

#[test]
fn reply_without_a_draft_model_says_so() {
    let mut c = controller();
    c.handle(Input::Caps {
        feature: Feature::Reply,
        caps: None,
    });
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    let out = ev(&mut c, Event::Action(Action::Reply));
    assert!(card(&out).status.is_some());
    assert!(
        generates(&out).is_empty() && !out.iter().any(|o| matches!(o, Output::ReadConversation(_)))
    );
}

#[test]
fn draft_errors_show_in_the_card() {
    let mut c = controller();
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Action(Action::Reply));
    c.handle(Input::Conversation {
        field: FieldId(1),
        conversation: None,
    });
    for ch in "say hi".chars() {
        ev(&mut c, Event::CardKey(CardKey::Char(ch)));
    }
    let out = ev(&mut c, Event::CardKey(CardKey::Enter));
    let id = generates(&out)[0].id;
    let out = c.handle(Input::Generated {
        id,
        update: GenerateUpdate::Done(Err(CompleteError::Unavailable("offline".into()))),
    });
    let view = card(&out);
    assert!(
        view.status
            .as_deref()
            .is_some_and(|s| s.contains("offline"))
    );
    assert!(view.preview.is_none());
}

/// Regression: Enter on the empty "which language?" input ran Fix instead of
/// waiting for the language.
#[test]
fn enter_without_a_language_waits() {
    let mut c = controller();
    let mut s = snap(1, 1, CHAT, "");
    s.selection = "teh cat".into();
    ev(&mut c, Event::Focus(s));
    ev(&mut c, Event::Action(Action::Reply));
    ev(&mut c, Event::CardKey(CardKey::Pick(5)));
    let out = ev(&mut c, Event::CardKey(CardKey::Enter));
    assert!(generates(&out).is_empty(), "{out:?}");
    assert_eq!(card(&out).input.as_deref(), Some(""));
}

/// Regression: alternatives kept the revision they were shown at, so cycling
/// after a snapshot without a text change sent a stale revision.
#[test]
fn alternatives_follow_the_current_revision() {
    let caps = Caps {
        cheap_alternatives: true,
        ..LOCAL
    };
    let mut c = controller_with(Settings::default(), caps);
    ev(&mut c, Event::Focus(snap(1, 1, CHAT, "")));
    ev(&mut c, Event::Changed(snap(1, 2, CHAT, "I will")));
    let r = one_complete(&c.handle(Input::Timer(TimerId::Pause)));
    complete(&mut c, r.id, &[" call you. ", " be late. "]);
    assert!(ev(&mut c, Event::Changed(snap(1, 3, CHAT, "I will"))).is_empty());
    let out = ev(&mut c, Event::Action(Action::NextAlternative));
    assert_eq!(suggestion(&out).unwrap().revision, 3);
}
