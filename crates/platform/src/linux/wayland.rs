//! The Wayland thread: the input method, the key gate, the popup surface and
//! the focused app.

use std::collections::HashMap;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use ghostcaret_engine::io::{
    AppInfo, BackendStatus, DismissReason, Event, FieldId, FieldKind, FieldSnapshot, InsertOutcome,
    InsertRequest, Overlay, Replace,
};
use ghostcaret_render::Image;
use rustix::event::{PollFd, PollFlags, Timespec};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{KeyState, KeymapFormat};
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::ChangeCause;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_keyboard_grab_v2::{
    self, ZwpInputMethodKeyboardGrabV2,
};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::{
    self, ZwpInputMethodV2,
};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_popup_surface_v2::{
    self, ZwpInputPopupSurfaceV2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self, ZwlrForeignToplevelManagerV1,
};

use super::commits::Commits;
use super::field::{self, Surrounding};
use super::keys::{self, Keyboard};
use super::shm::{BufferId, Buffers};
use super::{Command, GATE_LATENCY, RenderJob, Shared, Waker, apps, atspi, status};
use crate::StartError;
use crate::gate::{Gate, TapStyle, Verdict};

/// Longest text in one `commit_string`; protocol messages carry at most 4 KiB.
const COMMIT_CHUNK: usize = 3900;
/// Reconnections after the watchdog cut a stalled connection.
const RECONNECTS: u32 = 3;

/// A connected session, ready to run.
pub(crate) struct Session {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

/// Connects to the compositor the environment names.
pub(crate) fn open() -> Result<Connection, StartError> {
    Connection::connect_to_env().map_err(|e| {
        StartError::Unsupported(format!(
            "no Wayland session: {e} (Ghostcaret needs a Wayland compositor)"
        ))
    })
}

impl Session {
    /// Binds the protocols and becomes the seat's input method.
    pub fn connect(
        conn: Connection,
        events: Sender<Event>,
        shared: Arc<Shared>,
        render: Sender<RenderJob>,
    ) -> Result<Session, StartError> {
        let (globals, mut queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| StartError::Failed(format!("Wayland registry: {e}")))?;
        let qh = queue.handle();
        let g = Globals::bind(&globals, &qh)?;
        let im = g.im_manager.get_input_method(&g.seat, &qh, ());
        let vk = g.vk_manager.create_virtual_keyboard(&g.seat, &qh, ());
        let popup = Popup::new(&g, &im, &qh);
        let mut state = State {
            events,
            shared: shared.clone(),
            render,
            qh,
            im,
            vk,
            vk_keymap: false,
            grab: None,
            keyboard: None,
            gate: Gate::new(TapStyle::Defer),
            active: true,
            announce: false,
            unavailable: false,
            pending: Pending::default(),
            serial: 0,
            field: None,
            next_field: 1,
            keys_since_change: 0,
            commits: Commits::default(),
            popup,
            render_seq: 0,
            toplevels: Vec::new(),
            activated: None,
            names: HashMap::new(),
            announcer: None,
            g,
        };
        queue
            .roundtrip(&mut state)
            .map_err(|e| StartError::Failed(format!("Wayland roundtrip: {e}")))?;
        if state.unavailable {
            return Err(StartError::Unsupported(
                "another input method (such as fcitx5 or IBus) is already running on this seat"
                    .into(),
            ));
        }
        let socket = conn
            .backend()
            .poll_fd()
            .try_clone_to_owned()
            .map_err(|e| StartError::Failed(e.to_string()))?;
        *shared.socket.lock().expect("socket lock") = Some(socket);
        Ok(Session { conn, queue, state })
    }
}

/// Runs sessions until stopped; reconnects after a stall.
pub(crate) fn run(
    mut session: Session,
    commands: Receiver<Command>,
    waker: Arc<Waker>,
    events: Sender<Event>,
    shared: Arc<Shared>,
    render: Sender<RenderJob>,
) {
    let _ = events.send(Event::Status(BackendStatus::Ready));
    let mut reconnects = 0;
    loop {
        match session.run(&commands, &waker) {
            Ok(()) => return,
            Err(reason) => {
                session.state.end_field();
                let stalled = shared.stalled.swap(false, Ordering::AcqRel);
                let _ = events.send(status(if stalled {
                    "the input method stopped responding; keys go straight to apps".to_string()
                } else {
                    format!("the Wayland connection failed: {reason}")
                }));
                if !stalled || reconnects == RECONNECTS {
                    shared.idle();
                    return;
                }
                reconnects += 1;
                match open().and_then(|conn| {
                    Session::connect(conn, events.clone(), shared.clone(), render.clone())
                }) {
                    Ok(s) => {
                        session = s;
                        let _ = events.send(Event::Status(BackendStatus::Ready));
                    }
                    Err(e) => {
                        let _ = events.send(status(e.to_string()));
                        shared.idle();
                        return;
                    }
                }
            }
        }
    }
}

impl Session {
    /// The event loop: Wayland events and commands, never blocking except in
    /// the poll. Returns `Ok` when stopped.
    fn run(&mut self, commands: &Receiver<Command>, waker: &Waker) -> Result<(), String> {
        let state = &mut self.state;
        loop {
            self.queue
                .dispatch_pending(state)
                .map_err(|e| e.to_string())?;
            state.after_dispatch();
            for command in commands.try_iter() {
                if !state.command(command) {
                    let _ = self.conn.flush();
                    return Ok(());
                }
                state.after_dispatch();
            }
            if state.unavailable {
                return Err("the compositor withdrew the input method".into());
            }
            // Before the flush, so what the timers send goes out now.
            state.run_timers(Instant::now());
            let write_blocked = match self.conn.flush() {
                Ok(()) => false,
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    true
                }
                Err(e) => return Err(e.to_string()),
            };
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let timeout = state.next_deadline().map(|d| {
                let left = d.saturating_duration_since(Instant::now());
                Timespec {
                    tv_sec: left.as_secs() as i64,
                    tv_nsec: i64::from(left.subsec_nanos()),
                }
            });
            let mut wl_flags = PollFlags::IN;
            if write_blocked {
                wl_flags |= PollFlags::OUT;
            }
            let (wl_fd, wake_fd) = (guard.connection_fd(), waker.fd());
            let mut fds = [
                PollFd::new(&wl_fd, wl_flags),
                PollFd::new(&wake_fd, PollFlags::IN),
            ];
            state.shared.idle();
            let polled = rustix::event::poll(&mut fds, timeout.as_ref());
            state.shared.busy();
            match polled {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.to_string()),
            }
            let readable = fds[0]
                .revents()
                .intersects(PollFlags::IN | PollFlags::ERR | PollFlags::HUP);
            let woken = fds[1].revents().contains(PollFlags::IN);
            if readable {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.to_string()),
                }
            } else {
                drop(guard);
            }
            if woken {
                waker.drain();
            }
        }
    }
}

struct Globals {
    compositor: WlCompositor,
    shm: WlShm,
    seat: WlSeat,
    im_manager: ZwpInputMethodManagerV2,
    vk_manager: ZwpVirtualKeyboardManagerV1,
    fractional: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
}

impl Globals {
    fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Result<Globals, StartError> {
        let missing = |name: &str, e| {
            StartError::Unsupported(format!("the compositor does not offer {name} ({e})"))
        };
        let globals_ = globals;
        let toplevels = globals_
            .bind::<ZwlrForeignToplevelManagerV1, _, _>(qh, 1..=3, ())
            .ok();
        if toplevels.is_none() {
            log::warn!(
                "zwlr_foreign_toplevel_manager_v1 is missing; per-app rules cannot tell apps apart"
            );
        }
        Ok(Globals {
            compositor: globals
                .bind(qh, 4..=6, ())
                .map_err(|e| missing("wl_compositor", e))?,
            shm: globals
                .bind(qh, 1..=1, ())
                .map_err(|e| missing("wl_shm", e))?,
            seat: globals
                .bind(qh, 1..=9, ())
                .map_err(|e| missing("wl_seat", e))?,
            im_manager: globals.bind(qh, 1..=1, ()).map_err(|e| {
                missing("the input-method protocol (zwp_input_method_manager_v2)", e)
            })?,
            vk_manager: globals.bind(qh, 1..=1, ()).map_err(|e| {
                missing(
                    "the virtual-keyboard protocol (zwp_virtual_keyboard_manager_v1)",
                    e,
                )
            })?,
            fractional: globals.bind(qh, 1..=1, ()).ok(),
            viewporter: globals.bind(qh, 1..=1, ()).ok(),
        })
    }
}

/// Input-method state that applies on the next `done`.
#[derive(Default)]
struct Pending {
    activate: Option<bool>,
    surrounding: Option<Surrounding>,
    caused_by_us: Option<bool>,
    content: Option<(u32, u32)>,
    /// The popup got a new text input rectangle.
    rect: bool,
}

/// The focused text field.
struct Field {
    id: FieldId,
    revision: u64,
    app: AppInfo,
    kind: FieldKind,
    multiline: bool,
    /// `None` until the client sends surrounding text; some never do.
    surrounding: Option<Surrounding>,
}

impl Field {
    fn snapshot(&self) -> FieldSnapshot {
        let mut snap = FieldSnapshot {
            field: self.id,
            revision: self.revision,
            app: self.app.clone(),
            kind: self.kind,
            multiline: self.multiline,
            // The compositor places the popup; global caret coordinates are
            // not available to input methods.
            caret: None,
            before_truncated: self.surrounding.is_none(),
            ..FieldSnapshot::default()
        };
        if let Some(s) = &self.surrounding
            && !self.kind.is_secure_or_structured()
        {
            let p = s.parts();
            snap.before = p.before.to_owned();
            snap.before_truncated = p.before_truncated;
            snap.selection = p.selection.to_owned();
            snap.after = p.after.to_owned();
        }
        snap
    }
}

struct Toplevel {
    handle: ZwlrForeignToplevelHandleV1,
    app_id: String,
    title: String,
    activated: bool,
    pending_activated: Option<bool>,
}

/// The popup surface the compositor shows next to the caret.
struct Popup {
    surface: WlSurface,
    _role: ZwpInputPopupSurfaceV2,
    viewport: Option<WpViewport>,
    _fractional: Option<WpFractionalScaleV1>,
    /// Preferred scale in 120ths, from `wp_fractional_scale_v1`.
    scale120: Option<u32>,
    /// Integer scale from `wl_surface.preferred_buffer_scale`.
    buffer_scale: i32,
    buffers: Buffers,
    shown: bool,
    /// A frame waiting for a buffer the compositor still holds.
    waiting: Option<Image>,
}

impl Popup {
    fn new(g: &Globals, im: &ZwpInputMethodV2, qh: &QueueHandle<State>) -> Popup {
        let surface = g.compositor.create_surface(qh, ());
        // An empty input region: the popup never takes clicks.
        let region = g.compositor.create_region(qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        let role = im.get_input_popup_surface(&surface, qh, ());
        let viewport = g
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, qh, ()));
        let fractional = g
            .fractional
            .as_ref()
            .map(|f| f.get_fractional_scale(&surface, qh, ()));
        Popup {
            surface,
            _role: role,
            viewport,
            _fractional: fractional,
            scale120: None,
            buffer_scale: 1,
            buffers: Buffers::new(),
            shown: false,
            waiting: None,
        }
    }

    /// Output pixels per logical pixel.
    fn scale(&self) -> f32 {
        match (self.scale120, &self.viewport) {
            (Some(s), Some(_)) => s as f32 / 120.0,
            _ => self.buffer_scale as f32,
        }
    }

    fn show(&mut self, image: Image, shm: &WlShm, qh: &QueueHandle<State>) {
        if image.width == 0 || image.height == 0 {
            self.hide();
            return;
        }
        let scale = self.scale();
        let lw = (image.width as f32 / scale).ceil() as i32;
        let lh = (image.height as f32 / scale).ceil() as i32;
        let (bw, bh) = match self.viewport {
            Some(_) => (
                (lw as f32 * scale).round() as u32,
                (lh as f32 * scale).round() as u32,
            ),
            None => (
                (lw * self.buffer_scale) as u32,
                (lh * self.buffer_scale) as u32,
            ),
        };
        let Some(buffer) = self
            .buffers
            .fill(shm, qh, bw, bh, &image.pixels, image.width)
        else {
            self.waiting = Some(image);
            return;
        };
        self.waiting = None;
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, bw as i32, bh as i32);
        match &self.viewport {
            Some(v) => {
                self.surface.set_buffer_scale(1);
                v.set_destination(lw, lh);
            }
            None => self.surface.set_buffer_scale(self.buffer_scale),
        }
        self.surface.commit();
        self.shown = true;
    }

    fn hide(&mut self) {
        self.waiting = None;
        if self.shown {
            self.surface.attach(None, 0, 0);
            self.surface.commit();
            self.shown = false;
        }
    }
}

pub(crate) struct State {
    events: Sender<Event>,
    shared: Arc<Shared>,
    render: Sender<RenderJob>,
    qh: QueueHandle<State>,
    g: Globals,
    im: ZwpInputMethodV2,
    vk: ZwpVirtualKeyboardV1,
    vk_keymap: bool,
    grab: Option<ZwpInputMethodKeyboardGrabV2>,
    keyboard: Option<Keyboard>,
    gate: Gate,
    /// `Backend::set_active`.
    active: bool,
    announce: bool,
    unavailable: bool,
    pending: Pending,
    /// Number of `done` events received, for `commit`.
    serial: u32,
    field: Option<Field>,
    next_field: u64,
    /// Keys handled and text committed since the field last changed, to tell
    /// changes we caused from the caret moving under the mouse.
    keys_since_change: u32,
    commits: Commits,
    popup: Popup,
    /// Identifies the newest render request; older frames are dropped.
    render_seq: u64,
    toplevels: Vec<Toplevel>,
    /// Index into `toplevels` of the activated window.
    activated: Option<usize>,
    /// App names from desktop entries, by app id.
    names: HashMap<String, String>,
    announcer: Option<atspi::Announcer>,
}

impl State {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Returns false on [`Command::Stop`].
    fn command(&mut self, command: Command) -> bool {
        match command {
            Command::Overlay(overlay) => {
                let announce = match &overlay {
                    Overlay::Suggestion(v) if self.announce => Some(v.visible().to_owned()),
                    _ => None,
                };
                self.gate.set_overlay(overlay);
                if let (Some(text), Some(a)) = (announce, &self.announcer)
                    && matches!(self.gate.overlay(), Overlay::Suggestion(_))
                {
                    a.announce(text);
                }
            }
            Command::Bindings(b) => self.gate.set_bindings(b),
            Command::Active(active) => self.set_active(active),
            Command::Insert(request) => self.insert(request),
            Command::Frame { seq, image } => {
                if seq == self.render_seq
                    && !matches!(self.gate.overlay(), Overlay::Hidden)
                    && self.field.is_some()
                {
                    self.popup.show(image, &self.g.shm, &self.qh);
                }
            }
            Command::Announce(on) => {
                self.announce = on;
                if on && self.announcer.is_none() {
                    self.announcer = atspi::Announcer::start();
                }
            }
            Command::Stop => {
                self.popup.hide();
                if let Some(grab) = self.grab.take() {
                    grab.release();
                }
                return false;
            }
        }
        true
    }

    /// Sends what the gate reported and brings the popup up to date.
    fn after_dispatch(&mut self) {
        self.send_gate_events();
        if self.gate.take_display_changed() {
            self.render_seq += 1;
            match self.gate.overlay() {
                Overlay::Hidden => self.popup.hide(),
                overlay => {
                    let _ = self.render.send(RenderJob::Draw {
                        seq: self.render_seq,
                        overlay: overlay.clone(),
                        scale: self.popup.scale(),
                    });
                }
            }
        }
    }

    /// Sends the gate's reports now, so a dismissal reaches the controller
    /// before the focus event that caused it.
    fn send_gate_events(&mut self) {
        let events = &self.events;
        self.gate.drain_events(|e| {
            let _ = events.send(e);
        });
    }

    fn set_active(&mut self, active: bool) {
        if self.active == active {
            return;
        }
        self.active = active;
        self.gate.set_active(active);
        if active {
            if let Some(field) = &self.field {
                self.send(Event::Changed(field.snapshot()));
                self.grab_keyboard();
            }
        } else {
            self.release_keyboard();
            self.popup.hide();
        }
    }

    fn grab_keyboard(&mut self) {
        if self.grab.is_none() {
            self.gate.reset_keys();
            self.grab = Some(self.im.grab_keyboard(&self.qh, ()));
        }
    }

    fn release_keyboard(&mut self) {
        if let Some(grab) = self.grab.take() {
            grab.release();
            self.gate.reset_keys();
        }
    }

    fn on_key(&mut self, time: u32, code: u32, pressed: bool) {
        let start = Instant::now();
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        } as u32;
        let Some(kb) = &self.keyboard else {
            if self.vk_keymap {
                self.vk.key(time, code, state);
            }
            return;
        };
        let (sym, ch) = kb.sym(code);
        let k = keys::key_in(code, pressed, time, sym, ch, kb.mods());
        if pressed {
            self.commits.key_pressed();
        }
        match self.gate.key(&k) {
            Verdict::Pass | Verdict::Tap => self.vk.key(time, code, state),
            Verdict::Replay(withheld) => {
                self.vk.key(time, withheld, KeyState::Pressed as u32);
                self.vk.key(time, code, state);
            }
            Verdict::Consume => {}
            Verdict::Insert => {
                self.im.commit_string(self.gate.accepted().to_owned());
                self.im.commit(self.serial);
                self.commits.committed();
            }
        }
        self.keys_since_change = self.keys_since_change.saturating_add(1);
        GATE_LATENCY.record(start.elapsed().as_nanos() as u64);
    }

    fn insert(&mut self, request: InsertRequest) {
        let done = |outcome| Event::InsertDone {
            id: request.id,
            outcome,
        };
        let Some(field) = &self.field else {
            self.send(done(InsertOutcome::Failed(
                "no text field has focus".into(),
            )));
            return;
        };
        if field.id != request.field {
            self.send(done(InsertOutcome::Failed(
                "the text field lost focus".into(),
            )));
            return;
        }
        let confirmable = field.surrounding.is_some();
        if let Replace::Around { before, after } = request.replace {
            self.im.delete_surrounding_text(before as u32, after as u32);
        }
        // Replacing the selection is what committing text does in every
        // toolkit; `Replace::Selection` needs no request of its own.
        let mut rest = request.text.as_str();
        loop {
            let mut cut = rest.len().min(COMMIT_CHUNK);
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            self.im.commit_string(rest[..cut].to_owned());
            self.im.commit(self.serial);
            rest = &rest[cut..];
            if rest.is_empty() {
                break;
            }
        }
        self.keys_since_change = self.keys_since_change.saturating_add(1);
        self.commits.committed();
        if confirmable {
            self.commits.track(request.id, request.text, Instant::now());
        } else {
            self.send(done(InsertOutcome::Unknown));
        }
    }

    /// Times out inserts, and asks the client again for its text after an
    /// unchanged reply to our commit (see [`super::commits`]).
    fn run_timers(&mut self, now: Instant) {
        let events = &self.events;
        let ask = self.commits.due(now, |id| {
            let _ = events.send(Event::InsertDone {
                id,
                outcome: InsertOutcome::Unknown,
            });
        });
        if ask {
            self.im.commit_string(String::new());
            self.im.commit(self.serial);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.commits.next_deadline()
    }

    /// Applies the pending state on `done` and answers it.
    fn done(&mut self) {
        self.serial = self.serial.wrapping_add(1);
        let p = &self.pending;
        let reported = p.activate.is_some()
            || p.surrounding.is_some()
            || p.content.is_some()
            || p.caused_by_us.is_some()
            || p.rect;
        self.apply_pending();
        // Chromium sends its next state only after a `done` whose serial
        // matches its count of commits, and niri, like wlroots, sends the
        // client `done` only when the input method commits. So every update
        // is answered with a commit. Toolkits do nothing on a `done` that
        // carries nothing, and an update that reports nothing is not
        // answered, so a client that commits on every `done` cannot loop.
        if reported && self.field.is_some() {
            self.im.commit(self.serial);
        }
    }

    fn apply_pending(&mut self) {
        let p = std::mem::take(&mut self.pending);
        match p.activate {
            Some(true) => self.begin_field(),
            Some(false) => {
                self.end_field();
                return;
            }
            None => {}
        }
        let begun = p.activate == Some(true);
        let Some(field) = &mut self.field else { return };
        let mut changed = false;
        if let Some((hint, purpose)) = p.content {
            let (kind, multiline) = field::kind(hint, purpose);
            changed |= (kind, multiline) != (field.kind, field.multiline);
            (field.kind, field.multiline) = (kind, multiline);
        }
        let mut moved = false;
        let mut new_text = false;
        if let Some(s) = p.surrounding {
            if field.surrounding.as_ref() == Some(&s) {
                self.commits.unchanged(Instant::now());
            } else {
                let old_before = field
                    .surrounding
                    .as_ref()
                    .map(|o| o.full_before().to_owned());
                moved = old_before.as_deref() != Some(s.full_before()) || s.cursor != s.anchor;
                field.surrounding = Some(s);
                changed = true;
                new_text = true;
            }
        }
        if begun {
            let snap = field.snapshot();
            *self.shared.focus.lock().expect("focus lock") = Some((snap.field, snap.app.clone()));
            self.gate.set_focus(Some(snap.field));
            self.send_gate_events();
            self.keys_since_change = 0;
            if self.active {
                self.grab_keyboard();
            }
            self.send(Event::Focus(snap));
            return;
        }
        if !changed {
            return;
        }
        field.revision += 1;
        let id = field.id;
        let snap = field.snapshot();
        let ours = p.caused_by_us == Some(true) && self.commits.has_inserts();
        if new_text {
            let before = field.surrounding.as_ref().map_or("", |s| s.full_before());
            let events = &self.events;
            self.commits.changed(before, |id| {
                let _ = events.send(Event::InsertDone {
                    id,
                    outcome: InsertOutcome::Inserted,
                });
            });
        }
        if moved
            && self.keys_since_change == 0
            && !ours
            && matches!(self.gate.overlay(), Overlay::Suggestion(_))
        {
            // The caret moved without a key: a click, or the app changed the
            // text. The suggestion no longer applies here.
            self.gate.set_overlay(Overlay::Hidden);
            self.send(Event::Dismissed {
                field: id,
                reason: DismissReason::CaretMoved,
            });
        }
        self.keys_since_change = 0;
        if self.active {
            self.send(Event::Changed(snap));
        }
    }

    fn begin_field(&mut self) {
        if self.field.is_some() {
            self.end_field();
        }
        let id = FieldId(self.next_field);
        self.next_field += 1;
        let app = self.app_info();
        self.field = Some(Field {
            id,
            revision: 0,
            app,
            kind: FieldKind::Text,
            multiline: false,
            surrounding: None,
        });
    }

    /// The field lost focus.
    fn end_field(&mut self) {
        if self.field.take().is_none() {
            return;
        }
        *self.shared.focus.lock().expect("focus lock") = None;
        self.gate.set_focus(None);
        self.send_gate_events();
        self.release_keyboard();
        self.popup.hide();
        let events = &self.events;
        self.commits.clear(|id| {
            let _ = events.send(Event::InsertDone {
                id,
                outcome: InsertOutcome::Unknown,
            });
        });
        self.send(Event::Blur);
    }

    fn app_info(&mut self) -> AppInfo {
        let Some(t) = self.activated.and_then(|i| self.toplevels.get(i)) else {
            return AppInfo::default();
        };
        let (id, title) = (t.app_id.clone(), t.title.clone());
        let name = self
            .names
            .entry(id.clone())
            .or_insert_with(|| apps::name(&id))
            .clone();
        AppInfo { id, name, title }
    }

    /// A different window was activated while a field is focused: the field
    /// belongs to it now, so it is a new focus.
    fn activated_changed(&mut self) {
        let app = self.app_info();
        let Some(field) = &mut self.field else { return };
        if field.app.id == app.id {
            field.app.title = app.title;
            return;
        }
        let surrounding = field.surrounding.take();
        let (kind, multiline) = (field.kind, field.multiline);
        self.begin_field();
        if let Some(field) = &mut self.field {
            (field.surrounding, field.kind, field.multiline) = (surrounding, kind, multiline);
            let snap = field.snapshot();
            *self.shared.focus.lock().expect("focus lock") = Some((snap.field, snap.app.clone()));
            self.gate.set_focus(Some(snap.field));
            self.send_gate_events();
            if self.active {
                self.grab_keyboard();
            }
            self.send(Event::Focus(snap));
        }
    }

    fn redraw(&mut self) {
        if !matches!(self.gate.overlay(), Overlay::Hidden) {
            self.render_seq += 1;
            let _ = self.render.send(RenderJob::Draw {
                seq: self.render_seq,
                overlay: self.gate.overlay().clone(),
                scale: self.popup.scale(),
            });
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlRegion);
delegate_noop!(State: ZwpInputMethodManagerV2);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ZwpVirtualKeyboardV1);
delegate_noop!(State: WpFractionalScaleManagerV1);
delegate_noop!(State: WpViewporter);
delegate_noop!(State: WpViewport);

impl Dispatch<ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_input_method_v2::Event as E;
        match event {
            E::Activate => {
                // Activation resets every other pending value.
                state.pending = Pending {
                    activate: Some(true),
                    ..Pending::default()
                };
            }
            E::Deactivate => state.pending.activate = Some(false),
            E::SurroundingText {
                text,
                cursor,
                anchor,
            } => {
                state.pending.surrounding = Some(Surrounding {
                    text,
                    cursor,
                    anchor,
                });
            }
            E::TextChangeCause { cause } => {
                state.pending.caused_by_us =
                    Some(matches!(cause, WEnum::Value(ChangeCause::InputMethod)));
            }
            E::ContentType { hint, purpose } => {
                let hint = match hint {
                    WEnum::Value(h) => h.bits(),
                    WEnum::Unknown(u) => u,
                };
                let purpose = match purpose {
                    WEnum::Value(p) => p as u32,
                    WEnum::Unknown(u) => u,
                };
                state.pending.content = Some((hint, purpose));
            }
            E::Done => state.done(),
            E::Unavailable => state.unavailable = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_input_method_keyboard_grab_v2::Event as E;
        match event {
            E::Key {
                time,
                key,
                state: key_state,
                ..
            } => {
                state.on_key(time, key, key_state == WEnum::Value(KeyState::Pressed));
            }
            E::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(kb) = &mut state.keyboard {
                    kb.update_mask(mods_depressed, mods_latched, mods_locked, group);
                }
                if state.vk_keymap {
                    state
                        .vk
                        .modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            E::Keymap { format, fd, size } => {
                if format != WEnum::Value(KeymapFormat::XkbV1) {
                    log::warn!("the keyboard grab sent a keymap format other than xkb v1");
                    return;
                }
                state
                    .vk
                    .keymap(KeymapFormat::XkbV1 as u32, fd.as_fd(), size);
                state.vk_keymap = true;
                state.keyboard = Keyboard::new(fd, size, state.keyboard.take());
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            log::debug!("text input rectangle relative to the popup: {x},{y} {width}x{height}");
            state.pending.rect = true;
        }
    }
}

impl Dispatch<WlSurface, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event
            && factor != state.popup.buffer_scale
        {
            state.popup.buffer_scale = factor.max(1);
            if state.popup.scale120.is_none() {
                state.redraw();
            }
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && state.popup.scale120 != Some(scale)
        {
            state.popup.scale120 = Some(scale);
            state.redraw();
        }
    }
}

impl Dispatch<WlBuffer, BufferId> for State {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        id: &BufferId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.popup.buffers.released(*id);
            if let Some(image) = state.popup.waiting.take() {
                state.popup.show(image, &state.g.shm, &state.qh);
            }
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(Toplevel {
                handle: toplevel,
                app_id: String::new(),
                title: String::new(),
                activated: false,
                pending_activated: None,
            });
        }
    }

    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event as E;
        let Some(i) = state.toplevels.iter().position(|t| &t.handle == handle) else {
            return;
        };
        match event {
            E::Title { title } => state.toplevels[i].title = title,
            E::AppId { app_id } => state.toplevels[i].app_id = app_id,
            E::State { state: bytes } => {
                let activated = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|&c| u32::from_ne_bytes(c) == ACTIVATED);
                state.toplevels[i].pending_activated = Some(activated);
            }
            E::Done => {
                let t = &mut state.toplevels[i];
                let was = t.activated;
                if let Some(a) = t.pending_activated.take() {
                    t.activated = a;
                }
                if t.activated && (!was || state.activated == Some(i)) {
                    state.activated = Some(i);
                    state.activated_changed();
                } else if was && !t.activated && state.activated == Some(i) {
                    state.activated = None;
                }
            }
            E::Closed => {
                handle.destroy();
                state.toplevels.remove(i);
                state.activated = match state.activated {
                    Some(a) if a == i => None,
                    Some(a) if a > i => Some(a - 1),
                    other => other,
                };
            }
            _ => {}
        }
    }
}

/// `zwlr_foreign_toplevel_handle_v1.state.activated`.
const ACTIVATED: u32 = 2;

#[cfg(test)]
mod tests;
