//! Ghostcaret: the Tauri app that hosts the engine.
//!
//! On desktop it lives in the tray, runs the OS backend and the engine's
//! runtime ([`oft_runtime`], with this app as its [`host`]), and opens the
//! settings window on demand; the window and its web view are destroyed when
//! closed. On mobile the keyboard hosts the engine, and this app is the
//! settings and setup UI.

mod commands;
#[cfg(desktop)]
mod host;
mod keychain;
mod keys;
mod memory;
mod paths;
mod settings_store;
mod setup;
#[cfg(desktop)]
mod tray;

use std::sync::{Arc, Mutex};

use oft_cloud::SentLog;
use oft_engine::settings::Settings;
use oft_store::stats::Stats;
use oft_store::style::StyleMemory;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

use settings_store::SettingsStore;
use setup::{SetupStore, Step};

/// State shared by the commands, the tray and the runtime handle.
pub struct AppState {
    /// The saved settings; the runtime reads them through its host.
    settings: Arc<Mutex<Settings>>,
    settings_error: Mutex<Option<String>>,
    store: SettingsStore,
    setup: SetupStore,
    setup_step: Mutex<Step>,
    stats: Option<Stats>,
    style: Option<StyleMemory>,
    sent_log: SentLog,
    #[cfg(desktop)]
    runtime: oft_runtime::Runtime,
    #[cfg(feature = "local")]
    models: oft_local::store::ModelStore,
    #[cfg(feature = "local")]
    download: Mutex<Option<(String, oft_engine::complete::CancelToken)>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AppState {
    fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    /// Installs newly saved settings; a valid save also clears the startup
    /// error about a rejected file.
    fn replace_settings(&self, settings: Settings) {
        *lock(&self.settings) = settings;
        *lock(&self.settings_error) = None;
    }

    fn settings_error(&self) -> Option<String> {
        lock(&self.settings_error).clone()
    }

    fn setup_step(&self) -> Step {
        *lock(&self.setup_step)
    }

    fn set_setup_step(&self, step: Step) -> std::io::Result<()> {
        self.setup.save(step)?;
        *lock(&self.setup_step) = step;
        Ok(())
    }
}

/// Shows the settings window, creating it if it was closed.
pub fn open_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()));
    #[cfg(desktop)]
    let builder = builder.title("Ghostcaret").inner_size(920.0, 680.0).min_inner_size(600.0, 460.0);
    if let Err(e) = builder.build() {
        log::error!("could not open the settings window: {e}");
    }
}

/// Redraws the tray from the current status and settings error.
#[cfg(desktop)]
fn refresh_tray(app: &AppHandle, state: &AppState) {
    let status = state.runtime.status();
    let error = state.settings_error();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || tray::update(&handle, &status, error.as_deref()));
}

#[cfg(mobile)]
fn refresh_tray(_app: &AppHandle, _state: &AppState) {}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(mobile)]
    let dirs = Some((app.path().app_config_dir()?, app.path().app_data_dir()?));
    #[cfg(desktop)]
    let dirs = None;
    if !paths::init(dirs) {
        return Err("the OS reports no home directory".into());
    }
    let store = SettingsStore::new(paths::config_dir());
    let loaded = store.load();
    if let Some(e) = &loaded.error {
        log::warn!("{e}");
    }
    let stats = Stats::open(paths::data_dir()).map_err(|e| log::warn!("usage stats unavailable: {e}")).ok();
    let style = StyleMemory::open(paths::data_dir()).map_err(|e| log::warn!("style memory unavailable: {e}")).ok();
    let sent_log = SentLog::default();
    let setup = SetupStore::new(paths::config_dir());
    let step = setup.load();
    let settings = Arc::new(Mutex::new(loaded.settings));

    #[cfg(desktop)]
    let runtime = {
        let host = Arc::new(host::DesktopHost::new(app.handle().clone(), settings.clone()));
        let config = oft_runtime::Config {
            stats: stats.clone(),
            style: style.clone(),
            sent_log: sent_log.clone(),
            #[cfg(feature = "local")]
            models_dir: paths::models_dir(),
            #[cfg(feature = "local")]
            load_options: oft_local::LoadOptions::default(),
        };
        let runtime = oft_runtime::start(host, config, |events| oft_platform::start(events).map_err(|e| e.to_string()))?;
        let commands = runtime.commands();
        memory::watch_pressure(move || {
            let _ = commands.send(oft_runtime::Command::MemoryPressure);
        });
        runtime
    };

    let state = Arc::new(AppState {
        settings,
        settings_error: Mutex::new(loaded.error),
        store,
        setup,
        setup_step: Mutex::new(step),
        stats,
        style,
        sent_log,
        #[cfg(desktop)]
        runtime,
        #[cfg(feature = "local")]
        models: oft_local::store::ModelStore::new(paths::models_dir()),
        #[cfg(feature = "local")]
        download: Mutex::new(None),
    });
    app.manage(state.clone());

    #[cfg(desktop)]
    {
        let status = state.runtime.status();
        tray::create(app.handle(), &status, state.settings_error().as_deref())?;
        if step != Step::Done || state.settings_error().is_some() {
            open_main(app.handle());
        }
    }
    #[cfg(mobile)]
    open_main(app.handle());
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| open_main(app)));
    let app = builder
        .setup(setup)
        .invoke_handler(tauri::generate_handler![
            commands::overview,
            commands::save_settings,
            commands::check_keys,
            commands::set_api_key,
            commands::remove_provider,
            commands::test_provider,
            commands::provider_models,
            commands::sent_log,
            commands::clear_sent_log,
            commands::usage,
            commands::clear_usage,
            commands::seen_apps,
            commands::style_samples,
            commands::delete_style,
            commands::set_setup_step,
            #[cfg(desktop)]
            commands::pause,
            #[cfg(desktop)]
            commands::resume,
            #[cfg(desktop)]
            commands::timings,
            #[cfg(feature = "local")]
            commands::local_models::local_models,
            #[cfg(feature = "local")]
            commands::local_models::download_model,
            #[cfg(feature = "local")]
            commands::local_models::cancel_download,
            #[cfg(feature = "local")]
            commands::local_models::delete_model,
            #[cfg(feature = "local")]
            commands::local_models::check_model_file,
        ])
        .build(tauri::generate_context!())
        .expect("the Tauri app builds");
    app.run(|app, event| match event {
        // Closing the settings window leaves the app running in the tray.
        #[cfg(desktop)]
        RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
        #[cfg(desktop)]
        RunEvent::Exit => {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                state.runtime.send(oft_runtime::Command::Quit);
            }
        }
        _ => {
            let _ = app;
        }
    });
}
