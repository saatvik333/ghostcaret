//! What kind of field has focus, from the UI Automation properties of the
//! focused element and its process.

use oft_engine::io::FieldKind;

/// The properties classification looks at, as UI Automation reports them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Traits {
    /// Lower-case executable name, such as `windowsterminal.exe`.
    pub exe: String,
    pub class: String,
    pub automation_id: String,
    pub aria_role: String,
    /// `name=value` pairs separated by `;`, as Chromium and Firefox report them.
    pub aria_properties: String,
    pub password: bool,
    pub spinner: bool,
    pub document: bool,
    /// Win32 edit control styles, when the element is one.
    pub edit_number: bool,
    pub edit_multiline: bool,
}

/// Terminal executables and the window classes of their text areas.
const TERMINAL_EXES: &[&str] = &[
    "windowsterminal.exe",
    "openconsole.exe",
    "conhost.exe",
    "wezterm-gui.exe",
    "alacritty.exe",
    "mintty.exe",
    "conemu64.exe",
    "conemu.exe",
    "tabby.exe",
    "warp.exe",
];
const TERMINAL_CLASSES: &[&str] = &["ConsoleWindowClass", "PseudoConsoleWindow", "TermControl"];

/// Address bars: Chromium's omnibox and Firefox's URL bar.
const URL_CLASSES: &[&str] = &["OmniboxViewViews"];
const URL_IDS: &[&str] = &["urlbar-input"];

impl Traits {
    pub fn is_terminal(&self) -> bool {
        TERMINAL_EXES.contains(&self.exe.as_str())
            || TERMINAL_CLASSES.contains(&self.class.as_str())
    }

    fn aria(&self, name: &str) -> Option<&str> {
        self.aria_properties.split(';').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k.trim() == name).then(|| v.trim())
        })
    }

    pub fn kind(&self) -> FieldKind {
        if self.password {
            FieldKind::Password
        } else if self.is_terminal() {
            FieldKind::Terminal
        } else if self.spinner || self.edit_number {
            FieldKind::Number
        } else if URL_CLASSES.contains(&self.class.as_str())
            || URL_IDS.contains(&self.automation_id.as_str())
        {
            FieldKind::Url
        } else if self.aria_role.eq_ignore_ascii_case("searchbox")
            || self.automation_id.to_ascii_lowercase().contains("search")
        {
            FieldKind::Search
        } else {
            FieldKind::Text
        }
    }

    pub fn multiline(&self) -> bool {
        match self.aria("multiline") {
            Some(v) => v.eq_ignore_ascii_case("true"),
            None => self.document || self.edit_multiline,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traits(f: impl FnOnce(&mut Traits)) -> Traits {
        let mut t = Traits {
            exe: "notepad.exe".into(),
            class: "RichEditD2DPT".into(),
            ..Traits::default()
        };
        f(&mut t);
        t
    }

    #[test]
    fn kinds() {
        assert_eq!(traits(|_| {}).kind(), FieldKind::Text);
        assert_eq!(traits(|t| t.password = true).kind(), FieldKind::Password);
        assert_eq!(
            traits(|t| t.exe = "windowsterminal.exe".into()).kind(),
            FieldKind::Terminal
        );
        assert_eq!(
            traits(|t| t.class = "ConsoleWindowClass".into()).kind(),
            FieldKind::Terminal
        );
        assert_eq!(
            traits(|t| t.class = "OmniboxViewViews".into()).kind(),
            FieldKind::Url
        );
        assert_eq!(
            traits(|t| t.automation_id = "urlbar-input".into()).kind(),
            FieldKind::Url
        );
        assert_eq!(
            traits(|t| t.aria_role = "searchbox".into()).kind(),
            FieldKind::Search
        );
        assert_eq!(
            traits(|t| t.automation_id = "SearchTextBox".into()).kind(),
            FieldKind::Search
        );
        assert_eq!(traits(|t| t.spinner = true).kind(), FieldKind::Number);
        // A password field in a terminal is still a password field.
        assert_eq!(
            traits(|t| {
                t.password = true;
                t.exe = "conhost.exe".into()
            })
            .kind(),
            FieldKind::Password
        );
    }

    #[test]
    fn multiline_prefers_what_the_page_says() {
        assert!(
            traits(|t| t.aria_properties = "readonly=false;multiline=true;required=false".into())
                .multiline()
        );
        assert!(
            !traits(|t| {
                t.aria_properties = "multiline=false".into();
                t.document = true;
            })
            .multiline()
        );
        assert!(traits(|t| t.document = true).multiline());
        assert!(traits(|t| t.edit_multiline = true).multiline());
        assert!(!traits(|_| {}).multiline());
    }
}
