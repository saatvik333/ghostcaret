//! The runtime end to end, with a fake backend, host and models. Every wait
//! is for an event, bounded by a deadline; nothing sleeps.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use oft_cloud::SentLog;
use oft_engine::complete::{
    CancelToken, Candidate, Caps, CompleteError, Completer, Completion, CompletionRequest,
    GenerateRequest, Generator, StopCheck,
};
use oft_engine::io::{
    Action, AppInfo, Author, Backend, Bindings, CardKey, CardView, Conversation, Event, FieldId,
    FieldKind, FieldSnapshot, InsertOutcome, InsertRequest, Message, Overlay, Replace,
    SuggestionView,
};
use oft_engine::settings::{Appearance, ModelChoice, ProviderConfig, ProviderKind, Settings};

use crate::models::{CloudModel, Loader, LocalModel};
use crate::{Command, Config, Host, Runtime, StatusKind, StatusView, start_with};

const WAIT: Duration = Duration::from_secs(10);
const CHAT: &str = "org.telegram.desktop";
const LOCAL_FILE: &str = "/models/small.gguf";

type CompleteFn = Arc<
    dyn Fn(&CompletionRequest, &CancelToken) -> Result<Completion, CompleteError> + Send + Sync,
>;
type GenerateFn = Arc<
    dyn Fn(&GenerateRequest, &mut dyn FnMut(&str)) -> Result<String, CompleteError> + Send + Sync,
>;

#[derive(Clone)]
struct Script {
    complete: CompleteFn,
    generate: GenerateFn,
}

fn completion(request: &CompletionRequest, text: &str) -> Completion {
    Completion {
        id: request.id,
        candidates: vec![Candidate {
            text: text.into(),
            ..Candidate::default()
        }],
    }
}

/// Completes every request with `text`; generates nothing.
fn replies(text: &'static str) -> Script {
    Script {
        complete: Arc::new(move |request, _| Ok(completion(request, text))),
        generate: Arc::new(|_, _| Err(CompleteError::Failed("not scripted".into()))),
    }
}

/// Runs until cancelled, checking between tokens like a real model.
fn until_cancelled(
    request: &CompletionRequest,
    cancel: &CancelToken,
) -> Result<Completion, CompleteError> {
    let deadline = Instant::now() + WAIT;
    while !cancel.is_cancelled() {
        if Instant::now() > deadline {
            return Ok(completion(request, " stale"));
        }
        std::thread::yield_now();
    }
    Err(CompleteError::Cancelled)
}

struct FakeModel {
    local: bool,
    script: Script,
    unloads: Sender<()>,
}

impl std::fmt::Debug for FakeModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeModel")
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

impl Completer for FakeModel {
    fn caps(&self) -> Caps {
        Caps {
            token_probs: false,
            cheap_alternatives: false,
            prefill: false,
            local: self.local,
        }
    }

    fn complete(
        &self,
        request: &CompletionRequest,
        stop: &mut dyn StopCheck,
        cancel: &CancelToken,
    ) -> Result<Completion, CompleteError> {
        // The runtime hands every request a fresh stop rule.
        assert!(!stop.should_stop("", None));
        (self.script.complete)(request, cancel)
    }
}

impl Generator for FakeModel {
    fn generate(
        &self,
        request: &GenerateRequest,
        _cancel: &CancelToken,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, CompleteError> {
        (self.script.generate)(request, on_delta)
    }
}

impl CloudModel for FakeModel {
    fn warm(&self) {}
}

impl LocalModel for FakeModel {
    fn unload(&self) {
        let _ = self.unloads.send(());
    }
}

struct FakeLoader {
    cloud: Script,
    local: Script,
    fallback: Option<PathBuf>,
    loads: Sender<PathBuf>,
    unloads: Sender<()>,
}

impl Loader for FakeLoader {
    fn provider(
        &self,
        config: &ProviderConfig,
        key: Option<String>,
    ) -> Result<Arc<dyn CloudModel>, String> {
        assert_eq!(
            (config.id.as_str(), key.as_deref()),
            ("cloud", Some("test-key"))
        );
        Ok(Arc::new(FakeModel {
            local: false,
            script: self.cloud.clone(),
            unloads: self.unloads.clone(),
        }))
    }

    fn resolve(&self, model: &str) -> Result<PathBuf, String> {
        Ok(PathBuf::from(model))
    }

    fn fallback(&self) -> Option<PathBuf> {
        self.fallback.clone()
    }

    fn load(
        &self,
        path: &Path,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Arc<dyn LocalModel>, String> {
        progress(0.5);
        progress(1.0);
        let _ = self.loads.send(path.to_owned());
        Ok(Arc::new(FakeModel {
            local: true,
            script: self.local.clone(),
            unloads: self.unloads.clone(),
        }))
    }
}

struct FakeHost {
    settings: Result<Settings, String>,
    statuses: Sender<StatusView>,
}

impl Host for FakeHost {
    fn settings(&self) -> Result<Settings, String> {
        self.settings.clone()
    }

    fn api_key(&self, _provider: &str) -> Result<Option<String>, String> {
        Ok(Some("test-key".into()))
    }

    fn status(&self, status: &StatusView) {
        let _ = self.statuses.send(status.clone());
    }
}

#[derive(Debug)]
enum Call {
    Overlay(Overlay),
    Insert(InsertRequest),
}

struct FakeBackend {
    calls: Sender<Call>,
    conversation: Option<Conversation>,
}

impl Backend for FakeBackend {
    fn set_overlay(&self, overlay: Overlay) {
        let _ = self.calls.send(Call::Overlay(overlay));
    }
    fn set_bindings(&self, _bindings: Bindings) {}
    fn set_appearance(&self, _appearance: Appearance) {}
    fn set_active(&self, _active: bool) {}
    fn insert(&self, request: InsertRequest) {
        let _ = self.calls.send(Call::Insert(request));
    }
    fn conversation(&self, _field: FieldId) -> Option<Conversation> {
        self.conversation.clone()
    }
}

struct Harness {
    runtime: Runtime,
    calls: Receiver<Call>,
    statuses: Receiver<StatusView>,
    loads: Receiver<PathBuf>,
    unloads: Receiver<()>,
}

struct Setup {
    settings: Result<Settings, String>,
    cloud: Script,
    local: Script,
    fallback: Option<PathBuf>,
    conversation: Option<Conversation>,
}

impl Default for Setup {
    fn default() -> Setup {
        Setup {
            settings: Ok(Settings::default()),
            cloud: replies(" from the cloud. "),
            local: replies(" from the device. "),
            fallback: None,
            conversation: None,
        }
    }
}

impl Harness {
    fn start(setup: Setup) -> Harness {
        let (calls_tx, calls) = unbounded();
        let (statuses_tx, statuses) = unbounded();
        let (loads_tx, loads) = unbounded();
        let (unloads_tx, unloads) = unbounded();
        let host = Arc::new(FakeHost {
            settings: setup.settings,
            statuses: statuses_tx,
        });
        let loader = Arc::new(FakeLoader {
            cloud: setup.cloud,
            local: setup.local,
            fallback: setup.fallback,
            loads: loads_tx,
            unloads: unloads_tx,
        });
        let backend: Arc<dyn Backend> = Arc::new(FakeBackend {
            calls: calls_tx,
            conversation: setup.conversation,
        });
        let config = Config {
            stats: None,
            style: None,
            sent_log: SentLog::default(),
            #[cfg(feature = "local")]
            models_dir: PathBuf::new(),
            #[cfg(feature = "local")]
            load_options: oft_local::LoadOptions::default(),
        };
        let runtime =
            start_with(host, config, move |_| Ok(backend), loader).expect("the runtime starts");
        Harness {
            runtime,
            calls,
            statuses,
            loads,
            unloads,
        }
    }

    fn event(&self, event: Event) {
        self.runtime.event(event);
    }

    fn wait_status(&self, what: &str, wanted: impl Fn(&StatusView) -> bool) -> StatusView {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.statuses.recv_deadline(deadline) {
                Ok(status) if wanted(&status) => return status,
                Ok(_) => {}
                Err(_) => panic!("no status {what}; now {:?}", self.runtime.status()),
            }
        }
    }

    /// Skips backend calls until `wanted` takes one.
    fn wait_call<T>(&self, what: &str, mut wanted: impl FnMut(Call) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            let call = self
                .calls
                .recv_deadline(deadline)
                .unwrap_or_else(|_| panic!("no {what}"));
            if let Some(found) = wanted(call) {
                return found;
            }
        }
    }

    fn wait_suggestion(&self, revision: u64) -> SuggestionView {
        self.wait_call("suggestion", |call| match call {
            Call::Overlay(Overlay::Suggestion(view)) => {
                assert_eq!(
                    view.revision, revision,
                    "a suggestion for another revision: {view:?}"
                );
                Some(view)
            }
            _ => None,
        })
    }

    fn wait_card(&self, what: &str, wanted: impl Fn(&CardView) -> bool) -> CardView {
        self.wait_call(what, |call| match call {
            Call::Overlay(Overlay::Card(card)) if wanted(&card) => Some(card),
            _ => None,
        })
    }
}

fn snap(revision: u64, app: &str, before: &str) -> FieldSnapshot {
    FieldSnapshot {
        field: FieldId(1),
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

fn provider() -> ProviderConfig {
    ProviderConfig {
        id: "cloud".into(),
        label: "Cloud".into(),
        kind: ProviderKind::Anthropic,
        model: "m".into(),
    }
}

fn cloud_settings() -> Settings {
    let cloud = ModelChoice::Provider {
        provider: "cloud".into(),
    };
    Settings {
        autocomplete_model: cloud.clone(),
        draft_model: cloud,
        providers: vec![provider()],
        ..Settings::default()
    }
}

/// A local autocomplete model that never unloads for idleness; idle minutes
/// are milliseconds in tests.
fn local_settings() -> Settings {
    let model = ModelChoice::Local {
        model: LOCAL_FILE.into(),
    };
    Settings {
        autocomplete_model: model,
        unload_after_minutes: 0,
        ..Settings::default()
    }
}

fn active(status: &StatusView) -> bool {
    status.kind == StatusKind::Active
}

#[test]
fn typing_then_pausing_shows_the_completion() {
    let h = Harness::start(Setup {
        settings: Ok(cloud_settings()),
        ..Setup::default()
    });
    h.wait_status("active", active);
    h.event(Event::Focus(snap(1, CHAT, "")));
    h.event(Event::Changed(snap(2, CHAT, "I will")));
    let view = h.wait_suggestion(2);
    assert_eq!(view.field, FieldId(1));
    assert!(
        view.text.starts_with(" from") && !view.visible().is_empty(),
        "{view:?}"
    );
    let timings = h.runtime.timings();
    let shown: Vec<_> = timings.iter().filter(|t| t.shown).collect();
    assert_eq!(shown.len(), 1, "{timings:?}");
    assert!(shown[0].total_us >= shown[0].wait_us + shown[0].decode_us);
}

#[test]
fn a_new_edit_cancels_the_completion_in_flight() {
    let (started_tx, started) = unbounded();
    let cloud = Script {
        complete: Arc::new(move |request, cancel| {
            if request.revision == 2 {
                let _ = started_tx.send(request.id);
                return until_cancelled(request, cancel);
            }
            Ok(completion(request, " there. "))
        }),
        ..replies("")
    };
    let h = Harness::start(Setup {
        settings: Ok(cloud_settings()),
        cloud,
        ..Setup::default()
    });
    h.wait_status("active", active);
    h.event(Event::Focus(snap(1, CHAT, "")));
    h.event(Event::Changed(snap(2, CHAT, "I will")));
    let first = started
        .recv_timeout(WAIT)
        .expect("the first completion starts");
    h.event(Event::Changed(snap(3, CHAT, "I will be")));
    // Only the newer revision's suggestion appears.
    let view = h.wait_suggestion(3);
    assert!(view.text.starts_with(" there"), "{view:?}");
    let timings = h.runtime.timings();
    let cancelled = timings
        .iter()
        .find(|t| t.id == first)
        .expect("the first request is timed");
    assert!(cancelled.cancelled && !cancelled.shown, "{timings:?}");
}

#[test]
fn the_daily_cap_moves_autocomplete_to_the_local_model() {
    let sent = Arc::new(AtomicUsize::new(0));
    let counted = sent.clone();
    let cloud = Script {
        complete: Arc::new(move |request, _| {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(completion(request, " from the cloud. "))
        }),
        ..replies("")
    };
    let settings = Settings {
        daily_request_cap: Some(1),
        unload_after_minutes: 0,
        ..cloud_settings()
    };
    let h = Harness::start(Setup {
        settings: Ok(settings),
        cloud,
        fallback: Some(PathBuf::from("/models/fallback.gguf")),
        ..Setup::default()
    });
    h.wait_status("active", active);
    h.event(Event::Focus(snap(1, CHAT, "")));
    h.event(Event::Changed(snap(2, CHAT, "I will")));
    assert!(h.wait_suggestion(2).text.starts_with(" from the cloud"));

    // Over the cap: the request goes nowhere, and the fallback model loads.
    h.event(Event::Changed(snap(3, CHAT, "I will be")));
    assert_eq!(
        h.loads.recv_timeout(WAIT).unwrap(),
        Path::new("/models/fallback.gguf")
    );
    let status = h.wait_status("cap reached", |s| {
        s.kind == StatusKind::CloudUnavailable && s.local_model_loaded
    });
    assert_eq!(
        status.text,
        "Cloud unavailable: daily request cap reached; the local model took over"
    );
    assert_eq!(
        status.detail.as_deref(),
        Some("daily request cap reached; the local model took over")
    );

    h.event(Event::Changed(snap(4, CHAT, "I will be there")));
    assert!(h.wait_suggestion(4).text.starts_with(" from the device"));
    assert_eq!(sent.load(Ordering::Relaxed), 1);
}

#[test]
fn memory_pressure_unloads_the_local_model_until_it_is_needed() {
    let h = Harness::start(Setup {
        settings: Ok(local_settings()),
        ..Setup::default()
    });
    h.event(Event::Focus(snap(1, CHAT, "")));
    assert_eq!(h.loads.recv_timeout(WAIT).unwrap(), Path::new(LOCAL_FILE));
    h.wait_status("loaded", |s| s.local_model_loaded);

    h.runtime.send(Command::MemoryPressure);
    h.unloads.recv_timeout(WAIT).expect("the model is unloaded");
    h.wait_status("unloaded", |s| !s.local_model_loaded);

    // The next request loads it again.
    h.event(Event::Changed(snap(2, CHAT, "I will")));
    assert_eq!(h.loads.recv_timeout(WAIT).unwrap(), Path::new(LOCAL_FILE));
    h.wait_status("loaded again", |s| s.local_model_loaded);
}

#[test]
fn a_model_loaded_for_a_focus_unloads_when_idle() {
    // Reply & rewrite's model loads on focus even without local autocomplete.
    let model = ModelChoice::Local {
        model: LOCAL_FILE.into(),
    };
    let settings = Settings {
        draft_model: model,
        unload_after_minutes: 1,
        ..Settings::default()
    };
    let h = Harness::start(Setup {
        settings: Ok(settings),
        ..Setup::default()
    });
    h.event(Event::Focus(snap(1, CHAT, "")));
    assert_eq!(h.loads.recv_timeout(WAIT).unwrap(), Path::new(LOCAL_FILE));
    h.wait_status("loaded", |s| s.local_model_loaded);
    h.unloads
        .recv_timeout(WAIT)
        .expect("the idle model is unloaded");
    h.wait_status("unloaded", |s| !s.local_model_loaded);
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
fn reply_flow_streams_the_draft_and_inserts_it() {
    let cloud = Script {
        generate: Arc::new(|request, on_delta| {
            let asks = |text: &str| request.messages.iter().any(|m| m.content.contains(text));
            if asks("Accept and suggest noon") {
                on_delta("Sure, ");
                on_delta("noon works");
                return Ok("Sure, noon works!".into());
            }
            assert!(
                asks("Lunch on Tuesday?"),
                "the directions see the conversation: {request:?}"
            );
            Ok("1. Accept and suggest noon\n2. Decline politely\n3. Ask about Wednesday".into())
        }),
        ..replies("")
    };
    let h = Harness::start(Setup {
        settings: Ok(cloud_settings()),
        cloud,
        conversation: Some(chat_messages()),
        ..Setup::default()
    });
    h.wait_status("active", active);
    h.event(Event::Focus(snap(1, CHAT, "")));
    h.event(Event::Action(Action::FlowDraft));
    h.wait_card("directions", |card| card.items.len() == 3);
    h.event(Event::CardKey(CardKey::Pick(1)));
    h.wait_card("finished draft", |card| {
        card.preview.as_deref() == Some("Sure, noon works!")
    });
    h.event(Event::CardKey(CardKey::Enter));
    let insert = h.wait_call("insert", |call| match call {
        Call::Insert(request) => Some(request),
        Call::Overlay(_) => None,
    });
    assert_eq!(
        (insert.text.as_str(), insert.replace, insert.field),
        ("Sure, noon works!", Replace::Nothing, FieldId(1))
    );
    h.event(Event::InsertDone {
        id: insert.id,
        outcome: InsertOutcome::Inserted,
    });
    h.wait_call("hidden card", |call| {
        matches!(call, Call::Overlay(Overlay::Hidden)).then_some(())
    });
}

#[test]
fn rejected_settings_run_on_defaults_and_say_why() {
    let h = Harness::start(Setup {
        settings: Err("setting `boldness` is invalid".into()),
        ..Setup::default()
    });
    let status = h.wait_status("settings problem", |s| {
        s.kind == StatusKind::SettingsInvalid
    });
    assert_eq!(
        status.detail.as_deref(),
        Some("setting `boldness` is invalid")
    );
}
