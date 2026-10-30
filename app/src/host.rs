//! What the runtime asks of the desktop app: the saved settings, API keys
//! from the OS keychain, and status updates for the tray and the settings
//! window.

use std::sync::{Arc, Mutex};

use ghostcaret_engine::settings::Settings;
use ghostcaret_runtime::StatusView;
use tauri::{AppHandle, Manager};

use crate::{AppState, commands, keychain, lock, refresh_tray};

pub struct DesktopHost {
    app: AppHandle,
    /// Shared with [`AppState`], which replaces them when settings are saved.
    settings: Arc<Mutex<Settings>>,
}

impl DesktopHost {
    pub fn new(app: AppHandle, settings: Arc<Mutex<Settings>>) -> DesktopHost {
        DesktopHost { app, settings }
    }
}

impl ghostcaret_runtime::Host for DesktopHost {
    /// The app validates settings before saving them, and reports a rejected
    /// file itself while running on defaults, so this never fails.
    fn settings(&self) -> Result<Settings, String> {
        Ok(lock(&self.settings).clone())
    }

    fn api_key(&self, provider: &str) -> Result<Option<String>, String> {
        keychain::get(provider).map_err(|e| e.to_string())
    }

    fn status(&self, status: &StatusView) {
        commands::emit(&self.app, "status", status.clone());
        if let Some(state) = self.app.try_state::<Arc<AppState>>() {
            refresh_tray(&self.app, &state);
        }
    }
}
