//! The models serving autocomplete and Reply & rewrite, as the settings choose
//! them. Loading runs on a loader thread; results come back to the runtime as
//! [`Msg::Loaded`].
//!
//! One local model stays loaded at a time. It is loaded when a text field
//! first gets focus, and dropped on memory pressure or after the idle time in
//! the settings; the next focus loads it again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use oft_cloud::{Provider, SentLog};
use oft_engine::complete::{Caps, Completer, Generator};
use oft_engine::controller::Feature;
use oft_engine::settings::{ModelChoice, ProviderConfig, Settings};
use oft_local::store::ModelStore;
use oft_local::{LoadOptions, LocalModel};

use super::bridge::Bridge;
use super::runtime::Msg;

/// Context and threads for a phone: the curated small models fit 2048
/// tokens of context, and four threads keep decoding on the big cores.
fn load_options() -> LoadOptions {
    LoadOptions { n_ctx: 2048, threads: 4, ..LoadOptions::default() }
}

pub(crate) enum Loaded {
    Local { path: PathBuf, result: Result<LocalModel, String> },
    Cloud { config: ProviderConfig, result: Result<Arc<Provider>, String> },
}

enum Job {
    Local(PathBuf),
    Cloud(ProviderConfig),
    Warm(Arc<Provider>),
}

enum State<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// A model serving a feature.
#[derive(Clone)]
pub(crate) enum Served {
    Local(Arc<LocalModel>),
    Cloud(Arc<Provider>),
}

impl Served {
    pub(crate) fn completer(&self) -> Arc<dyn Completer> {
        match self {
            Served::Local(m) => m.clone(),
            Served::Cloud(p) => p.clone(),
        }
    }

    pub(crate) fn generator(&self) -> Arc<dyn Generator> {
        match self {
            Served::Local(m) => m.clone(),
            Served::Cloud(p) => p.clone(),
        }
    }

    fn caps(&self) -> Caps {
        match self {
            Served::Local(m) => m.caps(),
            Served::Cloud(p) => p.caps(),
        }
    }
}

pub(crate) struct Models {
    jobs: Sender<Job>,
    store: ModelStore,
    autocomplete: ModelChoice,
    draft: ModelChoice,
    providers: Vec<ProviderConfig>,
    local: Option<(PathBuf, State<Arc<LocalModel>>)>,
    cloud: HashMap<String, (ProviderConfig, State<Arc<Provider>>)>,
}

impl Models {
    pub(crate) fn start(models_dir: PathBuf, bridge: Arc<Bridge>, out: Sender<Msg>) -> std::io::Result<Models> {
        let (jobs, rx) = crossbeam_channel::bounded(8);
        let sent = SentLog::default();
        thread::Builder::new().name("oft-loader".into()).spawn(move || load(rx, out, bridge, sent))?;
        Ok(Models {
            jobs,
            store: ModelStore::new(models_dir),
            autocomplete: ModelChoice::None,
            draft: ModelChoice::None,
            providers: Vec::new(),
            local: None,
            cloud: HashMap::new(),
        })
    }

    /// Applies the model choices in `settings`. Models no longer chosen are
    /// dropped; nothing new loads until [`Models::prepare`].
    pub(crate) fn configure(&mut self, settings: &Settings) {
        self.autocomplete = settings.autocomplete_model.clone();
        self.draft = settings.draft_model.clone();
        self.providers = settings.providers.clone();
        let wanted_local = self.local_path();
        if self.local.as_ref().is_some_and(|(path, _)| Some(path) != wanted_local.as_ref()) {
            self.unload_local();
        }
        let wanted: Vec<ProviderConfig> = self.chosen_providers().cloned().collect();
        self.cloud.retain(|id, (config, _)| wanted.iter().any(|w| &w.id == id && w == config));
    }

    /// Starts loading whatever the settings choose that is not loaded yet, and
    /// opens the connections to chosen cloud providers. Called when a text
    /// field gets focus. A model that failed is retried after the settings
    /// change.
    pub(crate) fn prepare(&mut self) {
        if self.local.is_none()
            && let Some(path) = self.local_path()
            && self.jobs.try_send(Job::Local(path.clone())).is_ok()
        {
            self.local = Some((path, State::Loading));
        }
        let configs: Vec<ProviderConfig> = self.chosen_providers().cloned().collect();
        for config in configs {
            match self.cloud.get(&config.id) {
                None => {
                    if self.jobs.try_send(Job::Cloud(config.clone())).is_ok() {
                        self.cloud.insert(config.id.clone(), (config, State::Loading));
                    }
                }
                Some((_, State::Ready(provider))) => {
                    let _ = self.jobs.try_send(Job::Warm(provider.clone()));
                }
                Some(_) => {}
            }
        }
    }

    pub(crate) fn loaded(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::Local { path, result } => {
                if let Some((wanted, state)) = &mut self.local
                    && *wanted == path
                {
                    *state = match result {
                        Ok(model) => State::Ready(Arc::new(model)),
                        Err(why) => State::Failed(why),
                    };
                }
            }
            Loaded::Cloud { config, result } => {
                if let Some((wanted, state)) = self.cloud.get_mut(&config.id)
                    && *wanted == config
                {
                    *state = match result {
                        Ok(provider) => State::Ready(provider),
                        Err(why) => State::Failed(why),
                    };
                }
            }
        }
    }

    /// Frees the local model's memory at once, even while a worker still
    /// holds it; that request then ends as unavailable.
    pub(crate) fn unload_local(&mut self) {
        if let Some((_, State::Ready(model))) = self.local.take() {
            model.unload();
        }
    }

    pub(crate) fn has_local(&self) -> bool {
        matches!(self.local, Some((_, State::Ready(_))))
    }

    pub(crate) fn served(&self, feature: Feature) -> Option<Served> {
        match self.choice(feature) {
            ModelChoice::None => None,
            ModelChoice::Local { .. } => match &self.local {
                Some((path, State::Ready(model))) if Some(path) == self.resolve(feature).as_ref() => {
                    Some(Served::Local(model.clone()))
                }
                _ => None,
            },
            ModelChoice::Provider { provider } => match self.cloud.get(provider) {
                Some((_, State::Ready(p))) => Some(Served::Cloud(p.clone())),
                _ => None,
            },
        }
    }

    pub(crate) fn caps(&self, feature: Feature) -> Option<Caps> {
        self.served(feature).map(|s| s.caps())
    }

    pub(crate) fn is_loading(&self) -> bool {
        matches!(self.local, Some((_, State::Loading))) || self.cloud.values().any(|(_, s)| matches!(s, State::Loading))
    }

    /// Why a chosen model is unavailable, if one failed.
    pub(crate) fn failure(&self) -> Option<String> {
        if let Some(model) = self.unresolved_local() {
            return Some(format!("model {model} is not installed"));
        }
        let local = self.local.iter().map(|(_, s)| s);
        let cloud = self.cloud.values().map(|(_, s)| s);
        let mut failed = local.map(failed_reason).chain(cloud.map(failed_reason)).flatten();
        failed.next().map(str::to_owned)
    }

    fn choice(&self, feature: Feature) -> &ModelChoice {
        match feature {
            Feature::Autocomplete => &self.autocomplete,
            Feature::FlowDraft => &self.draft,
        }
    }

    fn resolve(&self, feature: Feature) -> Option<PathBuf> {
        match self.choice(feature) {
            ModelChoice::Local { model } => self.store.resolve(model),
            _ => None,
        }
    }

    /// The one local model to keep loaded: autocomplete's, else Reply & rewrite's.
    fn local_path(&self) -> Option<PathBuf> {
        self.resolve(Feature::Autocomplete).or_else(|| self.resolve(Feature::FlowDraft))
    }

    fn unresolved_local(&self) -> Option<&str> {
        [&self.autocomplete, &self.draft].into_iter().find_map(|choice| match choice {
            ModelChoice::Local { model } if self.store.resolve(model).is_none() => Some(model.as_str()),
            _ => None,
        })
    }

    fn chosen_providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers.iter().filter(|p| {
            [&self.autocomplete, &self.draft]
                .into_iter()
                .any(|c| matches!(c, ModelChoice::Provider { provider } if *provider == p.id))
        })
    }
}

fn failed_reason<T>(state: &State<T>) -> Option<&str> {
    match state {
        State::Failed(why) => Some(why),
        _ => None,
    }
}

fn load(jobs: Receiver<Job>, out: Sender<Msg>, bridge: Arc<Bridge>, sent: SentLog) {
    for job in jobs {
        let loaded = match job {
            Job::Local(path) => {
                let started = std::time::Instant::now();
                let result = LocalModel::load(&path, load_options()).map_err(|e| e.to_string());
                log::info!("local model load took {:?}: {}", started.elapsed(), if result.is_ok() { "ok" } else { "failed" });
                Loaded::Local { path, result }
            }
            Job::Cloud(config) => {
                let key = bridge.api_key(&config.id);
                let result = oft_cloud::provider(&config, key, sent.clone()).map_err(|e| e.to_string());
                Loaded::Cloud { config, result }
            }
            Job::Warm(provider) => {
                provider.warm();
                continue;
            }
        };
        if out.send(Msg::Loaded(loaded)).is_err() {
            return;
        }
    }
}
