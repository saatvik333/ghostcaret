//! Which model serves each feature, built from the settings.
//!
//! Each feature has a slot. A cloud provider is built with its key on a
//! helper thread, because reading the key may wait on a keychain prompt or
//! call into the JVM, and its connection is warmed when a text field gains
//! focus. The local model loads on the local-model thread, with progress,
//! when a text field gains focus or on the first request that needs it;
//! both features share it when they choose the same file. It is unloaded
//! after idleness, on memory pressure and when no slot uses it any more.
//! Loads and unloads run in order on that one thread, so a model never
//! loads while the previous one is still in memory, and the runtime thread
//! never waits on a model's lock.
//!
//! When the cloud fails or the daily cap is reached, autocomplete can move
//! to an installed local model ([`Hub::take_over_from_cloud`]) until the
//! runtime hands it back ([`Hub::restore_cloud`]).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, unbounded};
use oft_cloud::SentLog;
use oft_engine::complete::{Caps, CompleteError, Completer, Generator};
use oft_engine::controller::Feature;
use oft_engine::settings::{ModelChoice, ProviderConfig, Settings};

use crate::Host;

/// A cloud model as the runtime uses it.
pub(crate) trait CloudModel: Completer + Generator + fmt::Debug {
    /// Opens the connection ahead of the first request; may block for a
    /// round trip.
    fn warm(&self);
}

impl CloudModel for oft_cloud::Provider {
    fn warm(&self) {
        oft_cloud::Provider::warm(self);
    }
}

/// A loaded local model as the runtime uses it.
pub(crate) trait LocalModel: Completer + Generator + fmt::Debug {
    /// Frees the weights and the context; later requests fail as
    /// unavailable. Waits for a running request to let go of the model.
    fn unload(&self);
}

#[cfg(feature = "local")]
impl LocalModel for oft_local::LocalModel {
    fn unload(&self) {
        oft_local::LocalModel::unload(self);
    }
}

/// Builds and finds models: [`Models`] in the product, fakes in tests.
pub(crate) trait Loader: Send + Sync + 'static {
    /// Called on a helper thread with the key the host returned.
    fn provider(
        &self,
        config: &ProviderConfig,
        key: Option<String>,
    ) -> Result<Arc<dyn CloudModel>, String>;
    /// The file for a local model setting, or why there is none.
    fn resolve(&self, model: &str) -> Result<PathBuf, String>;
    /// An installed model autocomplete can move to when the cloud fails.
    fn fallback(&self) -> Option<PathBuf>;
    /// Loads a local model, reporting progress from 0 to 1. Called on the
    /// local-model thread.
    fn load(
        &self,
        path: &Path,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Arc<dyn LocalModel>, String>;
}

/// The product's models: `oft-cloud` providers, and `oft-local` models from
/// the model store.
pub(crate) struct Models {
    sent_log: SentLog,
    #[cfg(feature = "local")]
    store: oft_local::store::ModelStore,
    #[cfg(feature = "local")]
    options: oft_local::LoadOptions,
}

impl Models {
    pub(crate) fn new(config: &crate::Config) -> Models {
        Models {
            sent_log: config.sent_log.clone(),
            #[cfg(feature = "local")]
            store: oft_local::store::ModelStore::new(config.models_dir.clone()),
            #[cfg(feature = "local")]
            options: config.load_options.clone(),
        }
    }
}

impl Loader for Models {
    fn provider(
        &self,
        config: &ProviderConfig,
        key: Option<String>,
    ) -> Result<Arc<dyn CloudModel>, String> {
        oft_cloud::provider(config, key, self.sent_log.clone())
            .map(|p| p as Arc<dyn CloudModel>)
            .map_err(|e| e.to_string())
    }

    #[cfg(feature = "local")]
    fn resolve(&self, model: &str) -> Result<PathBuf, String> {
        self.store
            .resolve(model)
            .ok_or_else(|| "the local model is not downloaded yet".into())
    }

    #[cfg(not(feature = "local"))]
    fn resolve(&self, _model: &str) -> Result<PathBuf, String> {
        Err("this build has no local inference".into())
    }

    /// The first installed model from the curated list.
    #[cfg(feature = "local")]
    fn fallback(&self) -> Option<PathBuf> {
        let installed = self.store.installed().ok()?;
        installed
            .into_iter()
            .find(|i| i.entry.is_some())
            .map(|i| i.path)
    }

    #[cfg(not(feature = "local"))]
    fn fallback(&self) -> Option<PathBuf> {
        None
    }

    #[cfg(feature = "local")]
    fn load(
        &self,
        path: &Path,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Arc<dyn LocalModel>, String> {
        oft_local::LocalModel::load_with_progress(path, self.options.clone(), progress)
            .map(|m| Arc::new(m) as Arc<dyn LocalModel>)
            .map_err(|e| e.to_string())
    }

    /// Unreachable: no setting resolves to a file without local inference.
    #[cfg(not(feature = "local"))]
    fn load(
        &self,
        _path: &Path,
        _progress: &mut dyn FnMut(f32),
    ) -> Result<Arc<dyn LocalModel>, String> {
        Err("this build has no local inference".into())
    }
}

/// Results the helper threads send back to the runtime thread.
pub(crate) enum HubEvent {
    Built {
        generation: u64,
        provider: String,
        result: Result<Arc<dyn CloudModel>, String>,
    },
    Progress {
        load: u64,
        fraction: f32,
    },
    Loaded {
        load: u64,
        result: Result<Arc<dyn LocalModel>, String>,
    },
}

enum LocalJob {
    /// Unload the model this thread loaded last, then load `path`.
    Load {
        load: u64,
        path: PathBuf,
    },
    Unload(Arc<dyn LocalModel>),
}

#[derive(Debug, Clone)]
enum Slot {
    None,
    /// The provider with this id is being built.
    Building(String),
    Cloud(Arc<dyn CloudModel>),
    Local(PathBuf),
    Failed(String),
}

/// What the local model is doing, for the status line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LocalState {
    Idle,
    Loading(f32),
    Ready,
    Failed(String),
}

/// The local model in memory, or on its way there.
#[derive(Debug, Default)]
struct Resident {
    loaded: Option<(PathBuf, Arc<dyn LocalModel>)>,
    /// The load in progress: its number, its file and how far it got.
    loading: Option<(u64, PathBuf, f32)>,
    /// The last load of this file failed. Out of memory and broken files
    /// are reported, not retried in a loop; a settings change retries.
    failed: Option<(PathBuf, String)>,
}

/// Capabilities the local model reports, used before it is first loaded so
/// the controller knows a model is configured. Replaced by the loaded model's
/// own `caps()` once it is in memory.
const LOCAL_CAPS: Caps = Caps {
    token_probs: true,
    cheap_alternatives: true,
    prefill: true,
    local: true,
};

pub(crate) struct Hub {
    tx: Sender<HubEvent>,
    host: Arc<dyn Host>,
    loader: Arc<dyn Loader>,
    /// Bumped by every [`Hub::apply`], so providers built for older settings
    /// are ignored.
    generation: u64,
    /// Numbers the local loads, so a result for a superseded load is unloaded.
    loads: u64,
    autocomplete: Slot,
    draft: Slot,
    /// The cloud slot the local model replaced after the provider failed.
    displaced: Option<Slot>,
    warm: Sender<Arc<dyn CloudModel>>,
    local: Sender<LocalJob>,
    resident: Resident,
}

impl Hub {
    pub(crate) fn new(
        host: Arc<dyn Host>,
        loader: Arc<dyn Loader>,
    ) -> io::Result<(Hub, Receiver<HubEvent>)> {
        let (tx, rx) = unbounded();
        let (warm, warm_rx) = unbounded::<Arc<dyn CloudModel>>();
        thread::Builder::new()
            .name("oft-warm".into())
            .spawn(move || {
                while let Ok(mut provider) = warm_rx.recv() {
                    // Only the latest focus matters.
                    while let Ok(newer) = warm_rx.try_recv() {
                        provider = newer;
                    }
                    provider.warm();
                }
            })?;
        let (local, jobs) = unbounded();
        {
            let (loader, out) = (loader.clone(), tx.clone());
            thread::Builder::new()
                .name("oft-local".into())
                .spawn(move || local_thread(&*loader, &jobs, &out))?;
        }
        let hub = Hub {
            tx,
            host,
            loader,
            generation: 0,
            loads: 0,
            autocomplete: Slot::None,
            draft: Slot::None,
            displaced: None,
            warm,
            local,
            resident: Resident::default(),
        };
        Ok((hub, rx))
    }

    /// Rebuilds both slots from `settings`. Call when the model choices, the
    /// providers, a key or the installed models change.
    pub(crate) fn apply(&mut self, settings: &Settings) {
        self.generation += 1;
        self.displaced = None;
        // Both features on one provider share it, and its request limit.
        let mut building = Vec::new();
        self.autocomplete = self.slot_for(settings, &settings.autocomplete_model, &mut building);
        self.draft = self.slot_for(settings, &settings.draft_model, &mut building);
        if self
            .resident
            .loaded
            .as_ref()
            .is_some_and(|(p, _)| !self.uses(p))
        {
            self.unload();
        }
        // Its result is unloaded when it arrives.
        if self
            .resident
            .loading
            .as_ref()
            .is_some_and(|(_, p, _)| !self.uses(p))
        {
            self.resident.loading = None;
        }
        self.resident.failed = None;
    }

    fn uses(&self, path: &Path) -> bool {
        [&self.autocomplete, &self.draft]
            .into_iter()
            .any(|s| matches!(s, Slot::Local(p) if p == path))
    }

    /// The slot for `choice`. Starts building a provider unless `building`
    /// already names it.
    fn slot_for(
        &self,
        settings: &Settings,
        choice: &ModelChoice,
        building: &mut Vec<String>,
    ) -> Slot {
        match choice {
            ModelChoice::None => Slot::None,
            ModelChoice::Provider { provider } => {
                if building.contains(provider) {
                    return Slot::Building(provider.clone());
                }
                let Some(config) = settings
                    .providers
                    .iter()
                    .find(|p| &p.id == provider)
                    .cloned()
                else {
                    return Slot::Failed("the chosen provider is not configured".into());
                };
                let (tx, host, loader, generation) = (
                    self.tx.clone(),
                    self.host.clone(),
                    self.loader.clone(),
                    self.generation,
                );
                let spawned = thread::Builder::new()
                    .name("oft-provider".into())
                    .spawn(move || {
                        let result = host
                            .api_key(&config.id)
                            .and_then(|key| loader.provider(&config, key));
                        let _ = tx.send(HubEvent::Built {
                            generation,
                            provider: config.id,
                            result,
                        });
                    });
                match spawned {
                    Ok(_) => {
                        building.push(provider.clone());
                        Slot::Building(provider.clone())
                    }
                    Err(e) => Slot::Failed(format!("could not start: {e}")),
                }
            }
            ModelChoice::Local { model } => match self.loader.resolve(model) {
                Ok(path) => Slot::Local(path),
                Err(why) => Slot::Failed(why),
            },
        }
    }

    fn slot(&self, feature: Feature) -> &Slot {
        match feature {
            Feature::Autocomplete => &self.autocomplete,
            Feature::FlowDraft => &self.draft,
        }
    }

    /// Takes a helper thread's result. Returns true when capabilities or the
    /// model state may have changed.
    pub(crate) fn on_event(&mut self, event: HubEvent) -> bool {
        match event {
            HubEvent::Built {
                generation,
                provider,
                result,
            } => {
                if generation != self.generation {
                    return false;
                }
                let mut changed = false;
                for slot in [&mut self.autocomplete, &mut self.draft] {
                    if matches!(slot, Slot::Building(id) if *id == provider) {
                        *slot = match &result {
                            Ok(p) => Slot::Cloud(p.clone()),
                            Err(e) => Slot::Failed(e.clone()),
                        };
                        changed = true;
                    }
                }
                changed
            }
            HubEvent::Progress { load, fraction } => match &mut self.resident.loading {
                Some((l, _, f)) if *l == load => {
                    *f = fraction;
                    true
                }
                _ => false,
            },
            HubEvent::Loaded { load, result } => {
                let Some((_, path, _)) = self.resident.loading.take_if(|(l, _, _)| *l == load)
                else {
                    if let Ok(model) = result {
                        let _ = self.local.send(LocalJob::Unload(model));
                    }
                    return false;
                };
                match result {
                    Ok(model) => self.resident.loaded = Some((path, model)),
                    Err(e) => self.resident.failed = Some((path, e)),
                }
                true
            }
        }
    }

    /// A text field gained focus: open autocomplete's provider connection,
    /// and start loading the local model, autocomplete's or else Reply &
    /// rewrite's, so it is ready by the time the user pauses or asks.
    pub(crate) fn focus(&mut self) {
        if let Slot::Cloud(p) = &self.autocomplete {
            let _ = self.warm.send(p.clone());
        }
        let local = [&self.autocomplete, &self.draft]
            .into_iter()
            .find_map(|s| match s {
                Slot::Local(path) => Some(path.clone()),
                _ => None,
            });
        if let Some(path) = local {
            self.ensure_loaded(&path);
        }
    }

    /// Whether `feature`'s provider is still being built.
    pub(crate) fn is_building(&self, feature: Feature) -> bool {
        matches!(self.slot(feature), Slot::Building(_))
    }

    fn ensure_loaded(&mut self, path: &Path) {
        let loaded = self
            .resident
            .loaded
            .as_ref()
            .is_some_and(|(p, _)| p == path);
        let loading = self
            .resident
            .loading
            .as_ref()
            .is_some_and(|(_, p, _)| p == path);
        let failed = self
            .resident
            .failed
            .as_ref()
            .is_some_and(|(p, _)| p == path);
        if loaded || loading || failed {
            return;
        }
        // One local model at a time.
        self.unload();
        self.loads += 1;
        let job = LocalJob::Load {
            load: self.loads,
            path: path.to_owned(),
        };
        if self.local.send(job).is_ok() {
            self.resident.loading = Some((self.loads, path.to_owned(), 0.0));
        } else {
            self.resident.failed = Some((path.to_owned(), "the model loader stopped".into()));
        }
    }

    /// Frees the local model. It loads again on the next focus or request.
    pub(crate) fn unload(&mut self) {
        if let Some((_, model)) = self.resident.loaded.take() {
            let _ = self.local.send(LocalJob::Unload(model));
        }
    }

    pub(crate) fn has_local_loaded(&self) -> bool {
        self.resident.loaded.is_some()
    }

    /// Whether `feature` runs on the local model.
    pub(crate) fn is_local(&self, feature: Feature) -> bool {
        matches!(self.slot(feature), Slot::Local(_))
    }

    /// Whether `feature` runs on a cloud provider.
    pub(crate) fn is_cloud(&self, feature: Feature) -> bool {
        matches!(self.slot(feature), Slot::Cloud(_))
    }

    pub(crate) fn caps(&self, feature: Feature) -> Option<Caps> {
        match self.slot(feature) {
            Slot::Cloud(p) => Some(p.caps()),
            Slot::Local(path) => match &self.resident.loaded {
                Some((p, m)) if p == path => Some(m.caps()),
                _ => Some(LOCAL_CAPS),
            },
            Slot::None | Slot::Building(_) | Slot::Failed(_) => None,
        }
    }

    pub(crate) fn completer(
        &mut self,
        feature: Feature,
    ) -> Result<Arc<dyn Completer>, CompleteError> {
        match self.slot(feature).clone() {
            Slot::Cloud(p) => Ok(p),
            Slot::Local(path) => self.local_model(&path).map(|m| m as Arc<dyn Completer>),
            other => Err(unavailable(&other)),
        }
    }

    pub(crate) fn generator(
        &mut self,
        feature: Feature,
    ) -> Result<Arc<dyn Generator>, CompleteError> {
        match self.slot(feature).clone() {
            Slot::Cloud(p) => Ok(p),
            Slot::Local(path) => self.local_model(&path).map(|m| m as Arc<dyn Generator>),
            other => Err(unavailable(&other)),
        }
    }

    fn local_model(&mut self, path: &Path) -> Result<Arc<dyn LocalModel>, CompleteError> {
        if let Some((p, m)) = &self.resident.loaded
            && p == path
        {
            return Ok(m.clone());
        }
        if let Some((p, e)) = &self.resident.failed
            && p == path
        {
            return Err(CompleteError::Unavailable(e.clone()));
        }
        self.ensure_loaded(path);
        Err(CompleteError::Unavailable(
            "the local model is loading".into(),
        ))
    }

    /// The cloud provider for autocomplete failed: switch autocomplete to an
    /// installed local model, if there is one. Returns true if it did.
    pub(crate) fn take_over_from_cloud(&mut self) -> bool {
        if !matches!(self.autocomplete, Slot::Cloud(_)) {
            return false;
        }
        let Some(path) = self.fallback_local() else {
            return false;
        };
        self.displaced = Some(std::mem::replace(&mut self.autocomplete, Slot::Local(path)));
        true
    }

    /// Gives autocomplete back to the cloud provider after a takeover.
    pub(crate) fn restore_cloud(&mut self) -> bool {
        match self.displaced.take() {
            Some(slot) => {
                self.autocomplete = slot;
                true
            }
            None => false,
        }
    }

    pub(crate) fn took_over(&self) -> bool {
        self.displaced.is_some()
    }

    /// Reply & rewrite's local model, or an installed one.
    fn fallback_local(&self) -> Option<PathBuf> {
        if let Slot::Local(p) = &self.draft {
            return Some(p.clone());
        }
        self.loader.fallback()
    }

    pub(crate) fn local_state(&self) -> LocalState {
        if let Some((_, _, f)) = &self.resident.loading {
            return LocalState::Loading(*f);
        }
        if let Some((_, e)) = &self.resident.failed {
            return LocalState::Failed(e.clone());
        }
        if self.resident.loaded.is_some()
            && (self.is_local(Feature::Autocomplete) || self.is_local(Feature::FlowDraft))
        {
            return LocalState::Ready;
        }
        LocalState::Idle
    }

    /// Why `feature` cannot run, when its slot itself is the reason.
    pub(crate) fn problem(&self, feature: Feature) -> Option<String> {
        match self.slot(feature) {
            Slot::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }
}

fn unavailable(slot: &Slot) -> CompleteError {
    CompleteError::Unavailable(match slot {
        Slot::None => "no model is configured".into(),
        Slot::Building(_) => "the provider is starting".into(),
        Slot::Failed(e) => e.clone(),
        Slot::Cloud(_) | Slot::Local(_) => "unavailable".into(),
    })
}

/// The local-model thread: loads and unloads in the order the runtime asked.
fn local_thread(loader: &dyn Loader, jobs: &Receiver<LocalJob>, out: &Sender<HubEvent>) {
    let mut last: Option<Arc<dyn LocalModel>> = None;
    for job in jobs {
        match job {
            LocalJob::Unload(model) => {
                model.unload();
                if last.as_ref().is_some_and(|l| Arc::ptr_eq(l, &model)) {
                    last = None;
                }
            }
            LocalJob::Load { load, path } => {
                if let Some(previous) = last.take() {
                    previous.unload();
                }
                let started = Instant::now();
                let mut reported = -1.0f32;
                let result = loader.load(&path, &mut |fraction| {
                    if fraction - reported >= 0.02 || fraction >= 1.0 {
                        reported = fraction;
                        let _ = out.send(HubEvent::Progress { load, fraction });
                    }
                });
                match &result {
                    Ok(model) => {
                        log::info!("local model loaded in {:?}", started.elapsed());
                        last = Some(model.clone());
                    }
                    Err(e) => log::warn!("local model failed to load: {e}"),
                }
                if out.send(HubEvent::Loaded { load, result }).is_err() {
                    return;
                }
            }
        }
    }
}
