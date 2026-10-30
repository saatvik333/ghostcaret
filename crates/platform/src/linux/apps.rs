//! Human-readable app names from desktop entries.

use std::path::PathBuf;

/// The `Name` of the desktop entry whose file name is the app id (the
/// Wayland convention), or the app id itself.
pub(crate) fn name(app_id: &str) -> String {
    if app_id.is_empty() || app_id.contains('/') {
        return app_id.to_owned();
    }
    let file = format!("{app_id}.desktop");
    data_dirs()
        .into_iter()
        .find_map(|dir| std::fs::read_to_string(dir.join("applications").join(&file)).ok())
        .and_then(|entry| entry_name(&entry).map(str::to_owned))
        .unwrap_or_else(|| app_id.to_owned())
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`, with the specification's defaults.
fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from))
        .collect()
}

/// The untranslated `Name` key of the `[Desktop Entry]` group.
fn entry_name(entry: &str) -> Option<&str> {
    let mut in_main = false;
    for line in entry.lines().map(str::trim) {
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
        } else if in_main
            && let Some(value) = line.strip_prefix("Name")
            && let Some(value) = value.trim_start().strip_prefix('=')
        {
            return Some(value.trim()).filter(|v| !v.is_empty());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_main_group_name_only() {
        let entry = "[Desktop Entry]\nName[de]=Feuerfuchs\nGenericName=Browser\nName = Firefox\n\n[Desktop Action new]\nName=New Window\n";
        assert_eq!(entry_name(entry), Some("Firefox"));
        assert_eq!(entry_name("[Desktop Action x]\nName=Other\n"), None);
    }
}
