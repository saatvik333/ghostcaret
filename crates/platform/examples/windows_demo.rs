//! Runs the Windows backend with a tiny stand-in controller.
//!
//! Type `hello` in any text field: the suggestion ` world, how are you
//! today?` appears at the caret. Tab takes a word, ` takes the visible part,
//! Alt+→ shows more, Alt+↓/↑ switch between two alternatives, Esc dismisses.
//! Tap right Alt for a demo Reply & rewrite card (↑↓ or 1–3 choose, Tab types a
//! direction, Enter inserts, Esc closes). Events are printed as they arrive;
//! the key gate's p50 and p99 are printed at exit.
//!
//! ```sh
//! cargo xwin build -p ghostcaret-platform --example windows_demo --target x86_64-pc-windows-msvc --release
//! windows_demo.exe 120   # seconds to run, on Windows
//! ```

#[cfg(target_os = "windows")]
mod demo {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use ghostcaret_engine::io::{
        Action, Backend, CardKey, CardView, Event, FieldId, FieldSnapshot, InsertRequest, Overlay,
        Replace, SuggestionView,
    };

    const TRIGGER: &str = "hello";
    const ALTERNATIVES: [&str; 2] = [" world, how are you today?", " there, nice to meet you."];
    const DIRECTIONS: [&str; 3] = ["Say thanks", "Ask for details", "Decline politely"];
    const DRAFTS: [&str; 3] = [
        "Thanks so much, that works for me!",
        "Could you share a few more details first?",
        "Thanks for asking, but I can't make it this time.",
    ];

    /// Word units end after each run of letters and after each punctuation
    /// mark; Show More steps end after the first comma, then at each space.
    fn view(field: FieldId, revision: u64, alternative: usize) -> SuggestionView {
        let text = ALTERNATIVES[alternative];
        let mut word_ends = Vec::new();
        let mut in_word = false;
        for (i, c) in text.char_indices() {
            let end = i + c.len_utf8();
            if c.is_alphanumeric() {
                in_word = true;
                if text[end..]
                    .chars()
                    .next()
                    .is_none_or(|n| !n.is_alphanumeric())
                {
                    word_ends.push(end);
                }
            } else {
                if !c.is_whitespace() && (in_word || !word_ends.contains(&i)) {
                    word_ends.push(end);
                }
                in_word = false;
            }
        }
        let first_step = text.find(',').map_or(text.len(), |i| i + 1);
        let mut step_ends: Vec<usize> = text
            .match_indices(' ')
            .map(|(i, _)| i)
            .filter(|&i| i > first_step)
            .collect();
        step_ends.push(text.len());
        step_ends.dedup();
        SuggestionView {
            field,
            revision,
            text: text.into(),
            shown: first_step,
            word_ends,
            step_ends,
            alternative: Some((alternative as u8 + 1, ALTERNATIVES.len() as u8)),
        }
    }

    struct Demo {
        backend: Arc<dyn Backend>,
        snapshot: Option<FieldSnapshot>,
        /// Caret offset at which the suggestion was last offered.
        offered_at: Option<usize>,
        alternative: usize,
        card: Option<CardView>,
        next_insert: u64,
    }

    impl Demo {
        fn snapshot(&mut self, snap: FieldSnapshot) {
            println!(
                "  field {} rev {} {:?} ({}) kind {:?} caret {:?}: …{:?}|{:?}",
                snap.field.0,
                snap.revision,
                snap.app.id,
                snap.app.name,
                snap.kind,
                snap.caret
                    .map(|r| (r.x.round(), r.y.round(), r.height.round())),
                tail(&snap.before, 24),
                head(&snap.after, 12)
            );
            let at = snap.before.len();
            if self.card.is_none()
                && snap.selection.is_empty()
                && snap.before.ends_with(TRIGGER)
                && self.offered_at != Some(at)
                && !snap.kind.is_secure_or_structured()
            {
                self.offered_at = Some(at);
                self.alternative = 0;
                self.backend
                    .set_overlay(Overlay::Suggestion(view(snap.field, snap.revision, 0)));
            }
            self.snapshot = Some(snap);
        }

        fn cycle(&mut self, step: isize) {
            let Some(snap) = &self.snapshot else { return };
            let n = ALTERNATIVES.len() as isize;
            self.alternative = (self.alternative as isize + step).rem_euclid(n) as usize;
            self.backend.set_overlay(Overlay::Suggestion(view(
                snap.field,
                snap.revision,
                self.alternative,
            )));
        }

        fn open_card(&mut self) {
            let Some(snap) = &self.snapshot else { return };
            let card = CardView {
                field: snap.field,
                title: if snap.selection.is_empty() {
                    "Reply".into()
                } else {
                    "Rewrite".into()
                },
                items: DIRECTIONS.iter().map(|s| s.to_string()).collect(),
                selected: Some(0),
                input: None,
                preview: Some(DRAFTS[0].into()),
                status: None,
                hint: "↑↓ or 1–3 choose · Tab type · Enter insert · Esc close".into(),
            };
            self.show_card(card);
        }

        fn show_card(&mut self, card: CardView) {
            self.backend.set_overlay(Overlay::Card(card.clone()));
            self.card = Some(card);
        }

        fn card_key(&mut self, key: CardKey) {
            let Some(mut card) = self.card.take() else {
                return;
            };
            let n = card.items.len();
            let select = |card: &mut CardView, i: usize| {
                card.selected = Some(i);
                card.preview = Some(DRAFTS[i].into());
            };
            match key {
                CardKey::Escape => {
                    self.backend.set_overlay(Overlay::Hidden);
                    return;
                }
                CardKey::Up => {
                    let i = card.selected.map_or(0, |s| (s + n - 1) % n);
                    select(&mut card, i);
                }
                CardKey::Down => {
                    let i = card.selected.map_or(0, |s| (s + 1) % n);
                    select(&mut card, i);
                }
                CardKey::Pick(i) if (1..=n as u8).contains(&i) => select(&mut card, i as usize - 1),
                CardKey::Tab => {
                    card.input = if card.input.is_some() {
                        None
                    } else {
                        Some(String::new())
                    }
                }
                CardKey::Char(c) => {
                    if let Some(input) = &mut card.input {
                        input.push(c);
                        card.preview = Some(format!("(a draft for “{input}” would stream here)"));
                    }
                }
                CardKey::Backspace => {
                    if let Some(input) = &mut card.input {
                        input.pop();
                    }
                }
                CardKey::Enter => {
                    let text = card.preview.clone().unwrap_or_default();
                    let replace = if self
                        .snapshot
                        .as_ref()
                        .is_some_and(|s| !s.selection.is_empty())
                    {
                        Replace::Selection
                    } else {
                        Replace::Nothing
                    };
                    self.next_insert += 1;
                    self.backend.set_overlay(Overlay::Hidden);
                    self.backend.insert(InsertRequest {
                        id: self.next_insert,
                        field: card.field,
                        text,
                        replace,
                    });
                    return;
                }
                CardKey::Pick(_) => {}
            }
            self.show_card(card);
        }
    }

    fn tail(s: &str, n: usize) -> &str {
        let mut i = s.len().saturating_sub(n);
        while !s.is_char_boundary(i) {
            i += 1;
        }
        &s[i..]
    }

    fn head(s: &str, n: usize) -> &str {
        let mut i = s.len().min(n);
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        &s[..i]
    }

    pub fn main() {
        let seconds: u64 = std::env::args()
            .nth(1)
            .and_then(|a| a.parse().ok())
            .unwrap_or(120);
        let (tx, rx) = crossbeam_channel::unbounded();
        let backend = match ghostcaret_platform::start(tx) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("cannot start: {e}");
                std::process::exit(1);
            }
        };
        println!("Ghostcaret Windows backend demo: running for {seconds} s.");
        println!(
            "Type `{TRIGGER}` in a text field. Tab takes a word, ` takes the rest, Alt+Right shows more,"
        );
        println!(
            "Alt+Down/Up switches alternatives, Esc dismisses, a tap on right Alt opens a demo card."
        );
        let mut demo = Demo {
            backend,
            snapshot: None,
            offered_at: None,
            alternative: 0,
            card: None,
            next_insert: 0,
        };
        let end = Instant::now() + Duration::from_secs(seconds);
        while let Ok(event) = rx.recv_deadline(end) {
            match event {
                Event::Focus(snap) => {
                    println!("Focus");
                    demo.offered_at = None;
                    demo.card = None;
                    demo.snapshot(snap);
                }
                Event::Changed(snap) => {
                    println!("Changed");
                    demo.snapshot(snap);
                }
                Event::Blur => {
                    println!("Blur");
                    demo.snapshot = None;
                    demo.card = None;
                }
                Event::Action(Action::Reply) => {
                    println!("Action(ReplyAndRewrite)");
                    demo.open_card();
                }
                Event::Action(Action::NextAlternative) => {
                    println!("Action(NextAlternative)");
                    demo.cycle(1);
                }
                Event::Action(Action::PrevAlternative) => {
                    println!("Action(PrevAlternative)");
                    demo.cycle(-1);
                }
                Event::CardKey(key) => {
                    println!("CardKey({key:?})");
                    demo.card_key(key);
                }
                other => println!("{other:?}"),
            }
        }
        drop(demo);
        let s = ghostcaret_platform::windows::gate_latency();
        println!(
            "key gate over {} key events: p50 {:.1} µs, p99 {:.1} µs, max {:.1} µs",
            s.count,
            s.p50_ns as f64 / 1000.0,
            s.p99_ns as f64 / 1000.0,
            s.max_ns as f64 / 1000.0
        );
        println!("Press Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

#[cfg(target_os = "windows")]
fn main() {
    demo::main();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("windows_demo runs on Windows; build it with cargo xwin for x86_64-pc-windows-msvc");
}
