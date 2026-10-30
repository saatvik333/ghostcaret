//! What the runtime reports to its host: the status line and stage timings.

use serde::Serialize;

use crate::pause::PauseInfo;

/// The status as the tray, the settings window or the keyboard strip shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StatusView {
    pub kind: StatusKind,
    /// One line for the tray, in English.
    pub text: String,
    /// The part of `text` after its fixed prefix, for hosts that word the
    /// line themselves: why the cloud, a model or the backend is
    /// unavailable, which permission is missing, why the settings were
    /// rejected, the app suggestions are off in.
    pub detail: Option<String>,
    pub pauses: Vec<PauseInfo>,
    /// The app that last had a text field focused.
    pub app: Option<AppRef>,
    pub local_model_loaded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Starting,
    Active,
    Paused,
    OffInApp,
    NoModel,
    CloudUnavailable,
    PermissionNeeded,
    SecureInput,
    Unavailable,
    /// [`Host::settings`](crate::Host::settings) failed; running on defaults.
    SettingsInvalid,
    Loading,
    Downloading,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppRef {
    pub id: String,
    pub name: String,
}

impl StatusView {
    pub(crate) fn starting() -> StatusView {
        StatusView {
            kind: StatusKind::Starting,
            text: "Starting".into(),
            detail: None,
            pauses: Vec::new(),
            app: None,
            local_model_loaded: false,
        }
    }
}

/// Timings of one autocomplete request, in microseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Timing {
    pub id: u64,
    /// Unix milliseconds when the result arrived.
    pub at_ms: u64,
    /// Last keystroke to the request being issued (mostly the pause).
    pub wait_us: u64,
    /// The controller step that issued it: context, secret filter, prompt.
    pub build_us: u64,
    /// Waiting for the completion worker.
    pub queue_us: u64,
    /// Model time.
    pub decode_us: u64,
    /// Handing the suggestion to the backend, which renders it.
    pub render_us: u64,
    /// Last keystroke to the suggestion being handed over.
    pub total_us: u64,
    pub shown: bool,
    pub cancelled: bool,
}

/// Requests whose timings are kept, newest last.
pub(crate) const TIMINGS_KEPT: usize = 64;
