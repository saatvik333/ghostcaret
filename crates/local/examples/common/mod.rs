//! Helpers shared by the examples.
#![allow(dead_code, reason = "each example uses part of this module")]

use std::path::PathBuf;

use ghostcaret_engine::complete::{CompletionRequest, LengthMode, Segments, StopCheck, StopParams};
use ghostcaret_engine::io::{AppInfo, FieldId};
use ghostcaret_local::store::ModelStore;
use ghostcaret_local::{FlashAttention, KvCacheType, LoadOptions};

pub const LOAD_USAGE: &str =
    "[--threads N] [--gpu-layers N] [--ctx N] [--kv f16|q8_0|q4_0] [--fa auto|on|off] [--no-mmap]";

/// Applies a load option flag. `None`: not a load option; `Some(false)`:
/// a load option with a bad or missing value.
pub fn load_option(
    flag: &str,
    value: &mut dyn FnMut() -> Option<String>,
    options: &mut LoadOptions,
) -> Option<bool> {
    let ok = match flag {
        "--threads" => value()
            .and_then(|v| v.parse().ok())
            .map(|v| options.threads = v)
            .is_some(),
        "--gpu-layers" => value()
            .and_then(|v| v.parse().ok())
            .map(|v| options.gpu_layers = v)
            .is_some(),
        "--ctx" => value()
            .and_then(|v| v.parse().ok())
            .map(|v| options.n_ctx = v)
            .is_some(),
        "--no-mmap" => {
            options.mmap = false;
            true
        }
        "--kv" => {
            let kv = match value().as_deref() {
                Some("f16") => Some(KvCacheType::F16),
                Some("q8_0") => Some(KvCacheType::Q8_0),
                Some("q4_0") => Some(KvCacheType::Q4_0),
                _ => None,
            };
            kv.map(|kv| options.kv_cache = kv).is_some()
        }
        "--fa" => {
            let fa = match value().as_deref() {
                Some("auto") => Some(FlashAttention::Auto),
                Some("on") => Some(FlashAttention::On),
                Some("off") => Some(FlashAttention::Off),
                _ => None,
            };
            fa.map(|fa| options.flash_attention = fa).is_some()
        }
        _ => return None,
    };
    Some(ok)
}

/// A manifest id installed in the model store, or a path.
pub fn model_path(model: &str) -> PathBuf {
    ModelStore::default_dir()
        .map(ModelStore::new)
        .and_then(|s| s.resolve(model))
        .unwrap_or_else(|| PathBuf::from(model))
}

pub fn request(
    id: u64,
    before: &str,
    after: &str,
    alternatives: u8,
    max_tokens: u32,
) -> CompletionRequest {
    CompletionRequest {
        id,
        field: FieldId(1),
        revision: id,
        segments: Segments {
            app: AppInfo {
                id: "example".into(),
                name: "Notes".into(),
                title: String::new(),
            },
            before: before.into(),
            after: after.into(),
            ..Segments::default()
        },
        stop: StopParams {
            mode: LengthMode::Auto,
            ceiling: LengthMode::Sentence,
            boldness: 0.5,
            paragraph_words: 60,
        },
        max_tokens,
        alternatives,
    }
}

/// Lets generation run to `max_tokens`, so timings compare like with like.
pub struct NoStop;

impl StopCheck for NoStop {
    fn should_stop(&mut self, _: &str, _: Option<f32>) -> bool {
        false
    }
}

/// Resident memory of this process in MB.
pub fn rss_mb() -> f64 {
    status_kb("VmRSS:") / 1024.0
}

/// Peak resident memory of this process in MB.
pub fn peak_rss_mb() -> f64 {
    status_kb("VmHWM:") / 1024.0
}

fn status_kb(key: &str) -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0.0)
}

/// GPU memory of this process in MiB, from nvidia-smi when present.
pub fn vram_mib() -> Option<u64> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    let pid = std::process::id().to_string();
    String::from_utf8(out.stdout).ok()?.lines().find_map(|l| {
        let (p, m) = l.split_once(',')?;
        if p.trim() == pid {
            m.trim().parse().ok()
        } else {
            None
        }
    })
}
