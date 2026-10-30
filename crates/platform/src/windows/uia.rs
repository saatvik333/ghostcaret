//! The UI Automation thread: follows focus, reads the focused field when it
//! changes, confirms typed insertions, and reads the visible conversation.
//!
//! Everything is event driven. The focus-changed handler and per-field text
//! events come from UI Automation; WinEvents (caret moves, value and
//! selection changes, IA2 and console events) from the focused process are
//! cheap extra triggers for apps that raise only those. Reads are coalesced:
//! however many events arrive, the field is read once when they stop.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use ghostcaret_engine::io::{
    AppInfo, BackendStatus, Conversation, Event, FieldId, FieldKind, FieldSnapshot, InsertOutcome,
    MAX_AFTER, MAX_BEFORE, Rect,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, MONITOR_DEFAULTTONEAREST, MonitorFromRect};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TokenIntegrityLevel,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetWindowDpiAwarenessContext,
    LogicalToPhysicalPointForPerMonitorDPI, MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, HKL, MAPVK_VK_TO_VSC, MapVirtualKeyExW, ToUnicodeEx,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ES_MULTILINE, ES_NUMBER, EVENT_CONSOLE_CARET, EVENT_CONSOLE_END_APPLICATION,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_TEXTEDIT_CONVERSIONTARGETCHANGED,
    EVENT_OBJECT_TEXTSELECTIONCHANGED, EVENT_OBJECT_VALUECHANGE, EVENT_SYSTEM_FOREGROUND, GA_ROOT,
    GUITHREADINFO, GWL_STYLE, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId, OBJID_CARET, SMTO_ABORTIFHUNG,
    SendMessageTimeoutW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_IME_CONTROL,
};
use windows::core::{BOOL, BSTR, Interface, PCWSTR, Ref, implement};

use super::backend::Shared;
use super::conversation::{Item, conversation};
use super::field::Traits;
use super::hook;
use super::keys::{self, CharTable, Produced};
use super::mailbox::{Inbox, Mailbox, Ready, run};
use super::overlay;
use super::place::Px;
use super::text::{head, split_utf16, tail};

pub enum Msg {
    Focus(Element),
    /// A text or selection event on the focused element.
    Changed,
    Confirm {
        id: u64,
        field: FieldId,
        text: String,
    },
    Conversation {
        field: FieldId,
        reply: Sender<Option<Conversation>>,
    },
}

/// A UI Automation element handed from an event handler to this thread.
pub struct Element(IUIAutomationElement);

// SAFETY: UI Automation client objects live in the multithreaded apartment
// and may be used from any MTA thread; both ends of the hand-off are MTA.
unsafe impl Send for Element {}

/// How long an insertion waits for the field to show it.
const CONFIRM_WITHIN: Duration = Duration::from_millis(1500);
/// Bounds every cross-process UI Automation call, so a hung app cannot
/// stall reads of the next one.
const UIA_TIMEOUT_MS: u32 = 1500;
const EM_GETSEL: u32 = 0x00B0;
const IMC_GETOPENSTATUS: usize = 5;
/// ToUnicodeEx flag: do not change the keyboard state (Windows 10 1607+),
/// so building the table cannot disturb a pending dead key.
const TO_UNICODE_NO_STATE: u32 = 0x4;

thread_local! {
    static FOREGROUND: Cell<bool> = const { Cell::new(false) };
    static TEXT_EVENT: Cell<bool> = const { Cell::new(false) };
}

#[implement(IUIAutomationFocusChangedEventHandler)]
struct FocusHandler(Mailbox<Msg>);

impl IUIAutomationFocusChangedEventHandler_Impl for FocusHandler_Impl {
    fn HandleFocusChangedEvent(
        &self,
        sender: Ref<IUIAutomationElement>,
    ) -> windows::core::Result<()> {
        if let Some(e) = sender.cloned() {
            self.0.post(Msg::Focus(Element(e)));
        }
        Ok(())
    }
}

/// Text and selection events on the focused element. A flag coalesces bursts
/// into one wake-up.
#[implement(IUIAutomationEventHandler)]
struct TextHandler {
    mail: Mailbox<Msg>,
    pending: Arc<AtomicBool>,
}

impl IUIAutomationEventHandler_Impl for TextHandler_Impl {
    fn HandleAutomationEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
        _event: UIA_EVENT_ID,
    ) -> windows::core::Result<()> {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.mail.post(Msg::Changed);
        }
        Ok(())
    }
}

unsafe extern "system" fn win_event(
    _: HWINEVENTHOOK,
    event: u32,
    _: HWND,
    object: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    match event {
        EVENT_SYSTEM_FOREGROUND => FOREGROUND.set(true),
        EVENT_OBJECT_LOCATIONCHANGE if object != OBJID_CARET.0 => {}
        _ => TEXT_EVENT.set(true),
    }
}

/// Events from the focused process that mean its text or caret may have moved.
const PROCESS_EVENTS: [(u32, u32); 6] = [
    (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
    (EVENT_OBJECT_VALUECHANGE, EVENT_OBJECT_VALUECHANGE),
    (
        EVENT_OBJECT_TEXTSELECTIONCHANGED,
        EVENT_OBJECT_TEXTSELECTIONCHANGED,
    ),
    (
        EVENT_OBJECT_TEXTEDIT_CONVERSIONTARGETCHANGED,
        EVENT_OBJECT_TEXTEDIT_CONVERSIONTARGETCHANGED,
    ),
    // IAccessible2 events (Firefox, Chromium, LibreOffice, Java).
    (0x0101, 0x012F),
    (EVENT_CONSOLE_CARET, EVENT_CONSOLE_END_APPLICATION),
];

fn set_win_event(min: u32, max: u32, pid: u32, flags: u32) -> Option<HWINEVENTHOOK> {
    // SAFETY: an out-of-context hook with a callback that lives for the program.
    let h = unsafe {
        SetWinEventHook(
            min,
            max,
            None,
            Some(win_event),
            pid,
            0,
            WINEVENT_OUTOFCONTEXT | flags,
        )
    };
    (!h.is_invalid()).then_some(h)
}

fn unhook(hooks: &mut Vec<HWINEVENTHOOK>) {
    for h in hooks.drain(..) {
        // SAFETY: returned by SetWinEventHook on this thread.
        let _ = unsafe { UnhookWinEvent(h) };
    }
}

fn bstr(r: windows::core::Result<BSTR>) -> String {
    r.map(|b| String::from_utf16_lossy(&b)).unwrap_or_default()
}

fn wide(buf: &[u16], n: i32) -> String {
    String::from_utf16_lossy(&buf[..n.clamp(0, buf.len() as i32) as usize])
}

/// The first rectangle of a `GetBoundingRectangles` array, which it frees.
fn first_rect(array: *mut SAFEARRAY) -> Option<Px> {
    if array.is_null() {
        return None;
    }
    // SAFETY: a one-dimensional SAFEARRAY of doubles from UI Automation, read
    // only between AccessData and UnaccessData, then destroyed exactly once.
    unsafe {
        let mut rect = None;
        let upper = SafeArrayGetUBound(array, 1).unwrap_or(-1);
        let mut data = std::ptr::null_mut::<c_void>();
        if upper >= 3 && SafeArrayAccessData(array, &mut data).is_ok() {
            let v = std::slice::from_raw_parts(data.cast::<f64>(), 4);
            if v[3] > 0.0 {
                rect = Some(Px {
                    x: v[0].round() as i32,
                    y: v[1].round() as i32,
                    w: v[2].round() as i32,
                    h: v[3].round() as i32,
                });
            }
            let _ = SafeArrayUnaccessData(array);
        }
        let _ = SafeArrayDestroy(array);
        rect
    }
}

fn range_rect(range: &IUIAutomationTextRange) -> Option<Px> {
    // SAFETY: a valid range; the returned array is freed by first_rect.
    first_rect(unsafe { range.GetBoundingRectangles() }.ok()?)
}

fn scale_at(r: Px) -> f64 {
    let rect = RECT {
        left: r.x,
        top: r.y,
        right: r.x + r.w.max(1),
        bottom: r.y + r.h.max(1),
    };
    let (mut dpi, mut unused) = (96u32, 0u32);
    // SAFETY: plain queries with valid pointers.
    unsafe {
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused);
    }
    f64::from(dpi) / 96.0
}

/// Physical pixels to the contract's logical coordinates, at the scale of the
/// display the rectangle is on.
fn logical(r: Px) -> Rect {
    let s = scale_at(r);
    Rect {
        x: f64::from(r.x) / s,
        y: f64::from(r.y) / s,
        width: f64::from(r.w) / s,
        height: f64::from(r.h) / s,
    }
}

/// The system caret of the thread owning `hwnd`, in physical pixels. Works
/// for classic Win32 controls and apps that move the caret for magnifiers.
fn system_caret(hwnd: HWND) -> Option<Px> {
    // SAFETY: plain queries; `info` has cbSize set and the points are locals.
    unsafe {
        let thread = GetWindowThreadProcessId(hwnd, None);
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(thread, &mut info).ok()?;
        let owner = info.hwndCaret;
        if owner.is_invalid() {
            return None;
        }
        let r = info.rcCaret;
        let mut corners = [
            POINT {
                x: r.left,
                y: r.top,
            },
            POINT {
                x: r.right,
                y: r.bottom,
            },
        ];
        // The caret is in the window's own (possibly scaled) coordinates:
        // map it there, then to physical pixels.
        let previous = SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(owner));
        for p in &mut corners {
            let _ = ClientToScreen(owner, p);
        }
        SetThreadDpiAwarenessContext(previous);
        for p in &mut corners {
            let _ = LogicalToPhysicalPointForPerMonitorDPI(Some(owner), p);
        }
        let [a, b] = corners;
        (b.y > a.y).then(|| Px {
            x: a.x,
            y: a.y,
            w: (b.x - a.x).max(1),
            h: b.y - a.y,
        })
    }
}

/// The integrity level of a process; `None` when it cannot be read, which
/// for a foreground window means it runs above us.
fn integrity(pid: u32) -> Option<u32> {
    // SAFETY: handles are opened for query only and closed on every path;
    // the token buffer is pointer-aligned and large enough for the label.
    unsafe {
        let process = if pid == GetCurrentProcessId() {
            GetCurrentProcess()
        } else {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        if pid != GetCurrentProcessId() {
            let _ = CloseHandle(process);
        }
        opened.ok()?;
        let mut buf = [0u64; 16];
        let mut len = 0;
        let got = GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            size_of_val(&buf) as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        got.ok()?;
        let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        Some(*GetSidSubAuthority(sid, u32::from(count).checked_sub(1)?))
    }
}

fn exe_path(pid: u32) -> Option<String> {
    // SAFETY: a query-only handle, closed after use; the buffer length is passed.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        ok.ok()?;
        Some(wide(&buf, len as i32))
    }
}

/// The executable's FileDescription, which is the name people know an app by.
fn file_description(path: &str) -> Option<String> {
    let wpath: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let name = PCWSTR(wpath.as_ptr());
    // SAFETY: reads the version resource into an owned buffer and queries it
    // with NUL-terminated paths; returned pointers point into that buffer.
    unsafe {
        let size = GetFileVersionInfoSizeW(name, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(name, None, size, data.as_mut_ptr().cast()).ok()?;
        let mut p = std::ptr::null_mut::<c_void>();
        let mut len = 0u32;
        let query = |q: &str, p: &mut *mut c_void, len: &mut u32| {
            let q: Vec<u16> = q.encode_utf16().chain([0]).collect();
            VerQueryValueW(data.as_ptr().cast(), PCWSTR(q.as_ptr()), p, len).as_bool()
        };
        if !query("\\VarFileInfo\\Translation", &mut p, &mut len) || len < 4 {
            return None;
        }
        let (lang, cp) = (*p.cast::<u16>(), *p.cast::<u16>().add(1));
        let key = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription");
        if !query(&key, &mut p, &mut len) || len == 0 {
            return None;
        }
        let s = std::slice::from_raw_parts(p.cast::<u16>(), len as usize);
        let s = String::from_utf16_lossy(s)
            .trim_end_matches('\0')
            .trim()
            .to_owned();
        (!s.is_empty()).then_some(s)
    }
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: the buffer length is passed.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    wide(&buf, n)
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: the buffer length is passed.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    wide(&buf, n)
}

/// The characters each key types under `hkl`, read without touching any
/// keyboard state.
fn char_table(hkl: HKL) -> CharTable {
    CharTable::build(hkl.0 as usize, |vk, shift, altgr, caps| {
        let mut state = [0u8; 256];
        if shift {
            state[usize::from(keys::VK_SHIFT)] = 0x80;
            state[usize::from(keys::VK_LSHIFT)] = 0x80;
        }
        if altgr {
            for k in [
                keys::VK_CONTROL,
                keys::VK_LCONTROL,
                keys::VK_MENU,
                keys::VK_RMENU,
            ] {
                state[usize::from(k)] = 0x80;
            }
        }
        if caps {
            state[usize::from(keys::VK_CAPITAL)] = 0x01;
        }
        let mut buf = [0u16; 8];
        // SAFETY: valid buffers; the no-state flag leaves dead-key state alone.
        let n = unsafe {
            let scan = MapVirtualKeyExW(u32::from(vk), MAPVK_VK_TO_VSC, Some(hkl));
            ToUnicodeEx(
                u32::from(vk),
                scan,
                &state,
                &mut buf,
                TO_UNICODE_NO_STATE,
                Some(hkl),
            )
        };
        let one = |units: &[u16]| {
            let mut it = char::decode_utf16(units.iter().copied());
            match (it.next(), it.next()) {
                (Some(Ok(c)), None) => Some(c),
                _ => None,
            }
        };
        match n {
            n if n < 0 => one(&buf[..1]).map_or(Produced::None, Produced::Dead),
            1 | 2 => one(&buf[..n as usize]).map_or(Produced::None, Produced::Char),
            _ => Produced::None,
        }
    })
}

/// Whether an input method is open for `hwnd` (Chinese, Japanese, Korean).
fn ime_open(hkl: HKL, hwnd: HWND) -> bool {
    let lang = hkl.0 as usize & 0x3FF;
    if !matches!(lang, 0x04 | 0x11 | 0x12) {
        return false;
    }
    let mut open = 0usize;
    // SAFETY: asks the default IME window of `hwnd`, with a short timeout.
    unsafe {
        let ime = ImmGetDefaultIMEWnd(hwnd);
        if ime.is_invalid() {
            return false;
        }
        SendMessageTimeoutW(
            ime,
            WM_IME_CONTROL,
            WPARAM(IMC_GETOPENSTATUS),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            50,
            Some(&mut open),
        );
    }
    open != 0
}

/// What was last read from the field, to report only real changes.
#[derive(Clone, Debug, Default, PartialEq)]
struct Reading {
    before: String,
    before_truncated: bool,
    selection: String,
    after: String,
    caret: Option<Px>,
}

struct Field {
    id: FieldId,
    element: IUIAutomationElement,
    text: Option<IUIAutomationTextPattern>,
    text2: Option<IUIAutomationTextPattern2>,
    value: Option<IUIAutomationValuePattern>,
    /// A classic Win32 edit control, whose selection EM_GETSEL reports.
    edit: Option<HWND>,
    window: HWND,
    pid: u32,
    kind: FieldKind,
    multiline: bool,
    app: AppInfo,
    bounds: Option<Px>,
    last: Option<Reading>,
}

struct Confirm {
    id: u64,
    field: FieldId,
    /// The end of the inserted text, without carriage returns.
    expect: String,
    deadline: Instant,
}

struct Uia {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    text_handler: IUIAutomationEventHandler,
    text_pending: Arc<AtomicBool>,
    events: Sender<Event>,
    hook: Mailbox<hook::Command>,
    overlay: Mailbox<overlay::Msg>,
    shared: Arc<Shared>,
    field: Option<Field>,
    next_field: u64,
    revision: u64,
    process_hooks: Vec<HWINEVENTHOOK>,
    hooked_pid: u32,
    /// The layout the hook's table was built for, and whether an IME was open.
    layout: (usize, bool),
    table: CharTable,
    apps: HashMap<u32, (String, String)>,
    own_integrity: Option<u32>,
    status: BackendStatus,
    confirms: Vec<Confirm>,
}

fn strip_cr(s: &str) -> String {
    s.chars().filter(|&c| c != '\r').collect()
}

impl Uia {
    fn send(&self, e: Event) {
        let _ = self.events.send(e);
    }

    fn set_status(&mut self, status: BackendStatus) {
        if self.status != status {
            self.status = status.clone();
            self.send(Event::Status(status));
        }
    }

    fn app(&mut self, pid: u32, window: HWND) -> AppInfo {
        if self.apps.len() > 64 {
            self.apps.clear();
        }
        let (id, name) = self
            .apps
            .entry(pid)
            .or_insert_with(|| {
                let path = exe_path(pid).unwrap_or_default();
                let file = path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let stem = file
                    .rsplit_once('.')
                    .map_or(file.as_str(), |(s, _)| s)
                    .to_owned();
                (file.to_lowercase(), file_description(&path).unwrap_or(stem))
            })
            .clone();
        AppInfo {
            id,
            name,
            title: window_text(window),
        }
    }

    /// Drops the current field: its event registrations and pending confirmations.
    fn release_field(&mut self) -> Option<Field> {
        let f = self.field.take()?;
        for event in [
            UIA_Text_TextChangedEventId,
            UIA_Text_TextSelectionChangedEventId,
        ] {
            // SAFETY: removes a handler added for this element on this thread.
            let _ = unsafe {
                self.automation
                    .RemoveAutomationEventHandler(event, &f.element, &self.text_handler)
            };
        }
        for c in self.confirms.drain(..) {
            let _ = self.events.send(Event::InsertDone {
                id: c.id,
                outcome: InsertOutcome::Unknown,
            });
        }
        self.shared.field.store(0, Ordering::Release);
        Some(f)
    }

    fn blur(&mut self) {
        self.hook.post(hook::Command::Focus(None));
        if self.release_field().is_some() {
            self.overlay.post(overlay::Msg::Caret {
                field: None,
                caret: None,
                bounds: None,
            });
            self.send(Event::Blur);
        }
    }

    /// Whether `pid` runs at a higher integrity level than Ghostcaret, so
    /// Windows keeps us from reading or typing into it. A level that cannot
    /// be read counts as higher.
    fn above_us(&self, pid: u32) -> bool {
        match (integrity(pid), self.own_integrity) {
            (Some(theirs), Some(ours)) => theirs > ours,
            (None, Some(_)) => pid != 0,
            (_, None) => false,
        }
    }

    /// The foreground window changed: drop the old field at once, refuse
    /// windows we cannot reach, and look at what has focus now.
    fn foreground(&mut self) {
        self.hook.post(hook::Command::Focus(None));
        // SAFETY: plain query.
        let window = unsafe { GetForegroundWindow() };
        let mut pid = 0;
        // SAFETY: `pid` is a valid out-pointer.
        unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        if self.above_us(pid) {
            self.blur();
            let app = self.app(pid, window);
            let name = if app.name.is_empty() {
                "This app".to_owned()
            } else {
                app.name
            };
            self.set_status(BackendStatus::Unavailable(format!(
                "{name} runs as administrator; Ghostcaret can reach it only when Ghostcaret also runs as administrator"
            )));
            return;
        }
        self.set_status(BackendStatus::Ready);
        // SAFETY: a valid automation object and cache request.
        if let Ok(e) = unsafe { self.automation.GetFocusedElementBuildCache(&self.cache) } {
            self.focus(e);
        }
    }

    fn focus(&mut self, e: IUIAutomationElement) {
        if let Some(f) = &self.field {
            // SAFETY: two valid elements.
            if unsafe { self.automation.CompareElements(&e, &f.element) }
                .is_ok_and(|same| same.as_bool())
            {
                // Focus came back to the same field: re-arm the gate for it.
                self.hook.post(hook::Command::Focus(Some(f.id)));
                self.refresh();
                return;
            }
        }
        let Some(mut field) = self.classify(e) else {
            return self.blur();
        };
        self.release_field();
        self.next_field += 1;
        field.id = FieldId(self.next_field);
        let element = field.element.clone();
        for event in [
            UIA_Text_TextChangedEventId,
            UIA_Text_TextSelectionChangedEventId,
        ] {
            // SAFETY: registers our handler for the focused element only.
            let _ = unsafe {
                self.automation.AddAutomationEventHandler(
                    event,
                    &element,
                    TreeScope_Element,
                    None,
                    &self.text_handler,
                )
            };
        }
        if field.pid != self.hooked_pid {
            unhook(&mut self.process_hooks);
            self.process_hooks = PROCESS_EVENTS
                .iter()
                .filter_map(|&(min, max)| set_win_event(min, max, field.pid, 0))
                .collect();
            self.hooked_pid = field.pid;
        }
        self.check_layout(field.window);
        let reading = self.read(&field);
        self.hook.post(hook::Command::Focus(Some(field.id)));
        self.shared.field.store(field.id.0, Ordering::Release);
        self.revision += 1;
        let snapshot = self.snapshot(&field, &reading);
        self.share_text(&reading);
        self.overlay.post(overlay::Msg::Caret {
            field: Some(field.id),
            caret: reading.caret,
            bounds: field.bounds,
        });
        field.last = Some(reading);
        self.field = Some(field);
        self.send(Event::Focus(snapshot));
    }

    /// Reads the focused element's properties and decides whether it is a
    /// text field we serve.
    fn classify(&mut self, e: IUIAutomationElement) -> Option<Field> {
        // SAFETY: getters on an element fetched with our cache request; a
        // pattern or property the element lacks comes back as an error.
        unsafe {
            let pid = u32::try_from(e.CachedProcessId().ok()?).ok()?;
            // Our own windows, and apps that Windows does not let us read or type into.
            if pid == GetCurrentProcessId() || self.above_us(pid) {
                return None;
            }
            let control = e.CachedControlType().ok()?;
            let password = e.CachedIsPassword().is_ok_and(|b| b.as_bool());
            let text: Option<IUIAutomationTextPattern> =
                e.GetCachedPatternAs(UIA_TextPatternId).ok();
            let text2: Option<IUIAutomationTextPattern2> =
                e.GetCachedPatternAs(UIA_TextPattern2Id).ok();
            let value: Option<IUIAutomationValuePattern> =
                e.GetCachedPatternAs(UIA_ValuePatternId).ok();
            let native = e.CachedNativeWindowHandle().unwrap_or_default();
            let window = if native.is_invalid() {
                GetForegroundWindow()
            } else {
                GetAncestor(native, GA_ROOT)
            };
            let app = self.app(pid, window);
            let native_class = if native.is_invalid() {
                String::new()
            } else {
                class_name(native)
            };
            let edit = (native_class.eq_ignore_ascii_case("Edit")
                || native_class.starts_with("RichEdit"))
            .then_some(native);
            let style = edit.map_or(0, |h| GetWindowLongPtrW(h, GWL_STYLE) as i32);
            let traits = Traits {
                exe: app.id.clone(),
                class: bstr(e.CachedClassName()),
                automation_id: bstr(e.CachedAutomationId()),
                aria_role: bstr(e.CachedAriaRole()),
                aria_properties: bstr(e.CachedAriaProperties()),
                password,
                spinner: control == UIA_SpinnerControlTypeId,
                document: control == UIA_DocumentControlTypeId,
                edit_number: style & ES_NUMBER != 0,
                edit_multiline: style & ES_MULTILINE != 0,
            };
            let writable = value
                .as_ref()
                .is_some_and(|v| v.CachedIsReadOnly().is_ok_and(|r| !r.as_bool()))
                || control == UIA_EditControlTypeId
                || text
                    .as_ref()
                    .is_some_and(|t| traits.is_terminal() || caret_writable(t));
            if !(writable || password) || (text.is_none() && value.is_none() && !password) {
                return None;
            }
            let bounds = e.CachedBoundingRectangle().ok().map(|r| Px {
                x: r.left,
                y: r.top,
                w: r.right - r.left,
                h: r.bottom - r.top,
            });
            Some(Field {
                id: FieldId(0),
                element: e,
                text,
                text2,
                value,
                edit,
                window,
                pid,
                kind: traits.kind(),
                multiline: traits.multiline(),
                app,
                bounds,
                last: None,
            })
        }
    }

    fn check_layout(&mut self, window: HWND) {
        // SAFETY: plain queries.
        let hkl = unsafe { GetKeyboardLayout(GetWindowThreadProcessId(window, None)) };
        let now = (hkl.0 as usize, ime_open(hkl, window));
        if now == self.layout {
            return;
        }
        if now.0 != self.table.layout {
            self.table = char_table(hkl);
        }
        self.layout = now;
        self.hook.post(hook::Command::Layout {
            table: Box::new(self.table.clone()),
            ime_open: now.1,
        });
    }

    fn read(&self, f: &Field) -> Reading {
        if f.kind == FieldKind::Password {
            // Never read: report only that a password field has focus.
            return Reading::default();
        }
        // SAFETY: method calls on live UI Automation objects; the window
        // handles are only queried.
        unsafe {
            if let Some(t) = &f.text
                && let Some(r) = read_text(t, f.text2.as_ref())
            {
                let caret = r.caret.or_else(|| system_caret(f.window));
                return Reading { caret, ..r };
            }
            let Some(v) = &f.value else {
                return Reading::default();
            };
            let value = bstr(v.CurrentValue());
            let caret = system_caret(f.window);
            if let Some(edit) = f.edit {
                // The selection comes back packed in the result (pointer
                // arguments would not cross the process boundary); -1 means
                // an offset beyond 65,535.
                let mut packed = usize::MAX;
                let got = SendMessageTimeoutW(
                    edit,
                    EM_GETSEL,
                    WPARAM(0),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG,
                    100,
                    Some(&mut packed),
                );
                if got.0 != 0 && packed as u32 != u32::MAX {
                    let (start, end) = (packed & 0xFFFF, (packed >> 16) & 0xFFFF);
                    let (b, s, a) = split_utf16(&value, start, end);
                    let (before, cut) = tail(b, MAX_BEFORE);
                    return Reading {
                        before: before.into(),
                        before_truncated: cut,
                        selection: head(s, MAX_BEFORE).into(),
                        after: head(a, MAX_AFTER).into(),
                        caret,
                    };
                }
            }
            // Without a caret offset, the value is all we know: treat it as
            // text before the caret and say the view is partial.
            let (before, _) = tail(&value, MAX_BEFORE);
            Reading {
                before: before.into(),
                before_truncated: true,
                selection: String::new(),
                after: String::new(),
                caret,
            }
        }
    }

    fn snapshot(&self, f: &Field, r: &Reading) -> FieldSnapshot {
        FieldSnapshot {
            field: f.id,
            revision: self.revision,
            app: f.app.clone(),
            kind: f.kind,
            multiline: f.multiline,
            before: r.before.clone(),
            before_truncated: r.before_truncated,
            selection: r.selection.clone(),
            after: r.after.clone(),
            caret: r.caret.map(logical),
        }
    }

    fn share_text(&self, r: &Reading) {
        let mut text = self.shared.text.lock().unwrap_or_else(|e| e.into_inner());
        text.0.clone_from(&r.before);
        text.1.clone_from(&r.after);
    }

    /// Re-reads the field after a change event and reports what changed.
    fn refresh(&mut self) {
        let Some(mut f) = self.field.take() else {
            return;
        };
        self.check_layout(f.window);
        let reading = self.read(&f);
        if f.last.as_ref() != Some(&reading) {
            self.revision += 1;
            let snapshot = self.snapshot(&f, &reading);
            self.share_text(&reading);
            self.overlay.post(overlay::Msg::Caret {
                field: Some(f.id),
                caret: reading.caret,
                bounds: f.bounds,
            });
            self.send(Event::Changed(snapshot));
        }
        // Also when nothing changed: the text may have landed before the
        // confirmation request did.
        self.confirm(f.id, &reading.before);
        f.last = Some(reading);
        self.field = Some(f);
    }

    fn confirm(&mut self, field: FieldId, before: &str) {
        let before = strip_cr(before);
        let events = &self.events;
        self.confirms.retain(|c| {
            let landed = c.field == field && before.ends_with(&c.expect);
            if landed {
                let _ = events.send(Event::InsertDone {
                    id: c.id,
                    outcome: InsertOutcome::Inserted,
                });
            }
            !landed
        });
    }

    /// Milliseconds until the next confirmation expires, after expiring due ones.
    fn expire(&mut self) -> u32 {
        let now = Instant::now();
        let events = &self.events;
        self.confirms.retain(|c| {
            let due = c.deadline <= now;
            if due {
                let _ = events.send(Event::InsertDone {
                    id: c.id,
                    outcome: InsertOutcome::Unknown,
                });
            }
            !due
        });
        self.confirms
            .iter()
            .map(|c| {
                c.deadline
                    .saturating_duration_since(now)
                    .as_millis()
                    .min(u128::from(u32::MAX - 1)) as u32
                    + 1
            })
            .min()
            .unwrap_or(u32::MAX)
    }

    fn conversation(&self, field: FieldId) -> Option<Conversation> {
        let f = self.field.as_ref().filter(|f| f.id == field)?;
        let at = f.last.as_ref().and_then(|r| r.caret).or(f.bounds)?;
        // SAFETY: UI Automation calls on live objects, bounded by the transaction timeout.
        unsafe {
            let walker = self.automation.ControlViewWalker().ok()?;
            let root = self.automation.GetRootElement().ok()?;
            // The top-level window that holds the field.
            let mut window = f.element.clone();
            for _ in 0..64 {
                let Ok(parent) = walker.GetParentElement(&window) else {
                    break;
                };
                if self
                    .automation
                    .CompareElements(&parent, &root)
                    .is_ok_and(|b| b.as_bool())
                {
                    break;
                }
                window = parent;
            }
            let a = &self.automation;
            let kind = |id: UIA_CONTROLTYPE_ID| {
                a.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(id.0))
            };
            let kinds = a
                .CreateOrCondition(
                    &a.CreateOrCondition(
                        &kind(UIA_TextControlTypeId).ok()?,
                        &kind(UIA_ListItemControlTypeId).ok()?,
                    )
                    .ok()?,
                    &kind(UIA_DataItemControlTypeId).ok()?,
                )
                .ok()?;
            let onscreen = a
                .CreatePropertyCondition(UIA_IsOffscreenPropertyId, &VARIANT::from(false))
                .ok()?;
            let condition = a.CreateAndCondition(&kinds, &onscreen).ok()?;
            let cache = a.CreateCacheRequest().ok()?;
            for p in [
                UIA_NamePropertyId,
                UIA_ControlTypePropertyId,
                UIA_BoundingRectanglePropertyId,
            ] {
                cache.AddProperty(p).ok()?;
            }
            let found = window
                .FindAllBuildCache(TreeScope_Descendants, &condition, &cache)
                .ok()?;
            let mut items = Vec::new();
            for i in 0..found.Length().unwrap_or(0) {
                let Ok(e) = found.GetElement(i) else { continue };
                let Ok(r) = e.CachedBoundingRectangle() else {
                    continue;
                };
                let control = e.CachedControlType().unwrap_or_default();
                items.push(Item {
                    rect: Px {
                        x: r.left,
                        y: r.top,
                        w: r.right - r.left,
                        h: r.bottom - r.top,
                    },
                    text: bstr(e.CachedName()),
                    row: control == UIA_ListItemControlTypeId
                        || control == UIA_DataItemControlTypeId,
                });
            }
            Some(conversation(items, f.bounds.unwrap_or(at)))
        }
    }
}

/// Whether the text at the caret is editable, from the range's read-only
/// attribute. Unsupported or mixed values count as not editable.
unsafe fn caret_writable(t: &IUIAutomationTextPattern) -> bool {
    // SAFETY: calls on a live text pattern.
    unsafe {
        let Ok(ranges) = t.GetSelection() else {
            return false;
        };
        let Ok(range) = ranges.GetElement(0) else {
            return false;
        };
        let Ok(v) = range.GetAttributeValue(UIA_IsReadOnlyAttributeId) else {
            return false;
        };
        v.vt() == VT_BOOL && bool::try_from(&v).is_ok_and(|readonly| !readonly)
    }
}

/// Text around the selection (or caret) and the caret rectangle.
unsafe fn read_text(
    t: &IUIAutomationTextPattern,
    t2: Option<&IUIAutomationTextPattern2>,
) -> Option<Reading> {
    // SAFETY: calls on live UI Automation objects.
    unsafe {
        let selection = t.GetSelection().ok().and_then(|s| s.GetElement(0).ok());
        let mut active = BOOL(0);
        let caret = t2.and_then(|t2| t2.GetCaretRange(&mut active).ok());
        let range = selection.or_else(|| caret.clone())?;
        let selected = bstr(range.GetText(MAX_BEFORE as i32));

        let before = range.Clone().ok()?;
        before
            .MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &range,
                TextPatternRangeEndpoint_Start,
            )
            .ok()?;
        let moved = before
            .MoveEndpointByUnit(
                TextPatternRangeEndpoint_Start,
                TextUnit_Character,
                -(MAX_BEFORE as i32),
            )
            .unwrap_or(0);
        let before_text = bstr(before.GetText(-1));

        let after = range.Clone().ok()?;
        after
            .MoveEndpointByRange(
                TextPatternRangeEndpoint_Start,
                &range,
                TextPatternRangeEndpoint_End,
            )
            .ok()?;
        let _ = after.MoveEndpointByUnit(
            TextPatternRangeEndpoint_End,
            TextUnit_Character,
            MAX_AFTER as i32,
        );
        let after_text = bstr(after.GetText(-1));

        let (b, cut) = tail(&before_text, MAX_BEFORE);
        Some(Reading {
            before: b.into(),
            before_truncated: cut || moved <= -(MAX_BEFORE as i32),
            selection: head(&selected, MAX_BEFORE).into(),
            after: head(&after_text, MAX_AFTER).into(),
            caret: caret_rect(caret.as_ref().unwrap_or(&range)),
        })
    }
}

/// The caret's rectangle from a text range: its own rectangle when the
/// provider gives one for an empty range, else the left edge of the next
/// character or the right edge of the previous one.
unsafe fn caret_rect(range: &IUIAutomationTextRange) -> Option<Px> {
    // SAFETY: calls on a live range and its clones.
    unsafe {
        let at = range.Clone().ok()?;
        at.MoveEndpointByRange(
            TextPatternRangeEndpoint_Start,
            range,
            TextPatternRangeEndpoint_End,
        )
        .ok()?;
        if let Some(r) = range_rect(&at) {
            return Some(Px { w: r.w.max(1), ..r });
        }
        let next = at.Clone().ok()?;
        if next
            .MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, 1)
            .is_ok_and(|n| n == 1)
            && let Some(r) = range_rect(&next)
        {
            return Some(Px { w: 1, ..r });
        }
        let prev = at.Clone().ok()?;
        if prev
            .MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1)
            .is_ok_and(|n| n == -1)
            && let Some(r) = range_rect(&prev)
        {
            return Some(Px {
                x: r.right(),
                w: 1,
                ..r
            });
        }
        None
    }
}

fn cache_request(automation: &IUIAutomation) -> windows::core::Result<IUIAutomationCacheRequest> {
    // SAFETY: builds a cache request on a live automation object.
    unsafe {
        let cache = automation.CreateCacheRequest()?;
        for p in [
            UIA_ProcessIdPropertyId,
            UIA_ControlTypePropertyId,
            UIA_IsPasswordPropertyId,
            UIA_ClassNamePropertyId,
            UIA_AutomationIdPropertyId,
            UIA_NativeWindowHandlePropertyId,
            UIA_AriaRolePropertyId,
            UIA_AriaPropertiesPropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_ValueIsReadOnlyPropertyId,
        ] {
            cache.AddProperty(p)?;
        }
        for p in [UIA_TextPatternId, UIA_TextPattern2Id, UIA_ValuePatternId] {
            cache.AddPattern(p)?;
        }
        Ok(cache)
    }
}

pub fn run_thread(
    inbox: Inbox<Msg>,
    ready: Ready,
    mail: Mailbox<Msg>,
    events: Sender<Event>,
    hook: Mailbox<hook::Command>,
    overlay: Mailbox<overlay::Msg>,
    shared: Arc<Shared>,
) {
    // SAFETY: joins the multithreaded apartment for this thread's lifetime and
    // makes its coordinates physical pixels on every display.
    unsafe {
        if let Err(e) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
            return ready.fail(format!("cannot start COM: {e}"));
        }
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let setup = || -> windows::core::Result<(IUIAutomation, IUIAutomationCacheRequest)> {
        // SAFETY: creates the in-process UI Automation client on an MTA thread.
        unsafe {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
            if let Ok(a2) = automation.cast::<IUIAutomation2>() {
                let _ = a2.SetConnectionTimeout(UIA_TIMEOUT_MS);
                let _ = a2.SetTransactionTimeout(UIA_TIMEOUT_MS);
            }
            let cache = cache_request(&automation)?;
            let handler: IUIAutomationFocusChangedEventHandler = FocusHandler(mail.clone()).into();
            automation.AddFocusChangedEventHandler(&cache, &handler)?;
            Ok((automation, cache))
        }
    };
    let (automation, cache) = match setup() {
        Ok(a) => a,
        Err(e) => {
            // SAFETY: balances CoInitializeEx above.
            unsafe { CoUninitialize() };
            return ready.fail(format!("cannot start UI Automation: {e}"));
        }
    };
    let Some(foreground) = set_win_event(
        EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_FOREGROUND,
        0,
        WINEVENT_SKIPOWNPROCESS,
    ) else {
        // SAFETY: balances CoInitializeEx above.
        unsafe { CoUninitialize() };
        return ready.fail("cannot watch the foreground window".into());
    };
    ready.ok();

    let text_pending = Arc::new(AtomicBool::new(false));
    let text_handler: IUIAutomationEventHandler = TextHandler {
        mail: mail.clone(),
        pending: text_pending.clone(),
    }
    .into();
    // SAFETY: plain query of our own process.
    let own_integrity = integrity(unsafe { GetCurrentProcessId() });
    let uia = RefCell::new(Uia {
        automation,
        cache,
        text_handler,
        text_pending,
        events,
        hook,
        overlay,
        shared,
        field: None,
        next_field: 0,
        revision: 0,
        process_hooks: Vec::new(),
        hooked_pid: 0,
        layout: (0, false),
        table: CharTable::empty(),
        apps: HashMap::new(),
        own_integrity,
        status: BackendStatus::Ready,
        confirms: Vec::new(),
    });
    // Start from whatever has focus now.
    FOREGROUND.set(true);
    run(
        || {
            let mut u = uia.borrow_mut();
            let mut focus = None;
            for msg in inbox.drain() {
                match msg {
                    Msg::Focus(e) => focus = Some(e.0),
                    Msg::Confirm { id, field, text } => {
                        let tail = strip_cr(&text);
                        let expect = tail
                            .char_indices()
                            .rev()
                            .nth(63)
                            .map_or(tail.as_str(), |(i, _)| &tail[i..])
                            .to_owned();
                        u.confirms.push(Confirm {
                            id,
                            field,
                            expect,
                            deadline: Instant::now() + CONFIRM_WITHIN,
                        });
                        TEXT_EVENT.set(true);
                    }
                    Msg::Changed => {}
                    Msg::Conversation { field, reply } => {
                        let _ = reply.send(u.conversation(field));
                    }
                }
            }
            if let Some(e) = focus {
                u.focus(e);
            }
        },
        || {
            let mut u = uia.borrow_mut();
            if FOREGROUND.take() {
                u.foreground();
            }
            if TEXT_EVENT.take() | u.text_pending.swap(false, Ordering::AcqRel) {
                u.refresh();
            }
            u.expire()
        },
    );
    let mut u = uia.into_inner();
    u.release_field();
    unhook(&mut u.process_hooks);
    // SAFETY: tears down what this thread registered, then leaves COM.
    unsafe {
        let _ = UnhookWinEvent(foreground);
        let _ = u.automation.RemoveAllEventHandlers();
        drop(u);
        CoUninitialize();
    }
}
