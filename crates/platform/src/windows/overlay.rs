//! The overlay thread: one layered, click-through, never-activated topmost
//! window, redrawn with `UpdateLayeredWindow` from the renderer's output and
//! placed at the caret.

use std::cell::Cell;
use std::ffi::c_void;

use ghostcaret_engine::io::{Bindings, FieldId, Overlay, SuggestionView};
use ghostcaret_engine::settings::{Appearance, GhostStyle};
use ghostcaret_render::{Image, Renderer, Theme};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetMonitorInfoW,
    HBITMAP, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromRect, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{
    HCF_HIGHCONTRASTON, HIGHCONTRASTW, IRawElementProviderSimple, NotificationKind_Other,
    NotificationProcessing_ImportantMostRecent, UiaHostProviderFromHwnd, UiaRaiseNotificationEvent,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE,
    RegisterClassExW, SPI_GETHIGHCONTRAST, SW_HIDE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowPos, ShowWindow,
    SystemParametersInfoW, ULW_ALPHA, UPDATE_LAYERED_WINDOW_FLAGS, UpdateLayeredWindow,
    WM_MOUSEACTIVATE, WM_NCHITTEST, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_THEMECHANGED,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{BSTR, w};

use super::mailbox::{Inbox, Ready, run};
use super::place::{CaretGuess, Px, card_at, font_px, suggestion_at};

pub enum Msg {
    /// What the gate shows now; `consumed` is the text typed or taken from
    /// the suggestion's front since the previous message.
    Overlay {
        overlay: Overlay,
        consumed: String,
    },
    /// The focused field's caret, and its bounds for placing a card when the
    /// caret is unknown.
    Caret {
        field: Option<FieldId>,
        caret: Option<Px>,
        bounds: Option<Px>,
    },
    Bindings(Bindings),
    Appearance(Appearance),
}

/// Card width in logical pixels.
const CARD_WIDTH: f32 = 520.0;
/// Card font size in logical pixels.
const CARD_FONT: f32 = 15.0;
/// Gap between the caret line and a card, in logical pixels.
const CARD_GAP: f32 = 6.0;

thread_local! {
    /// Set by the window procedure when the colour scheme or contrast changes.
    static THEME_CHANGED: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Clicks fall through to whatever is below; activation never happens.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            THEME_CHANGED.set(true);
            LRESULT(0)
        }
        // SAFETY: forwards the arguments the system passed in.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// A 32-bit top-down DIB selected into a memory DC, grown as needed.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    width: i32,
    height: i32,
}

impl Surface {
    fn new(width: i32, height: i32) -> Option<Surface> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        // SAFETY: `info` describes a valid 32-bit DIB and `bits` receives its pixels.
        let bitmap =
            unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }.ok()?;
        // SAFETY: creates a memory DC compatible with the screen.
        let dc = unsafe { CreateCompatibleDC(None) };
        // SAFETY: both handles were just created and are valid.
        let previous = unsafe { SelectObject(dc, bitmap.into()) };
        Some(Surface {
            dc,
            bitmap,
            previous,
            bits: bits.cast(),
            width,
            height,
        })
    }

    /// Copies `img` into the top-left corner.
    fn fill(&mut self, img: &Image) {
        let (w, stride) = (img.width as usize, self.width as usize);
        for (row, line) in img.pixels.chunks_exact(w).enumerate() {
            // SAFETY: the surface is at least as wide and tall as the image, so
            // row `row` of `w` pixels lies inside the DIB's pixel buffer.
            unsafe { std::ptr::copy_nonoverlapping(line.as_ptr(), self.bits.add(row * stride), w) };
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: restores the DC's original bitmap, then frees what `new` created.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct Painter {
    hwnd: HWND,
    renderer: Renderer,
    overlay: Overlay,
    field: Option<FieldId>,
    guess: CaretGuess,
    bounds: Option<Px>,
    appearance: Appearance,
    dark: bool,
    high_contrast: bool,
    surface: Option<Surface>,
    /// What the surface holds, so a caret move only moves the window.
    rendered: Option<Rendered>,
    visible: bool,
    provider: Option<IRawElementProviderSimple>,
    /// The suggestion last announced, so typing through it stays quiet.
    announced: Option<(FieldId, u64)>,
}

struct Rendered {
    overlay: Overlay,
    theme: Theme,
    max_w: u32,
    anchor: (f32, f32),
    size: (i32, i32),
}

/// Where and how to draw: the theme, the card's width limit, the point to
/// place at (the caret, or a stand-in), and the display's work area.
struct Target {
    theme: Theme,
    max_w: u32,
    near: Option<Px>,
    work: Px,
    gap: i32,
}

/// The work area and scale (physical pixels per logical pixel) of the
/// display showing `at`.
fn display(at: Option<Px>) -> (Px, f32) {
    // SAFETY: plain queries with valid in and out pointers.
    unsafe {
        let monitor = match at {
            Some(c) => MonitorFromRect(
                &RECT {
                    left: c.x,
                    top: c.y,
                    right: c.x + c.w,
                    bottom: c.y + c.h,
                },
                MONITOR_DEFAULTTONEAREST,
            ),
            None => MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY),
        };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let r = info.rcWork;
        let (mut dpi, mut unused) = (96u32, 0u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused);
        (
            Px {
                x: r.left,
                y: r.top,
                w: r.right - r.left,
                h: r.bottom - r.top,
            },
            dpi as f32 / 96.0,
        )
    }
}

fn dark_mode() -> bool {
    let mut light = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: reads a DWORD into `light`; `size` holds its capacity.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut light).cast::<c_void>()),
            Some(&mut size),
        )
    };
    status.is_ok() && light == 0
}

fn high_contrast() -> bool {
    let mut hc = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: `hc` is a HIGHCONTRASTW with cbSize set, as SPI_GETHIGHCONTRAST requires.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some((&raw mut hc).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_ok() && hc.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0
}

impl Painter {
    fn style(&self) -> GhostStyle {
        if self.high_contrast {
            GhostStyle::HighContrast
        } else {
            self.appearance.style
        }
    }

    fn suggestion_theme(&self, caret: Px, scale: f32) -> Theme {
        Theme {
            scale,
            font_px: font_px(caret.h, scale),
            style: self.style(),
            dark: self.dark,
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Overlay { overlay, consumed } => {
                if !consumed.is_empty() {
                    if let Some(caret) = self.guess.caret() {
                        let (_, scale) = display(Some(caret));
                        let theme = self.suggestion_theme(caret, scale);
                        self.guess
                            .advanced(self.renderer.text_width(&consumed, &theme));
                    }
                } else if let Overlay::Suggestion(v) = &overlay {
                    self.announce(v);
                }
                self.overlay = overlay;
            }
            Msg::Caret {
                field,
                caret,
                bounds,
            } => {
                if field != self.field {
                    self.field = field;
                    self.guess = CaretGuess::default();
                }
                self.guess.reported(caret);
                self.bounds = bounds;
            }
            Msg::Bindings(b) => self.renderer.set_bindings(&b),
            Msg::Appearance(a) => self.appearance = a,
        }
    }

    fn announce(&mut self, view: &SuggestionView) {
        if !self.appearance.announce || self.announced == Some((view.field, view.revision)) {
            return;
        }
        self.announced = Some((view.field, view.revision));
        if self.provider.is_none() {
            // SAFETY: our own window, alive for the thread's lifetime.
            self.provider = unsafe { UiaHostProviderFromHwnd(self.hwnd) }.ok();
        }
        if let Some(p) = &self.provider {
            // SAFETY: a valid provider and owned strings.
            let _ = unsafe {
                UiaRaiseNotificationEvent(
                    p,
                    NotificationKind_Other,
                    NotificationProcessing_ImportantMostRecent,
                    &BSTR::from(view.visible()),
                    &BSTR::from("ghostcaret.suggestion"),
                )
            };
        }
    }

    fn target(&self) -> Option<Target> {
        let style = self.style();
        match &self.overlay {
            Overlay::Hidden => None,
            Overlay::Suggestion(_) => {
                // Without a caret position the suggestion goes just below the
                // field, starting at its left edge.
                let caret = self.guess.caret().or_else(|| {
                    let b = self.bounds?;
                    let (_, scale) = display(Some(b));
                    Some(Px {
                        x: b.x,
                        y: b.bottom() + (CARD_GAP * scale) as i32,
                        w: 1,
                        h: (CARD_FONT * 1.3 * scale) as i32,
                    })
                })?;
                let (work, scale) = display(Some(caret));
                let theme = Theme {
                    scale,
                    font_px: font_px(caret.h, scale),
                    style,
                    dark: self.dark,
                };
                Some(Target {
                    theme,
                    max_w: 0,
                    near: Some(caret),
                    work,
                    gap: 0,
                })
            }
            Overlay::Card(_) => {
                let near = self.guess.caret().or(self.bounds);
                let (work, scale) = display(near);
                let gap = (CARD_GAP * scale) as i32;
                let max_w = ((CARD_WIDTH * scale) as i32).min(work.w - 2 * gap).max(1) as u32;
                Some(Target {
                    theme: Theme {
                        scale,
                        font_px: CARD_FONT,
                        style,
                        dark: self.dark,
                    },
                    max_w,
                    near,
                    work,
                    gap,
                })
            }
        }
    }

    fn paint(&mut self) {
        let Some(t) = self.target() else {
            return self.hide();
        };
        let stale = self
            .rendered
            .as_ref()
            .is_none_or(|r| r.overlay != self.overlay || r.theme != t.theme || r.max_w != t.max_w);
        if stale {
            let (image, anchor) = match &self.overlay {
                Overlay::Suggestion(v) => (
                    self.renderer.suggestion(v, &t.theme),
                    self.renderer.suggestion_anchor(&t.theme),
                ),
                Overlay::Card(c) => (self.renderer.card(c, &t.theme, t.max_w), (0.0, 0.0)),
                Overlay::Hidden => return self.hide(),
            };
            self.rendered = None;
            if image.width == 0 || image.height == 0 || !self.fill(&image) {
                return self.hide();
            }
            let size = (image.width as i32, image.height as i32);
            self.rendered = Some(Rendered {
                overlay: self.overlay.clone(),
                theme: t.theme,
                max_w: t.max_w,
                anchor,
                size,
            });
        }
        let r = self.rendered.as_ref().expect("rendered above");
        let at = match (&self.overlay, t.near) {
            (Overlay::Card(_), near) => {
                // Without a caret or field, the card sits in the middle of the display.
                let w = t.work;
                let caret = near.unwrap_or(Px {
                    x: w.x + (w.w - r.size.0) / 2,
                    y: w.y + w.h / 3,
                    w: 0,
                    h: 0,
                });
                card_at(caret, r.size, w, t.gap)
            }
            (_, Some(caret)) => suggestion_at(caret, r.anchor, r.size, t.work),
            (_, None) => return self.hide(),
        };
        let size = r.size;
        self.show(at, size, stale);
    }

    /// Copies `img` into the surface, growing it when needed.
    fn fill(&mut self, img: &Image) -> bool {
        let (w, h) = (img.width as i32, img.height as i32);
        if self
            .surface
            .as_ref()
            .is_none_or(|s| s.width < w || s.height < h)
        {
            let (cw, ch) = self
                .surface
                .as_ref()
                .map_or((w, h), |s| (s.width.max(w), s.height.max(h)));
            self.surface = None;
            self.surface = Surface::new(cw, ch);
        }
        let Some(surface) = self.surface.as_mut() else {
            return false;
        };
        surface.fill(img);
        true
    }

    /// Puts the window at `(x, y)`; with `redraw`, also shows the surface's
    /// top-left `size` pixels as its content.
    fn show(&mut self, (x, y): (i32, i32), (w, h): (i32, i32), redraw: bool) {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let size = SIZE { cx: w, cy: h };
        let origin = POINT::default();
        let src = self.surface.as_ref().filter(|_| redraw).map(|s| s.dc);
        // SAFETY: our window; with a source, a DC holding a premultiplied
        // 32-bit DIB at least `w` by `h`; without one only the position changes.
        let updated = unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&POINT { x, y }),
                src.map(|_| &raw const size),
                src,
                src.map(|_| &raw const origin),
                COLORREF(0),
                src.map(|_| &raw const blend),
                if src.is_some() {
                    ULW_ALPHA
                } else {
                    UPDATE_LAYERED_WINDOW_FLAGS(0)
                },
            )
        };
        if let Err(e) = updated {
            log::warn!("UpdateLayeredWindow failed: {e}");
            return;
        }
        if !self.visible {
            // SAFETY: our window; shows it above other topmost windows without activating it.
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
            };
            self.visible = true;
        }
    }

    fn hide(&mut self) {
        if self.visible {
            // SAFETY: our window.
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.visible = false;
        }
    }
}

fn create_window() -> Result<HWND, String> {
    // SAFETY: plain Win32 window creation with a static class name and a
    // window procedure that lives for the program.
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("ghostcaret.overlay");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(overlay_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        // Fails harmlessly when a previous start registered the class.
        RegisterClassExW(&wc);
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Ghostcaret"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| format!("cannot create the overlay window: {e}"))
    }
}

pub fn run_thread(inbox: Inbox<Msg>, ready: Ready) {
    // SAFETY: changes only this thread's DPI awareness, before it creates windows.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let hwnd = match create_window() {
        Ok(h) => h,
        Err(e) => return ready.fail(e),
    };
    ready.ok();
    let mut p = Painter {
        hwnd,
        renderer: Renderer::new(),
        overlay: Overlay::Hidden,
        field: None,
        guess: CaretGuess::default(),
        bounds: None,
        appearance: Appearance::default(),
        dark: dark_mode(),
        high_contrast: high_contrast(),
        surface: None,
        rendered: None,
        visible: false,
        provider: None,
        announced: None,
    };
    let p = std::cell::RefCell::new(&mut p);
    run(
        || {
            let mut p = p.borrow_mut();
            for msg in inbox.drain() {
                p.handle(msg);
            }
            p.paint();
        },
        || {
            if THEME_CHANGED.take() {
                let mut p = p.borrow_mut();
                p.dark = dark_mode();
                p.high_contrast = high_contrast();
                p.paint();
            }
            u32::MAX
        },
    );
    let p = p.into_inner();
    p.surface = None;
    // SAFETY: our window, destroyed on the thread that created it.
    let _ = unsafe { DestroyWindow(p.hwnd) };
}
