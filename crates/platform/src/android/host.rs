//! What the runtime asks of the keyboard: the settings file the app writes,
//! API keys from the Android Keystore, and the strip's status line.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use oft_engine::settings::Settings;
use oft_runtime::StatusView;

use super::bridge::{Bridge, StripStatus};
use super::settings_file;

pub(crate) struct KeyboardHost {
    bridge: Arc<Bridge>,
    config_dir: PathBuf,
    /// The line the strip shows; most status changes leave it as it is.
    shown: Mutex<Option<StripStatus>>,
}

impl KeyboardHost {
    pub(crate) fn new(bridge: Arc<Bridge>, config_dir: PathBuf) -> KeyboardHost {
        KeyboardHost {
            bridge,
            config_dir,
            shown: Mutex::new(None),
        }
    }
}

impl oft_runtime::Host for KeyboardHost {
    fn settings(&self) -> Result<Settings, String> {
        settings_file::load(&self.config_dir)
    }

    fn api_key(&self, provider: &str) -> Result<Option<String>, String> {
        self.bridge.api_key(provider)
    }

    fn status(&self, status: &StatusView) {
        let line = StripStatus::of(status);
        let mut shown = self.shown.lock().unwrap_or_else(|e| e.into_inner());
        if shown.as_ref() != Some(&line) {
            self.bridge.status(&line);
            *shown = Some(line);
        }
    }
}
