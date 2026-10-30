//! Local inference through llama.cpp, and the local model store.
//!
//! [`LocalModel`] implements the engine's [`Completer`](ghostcaret_engine::complete::Completer)
//! and [`Generator`](ghostcaret_engine::complete::Generator) for a GGUF model;
//! [`store`] holds the curated manifest and the downloaded models.

mod heal;
mod kv;
mod llama;
mod model;
mod prompt;
mod sample;
pub mod store;

pub use llama::supports_gpu_offload;
pub use model::{
    FlashAttention, KvCacheType, LoadError, LoadOptions, LocalModel, MemoryKind, ModelInfo, Timings,
};
pub use prompt::TEMPLATE_VERSION;
