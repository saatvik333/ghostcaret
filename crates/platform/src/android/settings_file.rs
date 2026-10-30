//! The settings file in the keyboard's config directory, shared with the app
//! that writes it.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use oft_engine::settings::Settings;

pub(crate) fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.toml")
}

/// Reads the settings; a missing file means defaults. The error names what is
/// wrong without quoting the file.
pub(crate) fn load(config_dir: &Path) -> Result<Settings, String> {
    match fs::read_to_string(path(config_dir)) {
        Ok(text) => Settings::from_toml(&text).map_err(|e| e.to_string()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("cannot read settings: {e}")),
    }
}

/// Replaces the settings file atomically: readers see the old or the new
/// file, never a partial one, even across a crash.
pub(crate) fn save(config_dir: &Path, settings: &Settings) -> io::Result<()> {
    fs::create_dir_all(config_dir)?;
    let target = path(config_dir);
    let temp = config_dir.join("settings.toml.tmp");
    let mut file = File::create(&temp)?;
    file.write_all(settings.to_toml().as_bytes())?;
    file.sync_all()?;
    fs::rename(&temp, &target)?;
    File::open(config_dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use oft_engine::settings::ModelChoice;

    use super::*;

    #[test]
    fn saved_settings_load_back_and_leave_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("oft-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(load(&dir), Ok(Settings::default()));
        let settings = Settings {
            autocomplete_model: ModelChoice::Local { model: "/models/a.gguf".into() },
            ..Settings::default()
        };
        save(&dir, &settings).unwrap();
        assert_eq!(load(&dir), Ok(settings));
        assert!(!dir.join("settings.toml.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_settings_are_an_error_not_defaults() {
        let dir = std::env::temp_dir().join(format!("oft-settings-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(path(&dir), "boldness = 7.0\n").unwrap();
        assert!(load(&dir).unwrap_err().contains("boldness"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
