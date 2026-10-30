//! Which model serves each feature, built from settings.
//!
//! Cloud providers are built on a helper thread (the keychain may prompt) and
//! warmed when a field gains focus. The local model is loaded on first use on
//! a loader thread with progress, shared by both features when they choose
//! the same file, and unloaded after idleness or on memory pressure. Only one
//! local model is loaded at a time.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use oft_cloud::{Provider, SentLog};
use oft_engine::complete::{Caps, CompleteError, Completer, Generator};
use oft_engine::controller::Feature;
use oft_engine::settings::{ModelChoice, Settings};
#[cfg(feature = "local")]
use oft_local::{LoadOptions, LocalModel, store::ModelStore};

use crate::keychain;

/// Results the helper threads send back to the runtime.
#[derive(Debug)]
pub enum HubEvent {
    Built { generation: u64, feature: Feature, result: Result<Arc<Provider>, String> },
    #[cfg(feature = "local")]
    LoadProgress { path: PathBuf, fraction: f32 },
    #[cfg(feature = "local")]
    Loaded { path: PathBuf, result: Result<Arc<LocalModel>, String> },
}

#[derive(Debug, Clone)]
enum Slot {
    None,
    Building,
    Cloud(Arc<Provider>),
    #[cfg_attr(not(feature = "local"), allow(dead_code))]
    Local(PathBuf),
    Failed(String),
}

/// What the local model is doing, for the tray.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "local"), allow(dead_code))]
pub enum LocalState {
    Idle,
    Loading(f32),
    Ready,
    Failed(String),
}

#[cfg(feature = "local")]
#[derive(Debug, Default)]
struct Local {
    loaded: Option<(PathBuf, Arc<LocalModel>)>,
    loading: Option<(PathBuf, f32)>,
    failed: Option<(PathBuf, String)>,
}

/// Capabilities the local model reports, used before it is first loaded so
/// the controller knows a model is configured. Replaced by the loaded model's
/// own `caps()` once it is in memory.
#[cfg(feature = "local")]
const LOCAL_CAPS: Caps = Caps { token_probs: true, cheap_alternatives: true, prefill: true, local: true };

pub struct Hub {
    tx: Sender<HubEvent>,
    log: SentLog,
    generation: u64,
    autocomplete: Slot,
    draft: Slot,
    /// The cloud slot local inference replaced after the provider failed.
    displaced: Option<Slot>,
    warm: Sender<Arc<Provider>>,
    #[cfg(feature = "local")]
    store: ModelStore,
    #[cfg(feature = "local")]
    local: Local,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("generation", &self.generation)
            .field("autocomplete", &self.autocomplete)
            .field("draft", &self.draft)
            .finish_non_exhaustive()
    }
}

impl Hub {
    pub fn new(log: SentLog) -> (Hub, Receiver<HubEvent>) {
        let (tx, rx) = unbounded();
        let (warm, warm_rx) = unbounded::<Arc<Provider>>();
        let _ = thread::Builder::new().name("oft-warm".into()).spawn(move || {
            while let Ok(mut provider) = warm_rx.recv() {
                // Only the latest focus matters.
                while let Ok(newer) = warm_rx.try_recv() {
                    provider = newer;
                }
                provider.warm();
            }
        });
        let hub = Hub {
            tx,
            log,
            generation: 0,
            autocomplete: Slot::None,
            draft: Slot::None,
            displaced: None,
            warm,
            #[cfg(feature = "local")]
            store: ModelStore::new(crate::paths::models_dir()),
            #[cfg(feature = "local")]
            local: Local::default(),
        };
        (hub, rx)
    }

    /// Rebuilds both slots from `settings`. Call when the model choices, the
    /// providers, a key or the installed models change.
    pub fn apply(&mut self, settings: &Settings) {
        self.generation += 1;
        self.displaced = None;
        self.autocomplete = self.slot_for(settings, Feature::Autocomplete);
        self.draft = self.slot_for(settings, Feature::FlowDraft);
        #[cfg(feature = "local")]
        {
            let wanted = |s: &Slot, p: &PathBuf| matches!(s, Slot::Local(q) if q == p);
            let in_use = |p: &PathBuf| wanted(&self.autocomplete, p) || wanted(&self.draft, p);
            if self.local.loaded.as_ref().is_some_and(|(p, _)| !in_use(p)) {
                self.unload();
            }
            self.local.failed = None;
        }
    }

    fn slot_for(&self, settings: &Settings, feature: Feature) -> Slot {
        let choice = match feature {
            Feature::Autocomplete => &settings.autocomplete_model,
            Feature::FlowDraft => &settings.draft_model,
        };
        match choice {
            ModelChoice::None => Slot::None,
            ModelChoice::Provider { provider } => {
                let Some(config) = settings.providers.iter().find(|p| &p.id == provider).cloned() else {
                    return Slot::Failed("the chosen provider is not configured".into());
                };
                let (tx, log, generation) = (self.tx.clone(), self.log.clone(), self.generation);
                let spawned = thread::Builder::new().name("oft-provider".into()).spawn(move || {
                    let result = keychain::get(&config.id)
                        .map_err(|e| e.to_string())
                        .and_then(|key| oft_cloud::provider(&config, key, log).map_err(|e| e.to_string()));
                    let _ = tx.send(HubEvent::Built { generation, feature, result });
                });
                match spawned {
                    Ok(_) => Slot::Building,
                    Err(e) => Slot::Failed(format!("could not start: {e}")),
                }
            }
            ModelChoice::Local { model } => self.local_slot(model),
        }
    }

    #[cfg(feature = "local")]
    fn local_slot(&self, model: &str) -> Slot {
        match self.store.resolve(model) {
            Some(path) if path.is_file() => Slot::Local(path),
            _ => Slot::Failed("the local model is not downloaded yet".into()),
        }
    }

    #[cfg(not(feature = "local"))]
    fn local_slot(&self, _model: &str) -> Slot {
        Slot::Failed("this build has no local inference".into())
    }

    fn slot(&self, feature: Feature) -> &Slot {
        match feature {
            Feature::Autocomplete => &self.autocomplete,
            Feature::FlowDraft => &self.draft,
        }
    }

    fn slot_mut(&mut self, feature: Feature) -> &mut Slot {
        match feature {
            Feature::Autocomplete => &mut self.autocomplete,
            Feature::FlowDraft => &mut self.draft,
        }
    }

    /// Takes a helper thread's result. Returns true when capabilities or the
    /// model state may have changed.
    pub fn on_event(&mut self, event: HubEvent) -> bool {
        match event {
            HubEvent::Built { generation, feature, result } => {
                if generation != self.generation || !matches!(self.slot(feature), Slot::Building) {
                    return false;
                }
                *self.slot_mut(feature) = match result {
                    Ok(p) => Slot::Cloud(p),
                    Err(e) => Slot::Failed(e),
                };
                true
            }
            #[cfg(feature = "local")]
            HubEvent::LoadProgress { path, fraction } => match &mut self.local.loading {
                Some((p, f)) if *p == path => {
                    *f = fraction;
                    true
                }
                _ => false,
            },
            #[cfg(feature = "local")]
            HubEvent::Loaded { path, result } => {
                if self.local.loading.as_ref().is_none_or(|(p, _)| *p != path) {
                    if let Ok(m) = result {
                        m.unload();
                    }
                    return false;
                }
                self.local.loading = None;
                match result {
                    Ok(model) => self.local.loaded = Some((path, model)),
                    // Out of memory and broken files are reported, not retried in a loop.
                    Err(e) => self.local.failed = Some((path, e)),
                }
                true
            }
        }
    }

    /// A field gained focus: open the provider connection, or start loading
    /// the local model so it is ready by the time the user pauses.
    pub fn focus(&mut self) {
        for feature in [Feature::Autocomplete, Feature::FlowDraft] {
            match self.slot(feature).clone() {
                Slot::Cloud(p) if feature == Feature::Autocomplete => {
                    let _ = self.warm.send(p);
                }
                #[cfg(feature = "local")]
                Slot::Local(path) if feature == Feature::Autocomplete => self.ensure_loaded(&path),
                _ => {}
            }
        }
    }

    #[cfg(feature = "local")]
    fn ensure_loaded(&mut self, path: &PathBuf) {
        let loaded = self.local.loaded.as_ref().is_some_and(|(p, _)| p == path);
        let loading = self.local.loading.as_ref().is_some_and(|(p, _)| p == path);
        let failed = self.local.failed.as_ref().is_some_and(|(p, _)| p == path);
        if loaded || loading || failed {
            return;
        }
        // One local model at a time.
        self.unload();
        self.local.loading = Some((path.clone(), 0.0));
        let (tx, target) = (self.tx.clone(), path.clone());
        let spawned = thread::Builder::new().name("oft-load".into()).spawn(move || {
            let mut reported = -1.0f32;
            let result = LocalModel::load_with_progress(&target, LoadOptions::default(), &mut |fraction: f32| {
                if fraction - reported >= 0.02 || fraction >= 1.0 {
                    reported = fraction;
                    let _ = tx.send(HubEvent::LoadProgress { path: target.clone(), fraction });
                }
            })
            .map(Arc::new)
            .map_err(|e| e.to_string());
            let _ = tx.send(HubEvent::Loaded { path: target, result });
        });
        if let Err(e) = spawned {
            self.local.loading = None;
            self.local.failed = Some((path.clone(), format!("could not start loading: {e}")));
        }
    }

    /// Frees the local model. It loads again on the next focus or request.
    pub fn unload(&mut self) {
        #[cfg(feature = "local")]
        if let Some((_, model)) = self.local.loaded.take() {
            model.unload();
        }
    }

    pub fn has_local_loaded(&self) -> bool {
        #[cfg(feature = "local")]
        return self.local.loaded.is_some();
        #[cfg(not(feature = "local"))]
        return false;
    }

    /// Whether `feature` runs on the local model.
    pub fn is_local(&self, feature: Feature) -> bool {
        matches!(self.slot(feature), Slot::Local(_))
    }

    /// Whether `feature` runs on a cloud provider.
    pub fn is_cloud(&self, feature: Feature) -> bool {
        matches!(self.slot(feature), Slot::Cloud(_))
    }

    pub fn caps(&self, feature: Feature) -> Option<Caps> {
        match self.slot(feature) {
            Slot::Cloud(p) => Some(p.caps()),
            #[cfg(feature = "local")]
            Slot::Local(path) => match &self.local.loaded {
                Some((p, m)) if p == path => Some(m.caps()),
                _ => Some(LOCAL_CAPS),
            },
            _ => None,
        }
    }

    pub fn completer(&mut self, feature: Feature) -> Result<Arc<dyn Completer>, CompleteError> {
        match self.slot(feature).clone() {
            Slot::Cloud(p) => Ok(p),
            #[cfg(feature = "local")]
            Slot::Local(path) => self.local_model(&path).map(|m| m as Arc<dyn Completer>),
            other => Err(unavailable(&other)),
        }
    }

    pub fn generator(&mut self, feature: Feature) -> Result<Arc<dyn Generator>, CompleteError> {
        match self.slot(feature).clone() {
            Slot::Cloud(p) => Ok(p),
            #[cfg(feature = "local")]
            Slot::Local(path) => self.local_model(&path).map(|m| m as Arc<dyn Generator>),
            other => Err(unavailable(&other)),
        }
    }

    #[cfg(feature = "local")]
    fn local_model(&mut self, path: &PathBuf) -> Result<Arc<LocalModel>, CompleteError> {
        if let Some((p, m)) = &self.local.loaded
            && p == path
        {
            return Ok(m.clone());
        }
        if let Some((p, e)) = &self.local.failed
            && p == path
        {
            return Err(CompleteError::Unavailable(e.clone()));
        }
        self.ensure_loaded(path);
        Err(CompleteError::Unavailable("the local model is loading".into()))
    }

    /// The cloud provider for autocomplete failed: switch autocomplete to an
    /// installed local model, if there is one. Returns true if it did.
    pub fn take_over_from_cloud(&mut self) -> bool {
        if !matches!(self.autocomplete, Slot::Cloud(_)) {
            return false;
        }
        let Some(path) = self.fallback_local() else { return false };
        self.displaced = Some(std::mem::replace(&mut self.autocomplete, Slot::Local(path)));
        true
    }

    /// Gives autocomplete back to the cloud provider after a takeover.
    pub fn restore_cloud(&mut self) -> bool {
        match self.displaced.take() {
            Some(slot) => {
                self.autocomplete = slot;
                true
            }
            None => false,
        }
    }

    pub fn took_over(&self) -> bool {
        self.displaced.is_some()
    }

    #[cfg(feature = "local")]
    fn fallback_local(&self) -> Option<PathBuf> {
        if let Slot::Local(p) = &self.draft {
            return Some(p.clone());
        }
        let installed = self.store.installed().ok()?;
        installed.into_iter().find(|i| i.entry.is_some()).map(|i| i.path)
    }

    #[cfg(not(feature = "local"))]
    fn fallback_local(&self) -> Option<PathBuf> {
        None
    }

    pub fn local_state(&self) -> LocalState {
        #[cfg(feature = "local")]
        {
            let uses_local = |s: &Slot| matches!(s, Slot::Local(_));
            if let Some((_, f)) = &self.local.loading {
                return LocalState::Loading(*f);
            }
            if let Some((_, e)) = &self.local.failed {
                return LocalState::Failed(e.clone());
            }
            if self.local.loaded.is_some() && (uses_local(&self.autocomplete) || uses_local(&self.draft)) {
                return LocalState::Ready;
            }
        }
        LocalState::Idle
    }

    /// Why autocomplete cannot run, when its slot itself is the reason.
    pub fn problem(&self, feature: Feature) -> Option<String> {
        match self.slot(feature) {
            Slot::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }
}

fn unavailable(slot: &Slot) -> CompleteError {
    CompleteError::Unavailable(match slot {
        Slot::None => "no model is configured".into(),
        Slot::Building => "the provider is starting".into(),
        Slot::Failed(e) => e.clone(),
        Slot::Cloud(_) | Slot::Local(_) => "unavailable".into(),
    })
}
