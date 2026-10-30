//! The insert thread: insertions the controller asks for (drafts, rewrites,
//! alternatives), typed as Unicode keystrokes or pasted through the clipboard,
//! which is restored as soon as the app has taken the text.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossbeam_channel::Sender;
use oft_engine::io::{Event, InsertOutcome, InsertRequest, Replace};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardOwner,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_LEFT, VK_RIGHT, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, KillTimer, PostMessageW,
    RegisterClassExW, SetTimer, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROYCLIPBOARD,
    WM_RENDERALLFORMATS, WM_RENDERFORMAT, WM_TIMER, WNDCLASSEXW,
};
use windows::core::{PCWSTR, w};

use super::backend::Shared;
use super::mailbox::{Inbox, Mailbox, Ready, run};
use super::send;
use super::text::{caret_stops, paste_instead};
use super::uia;

/// An insertion with the text around the caret it was asked against, for
/// working out what `Replace::Around` covers.
pub struct Request {
    pub request: InsertRequest,
    pub before: String,
    pub after: String,
}

const WM_RESTORE: u32 = WM_APP + 2;
const PASTE_DEADLINE: usize = 1;
const RESTORE_RETRY: usize = 2;
/// How long the app has to ask for pasted text before the paste is given up.
const PASTE_DEADLINE_MS: u32 = 2000;
const RESTORE_RETRY_MS: u32 = 20;
const RESTORE_RETRIES: u32 = 50;
/// Most clipboard bytes kept for restoring; larger content is not saved.
const SAVED_MAX: usize = 64 << 20;
const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);

/// A paste in flight: the text offered by delayed rendering and the
/// clipboard content to put back.
struct Paste {
    id: u64,
    text: Vec<u16>,
    saved: Vec<(u32, Vec<u8>)>,
    rendered: bool,
    restoring: bool,
    retries: u32,
}

struct Thread {
    hwnd: HWND,
    events: Sender<Event>,
    queue: VecDeque<Request>,
}

thread_local! {
    static PASTE: RefCell<Option<Paste>> = const { RefCell::new(None) };
    static THREAD: RefCell<Option<Thread>> = const { RefCell::new(None) };
}

fn done(events: &Sender<Event>, id: u64, outcome: InsertOutcome) {
    let _ = events.send(Event::InsertDone { id, outcome });
}

fn pasting() -> bool {
    PASTE.with(|p| p.borrow().is_some())
}

/// Clipboard formats whose handles are global memory that can be copied.
fn is_memory(format: u32) -> bool {
    // CF_BITMAP, CF_METAFILEPICT, CF_PALETTE, CF_ENHMETAFILE and the display
    // formats are GDI handles; 0x200..=0x3FF are private and GDI-object ranges.
    !matches!(format, 2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E)
        && !(0x200..=0x3FF).contains(&format)
}

fn open_clipboard(hwnd: HWND) -> bool {
    for attempt in 0..6u32 {
        // SAFETY: our window.
        if unsafe { OpenClipboard(Some(hwnd)) }.is_ok() {
            return true;
        }
        // Another app holds the clipboard for a moment; back off briefly.
        std::thread::sleep(Duration::from_millis(2 << attempt));
    }
    false
}

/// Copies every memory-backed format on the open clipboard.
fn save_formats() -> Vec<(u32, Vec<u8>)> {
    let mut saved = Vec::new();
    let mut total = 0;
    let mut format = 0;
    loop {
        // SAFETY: the clipboard is open on this thread.
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        if !is_memory(format) {
            continue;
        }
        // SAFETY: the clipboard is open; the handle stays owned by the clipboard.
        let Ok(handle) = (unsafe { GetClipboardData(format) }) else {
            continue;
        };
        let global = HGLOBAL(handle.0);
        // SAFETY: memory formats hold global memory handles.
        let size = unsafe { GlobalSize(global) };
        if size == 0 || total + size > SAVED_MAX {
            continue;
        }
        // SAFETY: as above; locked only while copying `size` bytes.
        unsafe {
            let p = GlobalLock(global);
            if p.is_null() {
                continue;
            }
            saved.push((
                format,
                std::slice::from_raw_parts(p.cast::<u8>(), size).to_vec(),
            ));
            let _ = GlobalUnlock(global);
        }
        total += size;
    }
    saved
}

/// Puts `bytes` on the open clipboard as `format`.
fn put(format: u32, bytes: &[u8]) {
    // SAFETY: allocates, fills and hands over global memory; on failure it is freed.
    unsafe {
        let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else {
            return;
        };
        let p = GlobalLock(global);
        if p.is_null() {
            let _ = GlobalFree(Some(global));
            return;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(global);
        if SetClipboardData(format, Some(HANDLE(global.0))).is_err() {
            let _ = GlobalFree(Some(global));
        }
    }
}

fn format_named(name: PCWSTR) -> u32 {
    // SAFETY: a static, NUL-terminated name.
    unsafe { RegisterClipboardFormatW(name) }
}

/// Keeps what Ghostcaret puts on the clipboard out of clipboard history,
/// cloud clipboard and clipboard managers.
fn put_private_markers() {
    put(
        format_named(w!("ExcludeClipboardContentFromMonitorProcessing")),
        &0u32.to_le_bytes(),
    );
    put(
        format_named(w!("CanIncludeInClipboardHistory")),
        &0u32.to_le_bytes(),
    );
    put(
        format_named(w!("CanUploadToCloudClipboard")),
        &0u32.to_le_bytes(),
    );
}

fn unicode_bytes(text: &[u16]) -> Vec<u8> {
    text.iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// Puts the saved content back. Returns false while another app still has
/// the clipboard open (it is reading the pasted text).
fn restore(hwnd: HWND) -> bool {
    // SAFETY: our window.
    if unsafe { OpenClipboard(Some(hwnd)) }.is_err() {
        return false;
    }
    let saved = PASTE.with(|p| {
        let mut p = p.borrow_mut();
        let p = p.as_mut().expect("restoring a paste");
        p.restoring = true;
        std::mem::take(&mut p.saved)
    });
    // SAFETY: the clipboard is open on this thread.
    unsafe {
        let _ = EmptyClipboard();
    }
    for (format, bytes) in &saved {
        put(*format, bytes);
    }
    // The restored content was already in history when it was first copied.
    put_private_markers();
    // SAFETY: opened above.
    unsafe {
        let _ = CloseClipboard();
    }
    true
}

/// Ends the paste in flight and starts the next queued insertion.
fn finish(hwnd: HWND, outcome: impl FnOnce(bool) -> InsertOutcome) {
    // SAFETY: our window's timers; missing ones are ignored.
    unsafe {
        let _ = KillTimer(Some(hwnd), PASTE_DEADLINE);
        let _ = KillTimer(Some(hwnd), RESTORE_RETRY);
    }
    let Some(p) = PASTE.with(|p| p.borrow_mut().take()) else {
        return;
    };
    THREAD.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            done(&t.events, p.id, outcome(p.rendered));
        }
    });
    pump();
}

fn try_restore(hwnd: HWND) {
    if restore(hwnd) {
        finish(hwnd, |rendered| {
            if rendered {
                InsertOutcome::Inserted
            } else {
                InsertOutcome::Failed("the app did not paste".into())
            }
        });
        return;
    }
    let retry = PASTE.with(|p| {
        let mut p = p.borrow_mut();
        let p = p.as_mut().expect("restoring a paste");
        p.retries += 1;
        p.retries <= RESTORE_RETRIES
    });
    if retry {
        // SAFETY: our window.
        unsafe { SetTimer(Some(hwnd), RESTORE_RETRY, RESTORE_RETRY_MS, None) };
    } else {
        log::warn!("the clipboard stayed busy; the previous clipboard content was not restored");
        finish(hwnd, |_| InsertOutcome::Unknown);
    }
}

unsafe extern "system" fn clipboard_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // An app is pasting: hand it the text (the clipboard is open on its side).
        WM_RENDERFORMAT if wparam.0 == usize::from(CF_UNICODETEXT.0) => {
            let rendered = PASTE.with(|p| {
                let Ok(mut p) = p.try_borrow_mut() else {
                    return false;
                };
                let Some(p) = p.as_mut() else { return false };
                put(u32::from(CF_UNICODETEXT.0), &unicode_bytes(&p.text));
                p.rendered = true;
                true
            });
            if rendered {
                // SAFETY: our window.
                let _ = unsafe { PostMessageW(Some(hwnd), WM_RESTORE, WPARAM(0), LPARAM(0)) };
            }
            LRESULT(0)
        }
        WM_RENDERALLFORMATS => {
            // Closing while still offering text: render it so it stays pasteable.
            // SAFETY: our window; the text is put only while we still own the clipboard.
            unsafe {
                if OpenClipboard(Some(hwnd)).is_ok() {
                    if GetClipboardOwner().is_ok_and(|owner| owner == hwnd) {
                        PASTE.with(|p| {
                            if let Some(p) = p.borrow().as_ref() {
                                put(u32::from(CF_UNICODETEXT.0), &unicode_bytes(&p.text));
                            }
                        });
                    }
                    let _ = CloseClipboard();
                }
            }
            LRESULT(0)
        }
        // Someone else replaced the clipboard mid-paste (the user copied
        // something): keep their content rather than restoring over it.
        WM_DESTROYCLIPBOARD => {
            let abandoned = PASTE.with(|p| {
                p.try_borrow()
                    .is_ok_and(|p| p.as_ref().is_some_and(|p| !p.restoring))
            });
            if abandoned {
                finish(hwnd, |rendered| {
                    if rendered {
                        InsertOutcome::Inserted
                    } else {
                        InsertOutcome::Unknown
                    }
                });
            }
            LRESULT(0)
        }
        WM_RESTORE => {
            if pasting() {
                try_restore(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            // SAFETY: our window's timer.
            let _ = unsafe { KillTimer(Some(hwnd), wparam.0) };
            let rendered = PASTE.with(|p| p.borrow().as_ref().map(|p| p.rendered));
            match (wparam.0, rendered) {
                // The app never asked for the text.
                (PASTE_DEADLINE, Some(false)) => try_restore(hwnd),
                (RESTORE_RETRY, Some(_)) => try_restore(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        // SAFETY: forwards the arguments the system passed in.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Keystrokes that select what `replace` covers, so the insertion replaces it.
fn selection(
    out: &mut Vec<INPUT>,
    replace: Replace,
    before: &str,
    after: &str,
    empty: bool,
) -> Result<(), String> {
    match replace {
        Replace::Nothing => {}
        Replace::Selection => {
            if empty {
                send::tap(out, VK_DELETE);
            }
        }
        Replace::Around {
            before: b,
            after: a,
        } => {
            let changed = || "the text around the caret changed".to_string();
            let start = before
                .len()
                .checked_sub(b)
                .filter(|&s| before.is_char_boundary(s))
                .ok_or_else(changed)?;
            let behind = &before[start..];
            let ahead = after.get(..a).ok_or_else(changed)?;
            let (back, forward) = (caret_stops(behind), caret_stops(ahead));
            for _ in 0..forward {
                send::tap(out, VK_RIGHT);
            }
            send::press(out, VK_SHIFT, true);
            for _ in 0..back + forward {
                send::tap(out, VK_LEFT);
            }
            send::press(out, VK_SHIFT, false);
            if empty && back + forward > 0 {
                send::tap(out, VK_DELETE);
            }
        }
    }
    Ok(())
}

/// Starts queued insertions until one is waiting on a paste.
fn pump() {
    while !pasting() {
        let Some((req, hwnd, events)) = THREAD.with(|t| {
            t.borrow_mut()
                .as_mut()
                .and_then(|t| Some((t.queue.pop_front()?, t.hwnd, t.events.clone())))
        }) else {
            return;
        };
        start(req, hwnd, &events);
    }
}

fn start(req: Request, hwnd: HWND, events: &Sender<Event>) {
    let Request {
        request,
        before,
        after,
    } = req;
    let mut inputs = Vec::new();
    if let Err(why) = selection(
        &mut inputs,
        request.replace,
        &before,
        &after,
        request.text.is_empty(),
    ) {
        return done(events, request.id, InsertOutcome::Failed(why));
    }
    if !paste_instead(&request.text) {
        send::text(&mut inputs, &request.text);
        if !send::send(&inputs) {
            return done(events, request.id, InsertOutcome::Failed(BLOCKED.into()));
        }
        CONFIRM.with(|c| {
            if let Some(uia) = c.borrow().as_ref() {
                uia.post(uia::Msg::Confirm {
                    id: request.id,
                    field: request.field,
                    text: request.text,
                });
            }
        });
        return;
    }
    if !send::send(&inputs) {
        return done(events, request.id, InsertOutcome::Failed(BLOCKED.into()));
    }
    if !open_clipboard(hwnd) {
        return done(
            events,
            request.id,
            InsertOutcome::Failed("the clipboard is busy".into()),
        );
    }
    let saved = save_formats();
    // SAFETY: the clipboard is open on this thread; the NULL handle offers
    // the text by delayed rendering, so the paste itself tells us when the
    // app has read it.
    unsafe {
        let _ = EmptyClipboard();
        put_private_markers();
        let _ = SetClipboardData(u32::from(CF_UNICODETEXT.0), None);
        let _ = CloseClipboard();
    }
    let text = request.text.encode_utf16().chain([0]).collect();
    PASTE.with(|p| {
        *p.borrow_mut() = Some(Paste {
            id: request.id,
            text,
            saved,
            rendered: false,
            restoring: false,
            retries: 0,
        })
    });
    // SAFETY: our window.
    unsafe { SetTimer(Some(hwnd), PASTE_DEADLINE, PASTE_DEADLINE_MS, None) };
    let mut keys = Vec::new();
    send::press(&mut keys, VK_CONTROL, true);
    send::tap(&mut keys, VK_V);
    send::press(&mut keys, VK_CONTROL, false);
    if !send::send(&keys) {
        try_restore(hwnd);
    }
}

const BLOCKED: &str = "Windows blocked the input; the app may be running as administrator";

thread_local! {
    static CONFIRM: RefCell<Option<Mailbox<uia::Msg>>> = const { RefCell::new(None) };
}

fn create_window() -> Result<HWND, String> {
    // SAFETY: a message-only window with a static class and a procedure that
    // lives for the program.
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("ghostcaret.clipboard");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(clipboard_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Ghostcaret"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| format!("cannot create the clipboard window: {e}"))
    }
}

pub fn run_thread(
    inbox: Inbox<Request>,
    ready: Ready,
    events: Sender<Event>,
    uia: Mailbox<uia::Msg>,
    shared: Arc<Shared>,
) {
    let hwnd = match create_window() {
        Ok(h) => h,
        Err(e) => return ready.fail(e),
    };
    ready.ok();
    CONFIRM.with(|c| *c.borrow_mut() = Some(uia));
    THREAD.with(|t| {
        *t.borrow_mut() = Some(Thread {
            hwnd,
            events: events.clone(),
            queue: VecDeque::new(),
        })
    });
    run(
        || {
            for req in inbox.drain() {
                if shared.field.load(Ordering::Acquire) != req.request.field.0 {
                    done(
                        &events,
                        req.request.id,
                        InsertOutcome::Failed("the field no longer has focus".into()),
                    );
                    continue;
                }
                THREAD.with(|t| {
                    t.borrow_mut()
                        .as_mut()
                        .expect("insert thread state")
                        .queue
                        .push_back(req)
                });
            }
            pump();
        },
        || u32::MAX,
    );
    if pasting() {
        try_restore(hwnd);
    }
    THREAD.with(|t| t.borrow_mut().take());
    // SAFETY: our window, destroyed on the thread that created it.
    let _ = unsafe { DestroyWindow(hwnd) };
}
