//! Linux backend: a native Wayland input method.
//!
//! Ghostcaret binds `zwp_input_method_v2` itself, so it works on any
//! compositor with the input-method protocols (niri, sway, Hyprland, KDE)
//! without fcitx5 or IBus. Apps reach it through `zwp_text_input_v3`: GTK,
//! Qt 6, Firefox, and Chromium or Electron started with
//! `--enable-wayland-ime --wayland-text-input-version=3`.
//!
//! - Focus, text and caret: `activate`/`deactivate`, `surrounding_text` and
//!   `content_type`; the app comes from `zwlr_foreign_toplevel_manager_v1`.
//! - Keys: a keyboard grab while a field is focused. The gate decides each
//!   key; keys it does not consume are forwarded unchanged through a virtual
//!   keyboard that uses the grab's own keymap.
//! - Display: an input-method popup surface, which the compositor places at
//!   the text field's caret, drawn by `ghostcaret-render` on its own thread.
//! - Insert: `delete_surrounding_text` + `commit_string` + `commit`,
//!   confirmed by the surrounding text that follows, see [`commits`].
//!   Every state update from the app is answered with a `commit`, which
//!   Chromium needs before it sends the next one.
//! - Conversation: AT-SPI, see [`atspi`].
//!
//! Threads: one owns the Wayland connection and the key gate and never
//! blocks; one renders; a watchdog cuts the connection if the Wayland thread
//! stalls, so the compositor drops the grab and keys flow to apps again.

mod apps;
mod atspi;
mod bus;
mod commits;
mod field;
mod keys;
mod portal;
mod shm;
mod wayland;

use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use ghostcaret_engine::io::{
    AppInfo, Backend, BackendStatus, Bindings, Conversation, Event, FieldId, InsertRequest, Overlay,
};
use ghostcaret_engine::settings::{Appearance, GhostStyle};
use ghostcaret_render::{Image, Renderer, Theme};

use crate::{StartError, latency};

/// Time the key gate spends per key event on the Wayland thread: from the
/// key event reaching the dispatcher to its verdict being queued for the
/// compositor.
pub fn gate_latency() -> latency::Summary {
    GATE_LATENCY.summary()
}

static GATE_LATENCY: latency::Histogram = latency::Histogram::new();

/// Body text size of the overlay in logical pixels.
const FONT_PX: f32 = 15.0;
/// Widest Reply & rewrite card in logical pixels.
const CARD_MAX_PX: f32 = 460.0;
/// The Wayland thread is stalled when it spends this long outside its poll.
const STALL: Duration = Duration::from_millis(500);

pub fn start(events: Sender<Event>) -> Result<Arc<dyn Backend>, StartError> {
    Ok(LinuxBackend::start(events)?)
}

/// Messages to the Wayland thread.
pub(crate) enum Command {
    Overlay(Overlay),
    Bindings(Bindings),
    Active(bool),
    Insert(InsertRequest),
    /// A rendered overlay for the popup; `seq` names the request.
    Frame {
        seq: u64,
        image: Image,
    },
    /// Announce shown suggestions to screen readers.
    Announce(bool),
    Stop,
}

/// What the render thread draws.
pub(crate) enum RenderJob {
    Draw {
        seq: u64,
        overlay: Overlay,
        scale: f32,
    },
    Bindings(Bindings),
    Style(GhostStyle),
    Dark(bool),
    Stop,
}

/// State shared between the Wayland thread and the handle.
pub(crate) struct Shared {
    /// The focused field and its app, for [`Backend::conversation`].
    pub focus: Mutex<Option<(FieldId, AppInfo)>>,
    epoch: Instant,
    /// Milliseconds since `epoch` (plus one) when the Wayland thread left its
    /// poll; zero while it waits in the poll.
    busy_since: AtomicU64,
    watchdog_parked: AtomicBool,
    watchdog: Mutex<Option<thread::Thread>>,
    /// A duplicate of the Wayland socket, which the watchdog shuts down.
    socket: Mutex<Option<OwnedFd>>,
    /// Set when the watchdog cut a stalled connection.
    stalled: AtomicBool,
}

impl Shared {
    pub fn new() -> Shared {
        Shared {
            focus: Mutex::new(None),
            epoch: Instant::now(),
            busy_since: AtomicU64::new(0),
            watchdog_parked: AtomicBool::new(false),
            watchdog: Mutex::new(None),
            socket: Mutex::new(None),
            stalled: AtomicBool::new(false),
        }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64 + 1
    }

    /// Called when the Wayland thread starts work after waiting.
    pub fn busy(&self) {
        self.busy_since.store(self.now_ms(), Ordering::Relaxed);
        if self.watchdog_parked.swap(false, Ordering::AcqRel)
            && let Some(t) = self.watchdog.lock().expect("watchdog lock").as_ref()
        {
            t.unpark();
        }
    }

    /// Called before the Wayland thread waits.
    pub fn idle(&self) {
        self.busy_since.store(0, Ordering::Relaxed);
    }

    /// Shuts the Wayland socket down. The compositor then drops the input
    /// method and its keyboard grab, so keys reach apps directly again.
    fn cut(&self) {
        if let Some(fd) = self.socket.lock().expect("socket lock").as_ref() {
            self.stalled.store(true, Ordering::Release);
            let _ = rustix::net::shutdown(fd, rustix::net::Shutdown::Both);
        }
    }
}

/// Wakes the Wayland thread's poll.
pub(crate) struct Waker(OwnedFd);

impl Waker {
    pub fn new() -> Result<Waker, StartError> {
        use rustix::event::EventfdFlags;
        rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .map(Waker)
            .map_err(|e| StartError::Failed(format!("cannot create an eventfd: {e}")))
    }

    pub fn wake(&self) {
        // An eventfd write only fails when the counter would overflow, in
        // which case the thread is awake anyway.
        let _ = rustix::io::write(&self.0, &1u64.to_ne_bytes());
    }

    pub fn drain(&self) {
        let mut buf = [0u8; 8];
        let _ = rustix::io::read(&self.0, &mut buf);
    }

    pub fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// The Linux backend handle. Dropping the last reference stops its threads.
pub struct LinuxBackend {
    commands: Sender<Command>,
    render: Sender<RenderJob>,
    waker: Arc<Waker>,
    shared: Arc<Shared>,
    conversation: Mutex<atspi::Reader>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for LinuxBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxBackend").finish_non_exhaustive()
    }
}

impl LinuxBackend {
    /// Connects to the Wayland compositor and becomes the seat's input
    /// method. Fails with [`StartError::Unsupported`] naming what is missing
    /// when the session has no Wayland display, lacks a required protocol,
    /// or another input method is already running.
    pub fn start(events: Sender<Event>) -> Result<Arc<LinuxBackend>, StartError> {
        let shared = Arc::new(Shared::new());
        let waker = Arc::new(Waker::new()?);
        let (commands, command_rx) = crossbeam_channel::unbounded();
        let (render, render_rx) = crossbeam_channel::unbounded();

        let mut threads = Vec::new();
        threads.push(spawn("ghostcaret-render", {
            let (commands, waker) = (commands.clone(), waker.clone());
            move || render_loop(render_rx, commands, waker)
        })?);
        // The session connects on its own thread because keyboard state is
        // not `Send`; the outcome comes back before `start` returns.
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        threads.insert(
            0,
            spawn("ghostcaret-wayland", {
                let (shared, waker, render) = (shared.clone(), waker.clone(), render.clone());
                move || match wayland::open().and_then(|conn| {
                    wayland::Session::connect(conn, events.clone(), shared.clone(), render.clone())
                }) {
                    Ok(session) => {
                        let _ = ready_tx.send(Ok(()));
                        wayland::run(session, command_rx, waker, events, shared, render);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })?,
        );
        let ready = ready_rx
            .recv()
            .unwrap_or_else(|_| Err(StartError::Failed("the Wayland thread exited".into())));
        if let Err(e) = ready {
            let _ = render.send(RenderJob::Stop);
            for t in threads {
                let _ = t.join();
            }
            return Err(e);
        }
        let watchdog = spawn("ghostcaret-watchdog", {
            let shared = shared.clone();
            move || watchdog(shared)
        })?;
        *shared.watchdog.lock().expect("watchdog lock") = Some(watchdog.thread().clone());
        // The watchdog parks for good once the Wayland thread is gone; it is
        // not joined.
        drop(watchdog);
        spawn_theme_watch(render.clone());
        Ok(Arc::new(LinuxBackend {
            commands,
            render,
            waker,
            shared,
            conversation: Mutex::new(atspi::Reader::default()),
            threads: Mutex::new(threads),
        }))
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            self.waker.wake();
        }
    }
}

impl Backend for LinuxBackend {
    fn set_overlay(&self, overlay: Overlay) {
        self.send(Command::Overlay(overlay));
    }

    fn set_bindings(&self, bindings: Bindings) {
        let _ = self.render.send(RenderJob::Bindings(bindings.clone()));
        self.send(Command::Bindings(bindings));
    }

    fn set_active(&self, active: bool) {
        self.send(Command::Active(active));
    }

    fn set_appearance(&self, appearance: Appearance) {
        let _ = self.render.send(RenderJob::Style(appearance.style));
        self.send(Command::Announce(appearance.announce));
    }

    fn insert(&self, request: InsertRequest) {
        self.send(Command::Insert(request));
    }

    fn conversation(&self, field: FieldId) -> Option<Conversation> {
        let app = match &*self.shared.focus.lock().expect("focus lock") {
            Some((focused, app)) if *focused == field => app.clone(),
            _ => return None,
        };
        self.conversation
            .lock()
            .expect("conversation lock")
            .read(&app)
    }
}

impl Drop for LinuxBackend {
    fn drop(&mut self) {
        self.send(Command::Stop);
        let _ = self.render.send(RenderJob::Stop);
        for t in self.threads.lock().expect("threads lock").drain(..) {
            let _ = t.join();
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, StartError> {
    thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| StartError::Failed(format!("cannot start the {name} thread: {e}")))
}

/// Draws overlays off the Wayland thread. Only the newest pending drawing is
/// rendered; older ones are skipped.
fn render_loop(jobs: Receiver<RenderJob>, commands: Sender<Command>, waker: Arc<Waker>) {
    let mut renderer = Renderer::new();
    let mut theme = Theme {
        font_px: FONT_PX,
        ..Theme::default()
    };
    while let Ok(first) = jobs.recv() {
        let mut draw = None;
        for job in std::iter::once(first).chain(jobs.try_iter()) {
            match job {
                RenderJob::Draw {
                    seq,
                    overlay,
                    scale,
                } => draw = Some((seq, overlay, scale)),
                RenderJob::Bindings(b) => renderer.set_bindings(&b),
                RenderJob::Style(style) => theme.style = style,
                RenderJob::Dark(dark) => theme.dark = dark,
                RenderJob::Stop => return,
            }
        }
        let Some((seq, overlay, scale)) = draw else {
            continue;
        };
        theme.scale = scale;
        let image = match &overlay {
            Overlay::Hidden => Image::default(),
            Overlay::Suggestion(view) => renderer.suggestion(view, &theme),
            Overlay::Card(card) => {
                renderer.card(card, &theme, (CARD_MAX_PX * scale).round() as u32)
            }
        };
        if commands.send(Command::Frame { seq, image }).is_err() {
            return;
        }
        waker.wake();
    }
}

/// Cuts the Wayland connection when the Wayland thread stays busy longer
/// than [`STALL`]. Sleeps only while that thread is working; parks otherwise.
fn watchdog(shared: Arc<Shared>) {
    loop {
        shared.watchdog_parked.store(true, Ordering::Release);
        if shared.busy_since.load(Ordering::Relaxed) == 0 {
            thread::park();
        }
        shared.watchdog_parked.store(false, Ordering::Release);
        loop {
            thread::sleep(STALL / 2);
            let since = shared.busy_since.load(Ordering::Relaxed);
            if since == 0 {
                break;
            }
            if shared.now_ms().saturating_sub(since) > STALL.as_millis() as u64 {
                shared.cut();
                break;
            }
        }
    }
}

/// Follows the desktop's light or dark preference through the XDG settings
/// portal, without polling.
fn spawn_theme_watch(render: Sender<RenderJob>) {
    let _ = thread::Builder::new()
        .name("ghostcaret-theme".into())
        .spawn(move || {
            let Ok(watch) = portal::ColorScheme::connect() else {
                return;
            };
            if let Some(dark) = watch.dark() {
                let _ = render.send(RenderJob::Dark(dark));
            }
            for dark in watch.changes() {
                if render.send(RenderJob::Dark(dark)).is_err() {
                    return;
                }
            }
        });
}

/// Turns a status into the event the controller expects.
pub(crate) fn status(reason: impl Into<String>) -> Event {
    Event::Status(BackendStatus::Unavailable(reason.into()))
}
