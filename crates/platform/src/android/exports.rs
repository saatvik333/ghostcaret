//! JNI entry points of `io.github.saatvik333.ghostcaret.ime.Native`.
//!
//! Kotlin calls every one of them on the keyboard's UI thread. Each converts
//! its arguments once, hands an event or a command to the runtime without
//! blocking, and returns. Kotlin holds the handle returned by `start`, passes
//! it to every call, and zeroes its copy before passing it to `stop`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use jni::errors::LogErrorAndDefault;
use jni::objects::{JClass, JObject, JString, Reference};
use jni::sys::{jboolean, jint, jlong};
use jni::{Env, EnvUnowned};
use oft_cloud::SentLog;
use oft_engine::io::{AppInfo, Backend, Event, FieldId, FieldKind, FieldSnapshot, InsertOutcome};
use oft_engine::settings::ModelChoice;
use oft_local::LoadOptions;
use oft_runtime::{Command, Config, Runtime};
use oft_store::stats::Stats;
use oft_store::style::StyleMemory;

use super::bridge::Bridge;
use super::host::KeyboardHost;
use super::{settings_file, text};

type Result<T, E = Failure> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error(transparent)]
    Jni(#[from] jni::errors::Error),
    #[error("cannot start the engine thread: {0}")]
    Thread(#[from] std::io::Error),
}

/// The engine behind a handle from `start`.
struct Keyboard {
    runtime: Runtime,
    /// The focused field. Kotlin sends the app and the field kind only on
    /// focus; changes are completed from here. Used on the UI thread only.
    focus: RefCell<Option<Focused>>,
}

struct Focused {
    field: FieldId,
    app: AppInfo,
    kind: FieldKind,
    multiline: bool,
}

/// A field change, without what only the focus carries.
struct Change {
    field: FieldId,
    revision: u64,
    before: String,
    before_truncated: bool,
    selection: String,
    after: String,
}

impl Keyboard {
    fn focus(&self, snapshot: FieldSnapshot) {
        let (field, app, kind, multiline) = (
            snapshot.field,
            snapshot.app.clone(),
            snapshot.kind,
            snapshot.multiline,
        );
        *self.focus.borrow_mut() = Some(Focused {
            field,
            app,
            kind,
            multiline,
        });
        self.runtime.event(Event::Focus(snapshot));
    }

    /// Changes to a field that no longer has focus are dropped.
    fn changed(&self, change: Change) {
        let focus = self.focus.borrow();
        let Some(focus) = focus.as_ref().filter(|f| f.field == change.field) else {
            return;
        };
        let snapshot = FieldSnapshot {
            field: change.field,
            revision: change.revision,
            app: focus.app.clone(),
            kind: focus.kind,
            multiline: focus.multiline,
            before: change.before,
            before_truncated: change.before_truncated,
            selection: change.selection,
            after: change.after,
            caret: None,
        };
        self.runtime.event(Event::Changed(snapshot));
    }

    fn blur(&self) {
        *self.focus.borrow_mut() = None;
        self.runtime.event(Event::Blur);
    }
}

/// The keyboard behind a handle from `start`, or `None` for the zero handle
/// Kotlin uses when the engine is unavailable or stopped.
fn keyboard<'a>(handle: jlong) -> Option<&'a Keyboard> {
    // SAFETY: a non-zero handle is the pointer `start` leaked from a `Box`.
    // Kotlin makes every call on its UI thread, which also makes the
    // `RefCell` inside sound, and zeroes its copy before `stop` frees the
    // box, so the box outlives this call.
    (handle != 0).then(|| unsafe { &*(handle as *const Keyboard) })
}

fn event(handle: jlong, event: Event) {
    if let Some(keyboard) = keyboard(handle) {
        keyboard.runtime.event(event);
    }
}

fn command(handle: jlong, command: Command) {
    if let Some(keyboard) = keyboard(handle) {
        keyboard.runtime.send(command);
    }
}

fn string(env: &Env, value: &JString) -> jni::errors::Result<String> {
    if value.is_null() {
        Ok(String::new())
    } else {
        value.try_to_string(env)
    }
}

/// Context and threads for a phone: the curated small models fit 2048
/// tokens of context, and four threads keep decoding on the big cores.
fn load_options() -> LoadOptions {
    LoadOptions {
        n_ctx: 2048,
        threads: 4,
        ..LoadOptions::default()
    }
}

fn opened<T, E: std::fmt::Display>(what: &str, result: std::result::Result<T, E>) -> Option<T> {
    result
        .map_err(|e| log::error!("cannot open {what}: {e}"))
        .ok()
}

/// Starts the engine. `trace` (Kotlin: `Log.isLoggable("oft", DEBUG)`, set
/// with `adb shell setprop log.tag.oft DEBUG`) also logs stage timings.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_start<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    bridge: JObject<'l>,
    config_dir: JString<'l>,
    data_dir: JString<'l>,
    trace: jboolean,
) -> jlong {
    env.with_env(|env| -> Result<jlong> {
        let level = if trace {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        };
        android_logger::init_once(
            android_logger::Config::default()
                .with_tag("oft")
                .with_max_level(level),
        );
        let bridge = Arc::new(Bridge::new(env, &bridge)?);
        let config_dir = PathBuf::from(string(env, &config_dir)?);
        let data_dir = PathBuf::from(string(env, &data_dir)?);
        let host = Arc::new(KeyboardHost::new(bridge.clone(), config_dir));
        let config = Config {
            stats: opened("usage stats", Stats::open(&data_dir)),
            style: opened("style memory", StyleMemory::open(&data_dir)),
            sent_log: SentLog::default(),
            models_dir: data_dir.join("models"),
            load_options: load_options(),
        };
        let runtime = oft_runtime::start(host, config, move |_| Ok(bridge as Arc<dyn Backend>))?;
        Ok(Box::into_raw(Box::new(Keyboard {
            runtime,
            focus: RefCell::new(None),
        })) as jlong)
    })
    .resolve::<LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_stop<'l>(
    _env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: see `keyboard`; Kotlin passes each handle to `stop` once,
        // after zeroing its own copy, so nothing uses the box afterwards.
        drop(unsafe { Box::from_raw(handle as *mut Keyboard) });
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_focus<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
    field: jlong,
    app_id: JString<'l>,
    app_name: JString<'l>,
    kind: jint,
    multiline: jboolean,
    before: JString<'l>,
    before_truncated: jboolean,
    selection: JString<'l>,
    after: JString<'l>,
) {
    env.with_env(|env| -> Result<()> {
        let Some(keyboard) = keyboard(handle) else {
            return Ok(());
        };
        let mut before = string(env, &before)?;
        let mut after = string(env, &after)?;
        let before_truncated = text::trim_before(&mut before) || before_truncated;
        text::trim_after(&mut after);
        let snapshot = FieldSnapshot {
            field: FieldId(field as u64),
            revision: 0,
            app: AppInfo {
                id: string(env, &app_id)?,
                name: string(env, &app_name)?,
                title: String::new(),
            },
            kind: text::field_kind(kind),
            multiline,
            before,
            before_truncated,
            selection: string(env, &selection)?,
            after,
            caret: None,
        };
        keyboard.focus(snapshot);
        Ok(())
    })
    .resolve::<LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_changed<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
    field: jlong,
    revision: jlong,
    before: JString<'l>,
    before_truncated: jboolean,
    selection: JString<'l>,
    after: JString<'l>,
) {
    env.with_env(|env| -> Result<()> {
        let Some(keyboard) = keyboard(handle) else {
            return Ok(());
        };
        let mut before = string(env, &before)?;
        let mut after = string(env, &after)?;
        let before_truncated = text::trim_before(&mut before) || before_truncated;
        text::trim_after(&mut after);
        let change = Change {
            field: FieldId(field as u64),
            revision: revision as u64,
            before,
            before_truncated,
            selection: string(env, &selection)?,
            after,
        };
        keyboard.changed(change);
        Ok(())
    })
    .resolve::<LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_blur<'l>(
    _env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
) {
    if let Some(keyboard) = keyboard(handle) {
        keyboard.blur();
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_accepted<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
    field: jlong,
    text: JString<'l>,
    whole: jboolean,
) {
    env.with_env(|env| -> Result<()> {
        let text = string(env, &text)?;
        event(
            handle,
            Event::Accepted {
                field: FieldId(field as u64),
                text,
                whole,
            },
        );
        Ok(())
    })
    .resolve::<LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_typedThrough<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
    field: jlong,
    text: JString<'l>,
) {
    env.with_env(|env| -> Result<()> {
        let text = string(env, &text)?;
        event(
            handle,
            Event::TypedThrough {
                field: FieldId(field as u64),
                text,
            },
        );
        Ok(())
    })
    .resolve::<LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_dismissed<'l>(
    _env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
    field: jlong,
    reason: jint,
) {
    if let Some(reason) = text::dismiss_reason(reason) {
        event(
            handle,
            Event::Dismissed {
                field: FieldId(field as u64),
                reason,
            },
        );
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_action<'l>(
    _env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
    action: jint,
) {
    if let Some(action) = text::action(action) {
        event(handle, Event::Action(action));
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_cardKey<'l>(
    _env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
    key: jint,
    value: jint,
) {
    if let Some(key) = text::card_key(key, value) {
        event(handle, Event::CardKey(key));
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_insertDone<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
    id: jlong,
    outcome: jint,
    message: JString<'l>,
) {
    env.with_env(|env| -> Result<()> {
        let outcome = match outcome {
            0 => InsertOutcome::Inserted,
            1 => InsertOutcome::Failed(string(env, &message)?),
            _ => InsertOutcome::Unknown,
        };
        event(
            handle,
            Event::InsertDone {
                id: id as u64,
                outcome,
            },
        );
        Ok(())
    })
    .resolve::<LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_reloadSettings<'l>(
    _env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
) {
    command(handle, Command::SettingsChanged);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_memoryPressure<'l>(
    _env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
) {
    command(handle, Command::MemoryPressure);
}

/// Makes the GGUF file at `model` the local model for autocomplete, and for
/// Reply & rewrite unless a cloud provider already serves drafts. Returns `null`
/// on success, or the reason the settings could not be changed.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_useLocalModel<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    config_dir: JString<'l>,
    model: JString<'l>,
) -> JString<'l> {
    env.with_env(|env| -> Result<JString<'l>> {
        let config_dir = PathBuf::from(string(env, &config_dir)?);
        let model = string(env, &model)?;
        let saved = settings_file::load(&config_dir).and_then(|mut settings| {
            let choice = ModelChoice::Local { model };
            if !matches!(settings.draft_model, ModelChoice::Provider { .. }) {
                settings.draft_model = choice.clone();
            }
            settings.autocomplete_model = choice;
            settings_file::save(&config_dir, &settings)
                .map_err(|e| format!("cannot save settings: {e}"))
        });
        Ok(match saved {
            Ok(()) => JString::null(),
            Err(why) => JString::from_str(env, why)?,
        })
    })
    .resolve::<LogErrorAndDefault>()
}
