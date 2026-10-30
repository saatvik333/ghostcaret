//! Where Ghostcaret keeps its files: `ProjectDirs::from("", "", "ghostcaret")`
//! on desktop, the app's sandbox directories on mobile.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug)]
struct Dirs {
    config: PathBuf,
    data: PathBuf,
}

static DIRS: OnceLock<Dirs> = OnceLock::new();

/// Sets the directories once at startup. Returns false if the OS reports none.
pub fn init(mobile: Option<(PathBuf, PathBuf)>) -> bool {
    let dirs = match mobile {
        Some((config, data)) => Some(Dirs { config, data }),
        None => directories::ProjectDirs::from("", "", "ghostcaret")
            .map(|d| Dirs { config: d.config_dir().to_path_buf(), data: d.data_dir().to_path_buf() }),
    };
    match dirs {
        Some(d) => DIRS.set(d).is_ok() || DIRS.get().is_some(),
        None => false,
    }
}

fn dirs() -> &'static Dirs {
    DIRS.get().expect("paths::init runs before anything touches files")
}

/// `settings.toml` and the setup progress.
pub fn config_dir() -> &'static Path {
    &dirs().config
}

/// Usage stats.
pub fn data_dir() -> &'static Path {
    &dirs().data
}

/// Downloaded models.
#[cfg(feature = "local")]
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}
