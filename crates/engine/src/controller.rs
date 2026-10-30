//! The controller: a pure state machine that turns backend events, model
//! results and timers into overlay updates, insertions and model requests.
//!
//! It performs no I/O and reads no clock. The app runtime feeds it [`Input`]s
//! and executes the [`Output`]s it returns, which keeps every product rule in
//! one place and testable without an OS or a model.

use std::time::Duration;

use crate::complete::{Caps, CompleteError, Completion, CompletionRequest, GenerateRequest};
use crate::io::{AppInfo, Bindings, Conversation, Event, FieldId, InsertRequest, Overlay};
use crate::settings::Settings;

/// Timers the controller asks the runtime to run. Starting a timer with an id
/// that is already running restarts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerId {
    /// The pause after typing before a suggestion is requested.
    Pause,
    /// Prefill debounce while the user is still typing.
    Prefill,
    /// The pause in an empty reply box before offering a draft.
    DraftOffer,
}

/// Which model serves a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    Autocomplete,
    FlowDraft,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GenerateUpdate {
    Delta(String),
    Done(Result<String, CompleteError>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Event(Event),
    Completed { id: u64, result: Result<Completion, CompleteError> },
    Generated { id: u64, update: GenerateUpdate },
    Timer(TimerId),
    Conversation { field: FieldId, conversation: Option<Conversation> },
    Settings(Box<Settings>),
    /// Capabilities of the current model for a feature; `None` when no model
    /// is configured or loaded.
    Caps { feature: Feature, caps: Option<Caps> },
    /// New clipboard text, already checked against password-manager markers.
    Clipboard(String),
}

/// Usage counters; never contain typed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stat {
    Shown { app: String },
    Accepted { app: String, chars: u32, whole: bool },
    Dismissed { app: String },
    DraftInserted { app: String },
}

/// Tray status the controller derives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Active,
    Paused,
    OffInApp(AppInfo),
    NoModel,
    CloudUnavailable(String),
    Backend(crate::io::BackendStatus),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Overlay(Overlay),
    Insert(InsertRequest),
    Bindings(Bindings),
    Active(bool),
    Complete(CompletionRequest),
    Prefill(CompletionRequest),
    Cancel(u64),
    Generate(GenerateRequest),
    CancelGenerate(u64),
    StartTimer { id: TimerId, after: Duration },
    StopTimer(TimerId),
    ReadConversation(FieldId),
    Record(Stat),
    Status(Status),
}
