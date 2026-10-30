//! Commands the web UI may call. Every argument is deserialized into the
//! engine's own types and validated here; API keys travel only from the UI to
//! the keychain, never back.

use std::sync::Arc;

use oft_cloud::{ModelInfo, SentEntry};
use oft_engine::io::Bindings;
use oft_engine::settings::{ModelChoice, ProviderConfig, Settings, SettingsError};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::keys::{self, KeyProblem};
use crate::settings_store::SaveError;
use crate::setup::Step;
use crate::stats::{SeenApp, Usage};
use crate::{AppState, keychain, memory};

/// An error shown beside the control named by `key` (a settings path such as
/// `boldness` or `keys.take_all`), or in the page's live region.
#[derive(Debug, Serialize)]
pub struct CmdError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl CmdError {
    fn new(message: impl Into<String>) -> CmdError {
        CmdError { message: message.into(), key: None }
    }
    fn at(key: impl Into<String>, message: impl Into<String>) -> CmdError {
        CmdError { message: message.into(), key: Some(key.into()) }
    }
}

type Result<T, E = CmdError> = std::result::Result<T, E>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| CmdError::new(e.to_string()))?
}

#[derive(Debug, Serialize)]
pub struct Overview {
    pub version: &'static str,
    pub os: &'static str,
    pub settings: Settings,
    /// Why the settings file was rejected at startup, if it was.
    pub settings_error: Option<String>,
    /// Provider ids that have a key in the keychain.
    pub keys: Vec<String>,
    pub setup: Step,
    pub ram_bytes: Option<u64>,
    pub local_available: bool,
    #[cfg(desktop)]
    pub status: crate::runtime::StatusView,
}

#[tauri::command]
pub async fn overview(state: State<'_, Arc<AppState>>) -> Result<Overview> {
    let state = state.inner().clone();
    blocking(move || {
        let settings = state.settings();
        let keys = settings.providers.iter().filter(|p| keychain::has(&p.id)).map(|p| p.id.clone()).collect();
        Ok(Overview {
            version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            settings_error: state.settings_error(),
            keys,
            setup: state.setup_step(),
            ram_bytes: memory::total_bytes(),
            local_available: cfg!(feature = "local"),
            #[cfg(desktop)]
            status: state.runtime.shared.status.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            settings,
        })
    })
    .await
}

fn settings_error(err: SettingsError) -> CmdError {
    match err {
        SettingsError::Invalid { key, reason } => CmdError::at(key, reason),
        other => CmdError::new(other.to_string()),
    }
}

#[tauri::command]
pub async fn save_settings(app: AppHandle, state: State<'_, Arc<AppState>>, settings: Settings) -> Result<()> {
    if let Some(problem) = keys::check(&settings.keys).into_iter().next() {
        let slot = serde_json::to_value(problem.slot).ok().and_then(|v| v.as_str().map(str::to_owned));
        return Err(CmdError::at(format!("keys.{}", slot.unwrap_or_default()), problem.message));
    }
    let state = state.inner().clone();
    blocking(move || {
        state.store.save(&settings).map_err(|e| match e {
            SaveError::Invalid(e) => settings_error(e),
            SaveError::Io(e) => CmdError::new(format!("Could not save settings: {e}")),
        })?;
        state.replace_settings(settings.clone());
        #[cfg(desktop)]
        state.runtime.send(crate::runtime::Command::Settings(Box::new(settings)));
        crate::refresh_tray(&app, &state);
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn check_keys(keys: Bindings) -> Vec<KeyProblem> {
    keys::check(&keys)
}

fn checked_key(key: &str) -> Result<&str> {
    let key = key.trim();
    if key.is_empty() {
        return Err(CmdError::at("key", "Paste the key first."));
    }
    if key.chars().any(char::is_control) || key.len() > 4096 {
        return Err(CmdError::at("key", "This does not look like an API key."));
    }
    Ok(key)
}

/// Stores a key for a configured provider. The UI clears its copy afterwards.
#[tauri::command]
pub async fn set_api_key(state: State<'_, Arc<AppState>>, provider: String, key: String) -> Result<()> {
    let state = state.inner().clone();
    blocking(move || {
        let key = checked_key(&key)?;
        keychain::set(&provider, key).map_err(|e| CmdError::at("key", e.to_string()))?;
        #[cfg(desktop)]
        state.runtime.send(crate::runtime::Command::ModelsChanged);
        let _ = state;
        Ok(())
    })
    .await
}

/// Removes a provider, its key, and any model choice that used it.
#[tauri::command]
pub async fn remove_provider(app: AppHandle, state: State<'_, Arc<AppState>>, provider: String) -> Result<Settings> {
    let state = state.inner().clone();
    blocking(move || {
        let mut settings = state.settings();
        settings.providers.retain(|p| p.id != provider);
        for choice in [&mut settings.autocomplete_model, &mut settings.draft_model] {
            if matches!(choice, ModelChoice::Provider { provider: p } if *p == provider) {
                *choice = ModelChoice::None;
            }
        }
        keychain::delete(&provider).map_err(|e| CmdError::new(e.to_string()))?;
        state.store.save(&settings).map_err(|e| CmdError::new(e.to_string()))?;
        state.replace_settings(settings.clone());
        #[cfg(desktop)]
        state.runtime.send(crate::runtime::Command::Settings(Box::new(settings.clone())));
        crate::refresh_tray(&app, &state);
        Ok(settings)
    })
    .await
}

fn validate_provider(config: &ProviderConfig) -> Result<()> {
    if config.id.trim().is_empty() {
        return Err(CmdError::at("id", "The provider needs a name."));
    }
    if let oft_engine::settings::ProviderKind::OpenAiCompatible { base_url } = &config.kind {
        let ok = base_url.starts_with("https://") || base_url.starts_with("http://");
        if !ok {
            return Err(CmdError::at("base_url", "Enter an address that starts with https:// or http://."));
        }
    }
    Ok(())
}

/// The key to use for a test: the one just pasted, or the stored one.
fn key_for(config: &ProviderConfig, pasted: Option<String>) -> Result<Option<String>> {
    match pasted.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => checked_key(k).map(|k| Some(k.to_owned())),
        None => keychain::get(&config.id).map_err(|e| CmdError::at("key", e.to_string())),
    }
}

fn build_provider(state: &AppState, config: &ProviderConfig, key: Option<String>) -> Result<Arc<oft_cloud::Provider>> {
    validate_provider(config)?;
    let key = key_for(config, key)?;
    oft_cloud::provider(config, key, state.sent_log.clone()).map_err(|e| CmdError::at("key", e.to_string()))
}

/// Sends one small request to check the address, key and model.
#[tauri::command]
pub async fn test_provider(state: State<'_, Arc<AppState>>, config: ProviderConfig, key: Option<String>) -> Result<()> {
    let state = state.inner().clone();
    blocking(move || build_provider(&state, &config, key)?.test_connection().map_err(|e| CmdError::at("test", e.to_string())))
        .await
}

#[tauri::command]
pub async fn provider_models(
    state: State<'_, Arc<AppState>>,
    config: ProviderConfig,
    key: Option<String>,
) -> Result<Vec<ModelInfo>> {
    let state = state.inner().clone();
    blocking(move || build_provider(&state, &config, key)?.list_models().map_err(|e| CmdError::at("model", e.to_string())))
        .await
}

#[tauri::command]
pub fn sent_log(state: State<'_, Arc<AppState>>) -> Vec<SentEntry> {
    state.sent_log.entries()
}

#[tauri::command]
pub fn clear_sent_log(state: State<'_, Arc<AppState>>) {
    state.sent_log.clear();
}

#[tauri::command]
pub async fn usage(state: State<'_, Arc<AppState>>, days: u32) -> Result<Usage> {
    let state = state.inner().clone();
    blocking(move || {
        let stats = state.stats.as_ref().ok_or_else(|| CmdError::new("Usage stats are unavailable."))?;
        stats.usage(days.clamp(1, 366)).map_err(|e| CmdError::new(e.to_string()))
    })
    .await
}

#[tauri::command]
pub fn clear_usage(state: State<'_, Arc<AppState>>) -> Result<()> {
    let stats = state.stats.as_ref().ok_or_else(|| CmdError::new("Usage stats are unavailable."))?;
    stats.clear();
    Ok(())
}

#[tauri::command]
pub async fn seen_apps(state: State<'_, Arc<AppState>>) -> Result<Vec<SeenApp>> {
    let state = state.inner().clone();
    blocking(move || match &state.stats {
        Some(stats) => stats.apps().map_err(|e| CmdError::new(e.to_string())),
        None => Ok(Vec::new()),
    })
    .await
}

#[tauri::command]
pub async fn style_samples(state: State<'_, Arc<AppState>>) -> Result<Vec<crate::style::Sample>> {
    let state = state.inner().clone();
    blocking(move || match &state.style {
        Some(style) => style.all().map_err(|e| CmdError::new(e.to_string())),
        None => Ok(Vec::new()),
    })
    .await
}

/// Deletes one remembered message (`id`), one app's (`app`), or all.
#[tauri::command]
pub fn delete_style(state: State<'_, Arc<AppState>>, id: Option<i64>, app: Option<String>) -> Result<()> {
    let style = state.style.as_ref().ok_or_else(|| CmdError::new("Style memory is unavailable."))?;
    style.delete(id, app);
    Ok(())
}

#[tauri::command]
pub async fn set_setup_step(state: State<'_, Arc<AppState>>, step: Step) -> Result<()> {
    let state = state.inner().clone();
    blocking(move || state.set_setup_step(step).map_err(|e| CmdError::new(format!("Could not save progress: {e}")))).await
}

#[cfg(desktop)]
#[tauri::command]
pub fn pause(state: State<'_, Arc<AppState>>, scope: crate::pause::Scope, length: crate::pause::PauseFor) {
    state.runtime.send(crate::runtime::Command::Pause(scope, length));
}

#[cfg(desktop)]
#[tauri::command]
pub fn resume(state: State<'_, Arc<AppState>>, app: Option<String>) {
    state.runtime.send(crate::runtime::Command::Resume(app));
}

#[cfg(desktop)]
#[tauri::command]
pub fn timings(state: State<'_, Arc<AppState>>) -> Vec<crate::runtime::Timing> {
    state.runtime.shared.timings.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect()
}

/// Emits progress events while a model downloads; see `local_models`.
pub fn emit<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    if let Err(e) = app.emit(event, payload) {
        log::warn!("could not emit {event}: {e}");
    }
}

#[cfg(feature = "local")]
pub mod local_models {
    //! The curated model list, downloads and deletion.

    use std::sync::Arc;
    use std::thread;

    use oft_engine::complete::CancelToken;
    use oft_engine::settings::ModelChoice;
    use oft_local::store::{Manifest, ModelEntry, Progress, Tier};
    use serde::Serialize;
    use tauri::{AppHandle, State};

    use super::{CmdError, Result, blocking, emit};
    use crate::{AppState, memory};

    const GB: u64 = 1 << 30;

    #[derive(Debug, Serialize)]
    pub struct ModelView {
        pub id: &'static str,
        pub name: &'static str,
        pub license: &'static str,
        pub size: u64,
        pub min_ram_gb: u32,
        pub tier: &'static str,
        pub installed: bool,
        pub recommended: bool,
        /// Settings refer to it for autocomplete or reply & rewrite.
        pub in_use: bool,
    }

    #[derive(Debug, Serialize)]
    pub struct ModelsView {
        pub models: Vec<ModelView>,
        pub ram_bytes: Option<u64>,
        pub downloading: Option<String>,
        pub dir: String,
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct DownloadProgress {
        pub id: &'static str,
        pub downloaded: u64,
        pub total: u64,
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct DownloadDone {
        pub id: &'static str,
        pub error: Option<String>,
        pub cancelled: bool,
    }

    /// The model setup offers: the standard model with 16 GB or more, the
    /// small one below that.
    pub fn recommended(ram: Option<u64>) -> Option<&'static ModelEntry> {
        let entries = Manifest::builtin().entries();
        let ram = ram.unwrap_or(0);
        let tier = if ram >= 16 * GB { Tier::Standard } else { Tier::Small };
        entries
            .iter()
            .find(|e| e.tier == tier && u64::from(e.min_ram_gb) * GB <= ram)
            .or_else(|| entries.iter().find(|e| e.tier == Tier::Small))
    }

    fn tier_name(t: Tier) -> &'static str {
        match t {
            Tier::Small => "small",
            Tier::Standard => "standard",
            Tier::Comparison => "comparison",
        }
    }

    fn entry(id: &str) -> Result<&'static ModelEntry> {
        Manifest::builtin().get(id).ok_or_else(|| CmdError::new("Unknown model."))
    }

    #[tauri::command]
    pub async fn local_models(state: State<'_, Arc<AppState>>) -> Result<ModelsView> {
        let state = state.inner().clone();
        blocking(move || {
            let ram = memory::total_bytes();
            let rec = recommended(ram).map(|e| e.id);
            let settings = state.settings();
            let uses = |id: &str| {
                [&settings.autocomplete_model, &settings.draft_model]
                    .iter()
                    .any(|c| matches!(c, ModelChoice::Local { model } if model == id))
            };
            let models = Manifest::builtin()
                .entries()
                .iter()
                .map(|e| ModelView {
                    id: e.id,
                    name: e.name,
                    license: e.license,
                    size: e.size,
                    min_ram_gb: e.min_ram_gb,
                    tier: tier_name(e.tier),
                    installed: state.models.is_installed(e),
                    recommended: rec == Some(e.id),
                    in_use: uses(e.id),
                })
                .collect();
            let downloading = state.download.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(id, _)| id.clone());
            Ok(ModelsView { models, ram_bytes: ram, downloading, dir: state.models.dir().display().to_string() })
        })
        .await
    }

    /// Starts a resumable, verified download. Progress arrives as
    /// `model-progress` events and the end as one `model-done` event.
    #[tauri::command]
    pub fn download_model(app: AppHandle, state: State<'_, Arc<AppState>>, id: String) -> Result<()> {
        let entry = entry(&id)?;
        let cancel = CancelToken::new();
        {
            let mut slot = state.download.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((other, _)) = slot.as_ref() {
                return Err(CmdError::new(format!("Another download ({other}) is running.")));
            }
            *slot = Some((id, cancel.clone()));
        }
        let state = state.inner().clone();
        thread::Builder::new()
            .name("oft-download".into())
            .spawn(move || {
                let mut last = 0u64;
                let mut on_progress = |p: Progress| {
                    // About 200 updates per download, whatever its size.
                    if p.downloaded == p.total || p.downloaded.saturating_sub(last) >= p.total / 200 {
                        last = p.downloaded;
                        emit(&app, "model-progress", DownloadProgress { id: entry.id, downloaded: p.downloaded, total: p.total });
                        #[cfg(desktop)]
                        if p.total > 0 {
                            let f = p.downloaded as f32 / p.total as f32;
                            state.runtime.send(crate::runtime::Command::Download(Some(f)));
                        }
                    }
                };
                let result = state.models.download(entry, &mut on_progress, &cancel);
                *state.download.lock().unwrap_or_else(|e| e.into_inner()) = None;
                #[cfg(desktop)]
                state.runtime.send(crate::runtime::Command::Download(None));
                let cancelled = cancel.is_cancelled();
                let error = result.err().filter(|_| !cancelled).map(|e| e.to_string());
                emit(&app, "model-done", DownloadDone { id: entry.id, error, cancelled });
            })
            .map_err(|e| CmdError::new(format!("Could not start the download: {e}")))?;
        Ok(())
    }

    /// Stops the running download; the partial file is kept for resuming.
    #[tauri::command]
    pub fn cancel_download(state: State<'_, Arc<AppState>>) {
        if let Some((_, cancel)) = state.download.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            cancel.cancel();
        }
    }

    /// Deletes a downloaded model. Settings that used it fall back to no model.
    #[tauri::command]
    pub async fn delete_model(app: AppHandle, state: State<'_, Arc<AppState>>, id: String) -> Result<()> {
        let entry = entry(&id)?;
        let state = state.inner().clone();
        blocking(move || {
            let mut settings = state.settings();
            let mut changed = false;
            for choice in [&mut settings.autocomplete_model, &mut settings.draft_model] {
                if matches!(choice, ModelChoice::Local { model } if model == entry.id) {
                    *choice = ModelChoice::None;
                    changed = true;
                }
            }
            if changed {
                state.store.save(&settings).map_err(|e| CmdError::new(e.to_string()))?;
                state.replace_settings(settings.clone());
                #[cfg(desktop)]
                state.runtime.send(crate::runtime::Command::Settings(Box::new(settings)));
            }
            state.models.delete(entry).map_err(|e| CmdError::new(format!("Could not delete the model: {e}")))?;
            #[cfg(desktop)]
            state.runtime.send(crate::runtime::Command::ModelsChanged);
            crate::refresh_tray(&app, &state);
            Ok(())
        })
        .await
    }

    /// Checks that `path` is a GGUF file before settings point at it.
    #[tauri::command]
    pub async fn check_model_file(path: String) -> Result<u64> {
        blocking(move || {
            use std::io::Read;
            let mut file = std::fs::File::open(&path).map_err(|e| CmdError::at("path", format!("Cannot open the file: {}", e.kind())))?;
            let mut magic = [0u8; 4];
            file.read_exact(&mut magic).map_err(|_| CmdError::at("path", "The file is too short to be a model."))?;
            if &magic != b"GGUF" {
                return Err(CmdError::at("path", "This is not a GGUF model file."));
            }
            file.metadata().map(|m| m.len()).map_err(|e| CmdError::at("path", e.to_string()))
        })
        .await
    }
}
