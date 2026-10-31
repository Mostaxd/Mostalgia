//! The Windows side of Mori's desktop window and the live-captions window,
//! with the same calls as the X11 side (`x11.rs`, re-exported there as
//! `X11`): always on top, no taskbar or Alt+Tab entry, placement on a
//! monitor's work area, and clicks on her transparent surroundings going
//! to the windows below. All coordinates are physical screen pixels (GPUI
//! windows are per-monitor DPI aware).
//!
//! Click-through: Windows has no input shape, so a small thread watches the
//! pointer and makes the window transparent to the mouse
//! (`WS_EX_LAYERED | WS_EX_TRANSPARENT`) while it's outside every input
//! rectangle, and takes input again once it's over one.
//!
//! Moves and resizes go through a thread of their own: a `SetWindowPos`
//! from GPUI's thread sends `WM_SIZE` right away, in the middle of the
//! frame that asked for it, and GPUI drops the resize there (the app is
//! busy), so it kept laying the window out at its old size.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Result;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IsWindow, LWA_ALPHA, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_STATICEDGE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_POPUP, WS_SYSMENU,
};

use super::x11::{Monitor, Rect};

const MONITORINFOF_PRIMARY: u32 = 1;

pub struct Win32 {
    hwnd: isize,
    /// Input rectangles (window pixels) for the click-through watcher;
    /// `None` until set (the whole window takes input).
    input: Arc<Mutex<Option<Vec<Rect>>>>,
    watcher: Option<Arc<AtomicBool>>,
    /// Where the window should go, for the mover thread (see the top).
    mover: OnceLock<Sender<Rect>>,
}

impl Drop for Win32 {
    fn drop(&mut self) {
        if let Some(stop) = &self.watcher {
            stop.store(true, Ordering::SeqCst);
        }
    }
}

fn h(hwnd: isize) -> HWND {
    hwnd as HWND
}

fn rect_of(r: &RECT) -> Rect {
    Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
}

fn ex_style(hwnd: isize) -> isize {
    // SAFETY: reading a window's extended style; a stale handle reads 0.
    unsafe { GetWindowLongPtrW(h(hwnd), GWL_EXSTYLE) as isize }
}

fn style(hwnd: isize) -> isize {
    // SAFETY: reading a window's style; a stale handle reads 0.
    unsafe { GetWindowLongPtrW(h(hwnd), GWL_STYLE) as isize }
}

fn set_ex_style(hwnd: isize, style: isize) {
    // SAFETY: setting a window's extended style (allowed from any thread).
    unsafe {
        SetWindowLongPtrW(h(hwnd), GWL_EXSTYLE, style as _);
    }
}

fn window_rect(hwnd: isize) -> Result<Rect> {
    let mut r: RECT = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: `r` is a valid out pointer.
    anyhow::ensure!(unsafe { GetWindowRect(h(hwnd), &mut r) } != 0, "reading the window's position");
    Ok(rect_of(&r))
}

fn pointer_state() -> Result<(i32, i32, bool)> {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: `p` is a valid out pointer.
    anyhow::ensure!(unsafe { GetCursorPos(&mut p) } != 0, "reading the pointer position");
    // SAFETY: plain query of the async key state.
    let down = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000 != 0;
    Ok((p.x, p.y, down))
}

/// Whether the window should let the mouse through: the pointer is in the
/// window but outside every input rectangle (window pixels).
pub fn passes_through(window: Rect, input: &[Rect], pointer: (i32, i32)) -> bool {
    let (x, y) = (pointer.0 - window.x, pointer.1 - window.y);
    !input.iter().any(|r| r.contains(x, y))
}

impl Win32 {
    pub fn connect(hwnd: isize) -> Result<Self> {
        // SAFETY: checks a handle value.
        anyhow::ensure!(unsafe { IsWindow(h(hwnd)) } != 0, "not a window");
        Ok(Self { hwnd, input: Arc::default(), watcher: None, mover: OnceLock::new() })
    }

    /// Keeps the window above others and out of the taskbar and Alt+Tab,
    /// without any frame: GPUI's windows keep a caption (Windows gives
    /// every top-level window one), which showed as a thin outline around
    /// her and, as a tool window, a small close button at the top.
    pub fn setup_window(&self) -> Result<()> {
        let frame = (WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX) as isize;
        let window_style = (style(self.hwnd) & !frame) | WS_POPUP as isize;
        let edges = (WS_EX_APPWINDOW | WS_EX_DLGMODALFRAME | WS_EX_WINDOWEDGE | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE) as isize;
        let ex = (ex_style(self.hwnd) | WS_EX_TOOLWINDOW as isize) & !edges;
        // SAFETY: plain window calls on our own window; the attribute
        // values outlive the calls.
        unsafe {
            // The taskbar only drops a button when the window is shown anew.
            ShowWindow(h(self.hwnd), SW_HIDE);
            SetWindowLongPtrW(h(self.hwnd), GWL_STYLE, window_style as _);
            set_ex_style(self.hwnd, ex);
            // Windows 11 draws its own border and rounds the corners of
            // every window; neither here (fails harmlessly before 11).
            let none: u32 = DWMWA_COLOR_NONE;
            DwmSetWindowAttribute(h(self.hwnd), DWMWA_BORDER_COLOR as u32, &none as *const u32 as _, 4);
            let square = DWMWCP_DONOTROUND;
            DwmSetWindowAttribute(h(self.hwnd), DWMWA_WINDOW_CORNER_PREFERENCE as u32, &square as *const _ as _, 4);
            ShowWindow(h(self.hwnd), SW_SHOWNOACTIVATE);
            SetWindowPos(h(self.hwnd), HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
        Ok(())
    }

    /// Never takes keyboard focus when clicked (the live captions mustn't
    /// take it from the call window).
    pub fn no_input_focus(&self) -> Result<()> {
        set_ex_style(self.hwnd, ex_style(self.hwnd) | WS_EX_NOACTIVATE as isize);
        Ok(())
    }

    /// The window's top-left on the screen.
    pub fn position(&self) -> Result<(i32, i32)> {
        let r = window_rect(self.hwnd)?;
        Ok((r.x, r.y))
    }

    /// X11 keeps her pixels in place while the window resizes (bit
    /// gravity); Windows redraws the composited window as one frame, so
    /// there's nothing to set.
    pub fn set_gravity(&self, _right: bool, _bottom: bool) -> Result<()> {
        Ok(())
    }

    /// Moves the window a moment later, from the mover thread (see the
    /// top), so GPUI hears of the new size once it's free to.
    pub fn move_resize(&self, r: Rect) -> Result<()> {
        let mover = self.mover.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Rect>();
            let hwnd = self.hwnd;
            let spawned = std::thread::Builder::new().name("mori-mover".into()).spawn(move || {
                while let Ok(mut r) = rx.recv() {
                    // Only the latest of a burst (a drag).
                    if let Some(last) = rx.try_iter().last() {
                        r = last;
                    }
                    // SAFETY: plain window call; from this thread it's
                    // posted to the window's own (`SWP_ASYNCWINDOWPOS`).
                    unsafe {
                        SetWindowPos(h(hwnd), HWND_TOPMOST, r.x, r.y, r.w.max(1), r.h.max(1), SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
                    }
                }
            });
            if let Err(e) = spawned {
                eprintln!("[mori] window mover: {e}");
            }
            tx
        });
        mover.send(r).map_err(|_| anyhow::anyhow!("moving the window"))
    }

    /// Only these rectangles (window pixels) take mouse input; clicks
    /// elsewhere go to the window below.
    pub fn set_input_rects(&mut self, rects: &[Rect]) -> Result<()> {
        *self.input.lock().unwrap() = Some(rects.to_vec());
        if self.watcher.is_none() {
            let stop = Arc::new(AtomicBool::new(false));
            self.watcher = Some(stop.clone());
            let (hwnd, input) = (self.hwnd, self.input.clone());
            std::thread::Builder::new().name("mori-clickthrough".into()).spawn(move || watch_pointer(hwnd, &input, &stop))?;
        }
        Ok(())
    }

    /// The pointer on the screen and whether the left button is down.
    pub fn pointer(&self) -> Result<(i32, i32, bool)> {
        pointer_state()
    }

    /// The work area (without the taskbar) of monitor `m`.
    pub fn work_area_on(&self, m: &Rect) -> Rect {
        let r = RECT { left: m.x, top: m.y, right: m.x + m.w, bottom: m.y + m.h };
        // SAFETY: `info` is a properly sized MONITORINFO.
        unsafe {
            let mon = MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut info) != 0 {
                return rect_of(&info.rcWork).intersect(m).unwrap_or(*m);
            }
        }
        *m
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        unsafe extern "system" fn each(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
            // SAFETY: `data` is the `Vec<Monitor>` passed below.
            let out = unsafe { &mut *(data as *mut Vec<Monitor>) };
            let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(mon, &mut info) } != 0 {
                out.push(Monitor { rect: rect_of(&info.rcMonitor), primary: info.dwFlags & MONITORINFOF_PRIMARY != 0 });
            }
            1
        }
        let mut out: Vec<Monitor> = Vec::new();
        // SAFETY: the callback only runs during this call, with `out` alive.
        unsafe {
            EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(each), &mut out as *mut Vec<Monitor> as LPARAM);
        }
        if out.is_empty() {
            out.push(Monitor { rect: Rect { x: 0, y: 0, w: 1920, h: 1080 }, primary: true });
        }
        out
    }
}

/// Switches the window between taking the mouse and letting it through,
/// following the pointer, until `stop` or the window is gone. Nothing
/// changes while the left button is held (a drag in progress).
fn watch_pointer(hwnd: isize, input: &Mutex<Option<Vec<Rect>>>, stop: &AtomicBool) {
    let mut through = false;
    let mut layered = false;
    // SAFETY: checks a handle value.
    while !stop.load(Ordering::SeqCst) && unsafe { IsWindow(h(hwnd)) } != 0 {
        std::thread::sleep(Duration::from_millis(40));
        let Ok((x, y, down)) = pointer_state() else { continue };
        if down {
            continue;
        }
        let Ok(window) = window_rect(hwnd) else { continue };
        let want = match input.lock().unwrap().as_deref() {
            Some(rects) => window.contains(x, y) && passes_through(window, rects, (x, y)),
            None => false,
        };
        if want == through {
            continue;
        }
        let mut style = ex_style(hwnd);
        if want {
            if !layered {
                // Layered windows stay fully visible at alpha 255.
                style |= WS_EX_LAYERED as isize;
                set_ex_style(hwnd, style);
                // SAFETY: plain window call.
                unsafe { SetLayeredWindowAttributes(h(hwnd), 0, 255, LWA_ALPHA) };
                layered = true;
            }
            style |= WS_EX_TRANSPARENT as isize;
        } else {
            style &= !(WS_EX_TRANSPARENT as isize);
        }
        set_ex_style(hwnd, style);
        through = want;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mouse_passes_through_outside_the_input_rects_only() {
        let window = Rect { x: 1000, y: 200, w: 300, h: 400 };
        let mori = [Rect { x: 16, y: 16, w: 200, h: 300 }];
        assert!(!passes_through(window, &mori, (1100, 300)), "on her");
        assert!(passes_through(window, &mori, (1290, 590)), "transparent corner");
        assert!(passes_through(window, &[], (1100, 300)), "no input at all");
    }

    #[test]
    fn there_is_always_a_monitor() {
        let w = Win32 { hwnd: 0, input: Arc::default(), watcher: None, mover: OnceLock::new() };
        assert!(!w.monitors().is_empty());
    }
}
