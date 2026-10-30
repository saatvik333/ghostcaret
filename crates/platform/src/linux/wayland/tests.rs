//! The Wayland thread against a stand-in compositor that offers just enough
//! of the input-method protocols to script what an app sends and record what
//! the input method answers.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use ghostcaret_engine::io::{Event, FieldId, InsertOutcome, InsertRequest, Replace};
use rustix::event::{PollFd, PollFlags, Timespec};
use wayland_client::Connection;
use wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2;
use wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::{
    self, ZwpInputMethodManagerV2,
};
use wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::{
    self, ZwpInputMethodV2,
};
use wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::{
    self, ZwpVirtualKeyboardManagerV1,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_server::protocol::wl_compositor::{self, WlCompositor};
use wayland_server::protocol::wl_region::WlRegion;
use wayland_server::protocol::wl_seat::WlSeat;
use wayland_server::protocol::wl_shm::WlShm;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
};

use super::{Session, run};
use crate::linux::commits::INSERT_CONFIRM;
use crate::linux::{Command, Shared, Waker};

/// What the input method asked of the compositor.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Req {
    CommitString(String),
    Commit(u32),
}

#[derive(Default)]
struct Compositor {
    im: Option<ZwpInputMethodV2>,
    requests: Vec<Req>,
}

struct NoData;
impl wayland_server::backend::ClientData for NoData {}

macro_rules! globals {
    ($($i:ty),*) => {$(
        impl GlobalDispatch<$i, ()> for Compositor {
            fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, r: New<$i>, _: &(), init: &mut DataInit<'_, Self>) {
                init.init(r, ());
            }
        }
    )*};
}

macro_rules! ignore_requests {
    ($($i:ty),*) => {$(
        impl Dispatch<$i, ()> for Compositor {
            fn request(
                _: &mut Self,
                _: &Client,
                _: &$i,
                _: <$i as Resource>::Request,
                _: &(),
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
            }
        }
    )*};
}

globals!(
    WlCompositor,
    WlShm,
    WlSeat,
    ZwpInputMethodManagerV2,
    ZwpVirtualKeyboardManagerV1
);
ignore_requests!(
    WlShm,
    WlSeat,
    WlSurface,
    WlRegion,
    ZwpInputPopupSurfaceV2,
    ZwpInputMethodKeyboardGrabV2,
    ZwpVirtualKeyboardV1
);

impl Dispatch<WlCompositor, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlCompositor,
        request: wl_compositor::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                init.init(id, ());
            }
            wl_compositor::Request::CreateRegion { id } => {
                init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for Compositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwpInputMethodManagerV2,
        request: zwp_input_method_manager_v2::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_input_method_manager_v2::Request::GetInputMethod { input_method, .. } = request {
            state.im = Some(init.init(input_method, ()));
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for Compositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwpInputMethodV2,
        request: zwp_input_method_v2::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        use zwp_input_method_v2::Request as R;
        match request {
            R::CommitString { text } => state.requests.push(Req::CommitString(text)),
            R::Commit { serial } => state.requests.push(Req::Commit(serial)),
            R::GetInputPopupSurface { id, .. } => {
                init.init(id, ());
            }
            R::GrabKeyboard { keyboard } => {
                init.init(keyboard, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwpVirtualKeyboardManagerV1,
        request: zwp_virtual_keyboard_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_virtual_keyboard_manager_v1::Request::CreateVirtualKeyboard { id, .. } = request
        {
            init.init(id, ());
        }
    }
}

/// The Wayland thread connected to the stand-in compositor.
struct Harness {
    display: Display<Compositor>,
    c: Compositor,
    commands: Sender<Command>,
    waker: Arc<Waker>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

const WAIT: Duration = Duration::from_secs(5);

impl Harness {
    fn start() -> Harness {
        let (server, client) = UnixStream::pair().expect("socket pair");
        let display = Display::<Compositor>::new().expect("display");
        let mut dh = display.handle();
        dh.create_global::<Compositor, WlCompositor, ()>(4, ());
        dh.create_global::<Compositor, WlShm, ()>(1, ());
        dh.create_global::<Compositor, WlSeat, ()>(7, ());
        dh.create_global::<Compositor, ZwpInputMethodManagerV2, ()>(1, ());
        dh.create_global::<Compositor, ZwpVirtualKeyboardManagerV1, ()>(1, ());
        dh.insert_client(server, Arc::new(NoData)).expect("client");
        let (event_tx, events) = crossbeam_channel::unbounded();
        let (commands, command_rx) = crossbeam_channel::unbounded();
        let waker = Arc::new(Waker::new().expect("eventfd"));
        let thread = std::thread::spawn({
            let waker = waker.clone();
            move || {
                // Overlays are never set here, so nothing is rendered.
                let (render, _jobs) = crossbeam_channel::unbounded();
                let shared = Arc::new(Shared::new());
                let conn = Connection::from_socket(client).expect("connection");
                let session =
                    Session::connect(conn, event_tx.clone(), shared.clone(), render.clone())
                        .expect("session");
                run(session, command_rx, waker, event_tx, shared, render);
            }
        });
        let mut h = Harness {
            display,
            c: Compositor::default(),
            commands,
            waker,
            events,
            thread: Some(thread),
        };
        h.pump_until("the input method", |c| c.im.is_some());
        h
    }

    /// Dispatches the client's requests until `done` holds.
    fn pump_until(&mut self, what: &str, done: impl Fn(&Compositor) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.display
                .dispatch_clients(&mut self.c)
                .expect("dispatch");
            self.display.flush_clients().expect("flush");
            if done(&self.c) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no {what}; the input method asked {:?}",
                self.c.requests
            );
            let fd = self.display.backend().poll_fd();
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            let _ = rustix::event::poll(
                &mut fds,
                Some(&Timespec {
                    tv_sec: 0,
                    tv_nsec: 5_000_000,
                }),
            );
        }
    }

    /// Keeps the compositor running until the backend sends an event that
    /// `pick` accepts.
    fn event<T>(&mut self, what: &str, mut pick: impl FnMut(Event) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            while let Ok(e) = self.events.try_recv() {
                if let Some(t) = pick(e) {
                    return t;
                }
            }
            assert!(Instant::now() < deadline, "no {what} event");
            self.pump_until(what, |_| true);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The app's text-input state, as the compositor relays it.
    fn app_state(&mut self, activate: bool, text: &str) {
        let im = self.c.im.as_ref().expect("input method");
        if activate {
            im.activate();
        }
        let len = text.len() as u32;
        im.surrounding_text(text.into(), len, len);
        im.done();
        self.display.flush_clients().expect("flush");
    }

    fn focus(&mut self, text: &str) -> FieldId {
        self.app_state(true, text);
        self.event("focus", |e| match e {
            Event::Focus(s) => Some(s.field),
            _ => None,
        })
    }

    fn insert(&mut self, request: InsertRequest) {
        self.commands
            .send(Command::Insert(request))
            .expect("command");
        self.waker.wake();
    }

    fn asked(&self, req: &Req) -> usize {
        self.c.requests.iter().filter(|r| *r == req).count()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        self.waker.wake();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Regression: Chromium sends its next text only after a `done` whose serial
/// matches its count of commits, and niri sends `done` only when the input
/// method commits. Without an answer to every update, Chrome reported the
/// text it had at focus and nothing after.
#[test]
fn every_update_from_the_app_is_answered_with_a_commit() {
    let mut h = Harness::start();
    h.focus("hi");
    h.pump_until("answer to the focus", |c| {
        c.requests.contains(&Req::Commit(1))
    });
    h.app_state(false, "hi!");
    h.pump_until("answer to the update", |c| {
        c.requests.contains(&Req::Commit(2))
    });
    assert!(
        !h.c.requests
            .iter()
            .any(|r| matches!(r, Req::CommitString(_))),
        "{:?}",
        h.c.requests
    );
}

/// A client that commits on every `done` must not start a loop: an update
/// that reports nothing is not answered.
#[test]
fn an_empty_update_is_not_answered() {
    let mut h = Harness::start();
    h.focus("hi");
    h.pump_until("answer to the focus", |c| {
        c.requests.contains(&Req::Commit(1))
    });
    h.c.im.as_ref().expect("input method").done();
    h.app_state(false, "hi!");
    h.pump_until("answer to the update", |c| {
        c.requests.contains(&Req::Commit(3))
    });
    assert!(
        !h.c.requests.contains(&Req::Commit(2)),
        "{:?}",
        h.c.requests
    );
}

/// Regression: Firefox answers an insert with its old text and sends the new
/// text only on the next key, so the Reply & rewrite insert came back Unknown
/// although the text had landed.
#[test]
fn an_insert_answered_with_old_text_asks_again_and_is_confirmed() {
    let mut h = Harness::start();
    let field = h.focus("hello");
    h.insert(InsertRequest {
        id: 9,
        field,
        text: " world".into(),
        replace: Replace::Nothing,
    });
    h.pump_until("the insert", |c| {
        c.requests.contains(&Req::CommitString(" world".into()))
    });
    h.app_state(false, "hello");
    h.pump_until("a request for the new text", |c| {
        c.requests.contains(&Req::CommitString(String::new()))
    });
    h.app_state(false, "hello world");
    let outcome = h.event("InsertDone", |e| match e {
        Event::InsertDone { id: 9, outcome } => Some(outcome),
        _ => None,
    });
    assert_eq!(outcome, InsertOutcome::Inserted);
    assert_eq!(
        h.asked(&Req::CommitString(String::new())),
        1,
        "{:?}",
        h.c.requests
    );
}

#[test]
fn an_insert_the_app_never_shows_times_out_as_unknown() {
    let mut h = Harness::start();
    let field = h.focus("hello");
    let start = Instant::now();
    h.insert(InsertRequest {
        id: 3,
        field,
        text: " world".into(),
        replace: Replace::Nothing,
    });
    let outcome = h.event("InsertDone", |e| match e {
        Event::InsertDone { id: 3, outcome } => Some(outcome),
        _ => None,
    });
    assert_eq!(outcome, InsertOutcome::Unknown);
    assert!(start.elapsed() >= INSERT_CONFIRM);
}
