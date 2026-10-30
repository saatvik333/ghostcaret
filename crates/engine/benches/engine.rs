//! Microbenchmarks for the stop rules, the secret filter, prompt building and
//! the controller's keystroke path (docs/quality.md, "Tests").

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ghostcaret_engine::complete::{Candidate, Caps, Completion, LengthMode, StopCheck, StopParams};
use ghostcaret_engine::controller::{Controller, Feature, Input, Output, TimerId};
use ghostcaret_engine::io::{AppInfo, Event, FieldId, FieldSnapshot};
use ghostcaret_engine::settings::Settings;
use ghostcaret_engine::stop::{CutContext, Stopper, cut};
use ghostcaret_engine::{filter, prompt};

const BEFORE: &str = "Thanks for sending the draft over. I read through it this morning and";
const CONTINUATION: &str = " I think the structure works well, although the second section could be shorter. \
Could we talk about it tomorrow at noon? ";

fn candidate() -> Candidate {
    let mut token_ends: Vec<usize> = CONTINUATION
        .match_indices(' ')
        .map(|(i, _)| i)
        .filter(|&i| i > 0)
        .collect();
    token_ends.push(CONTINUATION.len());
    let token_probs = (0..token_ends.len())
        .map(|i| 0.97 - 0.01 * i as f32)
        .collect();
    Candidate {
        text: CONTINUATION.into(),
        token_ends,
        token_probs,
    }
}

fn auto() -> StopParams {
    StopParams {
        mode: LengthMode::Auto,
        ceiling: LengthMode::Paragraph,
        boldness: 0.5,
        paragraph_words: 60,
    }
}

fn stop_rules(c: &mut Criterion) {
    let candidate = candidate();
    let ctx = CutContext {
        before: BEFORE,
        after: "",
    };
    c.bench_function("cut", |b| b.iter(|| cut(&candidate, &auto(), &ctx)));
    c.bench_function("stopper, one generation", |b| {
        b.iter(|| {
            let mut stopper = Stopper::new(&auto());
            for (&end, &p) in candidate.token_ends.iter().zip(&candidate.token_probs) {
                if stopper.should_stop(&candidate.text[..end], Some(p)) {
                    break;
                }
            }
        })
    });
}

fn filter_and_prompt(c: &mut Criterion) {
    let text = format!("{BEFORE}{CONTINUATION}").repeat(16);
    c.bench_function("redact, 3 KB without secrets", |b| {
        b.iter(|| filter::redact(&text))
    });
    let field = snapshot(1, &text);
    let settings = Settings::default();
    c.bench_function("prompt build, 3 KB field", |b| {
        b.iter(|| prompt::build(&field, &settings, &prompt::Context::default()))
    });
}

fn snapshot(revision: u64, before: &str) -> FieldSnapshot {
    FieldSnapshot {
        field: FieldId(1),
        revision,
        app: AppInfo {
            id: "org.telegram.desktop".into(),
            name: "Telegram".into(),
            title: String::new(),
        },
        multiline: true,
        before: before.into(),
        ..FieldSnapshot::default()
    }
}

/// A controller showing a suggestion after `BEFORE`.
fn showing() -> Controller {
    let mut c = Controller::new(Settings::default());
    c.handle(Input::Settings(Box::default()));
    let caps = Caps {
        token_probs: true,
        cheap_alternatives: false,
        prefill: false,
        local: true,
    };
    c.handle(Input::Caps {
        feature: Feature::Autocomplete,
        caps: Some(caps),
    });
    c.handle(Input::Event(Event::Focus(snapshot(1, ""))));
    c.handle(Input::Event(Event::Changed(snapshot(2, BEFORE))));
    let out = c.handle(Input::Timer(TimerId::Pause));
    let Some(Output::Complete(request)) =
        out.into_iter().find(|o| matches!(o, Output::Complete(_)))
    else {
        panic!("no request")
    };
    let out = c.handle(Input::Completed {
        id: request.id,
        result: Ok(Completion {
            id: request.id,
            candidates: vec![candidate()],
        }),
    });
    assert!(
        out.iter().any(|o| matches!(o, Output::Overlay(_))),
        "no suggestion shown"
    );
    c
}

fn keystroke(c: &mut Criterion) {
    c.bench_function("keystroke, typing through a suggestion", |b| {
        b.iter_batched(
            || (showing(), snapshot(3, &format!("{BEFORE} "))),
            |(mut c, snap)| {
                c.handle(Input::Event(Event::TypedThrough {
                    field: FieldId(1),
                    text: " ".into(),
                }));
                let out = c.handle(Input::Event(Event::Changed(snap)));
                (c, out)
            },
            BatchSize::SmallInput,
        )
    });
    c.bench_function("keystroke, ordinary edit", |b| {
        let mut c = showing();
        let mut revision = 3;
        b.iter_batched(
            || {
                revision += 1;
                snapshot(
                    revision,
                    if revision % 2 == 0 {
                        "Thanks for the"
                    } else {
                        "Thanks for th"
                    },
                )
            },
            |snap| c.handle(Input::Event(Event::Changed(snap))),
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, stop_rules, filter_and_prompt, keystroke);
criterion_main!(benches);
