//! Loading and saving `settings.toml`.
//!
//! Saves are atomic: the new file is written beside the old one, flushed to
//! disk and renamed over it, so a crash never leaves a half-written file. A
//! file that fails to load is kept as `settings.toml.invalid`, the app starts
//! with defaults, and the error names the setting but never its value.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use oft_engine::settings::{Settings, SettingsError};

#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

/// What loading produced: the settings to run with and, when the file was
/// rejected, a message safe to show anywhere.
#[derive(Debug)]
pub struct Loaded {
    pub settings: Settings,
    pub error: Option<String>,
}

impl SettingsStore {
    pub fn new(config_dir: &Path) -> SettingsStore {
        SettingsStore {
            path: config_dir.join("settings.toml"),
        }
    }

    pub fn load(&self) -> Loaded {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Loaded {
                    settings: Settings::default(),
                    error: None,
                };
            }
            Err(e) => {
                return Loaded {
                    settings: Settings::default(),
                    error: Some(format!(
                        "Could not read settings ({}); using defaults.",
                        e.kind()
                    )),
                };
            }
        };
        match Settings::from_toml(&text) {
            Ok(settings) => Loaded {
                settings,
                error: None,
            },
            Err(err) => {
                let kept = self.path.with_extension("toml.invalid");
                let note = match fs::rename(&self.path, &kept) {
                    Ok(()) => "your file was kept as settings.toml.invalid".to_owned(),
                    Err(e) => format!("your file could not be set aside ({})", e.kind()),
                };
                Loaded {
                    settings: Settings::default(),
                    error: Some(format!(
                        "{}. Using default settings; {note}.",
                        capitalized(&err.to_string())
                    )),
                }
            }
        }
    }

    /// Validates and writes `settings` atomically.
    pub fn save(&self, settings: &Settings) -> Result<(), SaveError> {
        settings.validate().map_err(SaveError::Invalid)?;
        write_atomic(&self.path, settings.to_toml().as_bytes()).map_err(SaveError::Io)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Invalid(SettingsError),
    #[error("could not write settings: {0}")]
    Io(io::Error),
}

fn capitalized(text: &str) -> String {
    let mut s = text.to_owned();
    if let Some(first) = s.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    s
}

/// Writes `bytes` to `path` so readers see either the old or the new file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        sync_dir(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

// Windows commits a rename through `MoveFileEx` without a directory handle.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oft-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_gives_defaults_without_error() {
        let dir = scratch("missing");
        let loaded = SettingsStore::new(&dir).load();
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.error.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = scratch("roundtrip");
        let store = SettingsStore::new(&dir);
        let settings = Settings {
            boldness: 0.8,
            pause_ms: 120,
            ..Settings::default()
        };
        store.save(&settings).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.settings, settings);
        assert!(loaded.error.is_none());
        assert!(!dir.join("settings.toml.tmp").exists());
    }

    #[test]
    fn save_refuses_invalid_settings_and_keeps_old_file() {
        let dir = scratch("refuse");
        let store = SettingsStore::new(&dir);
        store.save(&Settings::default()).unwrap();
        let bad = Settings {
            boldness: 2.0,
            ..Settings::default()
        };
        let err = store.save(&bad).unwrap_err();
        assert!(matches!(
            err,
            SaveError::Invalid(SettingsError::Invalid {
                key: "boldness",
                ..
            })
        ));
        assert_eq!(store.load().settings, Settings::default());
    }

    #[test]
    fn invalid_file_is_kept_aside_and_error_names_key_not_value() {
        let dir = scratch("invalid");
        // Assembled at run time so the source holds nothing token-shaped.
        let secret = ["sk", "-ant-", "test", "-not-a-key"].concat();
        fs::write(
            dir.join("settings.toml"),
            format!("pause_ms = 40\n\n[context]\nabout_me = 7\nclipboard = \"{secret}\"\n"),
        )
        .unwrap();
        let loaded = SettingsStore::new(&dir).load();
        assert_eq!(loaded.settings, Settings::default());
        let error = loaded.error.unwrap();
        assert!(error.contains("context.about_me"), "{error}");
        assert!(!error.contains(&secret));
        assert!(!error.contains('7'));
        assert!(dir.join("settings.toml.invalid").exists());
        assert!(!dir.join("settings.toml").exists());
    }

    #[test]
    fn value_in_wrong_place_is_not_echoed() {
        let dir = scratch("echo");
        let secret = ["sk", "-proj-", "test", "-not-a-key"].concat();
        fs::write(
            dir.join("settings.toml"),
            format!("boldness = \"{secret}\"\n"),
        )
        .unwrap();
        let error = SettingsStore::new(&dir).load().error.unwrap();
        assert!(error.contains("`boldness`"), "{error}");
        assert!(!error.contains(&secret), "{error}");
    }

    #[test]
    fn validation_error_names_key() {
        let dir = scratch("validate");
        fs::write(dir.join("settings.toml"), "boldness = 4.0\n").unwrap();
        let error = SettingsStore::new(&dir).load().error.unwrap();
        assert!(error.contains("`boldness`"), "{error}");
        assert!(!error.contains('4'), "{error}");
    }
}
