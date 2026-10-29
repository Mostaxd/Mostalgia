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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IsWindow, LWA_ALPHA, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

use super::x11::{Monitor, Rect};

const MONITORINFOF_PRIMARY: u32 = 1;

pub struct Win32 {
    hwnd: isize,
    /// Input rectangles (window pixels) for the click-through watcher;
    /// `None` until set (the whole window takes input).
    input: Arc<Mutex<Option<Vec<Rect>>>>,
    watcher: Option<Arc<AtomicBool>>,
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
        Ok(Self { hwnd, input: Arc::default(), watcher: None })
    }

    /// Keeps the window above others and out of the taskbar and Alt+Tab.
    pub fn setup_window(&self) -> Result<()> {
        let style = (ex_style(self.hwnd) | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize);
        // SAFETY: plain window calls on our own window.
        unsafe {
            // The taskbar only drops a button when the window is shown anew.
            ShowWindow(h(self.hwnd), SW_HIDE);
            set_ex_style(self.hwnd, style);
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

    pub fn move_resize(&self, r: Rect) -> Result<()> {
        // SAFETY: plain window call on our own window.
        let ok = unsafe { SetWindowPos(h(self.hwnd), HWND_TOPMOST, r.x, r.y, r.w.max(1), r.h.max(1), SWP_NOACTIVATE) };
        anyhow::ensure!(ok != 0, "moving the window");
        Ok(())
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
        let w = Win32 { hwnd: 0, input: Arc::default(), watcher: None };
        assert!(!w.monitors().is_empty());
    }
}
