//! Calls from the engine into the keyboard (`io.github.saatvik333.ghostcaret.ime.Bridge`).
//! Every Kotlin method posts to the keyboard's UI thread and returns at once,
//! so no call here waits on the UI.

use jni::objects::{Global, IntoAuto, JIntArray, JObject, JObjectArray, JString, JValue, Reference};
use jni::signature::{JavaType, Primitive};
use jni::sys::jvalue;
use jni::{Env, JavaVM, jni_sig, jni_str};
use oft_engine::io::{Backend, Bindings, CardView, Conversation, FieldId, InsertRequest, Overlay, Replace, SuggestionView};
use oft_engine::settings::{Appearance, GhostStyle};

use super::text::{self, Slot};

/// What the strip shows when no suggestion is visible. Codes mirror
/// `io.github.saatvik333.ghostcaret.ime.Codes.Status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StripStatus {
    Clear,
    Paused,
    OffInApp,
    NoModel,
    CloudUnavailable(String),
    Loading,
    LoadFailed(String),
    SettingsInvalid(String),
}

impl StripStatus {
    fn code(&self) -> (i32, Option<&str>) {
        match self {
            StripStatus::Clear => (0, None),
            StripStatus::Paused => (1, None),
            StripStatus::OffInApp => (2, None),
            StripStatus::NoModel => (3, None),
            StripStatus::CloudUnavailable(why) => (4, Some(why)),
            StripStatus::Loading => (5, None),
            StripStatus::LoadFailed(why) => (6, Some(why)),
            StripStatus::SettingsInvalid(why) => (7, Some(why)),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Bridge {
    vm: JavaVM,
    object: Global<JObject<'static>>,
    show_suggestion: jni::objects::JMethodID,
    show_card: jni::objects::JMethodID,
    hide: jni::objects::JMethodID,
    insert: jni::objects::JMethodID,
    set_active: jni::objects::JMethodID,
    set_bindings: jni::objects::JMethodID,
    set_appearance: jni::objects::JMethodID,
    status: jni::objects::JMethodID,
    api_key: jni::objects::JMethodID,
}

const VOID: JavaType = JavaType::Primitive(Primitive::Void);

impl Bridge {
    /// Looks up the callback methods once; a missing method fails here, at
    /// start, instead of on first use.
    pub(crate) fn new(env: &mut Env, object: &JObject) -> jni::errors::Result<Bridge> {
        let class = env.get_object_class(object)?;
        Ok(Bridge {
            vm: env.get_java_vm()?,
            object: env.new_global_ref(object)?,
            show_suggestion: env.get_method_id(
                &class,
                jni_str!("showSuggestion"),
                jni_sig!("(JJLjava/lang/String;I[I[III)V"),
            )?,
            show_card: env.get_method_id(
                &class,
                jni_str!("showCard"),
                jni_sig!(
                    "(JLjava/lang/String;[Ljava/lang/String;ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"
                ),
            )?,
            hide: env.get_method_id(&class, jni_str!("hide"), jni_sig!("()V"))?,
            insert: env.get_method_id(&class, jni_str!("insert"), jni_sig!("(JJLjava/lang/String;III)V"))?,
            set_active: env.get_method_id(&class, jni_str!("setActive"), jni_sig!("(Z)V"))?,
            set_bindings: env.get_method_id(&class, jni_str!("setBindings"), jni_sig!("([I)V"))?,
            set_appearance: env.get_method_id(&class, jni_str!("setAppearance"), jni_sig!("(ZZ)V"))?,
            status: env.get_method_id(&class, jni_str!("status"), jni_sig!("(ILjava/lang/String;)V"))?,
            api_key: env.get_method_id(
                &class,
                jni_str!("apiKey"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            )?,
        })
    }

    pub(crate) fn status(&self, status: &StripStatus) {
        let (code, detail) = status.code();
        self.call("status", |env, bridge| {
            let detail = optional_string(env, detail)?;
            self.invoke(env, bridge, self.status, &[JValue::Int(code).as_jni(), JValue::from(&detail).as_jni()])
        });
    }

    /// The API key saved for a provider, decrypted by the keyboard from the
    /// Android Keystore. Blocks briefly; called from the model loader thread.
    pub(crate) fn api_key(&self, provider: &str) -> Option<String> {
        let result = self.vm.attach_current_thread(|env| -> jni::errors::Result<Option<String>> {
            let id = JString::from_str(env, provider)?;
            // SAFETY: `api_key` was looked up on the bridge's class with the
            // signature `(String) -> String`, matching the one argument.
            let key = unsafe {
                env.call_method_unchecked(&*self.object, self.api_key, JavaType::Object, &[JValue::from(&id).as_jni()])
            }?
            .l()?;
            if key.is_null() {
                return Ok(None);
            }
            let key = env.cast_local::<JString>(key)?;
            Ok(Some(key.try_to_string(env)?))
        });
        result.unwrap_or_else(|e| {
            log::error!("reading the API key for provider {provider} failed: {e}");
            None
        })
    }

    fn show_suggestion(&self, view: &SuggestionView) {
        self.call("showSuggestion", |env, bridge| {
            let shown = text::utf16_offsets(&view.text, &[view.shown])[0];
            let words = int_array(env, &text::utf16_offsets(&view.text, &view.word_ends))?;
            let steps = int_array(env, &text::utf16_offsets(&view.text, &view.step_ends))?;
            let (alternative, alternatives) = view.alternative.map_or((0, 0), |(i, n)| (i32::from(i), i32::from(n)));
            let text = JString::from_str(env, &view.text)?;
            self.invoke(
                env,
                bridge,
                self.show_suggestion,
                &[
                    JValue::Long(view.field.0 as i64).as_jni(),
                    JValue::Long(view.revision as i64).as_jni(),
                    JValue::from(&text).as_jni(),
                    JValue::Int(shown).as_jni(),
                    JValue::from(&words).as_jni(),
                    JValue::from(&steps).as_jni(),
                    JValue::Int(alternative).as_jni(),
                    JValue::Int(alternatives).as_jni(),
                ],
            )
        });
    }

    fn show_card(&self, card: &CardView) {
        self.call("showCard", |env, bridge| {
            let title = JString::from_str(env, &card.title)?;
            let items = JObjectArray::<JString>::new(env, card.items.len(), JString::null())?;
            for (i, item) in card.items.iter().enumerate() {
                // Freed each round, so long lists stay within the local frame.
                let item = JString::from_str(env, item)?.auto();
                items.set_element(env, i, &*item)?;
            }
            let selected = card.selected.map_or(-1, |i| i32::try_from(i).unwrap_or(-1));
            let input = optional_string(env, card.input.as_deref())?;
            let preview = optional_string(env, card.preview.as_deref())?;
            let status = optional_string(env, card.status.as_deref())?;
            let hint = JString::from_str(env, &card.hint)?;
            self.invoke(
                env,
                bridge,
                self.show_card,
                &[
                    JValue::Long(card.field.0 as i64).as_jni(),
                    JValue::from(&title).as_jni(),
                    JValue::from(&items).as_jni(),
                    JValue::Int(selected).as_jni(),
                    JValue::from(&input).as_jni(),
                    JValue::from(&preview).as_jni(),
                    JValue::from(&status).as_jni(),
                    JValue::from(&hint).as_jni(),
                ],
            )
        });
    }

    fn call(&self, what: &str, f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<()>) {
        if let Err(e) = self.vm.attach_current_thread(|env| f(env, &self.object)) {
            log::error!("keyboard callback {what} failed: {e}");
        }
    }

    fn invoke(
        &self,
        env: &mut Env,
        bridge: &JObject,
        method: jni::objects::JMethodID,
        args: &[jvalue],
    ) -> jni::errors::Result<()> {
        // SAFETY: every method id was looked up on the bridge's own class in
        // `new`, with a void signature whose parameters match `args` at each
        // call site above.
        unsafe { env.call_method_unchecked(bridge, method, VOID, args) }.map(drop)
    }
}

fn int_array<'local>(env: &mut Env<'local>, values: &[i32]) -> jni::errors::Result<JIntArray<'local>> {
    let array = JIntArray::new(env, values.len())?;
    array.set_region(env, 0, values)?;
    Ok(array)
}

fn optional_string<'local>(env: &mut Env<'local>, value: Option<&str>) -> jni::errors::Result<JObject<'local>> {
    match value {
        Some(value) => Ok(JString::from_str(env, value)?.into()),
        None => Ok(JObject::null()),
    }
}

impl Backend for Bridge {
    fn set_overlay(&self, overlay: Overlay) {
        match overlay {
            Overlay::Hidden => self.call("hide", |env, bridge| self.invoke(env, bridge, self.hide, &[])),
            Overlay::Suggestion(view) => self.show_suggestion(&view),
            Overlay::Card(card) => self.show_card(&card),
        }
    }

    /// The soft keyboard has its own touch controls; bindings apply to keys
    /// from a hardware keyboard.
    fn set_bindings(&self, bindings: Bindings) {
        let chord = |c: Option<oft_engine::io::Chord>| c.as_ref().map_or(text::UNBOUND, text::chord_slot);
        let slots: [Slot; 7] = [
            chord(bindings.take_word),
            chord(bindings.take_all),
            chord(bindings.show_more),
            chord(bindings.next_alternative),
            chord(bindings.prev_alternative),
            chord(bindings.dismiss),
            bindings.flow_draft.as_ref().map_or(text::UNBOUND, text::trigger_slot),
        ];
        self.call("setBindings", |env, bridge| {
            let array = int_array(env, slots.as_flattened())?;
            self.invoke(env, bridge, self.set_bindings, &[JValue::from(&array).as_jni()])
        });
    }

    /// High contrast draws the whole suggestion in full strength; announce
    /// makes the strip a polite live region for TalkBack.
    fn set_appearance(&self, appearance: Appearance) {
        let high_contrast = appearance.style == GhostStyle::HighContrast;
        self.call("setAppearance", |env, bridge| {
            let args = [JValue::Bool(high_contrast).as_jni(), JValue::Bool(appearance.announce).as_jni()];
            self.invoke(env, bridge, self.set_appearance, &args)
        });
    }

    fn set_active(&self, active: bool) {
        self.call("setActive", |env, bridge| {
            self.invoke(env, bridge, self.set_active, &[JValue::Bool(active).as_jni()])
        });
    }

    fn insert(&self, request: InsertRequest) {
        let (mode, before, after) = match request.replace {
            Replace::Nothing => (0, 0, 0),
            Replace::Selection => (1, 0, 0),
            Replace::Around { before, after } => (2, clamp(before), clamp(after)),
        };
        self.call("insert", |env, bridge| {
            let text = JString::from_str(env, &request.text)?;
            self.invoke(
                env,
                bridge,
                self.insert,
                &[
                    JValue::Long(request.id as i64).as_jni(),
                    JValue::Long(request.field.0 as i64).as_jni(),
                    JValue::from(&text).as_jni(),
                    JValue::Int(mode).as_jni(),
                    JValue::Int(before).as_jni(),
                    JValue::Int(after).as_jni(),
                ],
            )
        });
    }

    /// Keyboards cannot read the rest of the screen.
    fn conversation(&self, _field: FieldId) -> Option<Conversation> {
        None
    }
}

fn clamp(bytes: usize) -> i32 {
    i32::try_from(bytes).unwrap_or(i32::MAX)
}
