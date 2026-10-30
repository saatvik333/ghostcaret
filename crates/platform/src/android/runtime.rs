//! The engine runtime inside the keyboard, shaped like the desktop runtime:
//! one thread owns the [`Controller`], feeds it backend events, model results
//! and timers in arrival order, and executes its outputs. Models run on
//! worker threads, so the runtime thread never waits on a model.
//!
//! While nothing is due the thread sleeps in `recv`; no timer runs while the
//! user is not typing.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};
use oft_engine::complete::{CancelToken, CompleteError, Completer, CompletionRequest, GenerateRequest, Generator};
use oft_engine::controller::{Controller, Feature, GenerateUpdate, Input, Output, Status, TimerId};
use oft_engine::io::{AppInfo, Backend, Event, FieldId, FieldKind, FieldSnapshot};
use oft_engine::settings::Settings;
use oft_engine::stop::Stopper;
use oft_store::stats::Stats;
use oft_store::style::StyleMemory;

use super::bridge::{Bridge, StripStatus};
use super::models::{Loaded, Models, Served};
use super::settings_file;

/// Messages to the runtime thread. Kotlin's calls arrive in the order the UI
/// thread made them; worker results interleave between them.
pub(crate) enum Msg {
    Focus(FieldSnapshot),
    Changed(Change),
    Event(Event),
    /// A result from a model worker.
    Input(Input),
    /// A prefill finished; it produces no input.
    Prefilled(u64),
    Loaded(Loaded),
    ReloadSettings,
    MemoryPressure,
    Stop,
}

/// A field change. The runtime completes it into a [`FieldSnapshot`] with the
/// app and field kind from the focus, which Kotlin sends only once.
pub(crate) struct Change {
    pub field: FieldId,
    pub revision: u64,
    pub before: String,
    pub before_truncated: bool,
    pub selection: String,
    pub after: String,
}

pub(crate) struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
}

/// Room for bursts of keystrokes and streamed tokens; a full queue means the
/// runtime thread is stuck, and the UI thread drops events rather than wait.
const QUEUE: usize = 1024;

pub(crate) struct Runtime {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Runtime {
    pub(crate) fn start(bridge: Bridge, dirs: Dirs) -> std::io::Result<Runtime> {
        let (tx, rx) = crossbeam_channel::bounded(QUEUE);
        let engine = Engine::new(Arc::new(bridge), dirs, tx.clone())?;
        let thread = thread::Builder::new().name("oft-runtime".into()).spawn(move || engine.run(&rx))?;
        Ok(Runtime { tx, thread: Some(thread) })
    }

    /// Never blocks.
    pub(crate) fn send(&self, msg: Msg) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(msg) {
            log::error!("engine queue full; event dropped");
        }
    }
}

impl Drop for Runtime {
    /// Stops the runtime and its workers. Running requests are cancelled, so
    /// this waits at most for one model step. A model load in progress cannot
    /// be interrupted; the loader thread exits when it finishes.
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Timers the runtime runs: the controller's, and its own idle unload.
#[derive(Default)]
struct Timers {
    pause: Option<Instant>,
    prefill: Option<Instant>,
    draft_offer: Option<Instant>,
    unload: Option<Instant>,
}

impl Timers {
    fn slot(&mut self, id: TimerId) -> &mut Option<Instant> {
        match id {
            TimerId::Pause => &mut self.pause,
            TimerId::Prefill => &mut self.prefill,
            TimerId::DraftOffer => &mut self.draft_offer,
        }
    }

    fn next(&self) -> Option<Instant> {
        [self.pause, self.prefill, self.draft_offer, self.unload].into_iter().flatten().min()
    }

    /// Clears and returns the controller timers that are due at `now`.
    fn take_due(&mut self, now: Instant) -> impl Iterator<Item = TimerId> + use<> {
        let due = |slot: &mut Option<Instant>| slot.take_if(|at| *at <= now).is_some();
        let fired = [due(&mut self.pause), due(&mut self.prefill), due(&mut self.draft_offer)];
        [TimerId::Pause, TimerId::Prefill, TimerId::DraftOffer].into_iter().zip(fired).filter_map(|(id, f)| f.then_some(id))
    }
}

struct CompleteJob {
    request: CompletionRequest,
    model: Arc<dyn Completer>,
    cancel: CancelToken,
    prefill: bool,
}

struct GenerateJob {
    request: GenerateRequest,
    model: Arc<dyn Generator>,
    cancel: CancelToken,
}

struct Focused {
    field: FieldId,
    app: AppInfo,
    kind: FieldKind,
    multiline: bool,
}

struct Engine {
    controller: Controller,
    bridge: Arc<Bridge>,
    dirs: Dirs,
    settings: Settings,
    models: Models,
    timers: Timers,
    complete: Sender<CompleteJob>,
    generate: Sender<GenerateJob>,
    workers: [JoinHandle<()>; 2],
    /// Cancellation for every request a worker holds.
    running: Vec<(u64, CancelToken)>,
    focus: Option<Focused>,
    /// When the last keystroke reached the engine, for the latency trace.
    typed_at: Option<Instant>,
    caps: [Option<oft_engine::complete::Caps>; 2],
    controller_status: Status,
    settings_error: Option<String>,
    shown_status: Option<StripStatus>,
    pending: VecDeque<Input>,
    /// Usage counters and style memory, shared with the app's databases.
    stats: Option<Stats>,
    style: Option<StyleMemory>,
    /// The last app reported to the stats as seen.
    seen_app: Option<AppInfo>,
    tx: Sender<Msg>,
}

impl Engine {
    fn new(bridge: Arc<Bridge>, dirs: Dirs, tx: Sender<Msg>) -> std::io::Result<Engine> {
        let (settings, settings_error) = match settings_file::load(&dirs.config) {
            Ok(settings) => (settings, None),
            Err(why) => (Settings::default(), Some(why)),
        };
        let mut models = Models::start(dirs.data.join("models"), bridge.clone(), tx.clone())?;
        models.configure(&settings);
        let (complete, complete_rx) = crossbeam_channel::bounded(4);
        let (generate, generate_rx) = crossbeam_channel::bounded(4);
        let (complete_tx, generate_tx) = (tx.clone(), tx.clone());
        let workers = [
            thread::Builder::new().name("oft-complete".into()).spawn(move || complete_worker(&complete_rx, &complete_tx))?,
            thread::Builder::new().name("oft-generate".into()).spawn(move || generate_worker(&generate_rx, &generate_tx))?,
        ];
        let stats = opened("usage stats", Stats::open(&dirs.data));
        let style = opened("style memory", StyleMemory::open(&dirs.data));
        Ok(Engine {
            controller: Controller::new(settings.clone()),
            bridge,
            dirs,
            settings,
            models,
            timers: Timers::default(),
            complete,
            generate,
            workers,
            running: Vec::new(),
            focus: None,
            typed_at: None,
            caps: [None, None],
            controller_status: Status::Active,
            settings_error,
            shown_status: None,
            pending: VecDeque::new(),
            stats,
            style,
            seen_app: None,
            tx,
        })
    }

    fn run(mut self, rx: &Receiver<Msg>) {
        // The controller emits bindings, appearance and activity on settings.
        let settings = self.settings.clone();
        self.feed(Input::Settings(Box::new(settings)));
        self.show_status();
        loop {
            let msg = match self.timers.next() {
                Some(at) => match rx.recv_deadline(at) {
                    Ok(msg) => msg,
                    Err(RecvTimeoutError::Timeout) => {
                        self.fire_timers();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match rx.recv() {
                    Ok(msg) => msg,
                    Err(_) => break,
                },
            };
            match msg {
                Msg::Stop => break,
                Msg::Focus(snapshot) => self.focus(snapshot),
                Msg::Changed(change) => self.changed(change),
                Msg::Event(event) => {
                    if event == Event::Blur {
                        self.focus = None;
                    }
                    self.feed(Input::Event(event));
                }
                Msg::Input(input) => self.result(input),
                Msg::Prefilled(id) => self.finished(id),
                Msg::Loaded(loaded) => {
                    self.models.loaded(loaded);
                    self.sync_models();
                }
                Msg::ReloadSettings => self.reload_settings(),
                Msg::MemoryPressure => {
                    self.models.unload_local();
                    self.timers.unload = None;
                    self.sync_models();
                }
            }
        }
        self.shutdown();
    }

    fn focus(&mut self, snapshot: FieldSnapshot) {
        if self.seen_app.as_ref() != Some(&snapshot.app) {
            if let Some(stats) = &self.stats {
                stats.seen(snapshot.app.clone());
            }
            self.seen_app = Some(snapshot.app.clone());
        }
        self.focus = Some(Focused {
            field: snapshot.field,
            app: snapshot.app.clone(),
            kind: snapshot.kind,
            multiline: snapshot.multiline,
        });
        if !snapshot.kind.is_secure_or_structured() {
            self.models.prepare();
            self.sync_models();
        }
        self.load_style(snapshot.app.id.clone());
        self.feed(Input::Event(Event::Focus(snapshot)));
    }

    /// Sends the app's style samples to the controller once the style thread
    /// has read them, while style memory is on.
    fn load_style(&self, app: String) {
        if !self.settings.context.style_memory {
            return;
        }
        if let Some(style) = &self.style {
            let tx = self.tx.clone();
            style.load(app, move |samples| {
                let _ = tx.send(Msg::Input(Input::Style(samples)));
            });
        }
    }

    fn changed(&mut self, change: Change) {
        let Some(focus) = self.focus.as_ref().filter(|f| f.field == change.field) else {
            return;
        };
        let snapshot = FieldSnapshot {
            field: change.field,
            revision: change.revision,
            app: focus.app.clone(),
            kind: focus.kind,
            multiline: focus.multiline,
            before: change.before,
            before_truncated: change.before_truncated,
            selection: change.selection,
            after: change.after,
            caret: None,
        };
        self.typed_at = Some(Instant::now());
        self.feed(Input::Event(Event::Changed(snapshot)));
    }

    fn result(&mut self, input: Input) {
        match &input {
            Input::Completed { id, .. } | Input::Generated { id, update: GenerateUpdate::Done(_) } => {
                self.finished(*id);
            }
            _ => {}
        }
        self.feed(input);
    }

    fn finished(&mut self, id: u64) {
        self.running.retain(|(running, _)| *running != id);
        if self.running.is_empty() && self.models.has_local() && self.settings.unload_after_minutes > 0 {
            let idle = Duration::from_secs(u64::from(self.settings.unload_after_minutes) * 60);
            self.timers.unload = Some(Instant::now() + idle);
        }
    }

    fn fire_timers(&mut self) {
        let now = Instant::now();
        if self.timers.unload.is_some_and(|at| at <= now) {
            self.timers.unload = None;
            if self.running.is_empty() {
                self.models.unload_local();
                self.sync_models();
            }
        }
        let due: Vec<TimerId> = self.timers.take_due(now).collect();
        for id in due {
            self.feed(Input::Timer(id));
        }
    }

    fn reload_settings(&mut self) {
        let settings = match settings_file::load(&self.dirs.config) {
            Ok(settings) => {
                self.settings_error = None;
                settings
            }
            Err(why) => {
                self.settings_error = Some(why);
                Settings::default()
            }
        };
        self.models.configure(&settings);
        if self.focus.as_ref().is_some_and(|f| !f.kind.is_secure_or_structured()) {
            self.models.prepare();
        }
        self.settings = settings.clone();
        self.feed(Input::Settings(Box::new(settings)));
        self.sync_models();
    }

    /// Tells the controller when a feature's model became ready or went away.
    fn sync_models(&mut self) {
        for (i, feature) in [Feature::Autocomplete, Feature::FlowDraft].into_iter().enumerate() {
            let caps = self.models.caps(feature);
            if caps != self.caps[i] {
                self.caps[i] = caps;
                self.feed(Input::Caps { feature, caps });
            }
        }
        self.show_status();
    }

    /// Feeds one input and everything the outputs lead to, in order.
    fn feed(&mut self, input: Input) {
        let mut pending = std::mem::take(&mut self.pending);
        pending.push_back(input);
        while let Some(input) = pending.pop_front() {
            for output in self.controller.handle(input) {
                self.execute(output, &mut pending);
            }
        }
        self.pending = pending;
    }

    fn execute(&mut self, output: Output, pending: &mut VecDeque<Input>) {
        match output {
            Output::Overlay(overlay) => {
                if let (oft_engine::io::Overlay::Suggestion(_), Some(at)) = (&overlay, self.typed_at.take()) {
                    log::debug!("suggestion {:?} after the last change", at.elapsed());
                }
                self.bridge.set_overlay(overlay);
            }
            Output::Insert(request) => self.bridge.insert(request),
            Output::Bindings(bindings) => self.bridge.set_bindings(bindings),
            Output::Appearance(appearance) => self.bridge.set_appearance(appearance),
            Output::Active(active) => self.bridge.set_active(active),
            Output::Complete(request) => self.complete(request, false, pending),
            Output::Prefill(request) => self.complete(request, true, pending),
            Output::Cancel(id) | Output::CancelGenerate(id) => {
                if let Some((_, cancel)) = self.running.iter().find(|(r, _)| *r == id) {
                    cancel.cancel();
                }
            }
            Output::Generate(request) => self.generate(request, pending),
            Output::StartTimer { id, after } => *self.timers.slot(id) = Some(Instant::now() + after),
            Output::StopTimer(id) => *self.timers.slot(id) = None,
            Output::ReadConversation(field) => {
                pending.push_back(Input::Conversation { field, conversation: self.bridge.conversation(field) });
            }
            Output::Record(stat) => {
                if let Some(stats) = &self.stats {
                    stats.record(stat);
                }
            }
            Output::Remember { app, text } => {
                if !self.settings.context.style_memory {
                    return;
                }
                if let Some(style) = &self.style {
                    style.remember(app.clone(), text, now_ms());
                }
                if self.focus.as_ref().is_some_and(|f| f.app.id == app) {
                    self.load_style(app);
                }
            }
            Output::Status(status) => {
                self.controller_status = status;
                self.show_status();
            }
        }
    }

    fn complete(&mut self, request: CompletionRequest, prefill: bool, pending: &mut VecDeque<Input>) {
        let id = request.id;
        let served = self.models.served(Feature::Autocomplete);
        let model = match served {
            Some(served) if prefill || self.cloud_allowed(&served) => served.completer(),
            served => {
                if !prefill {
                    let why = if served.is_some() { daily_cap() } else { unavailable(&self.models) };
                    pending.push_back(Input::Completed { id, result: Err(why) });
                }
                return;
            }
        };
        self.timers.unload = None;
        let cancel = CancelToken::new();
        match self.complete.try_send(CompleteJob { request, model, cancel: cancel.clone(), prefill }) {
            Ok(()) => self.running.push((id, cancel)),
            Err(_) if prefill => {}
            Err(_) => pending.push_back(Input::Completed { id, result: Err(CompleteError::Unavailable("busy".into())) }),
        }
    }

    fn generate(&mut self, request: GenerateRequest, pending: &mut VecDeque<Input>) {
        let id = request.id;
        let model = match self.models.served(Feature::FlowDraft) {
            Some(served) if self.cloud_allowed(&served) => served.generator(),
            served => {
                let why = if served.is_some() { daily_cap() } else { unavailable(&self.models) };
                pending.push_back(Input::Generated { id, update: GenerateUpdate::Done(Err(why)) });
                return;
            }
        };
        self.timers.unload = None;
        let cancel = CancelToken::new();
        match self.generate.try_send(GenerateJob { request, model, cancel: cancel.clone() }) {
            Ok(()) => self.running.push((id, cancel)),
            Err(_) => {
                let update = GenerateUpdate::Done(Err(CompleteError::Unavailable("busy".into())));
                pending.push_back(Input::Generated { id, update });
            }
        }
    }

    /// Lets a cloud request through the daily cap and counts it. Requests
    /// run one at a time and take far longer than the stats write, so the
    /// stored count is current.
    fn cloud_allowed(&self, served: &Served) -> bool {
        if !matches!(served, Served::Cloud(_)) {
            return true;
        }
        let Some(stats) = &self.stats else { return true };
        if let Some(cap) = self.settings.daily_request_cap {
            match stats.cloud_requests_today() {
                Ok(used) if used >= cap => return false,
                Ok(_) => {}
                Err(e) => log::error!("cannot read today's cloud request count: {e}"),
            }
        }
        stats.cloud_request();
        true
    }

    /// The strip line: what blocks suggestions, or nothing.
    fn show_status(&mut self) {
        let status = match &self.controller_status {
            Status::Paused => StripStatus::Paused,
            Status::OffInApp(_) => StripStatus::OffInApp,
            other => {
                if let Some(why) = &self.settings_error {
                    StripStatus::SettingsInvalid(why.clone())
                } else if self.models.is_loading() {
                    StripStatus::Loading
                } else if let Some(why) = self.models.failure() {
                    StripStatus::LoadFailed(why)
                } else {
                    match other {
                        Status::NoModel => StripStatus::NoModel,
                        Status::CloudUnavailable(why) => StripStatus::CloudUnavailable(why.clone()),
                        _ => StripStatus::Clear,
                    }
                }
            }
        };
        if self.shown_status.as_ref() != Some(&status) {
            self.bridge.status(&status);
            self.shown_status = Some(status);
        }
    }

    fn shutdown(self) {
        for (_, cancel) in &self.running {
            cancel.cancel();
        }
        let Engine { complete, generate, workers, .. } = self;
        drop((complete, generate));
        for worker in workers {
            let _ = worker.join();
        }
    }
}

fn unavailable(models: &Models) -> CompleteError {
    CompleteError::Unavailable(if models.is_loading() { "loading".into() } else { "no model".into() })
}

fn daily_cap() -> CompleteError {
    CompleteError::Unavailable("daily request cap reached".into())
}

fn now_ms() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

fn opened<T, E: std::fmt::Display>(what: &str, result: Result<T, E>) -> Option<T> {
    result.map_err(|e| log::error!("cannot open {what}: {e}")).ok()
}

fn complete_worker(jobs: &Receiver<CompleteJob>, out: &Sender<Msg>) {
    for job in jobs {
        let id = job.request.id;
        // A prefill with newer work queued behind it would only delay it.
        if job.prefill {
            if !job.cancel.is_cancelled() && jobs.is_empty() {
                job.model.prefill(&job.request, &job.cancel);
            }
            if out.send(Msg::Prefilled(id)).is_err() {
                return;
            }
            continue;
        }
        let result = if job.cancel.is_cancelled() {
            Err(CompleteError::Cancelled)
        } else {
            let mut stop = Stopper::new(&job.request.stop);
            let started = Instant::now();
            let result = job.model.complete(&job.request, &mut stop, &job.cancel);
            log::debug!("completion {id} took {:?}", started.elapsed());
            result
        };
        if out.send(Msg::Input(Input::Completed { id, result })).is_err() {
            return;
        }
    }
}

fn generate_worker(jobs: &Receiver<GenerateJob>, out: &Sender<Msg>) {
    for job in jobs {
        let id = job.request.id;
        let result = if job.cancel.is_cancelled() {
            Err(CompleteError::Cancelled)
        } else {
            job.model.generate(&job.request, &job.cancel, &mut |delta| {
                let update = GenerateUpdate::Delta(delta.to_owned());
                let _ = out.send(Msg::Input(Input::Generated { id, update }));
            })
        };
        let update = GenerateUpdate::Done(result);
        if out.send(Msg::Input(Input::Generated { id, update })).is_err() {
            return;
        }
    }
}
