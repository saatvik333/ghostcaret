//! The hook thread: owns the key gate and the `WH_KEYBOARD_LL` hook.
//!
//! The hook callback runs on this thread inside its message loop, so the gate
//! state needs no lock: commands from other threads are applied between
//! callbacks. The callback decides, types accepted text with one `SendInput`
//! (which Windows queues ahead of the next key, because the next key waits
//! for this callback to return), and returns. Events are built and sent once
//! the queue is empty.

use std::cell::RefCell;
use std::time::Instant;

use crossbeam_channel::Sender;
use oft_engine::io::{BackendStatus, Event, FieldId, Key, Overlay};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, INPUT, VK_CAPITAL};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use super::keys::{self, CharTable, ModState, PacketChars};
use super::mailbox::{Inbox, Mailbox, Ready, run};
use super::overlay;
use super::send::{self, INJECTED};
use crate::gate::{Gate, KeyIn, TapStyle, Typed, Verdict};
use crate::latency::{Histogram, Summary};

pub enum Command {
    Overlay(Overlay),
    Bindings(oft_engine::io::Bindings),
    Active(bool),
    /// The focused field; sent by the UI Automation thread before it reports
    /// the focus, so overlays for the new field are accepted.
    Focus(Option<FieldId>),
    /// The foreground keyboard layout and whether an input method is open.
    Layout {
        table: Box<CharTable>,
        ime_open: bool,
    },
}

static GATE_LATENCY: Histogram = Histogram::new();

/// Time the hook callback spends per key, from entry to return.
pub fn gate_latency() -> Summary {
    GATE_LATENCY.summary()
}

/// Input events reserved for typing an accepted suggestion without growing.
const INPUTS: usize = 1024;

struct Hook {
    gate: Gate,
    mods: ModState,
    table: Box<CharTable>,
    packets: PacketChars,
    ime_open: bool,
    inputs: Vec<INPUT>,
}

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

impl Hook {
    fn key(&mut self, kb: &KBDLLHOOKSTRUCT, msg: u32) -> bool {
        let start = Instant::now();
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        // Low-level hooks report virtual keys below 0xFF.
        let vk = kb.vkCode as u8;
        let k = if vk == keys::VK_PACKET {
            // Unicode typed by another program, such as an on-screen keyboard.
            let ch = if down {
                self.packets.push(kb.scanCode as u16)
            } else {
                None
            };
            if down && ch.is_none() {
                return false;
            }
            KeyIn {
                code: keys::code(vk, false),
                down,
                time_ms: kb.time,
                key: ch.map(Key::Char),
                text: ch.map_or(Typed::None, Typed::Char),
                modifier: None,
                mods: self.mods.mods(),
            }
        } else {
            if !self.mods.record(vk, kb.scanCode, down) {
                return false;
            }
            let extended = kb.flags.0 & LLKHF_EXTENDED.0 != 0;
            let mut k = keys::key_in(vk, extended, down, kb.time, &self.mods, &self.table);
            if self.ime_open && matches!(k.text, Typed::Char(_)) {
                k.text = Typed::Unknown;
            }
            k
        };
        let held_alt = k.mods.alt || k.mods.meta;
        let consume = match self.gate.key(&k) {
            Verdict::Pass | Verdict::Replay(_) => false,
            Verdict::Tap => {
                self.inject_mask();
                false
            }
            Verdict::Consume => {
                if held_alt {
                    self.inject_mask();
                }
                true
            }
            Verdict::Insert => {
                self.inputs.clear();
                send::text(&mut self.inputs, self.gate.accepted());
                if held_alt {
                    send::mask(&mut self.inputs);
                }
                send::send(&self.inputs);
                true
            }
        };
        GATE_LATENCY.record(start.elapsed().as_nanos() as u64);
        consume
    }

    fn inject_mask(&mut self) {
        self.inputs.clear();
        send::mask(&mut self.inputs);
        send::send(&self.inputs);
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT valid for this call.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let ours = kb.flags.0 & LLKHF_INJECTED.0 != 0 && kb.dwExtraInfo == INJECTED;
        let consume = !ours
            && HOOK.with(|h| {
                // Busy means re-entered from another hook's SendInput while this
                // one is typing: let that key through untouched.
                let Ok(mut h) = h.try_borrow_mut() else {
                    return false;
                };
                h.as_mut().is_some_and(|h| h.key(kb, wparam.0 as u32))
            });
        if consume {
            return LRESULT(1);
        }
    }
    // SAFETY: forwards exactly what the system passed in.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn install() -> Result<HHOOK, String> {
    // SAFETY: the module handle of the running executable; the procedure lives in it.
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| format!("no module handle: {e}"))?;
    // SAFETY: keyboard_proc has the HOOKPROC signature and lives for the program.
    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module.into()), 0) }
        .map_err(|e| format!("cannot install the keyboard hook: {e}"))
}

fn uninstall(hook: &mut Option<HHOOK>) {
    if let Some(h) = hook.take() {
        // SAFETY: `h` was returned by SetWindowsHookExW on this thread.
        let _ = unsafe { UnhookWindowsHookEx(h) };
    }
}

pub fn run_thread(
    inbox: Inbox<Command>,
    ready: Ready,
    events: Sender<Event>,
    overlay: Mailbox<overlay::Msg>,
) {
    // SAFETY: reads this thread's view of the toggle; the low bit is Caps Lock.
    let caps = unsafe { GetKeyState(i32::from(VK_CAPITAL.0)) } & 1 != 0;
    HOOK.with(|h| {
        *h.borrow_mut() = Some(Hook {
            gate: Gate::new(TapStyle::Pass),
            mods: ModState::new(caps),
            table: Box::new(CharTable::empty()),
            packets: PacketChars::default(),
            ime_open: false,
            inputs: Vec::with_capacity(INPUTS),
        })
    });
    let mut hook = match install() {
        Ok(h) => Some(h),
        Err(e) => return ready.fail(e),
    };
    ready.ok();
    let mut active = true;
    run(
        || {
            for cmd in inbox.drain() {
                match cmd {
                    Command::Overlay(o) => with_hook(|h| h.gate.set_overlay(o)),
                    Command::Bindings(b) => with_hook(|h| h.gate.set_bindings(b)),
                    Command::Active(a) => {
                        active = a;
                        with_hook(|h| {
                            h.gate.set_active(a);
                            h.gate.reset_keys();
                        });
                        if a {
                            reinstall(&mut hook, &events);
                        } else {
                            uninstall(&mut hook);
                        }
                    }
                    Command::Focus(f) => {
                        with_hook(|h| h.gate.set_focus(f));
                        // Windows silently removes a low-level hook that once
                        // missed its timeout; reinstalling on each focus change
                        // keeps that from lasting.
                        if active && f.is_some() {
                            reinstall(&mut hook, &events);
                        }
                    }
                    Command::Layout { table, ime_open } => with_hook(|h| {
                        h.table = table;
                        h.ime_open = ime_open;
                    }),
                }
            }
        },
        || {
            with_hook(|h| {
                let mut consumed = String::new();
                h.gate.drain_events(|e| {
                    if let Event::Accepted { text, .. } | Event::TypedThrough { text, .. } = &e {
                        consumed.push_str(text);
                    }
                    let _ = events.send(e);
                });
                if h.gate.take_display_changed() {
                    overlay.post(overlay::Msg::Overlay {
                        overlay: h.gate.overlay().clone(),
                        consumed,
                    });
                }
            });
            u32::MAX
        },
    );
    uninstall(&mut hook);
    HOOK.with(|h| h.borrow_mut().take());
}

fn with_hook<R>(f: impl FnOnce(&mut Hook) -> R) -> R {
    HOOK.with(|h| {
        f(h.borrow_mut()
            .as_mut()
            .expect("the hook thread owns its state"))
    })
}

fn reinstall(hook: &mut Option<HHOOK>, events: &Sender<Event>) {
    uninstall(hook);
    match install() {
        Ok(h) => *hook = Some(h),
        Err(e) => {
            let _ = events.send(Event::Status(BackendStatus::Unavailable(e)));
        }
    }
}
