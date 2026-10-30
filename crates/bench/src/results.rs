//! The machine-readable result file and the configurations swept.

use std::collections::BTreeMap;

use ghostcaret_engine::complete::{LengthMode, StopParams};
use ghostcaret_engine::settings::Settings;
use serde::{Deserialize, Serialize};

use crate::corpus::{Corpus, Kind};
use crate::metrics::{Metrics, Tally};
use crate::pause::PauseModel;
use crate::system::Machine;

/// Bump when the file's meaning changes, so old files are not merged in.
pub const SCHEMA: u32 = 1;

/// One point of the sweep: a length mode, Auto's ceiling, and boldness.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub mode: LengthMode,
    /// Auto's ceiling in the corpus's app category; `None` for fixed modes.
    pub ceiling: Option<LengthMode>,
    pub boldness: f32,
}

impl Config {
    /// Default settings with this configuration applied to `kind`'s
    /// category.
    pub fn settings(&self, kind: Kind) -> Settings {
        let mut s = Settings {
            length: self.mode,
            boldness: self.boldness,
            ..Settings::default()
        };
        if let Some(c) = self.ceiling {
            match kind {
                Kind::Chat => s.ceilings.chat = c,
                Kind::Email => s.ceilings.email = c,
                Kind::Document => s.ceilings.document = c,
            }
        }
        s
    }

    /// Whether this is the shipped default for `kind`: Auto at the default
    /// ceiling and boldness.
    pub fn is_default(&self, kind: Kind) -> bool {
        let d = Settings::default();
        self.mode == d.length
            && self.boldness == d.boldness
            && self.ceiling == Some(default_ceiling(kind))
    }

    pub fn label(&self) -> String {
        match self.ceiling {
            Some(c) => format!("auto≤{}", mode_name(c)),
            None => mode_name(self.mode).to_owned(),
        }
    }
}

pub fn default_ceiling(kind: Kind) -> LengthMode {
    let c = Settings::default().ceilings;
    match kind {
        Kind::Chat => c.chat,
        Kind::Email => c.email,
        Kind::Document => c.document,
    }
}

pub fn mode_name(m: LengthMode) -> &'static str {
    match m {
        LengthMode::Word => "word",
        LengthMode::Phrase => "phrase",
        LengthMode::Sentence => "sentence",
        LengthMode::Paragraph => "paragraph",
        LengthMode::Auto => "auto",
    }
}

pub fn parse_mode(s: &str) -> Option<LengthMode> {
    Some(match s {
        "word" => LengthMode::Word,
        "phrase" => LengthMode::Phrase,
        "sentence" => LengthMode::Sentence,
        "paragraph" => LengthMode::Paragraph,
        "auto" => LengthMode::Auto,
        _ => return None,
    })
}

/// Fixed modes at every boldness, then Auto at every ceiling and boldness.
pub fn sweep(modes: &[LengthMode], ceilings: &[LengthMode], boldness: &[f32]) -> Vec<Config> {
    let mut out = Vec::new();
    for &mode in modes {
        if mode == LengthMode::Auto {
            for &c in ceilings {
                for &b in boldness {
                    out.push(Config {
                        mode,
                        ceiling: Some(c),
                        boldness: b,
                    });
                }
            }
        } else {
            for &b in boldness {
                out.push(Config {
                    mode,
                    ceiling: None,
                    boldness: b,
                });
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Results {
    pub schema: u32,
    pub tool: String,
    pub engine: EngineInfo,
    pub method: Method,
    pub corpora: Vec<CorpusInfo>,
    pub runs: Vec<Run>,
}

/// Everything in the engine and the local provider that changes what is
/// suggested.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineInfo {
    pub prompt_template_version: u32,
    pub settings_version: u32,
    pub careful_threshold: f32,
    pub bold_threshold: f32,
    pub phrase_min_words: u32,
    pub phrase_max_words: u32,
    pub paragraph_words: u32,
    pub pause_ms: u32,
    pub default_boldness: f32,
    pub default_ceilings: BTreeMap<String, String>,
}

impl EngineInfo {
    pub fn current() -> EngineInfo {
        let s = Settings::default();
        EngineInfo {
            prompt_template_version: ghostcaret_local::TEMPLATE_VERSION,
            settings_version: ghostcaret_engine::settings::SETTINGS_VERSION,
            careful_threshold: ghostcaret_engine::stop::CAREFUL_THRESHOLD,
            bold_threshold: ghostcaret_engine::stop::BOLD_THRESHOLD,
            phrase_min_words: ghostcaret_engine::stop::PHRASE_MIN_WORDS,
            phrase_max_words: ghostcaret_engine::stop::PHRASE_MAX_WORDS,
            paragraph_words: s.paragraph_words,
            pause_ms: s.pause_ms,
            default_boldness: s.boldness,
            default_ceilings: [Kind::Chat, Kind::Email, Kind::Document]
                .into_iter()
                .map(|k| {
                    (
                        kind_name(k).to_owned(),
                        mode_name(default_ceiling(k)).to_owned(),
                    )
                })
                .collect(),
        }
    }
}

pub fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Chat => "chat",
        Kind::Email => "email",
        Kind::Document => "document",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Method {
    pub pause: PauseModel,
    /// Share of positions inside a word that are pause points.
    pub within_word_pause_rate: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub language: String,
    pub license: String,
    pub license_url: String,
    pub source_url: String,
    pub why: String,
    pub sampling: String,
    pub app_id: String,
    pub app_name: String,
    pub samples: usize,
    pub words: usize,
    pub chars: usize,
    pub files: Vec<FileInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub url: String,
    pub sha256: String,
}

impl CorpusInfo {
    pub fn of(c: &Corpus) -> CorpusInfo {
        use unicode_segmentation::UnicodeSegmentation;
        let s = c.spec;
        CorpusInfo {
            id: s.id.into(),
            name: s.name.into(),
            kind: kind_name(s.kind).into(),
            language: s.language.into(),
            license: s.license.into(),
            license_url: s.license_url.into(),
            source_url: s.source_url.into(),
            why: s.why.into(),
            sampling: s.sampling.into(),
            app_id: s.app_id.into(),
            app_name: s.app_name.into(),
            samples: c.samples.len(),
            words: c
                .samples
                .iter()
                .map(|x| x.text.unicode_words().count())
                .sum(),
            chars: c.samples.iter().map(|x| x.text.chars().count()).sum(),
            files: c
                .files
                .iter()
                .map(|f| FileInfo {
                    name: f.name.clone(),
                    url: f.url.clone(),
                    sha256: f.sha256.into(),
                })
                .collect(),
        }
    }
}

/// One model's run over the corpora.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub model: ModelOut,
    /// `cuda`, `vulkan`, `metal` or `cpu`: the llama.cpp backend built in.
    pub backend: String,
    pub gpu_layers: u32,
    pub n_ctx: u32,
    pub threads: u32,
    pub machine: Machine,
    pub started: String,
    pub wall_s: f64,
    pub load1_start: f64,
    pub load1_max: f64,
    /// Latency is valid only when the 1-minute load average stayed at or
    /// below this during the whole run.
    pub max_load: f64,
    pub latency_valid: bool,
    /// Samples replayed per corpus (fewer than the corpus has when the run
    /// was limited).
    pub samples: BTreeMap<String, usize>,
    /// Model calls, shared by all configurations.
    pub generations: u64,
    pub tokens: u64,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelOut {
    pub id: String,
    pub name: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// The file's hash equals the curated manifest's.
    pub sha256_verified: bool,
    pub license: String,
    pub tier: String,
    /// llama.cpp's summary: architecture, size and weight type.
    pub description: String,
    pub architecture: String,
    pub parameters: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub corpus: String,
    pub config: Config,
    /// The stop rule the controller put on this configuration's requests.
    pub stop: StopParams,
    pub max_tokens: u32,
    pub tally: Tally,
    pub metrics: Metrics,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_expands_auto_per_ceiling() {
        let c = sweep(
            &[LengthMode::Word, LengthMode::Auto],
            &[LengthMode::Sentence, LengthMode::Paragraph],
            &[0.0, 1.0],
        );
        assert_eq!(c.len(), 2 + 2 * 2);
        assert_eq!(c[0].label(), "word");
        assert_eq!(c[2].label(), "auto≤sentence");
        assert_eq!(c[5].label(), "auto≤paragraph");
    }

    #[test]
    fn settings_set_only_the_corpus_category_ceiling() {
        let c = Config {
            mode: LengthMode::Auto,
            ceiling: Some(LengthMode::Phrase),
            boldness: 0.25,
        };
        let s = c.settings(Kind::Email);
        assert_eq!(s.ceilings.email, LengthMode::Phrase);
        assert_eq!(s.ceilings.chat, Settings::default().ceilings.chat);
        assert_eq!(s.boldness, 0.25);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn default_config_is_recognised() {
        let d = Settings::default();
        let c = Config {
            mode: LengthMode::Auto,
            ceiling: Some(d.ceilings.chat),
            boldness: d.boldness,
        };
        assert!(c.is_default(Kind::Chat));
        assert!(!Config { boldness: 0.0, ..c }.is_default(Kind::Chat));
    }
}
