//! JNI entry points of `io.github.saatvik333.ghostcaret.ime.Native`.
//!
//! Kotlin calls every one of them on the keyboard's UI thread. Each converts
//! its arguments once, hands a message to the runtime without blocking, and
//! returns. Kotlin holds the runtime handle returned by `start`, passes it to
//! every call, and zeroes its copy before passing it to `stop`.

use std::path::PathBuf;

use jni::errors::LogErrorAndDefault;
use jni::objects::{JClass, JObject, JString, Reference};
use jni::sys::{jboolean, jint, jlong};
use jni::{Env, EnvUnowned};
use oft_engine::io::{AppInfo, Event, FieldId, FieldSnapshot, InsertOutcome};
use oft_engine::settings::ModelChoice;

use super::bridge::Bridge;
use super::runtime::{Change, Dirs, Msg, Runtime};
use super::{settings_file, text};

type Result<T, E = Failure> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error(transparent)]
    Jni(#[from] jni::errors::Error),
    #[error("cannot start the engine thread: {0}")]
    Thread(#[from] std::io::Error),
}

/// The runtime behind a handle from `start`, or `None` for the zero handle
/// Kotlin uses when the engine is unavailable or stopped.
fn runtime<'a>(handle: jlong) -> Option<&'a Runtime> {
    // SAFETY: a non-zero handle is the pointer `start` leaked from a `Box`.
    // Kotlin makes every call on its UI thread and zeroes its copy before
    // `stop` frees the box, so the box outlives this call.
    (handle != 0).then(|| unsafe { &*(handle as *const Runtime) })
}

fn send(handle: jlong, msg: Msg) {
    if let Some(runtime) = runtime(handle) {
        runtime.send(msg);
    }
}

fn string(env: &Env, value: &JString) -> jni::errors::Result<String> {
    if value.is_null() { Ok(String::new()) } else { value.try_to_string(env) }
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
        let level = if trace { log::LevelFilter::Debug } else { log::LevelFilter::Info };
        android_logger::init_once(android_logger::Config::default().with_tag("oft").with_max_level(level));
        let bridge = Bridge::new(env, &bridge)?;
        let dirs = Dirs {
            config: PathBuf::from(string(env, &config_dir)?),
            data: PathBuf::from(string(env, &data_dir)?),
        };
        let runtime = Runtime::start(bridge, dirs)?;
        Ok(Box::into_raw(Box::new(runtime)) as jlong)
    })
    .resolve::<LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_stop<'l>(_env: EnvUnowned<'l>, _class: JClass<'l>, handle: jlong) {
    if handle != 0 {
        // SAFETY: see `runtime`; Kotlin passes each handle to `stop` once,
        // after zeroing its own copy, so nothing uses the box afterwards.
        drop(unsafe { Box::from_raw(handle as *mut Runtime) });
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
        if runtime(handle).is_none() {
            return Ok(());
        }
        let mut before = string(env, &before)?;
        let mut after = string(env, &after)?;
        let before_truncated = text::trim_before(&mut before) || before_truncated;
        text::trim_after(&mut after);
        let snapshot = FieldSnapshot {
            field: FieldId(field as u64),
            revision: 0,
            app: AppInfo { id: string(env, &app_id)?, name: string(env, &app_name)?, title: String::new() },
            kind: text::field_kind(kind),
            multiline,
            before,
            before_truncated,
            selection: string(env, &selection)?,
            after,
            caret: None,
        };
        send(handle, Msg::Focus(snapshot));
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
        if runtime(handle).is_none() {
            return Ok(());
        }
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
        send(handle, Msg::Changed(change));
        Ok(())
    })
    .resolve::<LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_blur<'l>(_env: EnvUnowned<'l>, _class: JClass<'l>, handle: jlong) {
    send(handle, Msg::Event(Event::Blur));
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
        send(handle, Msg::Event(Event::Accepted { field: FieldId(field as u64), text, whole }));
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
        send(handle, Msg::Event(Event::TypedThrough { field: FieldId(field as u64), text }));
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
        send(handle, Msg::Event(Event::Dismissed { field: FieldId(field as u64), reason }));
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
        send(handle, Msg::Event(Event::Action(action)));
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
        send(handle, Msg::Event(Event::CardKey(key)));
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
        send(handle, Msg::Event(Event::InsertDone { id: id as u64, outcome }));
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
    send(handle, Msg::ReloadSettings);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_saatvik333_ghostcaret_ime_Native_memoryPressure<'l>(
    _env: EnvUnowned<'l>,
    _class: JClass<'l>,
    handle: jlong,
) {
    send(handle, Msg::MemoryPressure);
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
            settings_file::save(&config_dir, &settings).map_err(|e| format!("cannot save settings: {e}"))
        });
        Ok(match saved {
            Ok(()) => JString::null(),
            Err(why) => JString::from_str(env, why)?,
        })
    })
    .resolve::<LogErrorAndDefault>()
}
