//! Android: the engine inside Ghostcaret's own keyboard.
//!
//! The keyboard service (`io.github.saatvik333.ghostcaret.ime`, Kotlin) owns the key path.
//! On its UI thread it applies Take Word, Take All, Show More and typing
//! through to its copy of the suggestion, commits through `InputConnection`,
//! and only then reports to the engine through the JNI entry points in
//! [`exports`]. None of them waits on the engine.
//!
//! The engine runs on a runtime thread ([`runtime`]) that owns the
//! [`Controller`](oft_engine::controller::Controller), its timers and the
//! model workers ([`models`]). It calls back into the keyboard through
//! [`bridge::Bridge`], the Android [`Backend`](oft_engine::io::Backend), whose
//! methods post to the UI thread and return at once.
//!
//! Kotlin counts text in UTF-16 units and the engine in UTF-8 bytes; [`text`]
//! converts at the boundary, once per value.
//!
//! Keyboards cannot read the rest of the screen, so
//! [`Backend::conversation`](oft_engine::io::Backend::conversation) is always
//! `None` here and Reply & rewrite works from the field text and the selection.

mod bridge;
mod exports;
mod models;
mod runtime;
mod settings_file;
mod text;
