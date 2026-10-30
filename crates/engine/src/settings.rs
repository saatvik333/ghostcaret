//! User settings, stored as TOML in the OS config directory.
//!
//! API keys are never stored here; they live in the OS keychain under the
//! provider's `id`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::complete::LengthMode;
use crate::io::Bindings;

/// The current settings file format. Bump on incompatible changes and migrate
/// older files in [`Settings::from_toml`].
pub const SETTINGS_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// Global on/off.
    pub enabled: bool,
    pub length: LengthMode,
    /// 0.0 careful … 1.0 bold. The default comes from the benchmark.
    pub boldness: f32,
    /// How far Auto may go in each kind of app.
    pub ceilings: Ceilings,
    /// Word cap for Paragraph suggestions.
    pub paragraph_words: u32,
    /// Lengthen or shorten per app from what the user accepts. Off by default.
    pub learn_length: bool,
    /// Pause after the last keystroke before asking for a suggestion.
    pub pause_ms: u32,
    pub keys: Bindings,
    pub autocomplete_model: ModelChoice,
    pub draft_model: ModelChoice,
    pub providers: Vec<ProviderConfig>,
    pub context: ContextSettings,
    pub flow_draft: FlowDraftSettings,
    /// Per-app rules keyed by `AppInfo::id`.
    pub apps: BTreeMap<String, AppRule>,
    /// Daily cap on cloud requests; `None` for no cap.
    pub daily_request_cap: Option<u32>,
    /// Small model and a longer pause while on battery.
    pub low_power: bool,
    /// Minutes of inactivity before the local model is unloaded; 0 = never.
    pub unload_after_minutes: u32,
    pub appearance: Appearance,
    pub update_check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            enabled: true,
            length: LengthMode::Auto,
            boldness: 0.5,
            ceilings: Ceilings::default(),
            paragraph_words: 60,
            learn_length: false,
            pause_ms: 30,
            keys: Bindings::default(),
            autocomplete_model: ModelChoice::None,
            draft_model: ModelChoice::None,
            providers: Vec::new(),
            context: ContextSettings::default(),
            flow_draft: FlowDraftSettings::default(),
            apps: BTreeMap::new(),
            daily_request_cap: None,
            low_power: false,
            unload_after_minutes: 30,
            appearance: Appearance::default(),
            update_check: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum SettingsError {
    /// The file is not valid TOML for this schema. Carries the place, never
    /// the text: TOML errors quote the offending line, and a key pasted in the
    /// wrong place must not reach the tray, the keyboard strip or a log.
    #[error("{}", describe_parse(*line, key.as_deref()))]
    Parse {
        line: Option<usize>,
        key: Option<String>,
    },
    #[error("settings file version {0} is newer than this app supports ({SETTINGS_VERSION})")]
    TooNew(u32),
    #[error("setting `{key}` is invalid: {reason}")]
    Invalid { key: &'static str, reason: String },
}

fn describe_parse(line: Option<usize>, key: Option<&str>) -> String {
    match (line, key) {
        (Some(line), Some(key)) => {
            format!("setting `{key}` on line {line} of the settings file is not valid")
        }
        (Some(line), None) => format!("line {line} of the settings file is not valid TOML"),
        (None, _) => "the settings file is not valid TOML".to_owned(),
    }
}

/// The 1-based line of `offset` in `text` and the dotted key defined there,
/// if any, so an error can name the place without quoting the value.
fn locate(text: &str, offset: usize) -> (usize, Option<String>) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_no = text[..line_start].matches('\n').count() + 1;
    let line = text[line_start..].lines().next().unwrap_or("").trim();
    let table = text[..line_start]
        .lines()
        .rev()
        .map(str::trim)
        .find_map(table_header)
        .filter(|_| table_header(line).is_none());
    let key = match table_header(line) {
        Some(h) => Some(h.to_owned()),
        None => line
            .split_once('=')
            .map(|(k, _)| clean_key(k))
            .filter(|k| !k.is_empty()),
    };
    let key = match (table, key) {
        (Some(t), Some(k)) => Some(format!("{t}.{k}")),
        (None, k) => k,
        (Some(t), None) => Some(t.to_owned()),
    };
    (line_no, key)
}

fn table_header(line: &str) -> Option<&str> {
    let inner = line
        .strip_prefix("[[")
        .and_then(|l| l.split_once("]]"))
        .map(|(h, _)| h);
    let inner = inner.or_else(|| {
        line.strip_prefix('[')
            .and_then(|l| l.split_once(']'))
            .map(|(h, _)| h)
    })?;
    Some(inner.trim())
}

fn clean_key(raw: &str) -> String {
    raw.split('.')
        .map(|part| part.trim().trim_matches(['"', '\'']))
        .collect::<Vec<_>>()
        .join(".")
}

impl Settings {
    pub fn from_toml(text: &str) -> Result<Settings, SettingsError> {
        let settings: Settings = toml::from_str(text).map_err(|e| {
            let place = e.span().map(|s| locate(text, s.start));
            SettingsError::Parse {
                line: place.as_ref().map(|p| p.0),
                key: place.and_then(|p| p.1),
            }
        })?;
        if settings.version > SETTINGS_VERSION {
            return Err(SettingsError::TooNew(settings.version));
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("settings always serialize")
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        let invalid = |key, reason: &str| {
            Err(SettingsError::Invalid {
                key,
                reason: reason.into(),
            })
        };
        if !(0.0..=1.0).contains(&self.boldness) {
            return invalid("boldness", "must be between 0 and 1");
        }
        if self.paragraph_words == 0 {
            return invalid("paragraph_words", "must be at least 1");
        }
        if self.pause_ms > 2_000 {
            return invalid("pause_ms", "must be at most 2000");
        }
        if self.ceilings.all().any(|m| m == LengthMode::Auto) {
            return invalid("ceilings", "a ceiling cannot be Auto");
        }
        let mut ids = std::collections::BTreeSet::new();
        for p in &self.providers {
            if p.id.is_empty() || !ids.insert(p.id.as_str()) {
                return invalid("providers", "every provider needs a unique, non-empty id");
            }
        }
        for (key, choice) in [
            ("autocomplete_model", &self.autocomplete_model),
            ("draft_model", &self.draft_model),
        ] {
            if let ModelChoice::Provider { provider } = choice
                && !ids.contains(provider.as_str())
            {
                return invalid(key, "refers to a provider that is not configured");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ceilings {
    pub chat: LengthMode,
    pub email: LengthMode,
    pub document: LengthMode,
    pub search: LengthMode,
    pub terminal: LengthMode,
    pub other: LengthMode,
}

impl Default for Ceilings {
    fn default() -> Self {
        Ceilings {
            chat: LengthMode::Sentence,
            email: LengthMode::Paragraph,
            document: LengthMode::Paragraph,
            search: LengthMode::Word,
            terminal: LengthMode::Sentence,
            other: LengthMode::Sentence,
        }
    }
}

impl Ceilings {
    pub fn for_category(&self, category: AppCategory) -> LengthMode {
        match category {
            AppCategory::Chat => self.chat,
            AppCategory::Email => self.email,
            AppCategory::Document => self.document,
            AppCategory::Search => self.search,
            AppCategory::Terminal => self.terminal,
            AppCategory::Code | AppCategory::Other => self.other,
        }
    }

    fn all(&self) -> impl Iterator<Item = LengthMode> {
        [
            self.chat,
            self.email,
            self.document,
            self.search,
            self.terminal,
            self.other,
        ]
        .into_iter()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppCategory {
    Chat,
    Email,
    Document,
    Search,
    Terminal,
    Code,
    #[default]
    Other,
}

/// Which model serves a feature.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ModelChoice {
    #[default]
    None,
    /// A model from the curated manifest (by id) or a GGUF file path.
    Local { model: String },
    /// A configured cloud provider, by `ProviderConfig::id`.
    Provider { provider: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Unique id; also the keychain entry name.
    pub id: String,
    /// Name shown in settings.
    pub label: String,
    pub kind: ProviderKind,
    /// Model name sent to the provider.
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ProviderKind {
    /// OpenAI, OpenRouter, Groq, Together, Ollama, LM Studio, llama-server,
    /// vLLM and anything else that speaks the OpenAI API.
    OpenAiCompatible {
        base_url: String,
    },
    Anthropic,
    Gemini,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextSettings {
    /// Visible conversation through the accessibility API.
    pub conversation: bool,
    /// Screenshot plus OCR when accessibility shows no text.
    pub screen_ocr: bool,
    pub clipboard: bool,
    /// Messages the user sent, kept locally for style.
    pub style_memory: bool,
    pub about_me: String,
}

impl Default for ContextSettings {
    fn default() -> Self {
        ContextSettings {
            conversation: true,
            screen_ocr: false,
            clipboard: false,
            style_memory: false,
            about_me: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowDraftSettings {
    pub enabled: bool,
    /// Offer a draft when the user pauses in an empty reply box.
    pub auto_offer: bool,
    /// The user's own rewrite actions, shown after the built-in ones.
    pub custom_actions: Vec<CustomAction>,
}

impl Default for FlowDraftSettings {
    fn default() -> Self {
        FlowDraftSettings {
            enabled: true,
            auto_offer: false,
            custom_actions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomAction {
    pub name: String,
    pub instruction: String,
}

/// Per-app overrides; `None` means "use the global value".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppRule {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub length: Option<LengthMode>,
    pub ceiling: Option<LengthMode>,
    pub category: Option<AppCategory>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GhostStyle {
    #[default]
    Faint,
    HighContrast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub style: GhostStyle,
    /// Announce suggestions through the platform's accessibility API.
    pub announce: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            style: GhostStyle::Faint,
            announce: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips() {
        let s = Settings::default();
        assert_eq!(Settings::from_toml(&s.to_toml()).unwrap(), s);
    }

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(Settings::from_toml("").unwrap(), Settings::default());
    }

    #[test]
    fn parse_error_names_the_place_but_never_the_value() {
        // A key pasted where a string setting belongs, unquoted, so TOML
        // rejects the line. Built at run time so scanners don't flag it.
        let secret = ["sk-", "live0123456789abcdefghijklmnop"].concat();
        let text = format!("[context]\nabout_me = {secret}\n");
        let err = Settings::from_toml(&text).unwrap_err();
        let message = err.to_string();
        assert!(!message.contains(&secret), "{message}");
        assert_eq!(
            err,
            SettingsError::Parse {
                line: Some(2),
                key: Some("context.about_me".into())
            }
        );
    }

    #[test]
    fn locate_reads_tables_and_quoted_keys() {
        let text = "a = 1\n[apps.\"org.telegram.desktop\"]\n\"length\" = 3\n";
        let at = text.find('3').unwrap();
        assert_eq!(
            locate(text, at),
            (3, Some("apps.\"org.telegram.desktop\".length".to_owned()))
        );
        assert_eq!(locate(text, 0), (1, Some("a".to_owned())));
    }

    #[test]
    fn rejects_out_of_range_boldness_naming_the_key() {
        let err = Settings::from_toml("boldness = 3.0").unwrap_err();
        assert!(matches!(
            err,
            SettingsError::Invalid {
                key: "boldness",
                ..
            }
        ));
    }

    #[test]
    fn rejects_unknown_provider_reference() {
        let err =
            Settings::from_toml("[autocomplete_model]\nkind = \"provider\"\nprovider = \"nope\"")
                .unwrap_err();
        assert!(matches!(err, SettingsError::Invalid { .. }));
    }
}
