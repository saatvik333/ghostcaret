//! The runtime thread: owns the controller, executes its outputs against the
//! backend and the models, runs the timers and keeps the status line.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Select, Sender, TryRecvError};
use oft_engine::complete::{CancelToken, Caps, CompleteError};
use oft_engine::controller::{Controller, Feature, GenerateUpdate, Input, Output, Status, TimerId};
use oft_engine::io::{AppInfo, Backend, BackendStatus, Event, FieldId, FieldKind, Overlay};
use oft_engine::settings::Settings;
use oft_store::stats::Stats;
use oft_store::style::StyleMemory;

use crate::models::{Hub, HubEvent, LocalState};
use crate::pause::{PauseInfo, Pauses, local_clock, local_day, now_ms};
use crate::status::{AppRef, StatusKind, StatusView, TIMINGS_KEPT, Timing};
use crate::workers::{GenJob, Job, WorkerMsg, Workers, conversation_reader};
use crate::{Command, Host};

/// State hosts read without going through the runtime thread.
#[derive(Debug)]
pub(crate) struct Shared {
    pub status: Mutex<StatusView>,
    pub timings: Mutex<VecDeque<Timing>>,
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

/// What `Settings::unload_after_minutes` counts; milliseconds in tests, so
/// they can watch an idle unload happen.
const UNLOAD_UNIT: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_secs(60)
};

/// A request a worker holds.
struct Running {
    cancel: CancelToken,
    /// It runs on the local model.
    local: bool,
}

/// Stage times of a completion in flight.
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

/// A status line: its kind, its text and the text's detail.
type Line = (StatusKind, String, Option<String>);

pub(crate) struct Core {
    controller: Controller,
    host: Arc<dyn Host>,
    backend: Option<Arc<dyn Backend>>,
    backend_status: Option<BackendStatus>,
    stats: Option<Stats>,
    style: Option<StyleMemory>,
    hub: Hub,
    settings: Settings,
    /// Why [`Host::settings`] failed; the runtime then runs on defaults.
    settings_error: Option<String>,
    pauses: Pauses,
    timers: Vec<(Timer, Instant)>,
    jobs: Sender<Job>,
    gens: Sender<GenJob>,
    workers: [JoinHandle<()>; 2],
    /// Started on the first conversation read.
    conversations: Option<Sender<FieldId>>,
    worker_tx: Sender<WorkerMsg>,
    worker_rx: Receiver<WorkerMsg>,
    completions: HashMap<u64, Running>,
    prefill: Option<(u64, Running)>,
    generations: HashMap<u64, Running>,
    /// Inputs produced while executing outputs, fed after the current batch.
    queued: VecDeque<Input>,
    feeding: bool,
    /// Something the status line depends on changed since the last publish.
    dirty: bool,
    last_key: Instant,
    pending: HashMap<u64, Pending>,
    /// The completion whose result is being handled, to time its overlay.
    finishing: Option<(u64, Pending)>,
    caps_sent: [Option<Caps>; 2],
    /// The app that last had a text field focused.
    app: Option<AppInfo>,
    /// The focused field's kind; `None` while no field has focus.
    focused: Option<FieldKind>,
    controller_status: Status,
    download: Option<f32>,
    cloud: CloudCount,
    shared: Arc<Shared>,
}

impl Core {
    pub(crate) fn new(
        host: Arc<dyn Host>,
        backend: Result<Arc<dyn Backend>, String>,
        stats: Option<Stats>,
        style: Option<StyleMemory>,
        hub: Hub,
        workers: Workers,
        shared: Arc<Shared>,
    ) -> Core {
        let (settings, settings_error) = read_settings(&*host);
        let (backend, backend_status) = match backend {
            Ok(b) => (Some(b), None),
            Err(e) => {
                log::warn!("platform backend unavailable: {e}");
                (None, Some(BackendStatus::Unavailable(e)))
            }
        };
        let today = stats.as_ref().map_or(0, |s| {
            s.cloud_requests_today().unwrap_or_else(|e| {
                log::warn!("cannot read today's cloud request count: {e}");
                0
            })
        });
        let Workers {
            jobs,
            gens,
            threads,
            tx: worker_tx,
            rx: worker_rx,
        } = workers;
        let mut core = Core {
            controller: Controller::new(settings.clone()),
            host,
            backend,
            backend_status,
            stats,
            style,
            hub,
            settings,
            settings_error,
            pauses: Pauses::default(),
            timers: Vec::new(),
            jobs,
            gens,
            workers: threads,
            conversations: None,
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
            focused: None,
            controller_status: Status::Active,
            download: None,
            cloud: CloudCount {
                day: local_day(now_ms()),
                count: today,
            },
            shared,
        };
        // The first Settings input makes the controller send bindings, the
        // appearance, the active state and its status.
        core.feed(Input::Settings(Box::new(core.settings.clone())));
        core.hub.apply(&core.settings);
        core.send_caps();
        core
    }

    /// Runs until [`Command::Quit`], then stops the workers.
    pub(crate) fn run(
        mut self,
        events: &Receiver<Event>,
        commands: &Receiver<Command>,
        hub_rx: &Receiver<HubEvent>,
    ) {
        let worker_rx = self.worker_rx.clone();
        let mut select = Select::new();
        let i_events = select.recv(events);
        let i_commands = select.recv(commands);
        let i_workers = select.recv(&worker_rx);
        let i_hub = select.recv(hub_rx);
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
                    Ok(Command::Quit) | Err(TryRecvError::Disconnected) => break,
                    Ok(command) => {
                        self.on_command(command);
                        self.dirty = true;
                    }
                    Err(TryRecvError::Empty) => {}
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
                        // A model loaded for a focus but never used still unloads.
                        if self.hub.has_local_loaded()
                            && !self.timers.iter().any(|(t, _)| *t == Timer::Unload)
                        {
                            self.arm_unload();
                        }
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
        self.shutdown();
    }

    /// Cancels what the workers hold, deactivates the backend, unloads the
    /// local model and waits for the workers. A model load in progress
    /// cannot be interrupted; its thread exits when it finishes.
    fn shutdown(self) {
        let running = self
            .completions
            .values()
            .chain(self.generations.values())
            .chain(self.prefill.as_ref().map(|(_, r)| r));
        for r in running {
            r.cancel.cancel();
        }
        if let Some(b) = &self.backend {
            b.set_active(false);
        }
        let Core {
            mut hub,
            jobs,
            gens,
            workers,
            conversations,
            ..
        } = self;
        hub.unload();
        drop((jobs, gens, conversations));
        for worker in workers {
            let _ = worker.join();
        }
    }

    fn on_event(&mut self, event: Event) {
        match &event {
            Event::Focus(snapshot) => {
                self.last_key = Instant::now();
                if self
                    .app
                    .as_ref()
                    .is_none_or(|a| a.id != snapshot.app.id || a.name != snapshot.app.name)
                {
                    if let Some(stats) = &self.stats {
                        stats.seen(snapshot.app.clone());
                    }
                    self.app = Some(snapshot.app.clone());
                }
                self.focused = Some(snapshot.kind);
                // Nothing is predicted in these fields: no model needs to be ready.
                if !snapshot.kind.is_secure_or_structured() {
                    self.hub.focus();
                }
                self.dirty = true;
                self.load_style(snapshot.app.id.clone());
            }
            Event::Changed(snapshot) => {
                self.last_key = Instant::now();
                self.focused = Some(snapshot.kind);
            }
            Event::Blur => self.focused = None,
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
            Command::SettingsChanged => {
                let (settings, error) = read_settings(&*self.host);
                self.settings_error = error;
                let models_changed = settings.autocomplete_model
                    != self.settings.autocomplete_model
                    || settings.draft_model != self.settings.draft_model
                    || settings.providers != self.settings.providers;
                self.settings = settings;
                if models_changed {
                    self.rebuild_models();
                }
                self.feed(Input::Settings(Box::new(self.settings.clone())));
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
                    // Its lock frees once the requests on it see the cancel.
                    let running = self
                        .completions
                        .values()
                        .chain(self.generations.values())
                        .chain(self.prefill.as_ref().map(|(_, r)| r));
                    for r in running.filter(|r| r.local) {
                        r.cancel.cancel();
                    }
                    self.stop_timer(Timer::Unload);
                    self.hub.unload();
                    self.send_caps();
                }
            }
            Command::Quit => unreachable!("handled by the loop"),
        }
    }

    fn rebuild_models(&mut self) {
        self.stop_timer(Timer::CloudRetry);
        self.hub.apply(&self.settings);
        if self
            .focused
            .is_some_and(|kind| !kind.is_secure_or_structured())
        {
            self.hub.focus();
        }
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
    /// when it does, in memory and in the stats, which keep the count across
    /// restarts.
    fn cloud_allowed(&mut self) -> bool {
        let today = local_day(now_ms());
        if self.cloud.day != today {
            self.cloud = CloudCount {
                day: today,
                count: 0,
            };
        }
        if self
            .settings
            .daily_request_cap
            .is_some_and(|cap| self.cloud.count >= cap)
        {
            return false;
        }
        self.cloud.count += 1;
        if let Some(stats) = &self.stats {
            stats.cloud_request();
        }
        true
    }

    fn cap_reached(&self) -> bool {
        self.cloud.day == local_day(now_ms())
            && self
                .settings
                .daily_request_cap
                .is_some_and(|cap| self.cloud.count >= cap)
    }

    fn on_worker(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Completed {
                id,
                result,
                queue,
                decode,
            } => {
                self.completions.remove(&id);
                let pending = self
                    .pending
                    .remove(&id)
                    .map(|p| Pending { queue, decode, ..p });
                let cancelled = matches!(result, Err(CompleteError::Cancelled));
                self.finishing = pending.map(|p| (id, p));
                self.feed(Input::Completed { id, result });
                if let Some((id, p)) = self.finishing.take() {
                    self.record_timing(id, p, None, cancelled);
                }
            }
            WorkerMsg::Input(input) => {
                if let Input::Generated {
                    id,
                    update: GenerateUpdate::Done(_),
                } = &input
                {
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
            Output::Appearance(appearance) => {
                if let Some(b) = backend {
                    b.set_appearance(appearance);
                }
            }
            Output::Active(active) => {
                if let Some(b) = backend {
                    b.set_active(active);
                }
            }
            Output::Complete(request) => {
                if let Some((_, p)) = self.prefill.take() {
                    p.cancel.cancel();
                }
                let id = request.id;
                match self
                    .cloud_gate(Feature::Autocomplete)
                    .and_then(|()| self.hub.completer(Feature::Autocomplete))
                {
                    Ok(model) => {
                        let cancel = CancelToken::new();
                        let issued = Instant::now();
                        let job = Job::Complete {
                            request,
                            model,
                            cancel: cancel.clone(),
                            queued: issued,
                        };
                        if self.jobs.send(job).is_err() {
                            self.queued.push_back(Input::Completed {
                                id,
                                result: Err(stopped()),
                            });
                            return;
                        }
                        let local = self.hub.is_local(Feature::Autocomplete);
                        self.completions.insert(id, Running { cancel, local });
                        let pending = Pending {
                            key_at: self.last_key,
                            issued,
                            build,
                            queue: Duration::ZERO,
                            decode: Duration::ZERO,
                        };
                        self.pending.insert(id, pending);
                        self.touch_local(Feature::Autocomplete);
                    }
                    Err(e) => self
                        .queued
                        .push_back(Input::Completed { id, result: Err(e) }),
                }
            }
            Output::Prefill(request) => {
                if let Ok(model) = self.hub.completer(Feature::Autocomplete) {
                    let (id, cancel) = (request.id, CancelToken::new());
                    let local = self.hub.is_local(Feature::Autocomplete);
                    if let Some((_, old)) = self.prefill.replace((
                        id,
                        Running {
                            cancel: cancel.clone(),
                            local,
                        },
                    )) {
                        old.cancel.cancel();
                    }
                    let _ = self.jobs.send(Job::Prefill {
                        request,
                        model,
                        cancel,
                    });
                    self.touch_local(Feature::Autocomplete);
                }
            }
            Output::Cancel(id) => {
                if let Some(r) = self.completions.remove(&id) {
                    r.cancel.cancel();
                } else if let Some((_, r)) = self.prefill.take_if(|(p, _)| *p == id) {
                    r.cancel.cancel();
                }
            }
            Output::Generate(request) => {
                let id = request.id;
                match self
                    .cloud_gate(Feature::FlowDraft)
                    .and_then(|()| self.hub.generator(Feature::FlowDraft))
                {
                    Ok(model) => {
                        let cancel = CancelToken::new();
                        if self
                            .gens
                            .send(GenJob {
                                request,
                                model,
                                cancel: cancel.clone(),
                            })
                            .is_err()
                        {
                            self.queued.push_back(Input::Generated {
                                id,
                                update: GenerateUpdate::Done(Err(stopped())),
                            });
                            return;
                        }
                        let local = self.hub.is_local(Feature::FlowDraft);
                        self.generations.insert(id, Running { cancel, local });
                        self.touch_local(Feature::FlowDraft);
                    }
                    Err(e) => self.queued.push_back(Input::Generated {
                        id,
                        update: GenerateUpdate::Done(Err(e)),
                    }),
                }
            }
            Output::CancelGenerate(id) => {
                if let Some(r) = self.generations.remove(&id) {
                    r.cancel.cancel();
                }
            }
            Output::StartTimer { id, after } => {
                self.start_timer(Timer::Controller(id), Instant::now() + after)
            }
            Output::StopTimer(id) => self.stop_timer(Timer::Controller(id)),
            Output::ReadConversation(field) => {
                let sent = self
                    .conversations()
                    .is_some_and(|tx| tx.send(field).is_ok());
                if !sent {
                    self.queued.push_back(Input::Conversation {
                        field,
                        conversation: None,
                    });
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
            Output::Remember { app, text } => {
                if !self.settings.context.style_memory {
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

    /// The conversation reader, started on first use; `None` without a
    /// backend.
    fn conversations(&mut self) -> Option<&Sender<FieldId>> {
        if self.conversations.is_none()
            && let Some(backend) = &self.backend
        {
            match conversation_reader(backend.clone(), self.worker_tx.clone()) {
                Ok(tx) => self.conversations = Some(tx),
                Err(e) => log::warn!("conversation reader failed to start: {e}"),
            }
        }
        self.conversations.as_ref()
    }

    /// Sends the focused app's style samples to the controller once the
    /// style thread has read them.
    fn load_style(&self, app: String) {
        if !self.settings.context.style_memory {
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
        if self
            .focused
            .is_some_and(|kind| !kind.is_secure_or_structured())
        {
            self.hub.focus();
        }
        self.send_caps();
        true
    }

    /// Restarts the idle-unload countdown when the local model serves a request.
    fn touch_local(&mut self, feature: Feature) {
        if self.hub.is_local(feature) {
            self.arm_unload();
        }
    }

    fn arm_unload(&mut self) {
        self.stop_timer(Timer::Unload);
        let minutes = self.settings.unload_after_minutes;
        if minutes > 0 {
            self.start_timer(Timer::Unload, Instant::now() + UNLOAD_UNIT * minutes);
        }
    }

    fn send_caps(&mut self) {
        for (i, feature) in [Feature::Autocomplete, Feature::FlowDraft]
            .into_iter()
            .enumerate()
        {
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
            at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            wait_us: us(p.issued.saturating_duration_since(p.key_at)),
            build_us: us(p.build),
            queue_us: us(p.queue),
            decode_us: us(p.decode),
            render_us: render.map_or(0, us),
            total_us: us(now.saturating_duration_since(p.key_at)),
            shown: render.is_some(),
            cancelled,
        };
        log::debug!("completion {id}: {timing:?}");
        let mut ring = self
            .shared
            .timings
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        let due: Vec<Timer> = self
            .timers
            .iter()
            .filter(|(_, at)| *at <= now)
            .map(|(t, _)| *t)
            .collect();
        self.timers.retain(|(_, at)| *at > now);
        for timer in due {
            match timer {
                Timer::Controller(id) => self.feed(Input::Timer(id)),
                Timer::Unload => {
                    // A long generation is not idleness.
                    if self
                        .completions
                        .values()
                        .chain(self.generations.values())
                        .any(|r| r.local)
                    {
                        self.arm_unload();
                    } else {
                        self.hub.unload();
                        self.send_caps();
                    }
                }
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

    /// Recomputes the status line and tells the host if it changed.
    fn publish(&mut self) {
        let view = self.status_view();
        {
            let mut current = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
            if *current == view {
                return;
            }
            current.clone_from(&view);
        }
        // The lock is released first: hosts read the status back.
        self.host.status(&view);
    }

    fn status_view(&self) -> StatusView {
        let app_id = self.app.as_ref().map(|a| a.id.as_str());
        let pauses = self.pauses.list();
        let (kind, text, detail) = self.status_line(app_id, &pauses);
        StatusView {
            kind,
            text,
            detail,
            pauses,
            app: self.app.as_ref().map(|a| AppRef {
                id: a.id.clone(),
                name: if a.name.is_empty() {
                    a.id.clone()
                } else {
                    a.name.clone()
                },
            }),
            local_model_loaded: self.hub.has_local_loaded(),
        }
    }

    fn status_line(&self, app_id: Option<&str>, pauses: &[PauseInfo]) -> Line {
        let until = |p: &PauseInfo| match p.until_ms {
            Some(ms) => format!(" until {}", local_clock(ms)),
            None => " until resumed".into(),
        };
        if let Some(p) = pauses.iter().find(|p| p.app.is_none()) {
            return (StatusKind::Paused, format!("Paused{}", until(p)), None);
        }
        if let Some(p) = pauses
            .iter()
            .find(|p| p.app.as_deref() == app_id && app_id.is_some())
        {
            let name = p.name.as_deref().unwrap_or_default();
            return (
                StatusKind::Paused,
                format!("Paused in {name}{}", until(p)),
                None,
            );
        }
        if let Some(line) = self.backend_status.as_ref().and_then(backend_line) {
            return line;
        }
        if let Some(why) = &self.settings_error {
            return with_detail(
                StatusKind::SettingsInvalid,
                "Settings problem: ",
                why.clone(),
            );
        }
        if let Some(f) = self.download {
            return (
                StatusKind::Downloading,
                format!("Model downloading, {:.0}%", f * 100.0),
                None,
            );
        }
        match self.hub.local_state() {
            LocalState::Loading(f) => {
                return (
                    StatusKind::Loading,
                    format!("Loading model, {:.0}%", f * 100.0),
                    None,
                );
            }
            LocalState::Failed(e) => {
                return with_detail(StatusKind::Error, "Local model unavailable: ", e);
            }
            LocalState::Idle | LocalState::Ready => {}
        }
        if self.hub.is_building(Feature::Autocomplete) {
            return (
                StatusKind::Loading,
                "Connecting to the provider".into(),
                None,
            );
        }
        if let Some(problem) = self.hub.problem(Feature::Autocomplete) {
            return with_detail(StatusKind::NoModel, "No model: ", problem);
        }
        let fallback = if self.hub.took_over() {
            "; the local model took over"
        } else {
            ""
        };
        let uses_cloud = self.hub.took_over()
            || self.hub.is_cloud(Feature::Autocomplete)
            || self.hub.is_cloud(Feature::FlowDraft);
        if uses_cloud && self.cap_reached() {
            return with_detail(
                StatusKind::CloudUnavailable,
                "Cloud unavailable: ",
                format!("{DAILY_CAP}{fallback}"),
            );
        }
        match &self.controller_status {
            Status::Active => (StatusKind::Active, "Active".into(), None),
            Status::Paused => (StatusKind::Paused, "Paused".into(), None),
            Status::OffInApp(app) => {
                let name = if app.name.is_empty() {
                    &app.id
                } else {
                    &app.name
                };
                (
                    StatusKind::OffInApp,
                    format!("Off in {name}"),
                    Some(name.clone()),
                )
            }
            Status::NoModel => (
                StatusKind::NoModel,
                "No model: choose a local model or add a key".into(),
                None,
            ),
            Status::CloudUnavailable(why) => with_detail(
                StatusKind::CloudUnavailable,
                "Cloud unavailable: ",
                format!("{why}{fallback}"),
            ),
            Status::Backend(status) => {
                backend_line(status).unwrap_or_else(|| (StatusKind::Active, "Active".into(), None))
            }
        }
    }
}

fn read_settings(host: &dyn Host) -> (Settings, Option<String>) {
    match host.settings() {
        Ok(settings) => (settings, None),
        Err(why) => {
            log::warn!("settings rejected, using defaults: {why}");
            (Settings::default(), Some(why))
        }
    }
}

/// The line for a backend problem; `None` when it is ready.
fn backend_line(status: &BackendStatus) -> Option<Line> {
    match status {
        BackendStatus::Ready => None,
        BackendStatus::PermissionMissing(what) => Some(with_detail(
            StatusKind::PermissionNeeded,
            "Permission needed: ",
            what.clone(),
        )),
        BackendStatus::SecureInput { app } => {
            let by = app
                .as_deref()
                .map(|a| format!(" by {a}"))
                .unwrap_or_default();
            Some((
                StatusKind::SecureInput,
                format!("Blocked by secure input{by}"),
                app.clone(),
            ))
        }
        BackendStatus::Unavailable(why) => Some(with_detail(
            StatusKind::Unavailable,
            "Unavailable: ",
            why.clone(),
        )),
    }
}

fn with_detail(kind: StatusKind, prefix: &str, detail: String) -> Line {
    (kind, format!("{prefix}{detail}"), Some(detail))
}

fn stopped() -> CompleteError {
    CompleteError::Failed("the model worker stopped".into())
}
