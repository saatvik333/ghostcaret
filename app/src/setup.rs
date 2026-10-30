//! First-run progress, kept so setup resumes at the step the user left.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    #[default]
    Welcome,
    Permissions,
    Model,
    Practice,
    Done,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct File {
    step: Step,
}

#[derive(Debug)]
pub struct SetupStore {
    path: PathBuf,
}

impl SetupStore {
    pub fn new(config_dir: &Path) -> SetupStore {
        SetupStore {
            path: config_dir.join("setup.toml"),
        }
    }

    /// The saved step; a missing or unreadable file starts setup from the top.
    pub fn load(&self) -> Step {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| toml::from_str::<File>(&t).ok())
            .map(|f| f.step)
            .unwrap_or_default()
    }

    pub fn save(&self, step: Step) -> std::io::Result<()> {
        let text = toml::to_string(&File { step }).expect("setup state serializes");
        crate::settings_store::write_atomic(&self.path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_at_saved_step() {
        let dir = std::env::temp_dir().join(format!("ghostcaret-setup-{}", std::process::id()));
        let store = SetupStore::new(&dir);
        assert_eq!(store.load(), Step::Welcome);
        store.save(Step::Practice).unwrap();
        assert_eq!(SetupStore::new(&dir).load(), Step::Practice);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
