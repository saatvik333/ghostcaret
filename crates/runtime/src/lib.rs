//! The runtime that drives the engine's controller for the desktop app.
//!
//! One thread owns the [`Controller`](oft_engine::controller::Controller).
//! It feeds it backend events, model results, host commands and timer
//! expiries in arrival order, and executes every
//! [`Output`](oft_engine::controller::Output) against the [`Backend`]. It
//! sleeps in `Select::ready_deadline` until one of those arrives or the
//! earliest timer is due; with no timer armed it blocks indefinitely, so an
//! idle runtime takes no CPU.
//!
//! Workers keep that thread from ever waiting on a model. One runs
//! completions and prefills, one at a time, each with a fresh stop rule and
//! its own cancel token; one streams Reply & rewrite generations; one reads
//! conversations; helper threads build cloud providers, warm their
//! connections, and load and unload the local model.
//!
//! The runtime also enforces the daily cloud cap (counted in `oft-store`'s
//! stats, so it survives restarts), moves autocomplete to an installed local
//! model when the cloud fails or the cap is reached and back five minutes
//! later, unloads the local model after idleness and on memory pressure,
//! keeps pauses, writes usage stats and style memory, and records stage
//! timings.
//!
//! # Host interface
//!
//! What the desktop app owns is split four ways, so the runtime's tests run
//! it against a fake host and a fake backend:
//!
//! - [`Host`]: what the runtime asks of the host: the saved settings, API
//!   keys, and showing the status.
//! - [`Command`]: what the host tells the runtime: settings or models
//!   changed, pauses, download progress, memory pressure, quit.
//! - [`Config`]: what the runtime runs with: the stores and, with local
//!   inference, the model directory and load options.
//! - The [`Backend`], made by the function passed to [`start`], which
//!   reports [`Event`]s on the channel it is given.

mod core;
mod models;
pub mod pause;
mod status;
mod workers;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::io;
#[cfg(feature = "local")]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Sender, unbounded};
use oft_cloud::SentLog;
use oft_engine::io::{Backend, Event};
use oft_engine::settings::Settings;
use oft_store::stats::Stats;
use oft_store::style::StyleMemory;

use crate::core::{Core, Shared};
use crate::models::{Hub, Loader, Models};
use crate::pause::{PauseFor, Scope};
use crate::status::TIMINGS_KEPT;
pub use crate::status::{AppRef, StatusKind, StatusView, Timing};
use crate::workers::Workers;

/// What the runtime asks of its host. Methods are called from runtime
/// threads; they may read [`Runtime::status`] but must never wait on the
/// runtime or drop it.
pub trait Host: Send + Sync + 'static {
    /// The saved settings, already validated. `Err` says why they could not
    /// be read, in words safe to show, never quoting the file; the runtime
    /// then runs on defaults and reports [`StatusKind::SettingsInvalid`].
    /// Called on the runtime thread at start and on
    /// [`Command::SettingsChanged`].
    fn settings(&self) -> Result<Settings, String>;

    /// The API key saved for the provider with this id; `Ok(None)` when there
    /// is none. Called on a helper thread while a provider is built, so it
    /// may block on a keychain prompt. `Err` carries a reason without the
    /// secret.
    fn api_key(&self, provider: &str) -> Result<Option<String>, String>;

    /// The status changed. Called on the runtime thread; must return
    /// promptly.
    fn status(&self, status: &StatusView);
}

/// What the runtime runs with.
#[derive(Debug)]
pub struct Config {
    /// Usage counters, and the count that enforces the daily cloud cap.
    pub stats: Option<Stats>,
    /// Style memory; written only while the settings turn it on.
    pub style: Option<StyleMemory>,
    /// Every cloud request body, for the host's "What was sent" view.
    pub sent_log: SentLog,
    /// Where downloaded models are (`oft_local::store::ModelStore`).
    #[cfg(feature = "local")]
    pub models_dir: PathBuf,
    #[cfg(feature = "local")]
    pub load_options: oft_local::LoadOptions,
}

/// What the host tells the runtime.
#[derive(Debug)]
pub enum Command {
    /// The saved settings changed: read them again with [`Host::settings`].
    SettingsChanged,
    /// A key was saved or removed, or a model was downloaded or deleted:
    /// rebuild the models.
    ModelsChanged,
    Pause(Scope, PauseFor),
    /// Resume one app, or everything with `None`.
    Resume(Option<String>),
    /// Model download progress from 0 to 1; `None` when the download ended,
    /// which rebuilds the models.
    Download(Option<f32>),
    /// The OS reports that memory is short: unload the local model now.
    MemoryPressure,
    /// Stop. Dropping the [`Runtime`] sends this and waits.
    Quit,
}

/// Handle to the running runtime. Dropping it stops the runtime: requests in
/// flight are cancelled, so this waits at most for one model step.
#[derive(Debug)]
pub struct Runtime {
    commands: Sender<Command>,
    events: Sender<Event>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Runtime {
    /// Never blocks.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// A command sender for other threads, such as a memory-pressure watcher.
    pub fn commands(&self) -> Sender<Command> {
        self.commands.clone()
    }

    /// Sends an event as the backend would, for tests with a fake backend.
    /// Never blocks. The runtime also keeps one sender so the channel stays
    /// open when the backend fails or drops its own.
    pub fn event(&self, event: Event) {
        let _ = self.events.send(event);
    }

    pub fn status(&self) -> StatusView {
        self.shared
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Timings of the latest autocomplete requests, oldest first.
    pub fn timings(&self) -> Vec<Timing> {
        self.shared
            .timings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the runtime. `backend` runs first on the runtime thread and gets
/// the sender for its [`Event`]s; when it fails the runtime runs without one
/// and reports [`StatusKind::Unavailable`] with its reason. Fails only when a
/// thread cannot be started.
pub fn start<B>(host: Arc<dyn Host>, config: Config, backend: B) -> io::Result<Runtime>
where
    B: FnOnce(Sender<Event>) -> Result<Arc<dyn Backend>, String> + Send + 'static,
{
    let loader = Arc::new(Models::new(&config));
    start_with(host, config, backend, loader)
}

fn start_with<B>(
    host: Arc<dyn Host>,
    config: Config,
    backend: B,
    loader: Arc<dyn Loader>,
) -> io::Result<Runtime>
where
    B: FnOnce(Sender<Event>) -> Result<Arc<dyn Backend>, String> + Send + 'static,
{
    let (commands, commands_rx) = unbounded();
    let (events, events_rx) = unbounded();
    let shared = Arc::new(Shared {
        status: Mutex::new(StatusView::starting()),
        timings: Mutex::new(VecDeque::with_capacity(TIMINGS_KEPT)),
    });
    let workers = Workers::start()?;
    let (hub, hub_rx) = Hub::new(host.clone(), loader)?;
    let Config { stats, style, .. } = config;
    let thread = {
        let (shared, events) = (shared.clone(), events.clone());
        thread::Builder::new()
            .name("oft-runtime".into())
            .spawn(move || {
                let backend = backend(events);
                Core::new(host, backend, stats, style, hub, workers, shared).run(
                    &events_rx,
                    &commands_rx,
                    &hub_rx,
                );
            })?
    };
    Ok(Runtime {
        commands,
        events,
        shared,
        thread: Some(thread),
    })
}
