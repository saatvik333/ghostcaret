//! The small safe layer over llama.cpp's C API that this crate needs. Every
//! `unsafe` block in the crate lives here.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr::{NonNull, null_mut};
use std::sync::Once;

use llama_cpp_sys_2 as sys;

pub type Token = sys::llama_token;
pub type Seq = sys::llama_seq_id;

/// `LLAMA_STATE_SEQ_FLAGS_PARTIAL_ONLY` from `llama.h`: save or restore only
/// the state that cannot be cut back per token (recurrent state, SWA cache).
pub const STATE_PARTIAL_ONLY: u32 = 1;

/// Whether this build has a GPU backend that can hold layers.
pub fn supports_gpu_offload() -> bool {
    init();
    // SAFETY: llama.cpp is initialized.
    unsafe { sys::llama_supports_gpu_offload() }
}

/// Initializes llama.cpp once per process and routes its log into `log`.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: called once, before any other llama.cpp function.
        unsafe {
            sys::llama_log_set(Some(log_callback), null_mut());
            #[cfg(feature = "dynamic-backends")]
            load_backends();
            sys::llama_backend_init();
        }
    });
}

/// Loads backend modules from the executable's directory, where packaging
/// places them, falling back to the build's own output directory.
#[cfg(feature = "dynamic-backends")]
unsafe fn load_backends() {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .filter(|dir| has_backend(dir))
        .unwrap_or_else(|| env!("GGML_BACKENDS_DIR").into());
    if let Some(dir) = dir.to_str().and_then(|d| CString::new(d).ok()) {
        // SAFETY: valid NUL-terminated path.
        unsafe { sys::ggml_backend_load_all_from_path(dir.as_ptr()) };
    }
}

#[cfg(feature = "dynamic-backends")]
fn has_backend(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("ggml-cpu"))
    })
}

unsafe extern "C" fn log_callback(level: sys::ggml_log_level, text: *const c_char, _: *mut c_void) {
    if text.is_null() {
        return;
    }
    // SAFETY: llama.cpp passes a NUL-terminated string valid for this call.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let text = text.trim_end();
    if text.is_empty() {
        return;
    }
    match level {
        sys::GGML_LOG_LEVEL_ERROR => log::error!(target: "llama", "{text}"),
        sys::GGML_LOG_LEVEL_WARN => log::warn!(target: "llama", "{text}"),
        sys::GGML_LOG_LEVEL_INFO => log::debug!(target: "llama", "{text}"),
        _ => log::trace!(target: "llama", "{text}"),
    }
}

pub struct ModelParams {
    pub gpu_layers: i32,
    pub mmap: bool,
}

/// A loaded model. Immutable after loading; llama.cpp's tokenizer and
/// metadata accessors are thread-safe.
pub struct Model {
    ptr: NonNull<sys::llama_model>,
    vocab: NonNull<sys::llama_vocab>,
}

// SAFETY: the model is read-only after loading and llama.cpp documents the
// vocabulary functions as thread-safe.
unsafe impl Send for Model {}
unsafe impl Sync for Model {}

unsafe extern "C" fn progress_callback(progress: f32, data: *mut c_void) -> bool {
    // SAFETY: `data` is the `&mut &mut dyn FnMut(f32)` passed by `Model::load`,
    // alive for the duration of the load call.
    let report = unsafe { &mut *data.cast::<&mut dyn FnMut(f32)>() };
    report(progress);
    true
}

impl Model {
    pub fn load(
        path: &Path,
        params: &ModelParams,
        mut progress: &mut dyn FnMut(f32),
    ) -> Option<Model> {
        init();
        let path = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        // SAFETY: default params are plain data.
        let mut raw = unsafe { sys::llama_model_default_params() };
        raw.n_gpu_layers = params.gpu_layers;
        raw.load_mode = if params.mmap {
            sys::LLAMA_LOAD_MODE_MMAP
        } else {
            sys::LLAMA_LOAD_MODE_NONE
        };
        raw.progress_callback = Some(progress_callback);
        raw.progress_callback_user_data = (&raw mut progress).cast();
        // SAFETY: valid path; the callback data outlives the call.
        let ptr = NonNull::new(unsafe { sys::llama_model_load_from_file(path.as_ptr(), raw) })?;
        // SAFETY: valid model; the vocabulary lives as long as the model.
        let vocab = NonNull::new(unsafe { sys::llama_model_get_vocab(ptr.as_ptr()) }.cast_mut())?;
        Some(Model { ptr, vocab })
    }

    fn vocab(&self) -> *const sys::llama_vocab {
        self.vocab.as_ptr()
    }

    pub fn n_vocab(&self) -> usize {
        // SAFETY: valid vocabulary.
        unsafe { sys::llama_vocab_n_tokens(self.vocab()) }.max(0) as usize
    }

    pub fn n_ctx_train(&self) -> u32 {
        // SAFETY: valid model.
        unsafe { sys::llama_model_n_ctx_train(self.ptr.as_ptr()) }.max(0) as u32
    }

    pub fn n_swa(&self) -> u32 {
        // SAFETY: valid model.
        unsafe { sys::llama_model_n_swa(self.ptr.as_ptr()) }.max(0) as u32
    }

    /// True for recurrent and hybrid models, whose memory cannot be cut back
    /// to an arbitrary position.
    pub fn has_recurrent_state(&self) -> bool {
        // SAFETY: valid model.
        unsafe {
            sys::llama_model_is_recurrent(self.ptr.as_ptr())
                || sys::llama_model_is_hybrid(self.ptr.as_ptr())
        }
    }

    pub fn has_encoder(&self) -> bool {
        // SAFETY: valid model.
        unsafe { sys::llama_model_has_encoder(self.ptr.as_ptr()) }
    }

    pub fn size_bytes(&self) -> u64 {
        // SAFETY: valid model.
        unsafe { sys::llama_model_size(self.ptr.as_ptr()) }
    }

    pub fn n_params(&self) -> u64 {
        // SAFETY: valid model.
        unsafe { sys::llama_model_n_params(self.ptr.as_ptr()) }
    }

    /// llama.cpp's one-line description: architecture, size and weight type.
    pub fn description(&self) -> String {
        let mut buf = [0u8; 256];
        // SAFETY: valid model and buffer.
        let n =
            unsafe { sys::llama_model_desc(self.ptr.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        let n = usize::try_from(n).unwrap_or(0).min(buf.len());
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    /// A GGUF metadata value as a string.
    pub fn meta(&self, key: &str) -> Option<String> {
        let key = CString::new(key).ok()?;
        let mut buf = vec![0u8; 256];
        loop {
            // SAFETY: valid model, key and buffer.
            let n = unsafe {
                sys::llama_model_meta_val_str(
                    self.ptr.as_ptr(),
                    key.as_ptr(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                )
            };
            let n = usize::try_from(n).ok()?;
            if n < buf.len() {
                buf.truncate(n);
                return String::from_utf8(buf).ok();
            }
            buf.resize(n + 1, 0);
        }
    }

    /// The model's default chat template, if its metadata has one.
    pub fn chat_template(&self) -> Option<CString> {
        // SAFETY: valid model; a null name selects the default template.
        let tmpl = unsafe { sys::llama_model_chat_template(self.ptr.as_ptr(), std::ptr::null()) };
        // SAFETY: llama.cpp returns a NUL-terminated string owned by the model, or null.
        (!tmpl.is_null()).then(|| unsafe { CStr::from_ptr(tmpl) }.to_owned())
    }

    /// Appends the tokens of `text` to `out`. `add_special` adds BOS (and EOS)
    /// as the model is configured; `parse_special` turns special-token text
    /// into special tokens.
    pub fn tokenize(
        &self,
        text: &str,
        add_special: bool,
        parse_special: bool,
        out: &mut Vec<Token>,
    ) {
        let Ok(len) = i32::try_from(text.len()) else {
            return;
        };
        let start = out.len();
        let mut room = text.len() / 2 + 8;
        loop {
            out.resize(start + room, 0);
            // SAFETY: valid vocabulary; `out[start..]` has `room` slots.
            let n = unsafe {
                sys::llama_tokenize(
                    self.vocab(),
                    text.as_ptr().cast(),
                    len,
                    out.as_mut_ptr().add(start),
                    i32::try_from(room).unwrap_or(i32::MAX),
                    add_special,
                    parse_special,
                )
            };
            if n >= 0 {
                out.truncate(start + n as usize);
                return;
            }
            if n == i32::MIN {
                out.truncate(start);
                return;
            }
            room = n.unsigned_abs() as usize;
        }
    }

    /// Appends the bytes of `token` to `out`. `special` renders control
    /// tokens as their text instead of nothing.
    pub fn piece(&self, token: Token, special: bool, out: &mut Vec<u8>) {
        let start = out.len();
        let mut room = 32;
        loop {
            out.resize(start + room, 0);
            // SAFETY: valid vocabulary; `out[start..]` has `room` bytes.
            let n = unsafe {
                sys::llama_token_to_piece(
                    self.vocab(),
                    token,
                    out.as_mut_ptr().add(start).cast(),
                    room as i32,
                    0,
                    special,
                )
            };
            if n >= 0 {
                out.truncate(start + n as usize);
                return;
            }
            room = n.unsigned_abs() as usize;
        }
    }

    /// Whether tokenizing with `add_special` puts a BOS token first.
    pub fn adds_bos(&self) -> bool {
        // SAFETY: valid vocabulary.
        unsafe { sys::llama_vocab_get_add_bos(self.vocab()) }
    }

    pub fn is_eog(&self, token: Token) -> bool {
        // SAFETY: valid vocabulary.
        unsafe { sys::llama_vocab_is_eog(self.vocab(), token) }
    }

    pub fn is_control(&self, token: Token) -> bool {
        // SAFETY: valid vocabulary.
        unsafe { sys::llama_vocab_is_control(self.vocab(), token) }
    }

    /// Formats a chat with a built-in llama.cpp template. `None` when the
    /// template is not one llama.cpp knows.
    pub fn apply_chat_template(tmpl: &CStr, messages: &[(&CStr, &CStr)]) -> Option<String> {
        let chat: Vec<sys::llama_chat_message> = messages
            .iter()
            .map(|(role, content)| sys::llama_chat_message {
                role: role.as_ptr(),
                content: content.as_ptr(),
            })
            .collect();
        let total: usize = messages.iter().map(|(_, c)| c.count_bytes()).sum();
        let mut buf = vec![0u8; total * 2 + 256];
        loop {
            // SAFETY: valid template, messages and buffer.
            let n = unsafe {
                sys::llama_chat_apply_template(
                    tmpl.as_ptr(),
                    chat.as_ptr(),
                    chat.len(),
                    true,
                    buf.as_mut_ptr().cast(),
                    i32::try_from(buf.len()).unwrap_or(i32::MAX),
                )
            };
            let n = usize::try_from(n).ok()?;
            if n <= buf.len() {
                buf.truncate(n);
                return String::from_utf8(buf).ok();
            }
            buf.resize(n, 0);
        }
    }

    pub(crate) fn as_ptr(&self) -> *mut sys::llama_model {
        self.ptr.as_ptr()
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: owned pointer; every context of this model was dropped first
        // (enforced by field order in the owner).
        unsafe { sys::llama_model_free(self.ptr.as_ptr()) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvType {
    F16,
    Q8_0,
    Q4_0,
}

pub struct ContextParams {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_seq_max: u32,
    pub threads: i32,
    pub threads_batch: i32,
    /// `None` lets llama.cpp decide per backend.
    pub flash_attn: Option<bool>,
    pub kv_type: KvType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// No room in the memory for the batch.
    NoSlot,
    /// The abort callback stopped the decode; finished ubatches stay in memory.
    Aborted,
    /// Any other failure; the context should be recreated.
    Failed(i32),
}

/// An inference context. Must be dropped before its model.
pub struct Context {
    ptr: NonNull<sys::llama_context>,
    n_vocab: usize,
}

// SAFETY: a context is used by one thread at a time (callers hold it behind
// a mutex); llama.cpp has no thread affinity.
unsafe impl Send for Context {}

impl Context {
    pub fn new(model: &Model, params: &ContextParams) -> Option<Context> {
        // SAFETY: default params are plain data.
        let mut raw = unsafe { sys::llama_context_default_params() };
        raw.n_ctx = params.n_ctx;
        raw.n_batch = params.n_batch;
        raw.n_ubatch = params.n_batch;
        raw.n_seq_max = params.n_seq_max;
        raw.n_threads = params.threads;
        raw.n_threads_batch = params.threads_batch;
        raw.flash_attn_type = match params.flash_attn {
            None => sys::LLAMA_FLASH_ATTN_TYPE_AUTO,
            Some(true) => sys::LLAMA_FLASH_ATTN_TYPE_ENABLED,
            Some(false) => sys::LLAMA_FLASH_ATTN_TYPE_DISABLED,
        };
        let kv = match params.kv_type {
            KvType::F16 => sys::GGML_TYPE_F16,
            KvType::Q8_0 => sys::GGML_TYPE_Q8_0,
            KvType::Q4_0 => sys::GGML_TYPE_Q4_0,
        };
        raw.type_k = kv;
        raw.type_v = kv;
        // Sequences share one cell pool, so copying a sequence costs no memory.
        raw.kv_unified = true;
        // Keep the full sliding-window cache, so the memory can be cut back
        // to any position like a plain attention cache.
        raw.swa_full = true;
        raw.no_perf = true;
        // SAFETY: valid model.
        let ptr = NonNull::new(unsafe { sys::llama_init_from_model(model.as_ptr(), raw) })?;
        Some(Context {
            ptr,
            n_vocab: model.n_vocab(),
        })
    }

    fn memory(&self) -> sys::llama_memory_t {
        // SAFETY: valid context.
        unsafe { sys::llama_get_memory(self.ptr.as_ptr()) }
    }

    pub fn n_ctx(&self) -> u32 {
        // SAFETY: valid context.
        unsafe { sys::llama_n_ctx(self.ptr.as_ptr()) }
    }

    /// Waits for the work a decode queued on the backends (GPU decodes
    /// return before the results are computed).
    pub fn synchronize(&mut self) {
        // SAFETY: valid context.
        unsafe { sys::llama_synchronize(self.ptr.as_ptr()) }
    }

    pub fn decode(&mut self, batch: &Batch) -> Result<(), DecodeError> {
        // SAFETY: valid context; the batch's arrays hold `n_tokens` entries.
        match unsafe { sys::llama_decode(self.ptr.as_ptr(), batch.raw) } {
            0 => Ok(()),
            1 => Err(DecodeError::NoSlot),
            2 => Err(DecodeError::Aborted),
            code => Err(DecodeError::Failed(code)),
        }
    }

    /// Runs `f` with `check` installed as the abort check: on the CPU
    /// backend, ggml calls it between graph nodes and a true result stops the
    /// decode with [`DecodeError::Aborted`].
    pub fn with_abort<R>(
        &mut self,
        check: &(dyn Fn() -> bool + Sync),
        f: impl FnOnce(&mut Context) -> R,
    ) -> R {
        unsafe extern "C" fn trampoline(data: *mut c_void) -> bool {
            // SAFETY: `data` points at the `&dyn Fn` below, which outlives the
            // installation (the guard clears it first).
            let check = unsafe { &*data.cast::<&(dyn Fn() -> bool + Sync)>() };
            check()
        }
        struct Clear(NonNull<sys::llama_context>);
        impl Drop for Clear {
            fn drop(&mut self) {
                // SAFETY: valid context.
                unsafe { sys::llama_set_abort_callback(self.0.as_ptr(), None, null_mut()) }
            }
        }
        let mut check = check;
        // SAFETY: valid context; `check` lives on this frame until `Clear`
        // uninstalls the callback, also when `f` panics.
        unsafe {
            sys::llama_set_abort_callback(
                self.ptr.as_ptr(),
                Some(trampoline),
                (&raw mut check).cast(),
            )
        };
        let _clear = Clear(self.ptr);
        f(self)
    }

    /// Logits of the batch entry `i`, which must have requested output.
    pub fn logits(&self, i: usize) -> Option<&[f32]> {
        // SAFETY: valid context; llama.cpp returns null for an entry without
        // output, and otherwise `n_vocab` floats valid until the next decode,
        // which needs `&mut self`.
        unsafe {
            let ptr = sys::llama_get_logits_ith(self.ptr.as_ptr(), i as i32);
            (!ptr.is_null()).then(|| std::slice::from_raw_parts(ptr, self.n_vocab))
        }
    }

    /// Removes positions `[p0, p1)` of `seq` (`p1 < 0`: to the end). False if
    /// the memory cannot remove a partial range.
    pub fn seq_rm(&mut self, seq: Seq, p0: i32, p1: i32) -> bool {
        // SAFETY: valid memory.
        unsafe { sys::llama_memory_seq_rm(self.memory(), seq, p0, p1) }
    }

    pub fn seq_cp(&mut self, src: Seq, dst: Seq) {
        // SAFETY: valid memory.
        unsafe { sys::llama_memory_seq_cp(self.memory(), src, dst, -1, -1) }
    }

    pub fn seq_pos_max(&self, seq: Seq) -> i32 {
        // SAFETY: valid memory.
        unsafe { sys::llama_memory_seq_pos_max(self.memory(), seq) }
    }

    /// Serializes the state of `seq` into `out`, replacing its contents.
    pub fn state_seq_get(&mut self, seq: Seq, flags: u32, out: &mut Vec<u8>) -> bool {
        // SAFETY: valid context.
        let size = unsafe { sys::llama_state_seq_get_size_ext(self.ptr.as_ptr(), seq, flags) };
        out.clear();
        out.resize(size, 0);
        // SAFETY: `out` holds `size` bytes.
        let n = unsafe {
            sys::llama_state_seq_get_data_ext(self.ptr.as_ptr(), out.as_mut_ptr(), size, seq, flags)
        };
        n == size
    }

    /// Restores a state produced by [`Self::state_seq_get`] with the same flags.
    pub fn state_seq_set(&mut self, seq: Seq, flags: u32, data: &[u8]) -> bool {
        // SAFETY: valid context; `data` came from `state_seq_get` on a context
        // with the same shape, which llama.cpp validates.
        let n = unsafe {
            sys::llama_state_seq_set_data_ext(
                self.ptr.as_ptr(),
                data.as_ptr(),
                data.len(),
                seq,
                flags,
            )
        };
        n == data.len()
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: owned pointer.
        unsafe { sys::llama_free(self.ptr.as_ptr()) }
    }
}

/// A reusable token batch with one sequence id per token.
pub struct Batch {
    raw: sys::llama_batch,
    capacity: usize,
}

// SAFETY: the batch owns its heap arrays.
unsafe impl Send for Batch {}

impl Batch {
    pub fn new(capacity: usize) -> Batch {
        // SAFETY: allocates arrays for `capacity` tokens with one seq id each.
        let raw = unsafe { sys::llama_batch_init(capacity as i32, 0, 1) };
        Batch { raw, capacity }
    }

    pub fn len(&self) -> usize {
        self.raw.n_tokens as usize
    }

    pub fn clear(&mut self) {
        self.raw.n_tokens = 0;
    }

    pub fn push(&mut self, token: Token, pos: usize, seq: Seq, logits: bool) {
        let i = self.len();
        assert!(i < self.capacity, "batch overflow");
        // SAFETY: `i` is below the allocated capacity; each token has room for
        // one seq id.
        unsafe {
            *self.raw.token.add(i) = token;
            *self.raw.pos.add(i) = pos as i32;
            *self.raw.n_seq_id.add(i) = 1;
            *(*self.raw.seq_id.add(i)) = seq;
            *self.raw.logits.add(i) = i8::from(logits);
        }
        self.raw.n_tokens += 1;
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        // SAFETY: allocated by `llama_batch_init`.
        unsafe { sys::llama_batch_free(self.raw) }
    }
}
