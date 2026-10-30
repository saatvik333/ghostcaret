//! The desktop's light or dark preference from the XDG settings portal.

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

pub(crate) struct ColorScheme {
    proxy: Proxy<'static>,
}

impl ColorScheme {
    pub fn connect() -> zbus::Result<ColorScheme> {
        let conn = Connection::session()?;
        let proxy = Proxy::new_owned(
            conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )?;
        Ok(ColorScheme { proxy })
    }

    /// The current preference; `None` when the portal does not say.
    pub fn dark(&self) -> Option<bool> {
        let value: OwnedValue = self.proxy.call("ReadOne", &(NAMESPACE, KEY)).ok()?;
        is_dark(&value)
    }

    /// Blocks for preference changes, one item per change.
    pub fn changes(&self) -> impl Iterator<Item = bool> + '_ {
        self.proxy.receive_signal("SettingChanged").into_iter().flatten().filter_map(|msg| {
            let (ns, key, value): (String, String, OwnedValue) = msg.body().deserialize().ok()?;
            if ns == NAMESPACE && key == KEY { is_dark(&value) } else { None }
        })
    }
}

/// `color-scheme` is 1 for dark, 2 for light and 0 for no preference
/// (treated as light). Older portals wrap the value in a second variant.
fn is_dark(value: &Value<'_>) -> Option<bool> {
    match value {
        Value::Value(inner) => is_dark(inner),
        Value::U32(scheme) => Some(*scheme == 1),
        _ => None,
    }
}
