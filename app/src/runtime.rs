//! The desktop runtime: one controller thread that owns the engine's state
//! machine, plus workers for completions, reply & rewrite generation, conversation
//! reads and model loading.
//!
//! The controller thread sleeps in `Select::ready_deadline` until a backend
//! event, a worker result, an app command or the earliest timer deadline
//! arrives. Nothing polls; with no timers armed it blocks indefinitely.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Select, Sender, unbounded};
use oft_cloud::SentLog;
use oft_engine::complete::{CancelToken, CompleteError, Completer, Completion, CompletionRequest, GenerateRequest, Generator};
use oft_engine::controller::{Controller, Feature, GenerateUpdate, Input, Output, Status, TimerId};
use oft_engine::io::{AppInfo, Backend, BackendStatus, Event, FieldId, Overlay};
use oft_engine::settings::Settings;
use oft_engine::stop::Stopper;
use serde::Serialize;

use crate::models::{Hub, HubEvent, LocalState};
use crate::pause::{PauseFor, PauseInfo, Pauses, Scope, local_clock, local_day, now_ms};
use crate::stats::Stats;
use crate::style::StyleMemory;

/// Commands from the app (tray, settings UI) to the controller thread.
#[derive(Debug)]
pub enum Command {
    /// Saved settings changed.
    Settings(Box<Settings>),
    /// A key was saved or removed, or models were downloaded or deleted:
    /// rebuild the model slots.
    ModelsChanged,
    Pause(Scope, PauseFor),
    /// Resume one app, or everything with `None`.
    Resume(Option<String>),
    /// Download progress from the model manager, 0–1; `None` when done.
    #[cfg_attr(not(feature = "local"), allow(dead_code))]
    Download(Option<f32>),
    MemoryPressure,
    Quit,
}

/// Timings of one autocomplete request, in microseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Timing {
    pub id: u64,
    /// Unix milliseconds when the result arrived.
    pub at_ms: u64,
    /// Last keystroke to the request being issued (mostly the pause).
    pub wait_us: u64,
    /// The controller step that issued it: context, secret filter, prompt.
    pub build_us: u64,
    /// Waiting for the completion worker.
    pub queue_us: u64,
    /// Model time.
    pub decode_us: u64,
    /// Handing the suggestion to the backend, which renders it.
    pub render_us: u64,
    /// Last keystroke to the suggestion being handed over.
    pub total_us: u64,
    pub shown: bool,
    pub cancelled: bool,
}

const TIMINGS_KEPT: usize = 64;

/// What the tray and the settings UI show.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StatusView {
    pub kind: StatusKind,
    /// One line for the tray.
    pub text: String,
    pub pauses: Vec<PauseInfo>,
    /// The app that last had a text field focused.
    pub app: Option<AppRef>,
    pub local_model_loaded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Starting,
    Active,
    Paused,
    OffInApp,
    NoModel,
    CloudUnavailable,
    PermissionNeeded,
    SecureInput,
    Unavailable,
    Loading,
    Downloading,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppRef {
    pub id: String,
    pub name: String,
}

/// State the app reads without going through the controller thread.
#[derive(Debug)]
pub struct Shared {
    pub status: Mutex<StatusView>,
    pub timings: Mutex<VecDeque<Timing>>,
}

/// Handle to the running runtime.
#[derive(Clone, Debug)]
pub struct Runtime {
    tx: Sender<Command>,
    pub shared: Arc<Shared>,
}

impl Runtime {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}

pub type Notify = Box<dyn Fn(&StatusView) + Send>;

/// Starts the OS backend and the controller thread.
pub fn start(settings: Settings, stores: Stores, notify: Notify) -> Runtime {
    let (tx, rx) = unbounded();
    let shared = Arc::new(Shared {
        status: Mutex::new(StatusView {
            kind: StatusKind::Starting,
            text: "Starting".into(),
            pauses: Vec::new(),
            app: None,
            local_model_loaded: false,
        }),
        timings: Mutex::new(VecDeque::with_capacity(TIMINGS_KEPT)),
    });
    let runtime = Runtime { tx: tx.clone(), shared: shared.clone() };
    {
        let tx = tx.clone();
        crate::memory::watch_pressure(move || {
            let _ = tx.send(Command::MemoryPressure);
        });
    }
    thread::Builder::new()
        .name("oft-controller".into())
        .spawn(move || {
            let (events_tx, events_rx) = unbounded();
            let (backend, backend_error) = match oft_platform::start(events_tx.clone()) {
                Ok(b) => (Some(b), None),
                Err(e) => {
                    log::warn!("platform backend unavailable: {e}");
                    (None, Some(e.to_string()))
                }
            };
            let mut core = Core::new(settings, backend, stores, shared, notify);
            if let Some(e) = backend_error {
                core.backend_status = Some(BackendStatus::Unavailable(e));
            }
            // Kept alive so the event channel never reports disconnection.
            let _events_tx = events_tx;
            core.run(&events_rx, &rx);
        })
        .expect("controller thread starts");
    runtime
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Timer {
    Controller(TimerId),
    Unload,
    PauseEnd,
    CloudRetry,
}

/// How long autocomplete stays on the local model after the cloud failed
/// before the provider is tried again.
const CLOUD_RETRY: Duration = Duration::from_secs(5 * 60);

const DAILY_CAP: &str = "daily request cap reached";

/// Results from the worker threads.
enum WorkerMsg {
    Completed { id: u64, result: Result<Completion, CompleteError>, queue: Duration, decode: Duration },
    Input(Input),
}

enum Job {
    Complete { request: CompletionRequest, model: Arc<dyn Completer>, cancel: CancelToken, queued: Instant },
    Prefill { request: CompletionRequest, model: Arc<dyn Completer>, cancel: CancelToken },
}

struct GenJob {
    request: GenerateRequest,
    model: Arc<dyn Generator>,
    cancel: CancelToken,
}

#[derive(Clone, Copy)]
struct Pending {
    key_at: Instant,
    issued: Instant,
    build: Duration,
    queue: Duration,
    decode: Duration,
}

/// Cloud requests sent today, for `Settings::daily_request_cap`.
#[derive(Clone, Copy, Debug)]
struct CloudCount {
    day: i64,
    count: u32,
}

struct Core {
    controller: Controller,
    backend: Option<Arc<dyn Backend>>,
    stats: Option<Stats>,
    style: Option<StyleMemory>,
    hub: Hub,
    hub_rx: Receiver<HubEvent>,
    saved: Settings,
    pauses: Pauses,
    timers: Vec<(Timer, Instant)>,
    jobs: Sender<Job>,
    gens: Sender<GenJob>,
    conversations: Sender<FieldId>,
    worker_tx: Sender<WorkerMsg>,
    worker_rx: Receiver<WorkerMsg>,
    completions: HashMap<u64, CancelToken>,
    prefill: Option<CancelToken>,
    generations: HashMap<u64, CancelToken>,
    /// Inputs produced while executing outputs, fed after the current batch.
    queued: VecDeque<Input>,
    feeding: bool,
    /// Something the status line depends on changed since the last publish.
    dirty: bool,
    last_key: Instant,
    pending: HashMap<u64, Pending>,
    /// The completion whose result is being handled, to time its overlay.
    finishing: Option<(u64, Pending)>,
    caps_sent: [Option<oft_engine::complete::Caps>; 2],
    app: Option<AppInfo>,
    controller_status: Status,
    backend_status: Option<BackendStatus>,
    download: Option<f32>,
    cloud: CloudCount,
    shared: Arc<Shared>,
    notify: Notify,
}

/// The stores the runtime writes to.
#[derive(Debug)]
pub struct Stores {
    pub stats: Option<Stats>,
    pub style: Option<StyleMemory>,
    pub log: SentLog,
}

impl Core {
    fn new(settings: Settings, backend: Option<Arc<dyn Backend>>, stores: Stores, shared: Arc<Shared>, notify: Notify) -> Core {
        let Stores { stats, style, log } = stores;
        let (hub, hub_rx) = Hub::new(log);
        let (worker_tx, worker_rx) = unbounded();
        let jobs = spawn_completion_worker(worker_tx.clone());
        let gens = spawn_generation_worker(worker_tx.clone());
        let conversations = spawn_conversation_reader(backend.clone(), worker_tx.clone());
        let today = stats.as_ref().and_then(|s| s.cloud_requests_today().ok()).unwrap_or(0);
        let mut core = Core {
            controller: Controller::new(settings.clone()),
            backend,
            stats,
            style,
            hub,
            hub_rx,
            saved: settings,
            pauses: Pauses::default(),
            timers: Vec::new(),
            jobs,
            gens,
            conversations,
            worker_tx,
            worker_rx,
            completions: HashMap::new(),
            prefill: None,
            generations: HashMap::new(),
            queued: VecDeque::new(),
            feeding: false,
            dirty: true,
            last_key: Instant::now(),
            pending: HashMap::new(),
            finishing: None,
            caps_sent: [None, None],
            app: None,
            controller_status: Status::Active,
            backend_status: None,
            download: None,
            cloud: CloudCount { day: local_day(now_ms()), count: today },
            shared,
            notify,
        };
        // The first Settings input makes the controller send bindings, the
        // active state and its status.
        core.feed(Input::Settings(Box::new(core.saved.clone())));
        core.hub.apply(&core.saved);
        core.send_caps();
        core
    }

    fn run(&mut self, events: &Receiver<Event>, commands: &Receiver<Command>) {
        let worker_rx = self.worker_rx.clone();
        let hub_rx = self.hub_rx.clone();
        let mut select = Select::new();
        let i_events = select.recv(events);
        let i_commands = select.recv(commands);
        let i_workers = select.recv(&worker_rx);
        let i_hub = select.recv(&hub_rx);
        self.publish();
        loop {
            let ready = match self.next_deadline() {
                Some(at) => select.ready_deadline(at).ok(),
                None => Some(select.ready()),
            };
            match ready {
                None => {}
                Some(i) if i == i_events => {
                    if let Ok(event) = events.try_recv() {
                        self.on_event(event);
                    }
                }
                Some(i) if i == i_commands => match commands.try_recv() {
                    Ok(Command::Quit) | Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                    Ok(command) => {
                        self.on_command(command);
                        self.dirty = true;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {}
                },
                Some(i) if i == i_workers => {
                    if let Ok(msg) = worker_rx.try_recv() {
                        self.on_worker(msg);
                    }
                }
                Some(i) if i == i_hub => {
                    if let Ok(event) = hub_rx.try_recv()
                        && self.hub.on_event(event)
                    {
                        self.send_caps();
                        self.dirty = true;
                    }
                }
                Some(_) => unreachable!("only four operations are registered"),
            }
            // Busy channels must not starve timers.
            if self.next_deadline().is_some_and(|at| at <= Instant::now()) {
                self.fire_timers();
                self.dirty = true;
            }
            if std::mem::take(&mut self.dirty) {
                self.publish();
            }
        }
        if let Some(b) = &self.backend {
            b.set_active(false);
        }
        self.hub.unload();
    }

    fn on_event(&mut self, event: Event) {
        match &event {
            Event::Focus(snapshot) => {
                self.last_key = Instant::now();
                if self.app.as_ref().is_none_or(|a| a.id != snapshot.app.id || a.name != snapshot.app.name) {
                    if let Some(stats) = &self.stats {
                        stats.seen(snapshot.app.clone());
                    }
                    self.app = Some(snapshot.app.clone());
                }
                self.hub.focus();
                self.dirty = true;
                self.load_style(snapshot.app.id.clone());
            }
            Event::Changed(_) => self.last_key = Instant::now(),
            Event::Status(s) => {
                self.backend_status = Some(s.clone());
                self.dirty = true;
            }
            _ => {}
        }
        self.feed(Input::Event(event));
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Settings(settings) => {
                let models_changed = settings.autocomplete_model != self.saved.autocomplete_model
                    || settings.draft_model != self.saved.draft_model
                    || settings.providers != self.saved.providers;
                self.saved = *settings;
                if models_changed {
                    self.rebuild_models();
                }
                self.feed(Input::Settings(Box::new(self.saved.clone())));
            }
            Command::ModelsChanged => self.rebuild_models(),
            Command::Pause(scope, length) => {
                self.pauses.set(scope, length, Instant::now());
                self.apply_pauses();
            }
            Command::Resume(app) => {
                self.pauses.resume(app.as_deref());
                self.apply_pauses();
            }
            Command::Download(progress) => {
                let finished = self.download.is_some() && progress.is_none();
                self.download = progress;
                if finished {
                    self.rebuild_models();
                }
            }
            Command::MemoryPressure => {
                if self.hub.has_local_loaded() {
                    log::info!("memory pressure: unloading the local model");
                    self.hub.unload();
                }
            }
            Command::Quit => unreachable!("handled by the loop"),
        }
    }

    fn rebuild_models(&mut self) {
        self.stop_timer(Timer::CloudRetry);
        self.hub.apply(&self.saved);
        self.send_caps();
    }

    /// Sends the current pauses and re-arms the timer for the next to end.
    fn apply_pauses(&mut self) {
        self.stop_timer(Timer::PauseEnd);
        if let Some(at) = self.pauses.next_deadline() {
            self.start_timer(Timer::PauseEnd, at);
        }
        self.feed(Input::Pause(self.pauses.state()));
    }

    /// Whether the daily cloud cap allows one more request now. Counts it
    /// when it does.
    fn cloud_allowed(&mut self) -> bool {
        let today = local_day(now_ms());
        if self.cloud.day != today {
            self.cloud = CloudCount { day: today, count: 0 };
        }
        if self.saved.daily_request_cap.is_some_and(|cap| self.cloud.count >= cap) {
            return false;
        }
        self.cloud.count += 1;
        if let Some(stats) = &self.stats {
            stats.cloud_request();
        }
        true
    }

    fn cap_reached(&self) -> bool {
        self.cloud.day == local_day(now_ms()) && self.saved.daily_request_cap.is_some_and(|cap| self.cloud.count >= cap)
    }

    fn on_worker(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Completed { id, result, queue, decode } => {
                self.completions.remove(&id);
                let pending = self.pending.remove(&id).map(|p| Pending { queue, decode, ..p });
                let cancelled = matches!(result, Err(CompleteError::Cancelled));
                self.finishing = pending.map(|p| (id, p));
                self.feed(Input::Completed { id, result });
                if let Some((id, p)) = self.finishing.take() {
                    self.record_timing(id, p, None, cancelled);
                }
            }
            WorkerMsg::Input(input) => {
                if let Input::Generated { id, update: GenerateUpdate::Done(_) } = &input {
                    self.generations.remove(id);
                }
                self.feed(input);
            }
        }
    }

    /// Feeds `input` and executes the outputs. Inputs that outputs produce
    /// (and calls made while executing) are queued and handled in order after
    /// the current batch, never nested inside it.
    fn feed(&mut self, input: Input) {
        self.queued.push_back(input);
        if self.feeding {
            return;
        }
        self.feeding = true;
        while let Some(input) = self.queued.pop_front() {
            let started = Instant::now();
            let outputs = self.controller.handle(input);
            let build = started.elapsed();
            for output in outputs {
                self.execute(output, build);
            }
        }
        self.feeding = false;
    }

    fn execute(&mut self, output: Output, build: Duration) {
        let backend = self.backend.clone();
        let backend = backend.as_deref();
        match output {
            Output::Overlay(overlay) => {
                let shown = matches!(overlay, Overlay::Suggestion(_));
                let started = Instant::now();
                if let Some(b) = backend {
                    b.set_overlay(overlay);
                }
                if shown && let Some((id, p)) = self.finishing.take() {
                    self.record_timing(id, p, Some(started.elapsed()), false);
                }
            }
            Output::Insert(request) => {
                if let Some(b) = backend {
                    b.insert(request);
                }
            }
            Output::Bindings(bindings) => {
                if let Some(b) = backend {
                    b.set_bindings(bindings);
                }
            }
            Output::Active(active) => {
                if let Some(b) = backend {
                    b.set_active(active);
                }
            }
            Output::Complete(request) => {
                if let Some(p) = self.prefill.take() {
                    p.cancel();
                }
                let id = request.id;
                match self.cloud_gate(Feature::Autocomplete).and_then(|()| self.hub.completer(Feature::Autocomplete)) {
                    Ok(model) => {
                        let cancel = CancelToken::new();
                        self.completions.insert(id, cancel.clone());
                        let now = Instant::now();
                        self.pending.insert(
                            id,
                            Pending { key_at: self.last_key, issued: now, build, queue: Duration::ZERO, decode: Duration::ZERO },
                        );
                        let _ = self.jobs.send(Job::Complete { request, model, cancel, queued: now });
                        self.touch_local(Feature::Autocomplete);
                    }
                    Err(e) => self.queued.push_back(Input::Completed { id, result: Err(e) }),
                }
            }
            Output::Prefill(request) => {
                if let Ok(model) = self.hub.completer(Feature::Autocomplete) {
                    let cancel = CancelToken::new();
                    if let Some(old) = self.prefill.replace(cancel.clone()) {
                        old.cancel();
                    }
                    let _ = self.jobs.send(Job::Prefill { request, model, cancel });
                    self.touch_local(Feature::Autocomplete);
                }
            }
            Output::Cancel(id) => {
                if let Some(c) = self.completions.remove(&id) {
                    c.cancel();
                }
            }
            Output::Generate(request) => {
                let id = request.id;
                match self.cloud_gate(Feature::FlowDraft).and_then(|()| self.hub.generator(Feature::FlowDraft)) {
                    Ok(model) => {
                        let cancel = CancelToken::new();
                        self.generations.insert(id, cancel.clone());
                        let _ = self.gens.send(GenJob { request, model, cancel });
                        self.touch_local(Feature::FlowDraft);
                    }
                    Err(e) => self.queued.push_back(Input::Generated { id, update: GenerateUpdate::Done(Err(e)) }),
                }
            }
            Output::CancelGenerate(id) => {
                if let Some(c) = self.generations.remove(&id) {
                    c.cancel();
                }
            }
            Output::StartTimer { id, after } => self.start_timer(Timer::Controller(id), Instant::now() + after),
            Output::StopTimer(id) => self.stop_timer(Timer::Controller(id)),
            Output::ReadConversation(field) => {
                if backend.is_some() {
                    let _ = self.conversations.send(field);
                } else {
                    self.queued.push_back(Input::Conversation { field, conversation: None });
                }
            }
            Output::Record(stat) => {
                if let Some(stats) = &self.stats {
                    stats.record(stat);
                }
            }
            Output::Status(status) => {
                if matches!(status, Status::CloudUnavailable(_)) {
                    self.take_over_from_cloud();
                }
                self.controller_status = status;
                self.dirty = true;
            }
            Output::Appearance(appearance) => {
                if let Some(b) = backend {
                    b.set_appearance(appearance);
                }
            }
            Output::Remember { app, text } => {
                if !self.saved.context.style_memory {
                    return;
                }
                let focused = self.app.as_ref().is_some_and(|a| a.id == app);
                if let Some(style) = &self.style {
                    style.remember(app.clone(), text, now_ms());
                }
                if focused {
                    self.load_style(app);
                }
            }
        }
    }

    /// Sends the focused app's style samples to the controller once the
    /// style thread has read them.
    fn load_style(&self, app: String) {
        if !self.saved.context.style_memory {
            return;
        }
        if let Some(style) = &self.style {
            let tx = self.worker_tx.clone();
            style.load(app, move |samples| {
                let _ = tx.send(WorkerMsg::Input(Input::Style(samples)));
            });
        }
    }

    /// Lets a cloud request through the daily cap. When the cap is reached,
    /// autocomplete moves to an installed local model if there is one.
    fn cloud_gate(&mut self, feature: Feature) -> Result<(), CompleteError> {
        if !self.hub.is_cloud(feature) || self.cloud_allowed() {
            return Ok(());
        }
        if feature == Feature::Autocomplete && self.take_over_from_cloud() {
            return Ok(());
        }
        self.dirty = true;
        Err(CompleteError::Unavailable(DAILY_CAP.into()))
    }

    fn take_over_from_cloud(&mut self) -> bool {
        if !self.hub.take_over_from_cloud() {
            return false;
        }
        self.start_timer(Timer::CloudRetry, Instant::now() + CLOUD_RETRY);
        self.send_caps();
        true
    }

    /// Restarts the idle-unload countdown when the local model serves a request.
    fn touch_local(&mut self, feature: Feature) {
        if !self.hub.is_local(feature) {
            return;
        }
        self.stop_timer(Timer::Unload);
        let minutes = self.saved.unload_after_minutes;
        if minutes > 0 {
            self.start_timer(Timer::Unload, Instant::now() + Duration::from_secs(u64::from(minutes) * 60));
        }
    }

    fn send_caps(&mut self) {
        for (i, feature) in [Feature::Autocomplete, Feature::FlowDraft].into_iter().enumerate() {
            let caps = self.hub.caps(feature);
            if self.caps_sent[i] != caps {
                self.caps_sent[i] = caps;
                self.feed(Input::Caps { feature, caps });
            }
        }
    }

    fn record_timing(&mut self, id: u64, p: Pending, render: Option<Duration>, cancelled: bool) {
        let us = |d: Duration| d.as_micros().min(u128::from(u64::MAX)) as u64;
        let now = Instant::now();
        let timing = Timing {
            id,
            at_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
            wait_us: us(p.issued.saturating_duration_since(p.key_at)),
            build_us: us(p.build),
            queue_us: us(p.queue),
            decode_us: us(p.decode),
            render_us: render.map_or(0, us),
            total_us: us(now.saturating_duration_since(p.key_at)),
            shown: render.is_some(),
            cancelled,
        };
        let mut ring = self.shared.timings.lock().unwrap_or_else(|e| e.into_inner());
        if ring.len() == TIMINGS_KEPT {
            ring.pop_front();
        }
        ring.push_back(timing);
    }

    fn start_timer(&mut self, timer: Timer, at: Instant) {
        match self.timers.iter_mut().find(|(t, _)| *t == timer) {
            Some(entry) => entry.1 = at,
            None => self.timers.push((timer, at)),
        }
    }

    fn stop_timer(&mut self, timer: Timer) {
        self.timers.retain(|(t, _)| *t != timer);
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.timers.iter().map(|(_, at)| *at).min()
    }

    fn fire_timers(&mut self) {
        let now = Instant::now();
        let due: Vec<Timer> = self.timers.iter().filter(|(_, at)| *at <= now).map(|(t, _)| *t).collect();
        self.timers.retain(|(_, at)| *at > now);
        for timer in due {
            match timer {
                Timer::Controller(id) => self.feed(Input::Timer(id)),
                Timer::Unload => self.hub.unload(),
                Timer::PauseEnd => {
                    self.pauses.expire(now);
                    self.apply_pauses();
                }
                Timer::CloudRetry => {
                    if self.hub.restore_cloud() {
                        self.send_caps();
                    }
                }
            }
        }
    }

    /// Recomputes the status line and tells the tray and UI if it changed.
    fn publish(&mut self) {
        let view = self.status_view();
        {
            let mut current = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
            if *current == view {
                return;
            }
            current.clone_from(&view);
        }
        // The lock is released first: the tray reads the status back.
        (self.notify)(&view);
    }

    fn status_view(&self) -> StatusView {
        let app_id = self.app.as_ref().map(|a| a.id.as_str());
        let pauses = self.pauses.list();
        let (kind, text) = self.status_line(app_id, &pauses);
        StatusView {
            kind,
            text,
            pauses,
            app: self.app.as_ref().map(|a| AppRef {
                id: a.id.clone(),
                name: if a.name.is_empty() { a.id.clone() } else { a.name.clone() },
            }),
            local_model_loaded: self.hub.has_local_loaded(),
        }
    }

    fn status_line(&self, app_id: Option<&str>, pauses: &[PauseInfo]) -> (StatusKind, String) {
        let until = |p: &PauseInfo| match p.until_ms {
            Some(ms) => format!(" until {}", local_clock(ms)),
            None => " until resumed".into(),
        };
        if let Some(p) = pauses.iter().find(|p| p.app.is_none()) {
            return (StatusKind::Paused, format!("Paused{}", until(p)));
        }
        if let Some(p) = pauses.iter().find(|p| p.app.as_deref() == app_id && app_id.is_some()) {
            let name = p.name.as_deref().unwrap_or_default();
            return (StatusKind::Paused, format!("Paused in {name}{}", until(p)));
        }
        match &self.backend_status {
            Some(BackendStatus::PermissionMissing(what)) => {
                return (StatusKind::PermissionNeeded, format!("Permission needed: {what}"));
            }
            Some(BackendStatus::SecureInput { app }) => {
                let by = app.as_deref().map(|a| format!(" by {a}")).unwrap_or_default();
                return (StatusKind::SecureInput, format!("Blocked by secure input{by}"));
            }
            Some(BackendStatus::Unavailable(why)) => return (StatusKind::Unavailable, format!("Unavailable: {why}")),
            Some(BackendStatus::Ready) | None => {}
        }
        if let Some(f) = self.download {
            return (StatusKind::Downloading, format!("Model downloading, {:.0}%", f * 100.0));
        }
        match self.hub.local_state() {
            LocalState::Loading(f) => return (StatusKind::Loading, format!("Loading model, {:.0}%", f * 100.0)),
            LocalState::Failed(e) => return (StatusKind::Error, format!("Local model unavailable: {e}")),
            LocalState::Idle | LocalState::Ready => {}
        }
        if let Some(problem) = self.hub.problem(Feature::Autocomplete) {
            return (StatusKind::NoModel, format!("No model: {problem}"));
        }
        let uses_cloud = self.hub.took_over() || self.hub.is_cloud(Feature::Autocomplete) || self.hub.is_cloud(Feature::FlowDraft);
        if uses_cloud && self.cap_reached() {
            let fallback = if self.hub.took_over() { "; the local model took over" } else { "" };
            return (StatusKind::CloudUnavailable, format!("Cloud unavailable: {DAILY_CAP}{fallback}"));
        }
        match &self.controller_status {
            Status::Active => (StatusKind::Active, "Active".into()),
            Status::Paused => (StatusKind::Paused, "Paused".into()),
            Status::OffInApp(app) => {
                let name = if app.name.is_empty() { &app.id } else { &app.name };
                (StatusKind::OffInApp, format!("Off in {name}"))
            }
            Status::NoModel => (StatusKind::NoModel, "No model: choose a local model or add a key".into()),
            Status::CloudUnavailable(why) => {
                let fallback = if self.hub.took_over() { "; the local model took over" } else { "" };
                (StatusKind::CloudUnavailable, format!("Cloud unavailable: {why}{fallback}"))
            }
            Status::Backend(BackendStatus::PermissionMissing(what)) => {
                (StatusKind::PermissionNeeded, format!("Permission needed: {what}"))
            }
            Status::Backend(BackendStatus::SecureInput { app }) => {
                let by = app.as_deref().map(|a| format!(" by {a}")).unwrap_or_default();
                (StatusKind::SecureInput, format!("Blocked by secure input{by}"))
            }
            Status::Backend(BackendStatus::Unavailable(why)) => (StatusKind::Unavailable, format!("Unavailable: {why}")),
            Status::Backend(BackendStatus::Ready) => (StatusKind::Active, "Active".into()),
        }
    }
}

fn spawn_completion_worker(results: Sender<WorkerMsg>) -> Sender<Job> {
    let (tx, rx) = unbounded::<Job>();
    thread::Builder::new()
        .name("oft-complete".into())
        .spawn(move || {
            for job in rx {
                match job {
                    Job::Complete { request, model, cancel, queued } => {
                        let started = Instant::now();
                        let result = if cancel.is_cancelled() {
                            Err(CompleteError::Cancelled)
                        } else {
                            let mut stop = Stopper::new(&request.stop);
                            model.complete(&request, &mut stop, &cancel)
                        };
                        let msg = WorkerMsg::Completed {
                            id: request.id,
                            result,
                            queue: started - queued,
                            decode: started.elapsed(),
                        };
                        if results.send(msg).is_err() {
                            return;
                        }
                    }
                    Job::Prefill { request, model, cancel } => {
                        if !cancel.is_cancelled() {
                            model.prefill(&request, &cancel);
                        }
                    }
                }
            }
        })
        .expect("completion worker starts");
    tx
}

fn spawn_generation_worker(results: Sender<WorkerMsg>) -> Sender<GenJob> {
    let (tx, rx) = unbounded::<GenJob>();
    thread::Builder::new()
        .name("oft-generate".into())
        .spawn(move || {
            for GenJob { request, model, cancel } in rx {
                let id = request.id;
                let result = if cancel.is_cancelled() {
                    Err(CompleteError::Cancelled)
                } else {
                    model.generate(&request, &cancel, &mut |delta| {
                        let update = GenerateUpdate::Delta(delta.to_owned());
                        let _ = results.send(WorkerMsg::Input(Input::Generated { id, update }));
                    })
                };
                let done = WorkerMsg::Input(Input::Generated { id, update: GenerateUpdate::Done(result) });
                if results.send(done).is_err() {
                    return;
                }
            }
        })
        .expect("generation worker starts");
    tx
}

fn spawn_conversation_reader(backend: Option<Arc<dyn Backend>>, results: Sender<WorkerMsg>) -> Sender<FieldId> {
    let (tx, rx) = unbounded::<FieldId>();
    if let Some(backend) = backend {
        thread::Builder::new()
            .name("oft-conversation".into())
            .spawn(move || {
                for field in rx {
                    let conversation = backend.conversation(field);
                    if results.send(WorkerMsg::Input(Input::Conversation { field, conversation })).is_err() {
                        return;
                    }
                }
            })
            .expect("conversation reader starts");
    }
    tx
}
