//! Starts the backend's threads and routes the controller's calls to them.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::Sender;
use ghostcaret_engine::io::{
    Backend, Bindings, Conversation, Event, FieldId, InsertOutcome, InsertRequest, Overlay,
};
use ghostcaret_engine::settings::Appearance;

use super::mailbox::{self, Mailbox};
use super::{hook, insert, overlay, uia};
use crate::StartError;

/// State the UI Automation thread publishes for the insert thread.
#[derive(Debug, Default)]
pub struct Shared {
    /// The focused field's id, 0 when none.
    pub field: AtomicU64,
    /// Text before and after the caret at the last read.
    pub text: Mutex<(String, String)>,
}

/// How long `conversation` waits for UI Automation to walk the window.
const CONVERSATION_WITHIN: Duration = Duration::from_secs(4);

/// The backend's threads, stopped in reverse start order.
#[derive(Default)]
struct Threads(Vec<(u32, JoinHandle<()>)>);

impl Threads {
    fn add<T>(
        &mut self,
        started: Result<(Mailbox<T>, JoinHandle<()>), String>,
    ) -> Result<Mailbox<T>, StartError> {
        let (mail, handle) = started.map_err(StartError::Failed)?;
        self.0.push((mail.thread(), handle));
        Ok(mail)
    }
}

impl Drop for Threads {
    fn drop(&mut self) {
        for (thread, handle) in self.0.drain(..).rev() {
            mailbox::quit(thread);
            let _ = handle.join();
        }
    }
}

struct WinBackend {
    events: Sender<Event>,
    hook: Mailbox<hook::Command>,
    uia: Mailbox<uia::Msg>,
    overlay: Mailbox<overlay::Msg>,
    insert: Mailbox<insert::Request>,
    shared: Arc<Shared>,
    _threads: Threads,
}

impl std::fmt::Debug for WinBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinBackend").finish_non_exhaustive()
    }
}

pub fn start(events: Sender<Event>) -> Result<Arc<dyn Backend>, StartError> {
    let shared = Arc::new(Shared::default());
    let mut threads = Threads::default();
    let overlay = threads.add(mailbox::spawn("ghostcaret-overlay", overlay::run_thread))?;
    let hook = threads.add(mailbox::spawn("ghostcaret-hook", {
        let (events, overlay) = (events.clone(), overlay.clone());
        move |inbox, ready| hook::run_thread(inbox, ready, events, overlay)
    }))?;
    // The UI Automation thread's event handlers post to its own mailbox.
    let uia = threads.add(mailbox::spawn_with_self("ghostcaret-uia", {
        let (events, hook, overlay, shared) = (
            events.clone(),
            hook.clone(),
            overlay.clone(),
            shared.clone(),
        );
        move |inbox, ready, mail| uia::run_thread(inbox, ready, mail, events, hook, overlay, shared)
    }))?;
    let insert = threads.add(mailbox::spawn("ghostcaret-insert", {
        let (events, uia, shared) = (events.clone(), uia.clone(), shared.clone());
        move |inbox, ready| insert::run_thread(inbox, ready, events, uia, shared)
    }))?;
    Ok(Arc::new(WinBackend {
        events,
        hook,
        uia,
        overlay,
        insert,
        shared,
        _threads: threads,
    }))
}

impl Backend for WinBackend {
    fn set_overlay(&self, overlay: Overlay) {
        self.hook.post(hook::Command::Overlay(overlay));
    }

    fn set_bindings(&self, bindings: Bindings) {
        self.overlay.post(overlay::Msg::Bindings(bindings.clone()));
        self.hook.post(hook::Command::Bindings(bindings));
    }

    fn set_appearance(&self, appearance: Appearance) {
        self.overlay.post(overlay::Msg::Appearance(appearance));
    }

    fn set_active(&self, active: bool) {
        self.hook.post(hook::Command::Active(active));
    }

    fn insert(&self, request: InsertRequest) {
        let id = request.id;
        let (before, after) = self
            .shared
            .text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !self.insert.post(insert::Request {
            request,
            before,
            after,
        }) {
            let _ = self.events.send(Event::InsertDone {
                id,
                outcome: InsertOutcome::Failed("busy".into()),
            });
        }
    }

    fn conversation(&self, field: FieldId) -> Option<Conversation> {
        let (reply, answer) = crossbeam_channel::bounded(1);
        if !self.uia.post(uia::Msg::Conversation { field, reply }) {
            return None;
        }
        answer.recv_timeout(CONVERSATION_WITHIN).ok().flatten()
    }
}
