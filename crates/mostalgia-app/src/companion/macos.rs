//! The macOS side of Mori's desktop window and the live captions: the same
//! interface as `x11.rs`, on the window's NSWindow. Floating above other
//! windows on every Space (also next to full-screen apps), no Dock icon,
//! placement on the screens' visible frames, and clicks outside the input
//! rectangles pass through to the windows below.
//!
//! Coordinates are the same kind as on X11: pixels with the origin at the
//! top left of the primary screen (Cocoa's points are bottom-left based),
//! at the window's backing scale when it was connected.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result};
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSFloatingWindowLevel, NSPanel, NSScreen, NSView, NSWindow, NSWindowButton,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSTimer};

use super::x11::{Monitor, Rect};

/// What `window_id` gives on macOS: the window's NSView.
pub type WindowId = usize;

pub struct MacWindow {
    window: Retained<NSWindow>,
    mtm: MainThreadMarker,
    scale: f64,
    /// Where the window takes the mouse (window pixels); `None` = everywhere.
    input: Rc<RefCell<Option<Vec<Rect>>>>,
    /// Switches click-through as the pointer moves (macOS has no input shapes).
    timer: RefCell<Option<Retained<NSTimer>>>,
}

impl Drop for MacWindow {
    fn drop(&mut self) {
        if let Some(t) = self.timer.borrow_mut().take() {
            t.invalidate();
        }
    }
}

/// The primary screen's height in points (Cocoa's y axis is flipped at it).
fn primary_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm).firstObject().map_or(0.0, |s| s.frame().size.height)
}

fn to_rect(r: NSRect, h0: f64, scale: f64) -> Rect {
    let top = h0 - (r.origin.y + r.size.height);
    Rect {
        x: (r.origin.x * scale).round() as i32,
        y: (top * scale).round() as i32,
        w: (r.size.width * scale).round() as i32,
        h: (r.size.height * scale).round() as i32,
    }
}

fn from_rect(r: Rect, h0: f64, scale: f64) -> NSRect {
    let (w, h) = (r.w.max(1) as f64 / scale, r.h.max(1) as f64 / scale);
    NSRect::new(NSPoint::new(r.x as f64 / scale, h0 - r.y as f64 / scale - h), NSSize::new(w, h))
}

/// Whether window-local pixel `(x, y)` (top-left origin) takes the mouse.
pub(crate) fn takes_mouse(input: &Option<Vec<Rect>>, x: i32, y: i32) -> bool {
    input.as_ref().is_none_or(|rects| rects.iter().any(|r| r.contains(x, y)))
}

impl MacWindow {
    /// The NSWindow behind a GPUI window's NSView.
    pub fn connect(view: WindowId) -> Result<Self> {
        let mtm = MainThreadMarker::new().context("not on the main thread")?;
        // SAFETY: `view` is the live NSView GPUI's AppKit handle points at.
        let view: &NSView = unsafe { &*(view as *const NSView) };
        let window = view.window().context("the view has no window")?;
        let scale = window.backingScaleFactor();
        Ok(Self { window, mtm, scale, input: Rc::default(), timer: RefCell::default() })
    }

    /// Floats above other windows on every Space, without title bar
    /// buttons, shadow or Dock icon (this process is only her window).
    pub fn setup_window(&self) -> Result<()> {
        NSApplication::sharedApplication(self.mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        let w = &self.window;
        w.setLevel(NSFloatingWindowLevel);
        w.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary | NSWindowCollectionBehavior::IgnoresCycle,
        );
        w.setHidesOnDeactivate(false);
        w.setHasShadow(false);
        w.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        w.setTitlebarAppearsTransparent(true);
        for b in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
            if let Some(button) = w.standardWindowButton(b) {
                button.setHidden(true);
            }
        }
        Ok(())
    }

    /// Keeps keyboard focus with the app in front: a panel only takes it
    /// for text input. (Our process is an accessory app after
    /// `setup_window`, so clicking it doesn't bring Mostalgia forward.)
    pub fn no_input_focus(&self) -> Result<()> {
        if let Some(panel) = self.window.downcast_ref::<NSPanel>() {
            panel.setBecomesKeyOnlyIfNeeded(true);
        }
        Ok(())
    }

    /// Lets the window be resized at its edges (GPUI's frameless windows
    /// aren't on macOS, and the corner grip's resize isn't implemented there).
    pub fn allow_resize(&self) {
        let mask = self.window.styleMask();
        self.window.setStyleMask(mask | NSWindowStyleMask::Resizable);
    }

    pub fn position(&self) -> Result<(i32, i32)> {
        let r = to_rect(self.window.frame(), primary_height(self.mtm), self.scale);
        Ok((r.x, r.y))
    }

    pub fn move_resize(&self, r: Rect) -> Result<()> {
        self.window.setFrame_display(from_rect(r, primary_height(self.mtm), self.scale), true);
        Ok(())
    }

    /// Only these rectangles (window pixels) take mouse input; elsewhere
    /// the window ignores the mouse, so clicks reach the window below.
    pub fn set_input_rects(&self, rects: &[Rect]) -> Result<()> {
        *self.input.borrow_mut() = Some(rects.to_vec());
        if self.timer.borrow().is_none() {
            let (window, input, scale) = (self.window.clone(), self.input.clone(), self.scale);
            let block = RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
                // Not mid-drag: the window keeps what it has until release.
                if NSEvent::pressedMouseButtons() & 1 != 0 {
                    return;
                }
                let m = NSEvent::mouseLocation();
                let f = window.frame();
                let x = ((m.x - f.origin.x) * scale) as i32;
                let y = ((f.origin.y + f.size.height - m.y) * scale) as i32;
                let take = takes_mouse(&input.borrow(), x, y);
                if window.ignoresMouseEvents() == take {
                    window.setIgnoresMouseEvents(!take);
                }
            });
            let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 30.0, true, &block) };
            *self.timer.borrow_mut() = Some(timer);
        }
        Ok(())
    }

    /// The pointer and whether the left button is down.
    pub fn pointer(&self) -> Result<(i32, i32, bool)> {
        let m = NSEvent::mouseLocation();
        let h0 = primary_height(self.mtm);
        let down = NSEvent::pressedMouseButtons() & 1 != 0;
        Ok(((m.x * self.scale).round() as i32, ((h0 - m.y) * self.scale).round() as i32, down))
    }

    /// The visible frame (without menu bar and Dock) of monitor `m` (one of
    /// `monitors`), or all of it if that screen is gone.
    pub fn work_area_on(&self, m: &Rect) -> Rect {
        let h0 = primary_height(self.mtm);
        let (cx, cy) = (m.x + m.w / 2, m.y + m.h / 2);
        NSScreen::screens(self.mtm)
            .iter()
            .find(|s| to_rect(s.frame(), h0, self.scale).contains(cx, cy))
            .and_then(|s| to_rect(s.visibleFrame(), h0, self.scale).intersect(m))
            .unwrap_or(*m)
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        let h0 = primary_height(self.mtm);
        NSScreen::screens(self.mtm)
            .iter()
            .enumerate()
            .map(|(i, s)| Monitor { rect: to_rect(s.frame(), h0, self.scale), primary: i == 0 })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flips_and_scales_cocoa_rects() {
        // A 1440x900 pt primary screen at 2x: a 100x50 window 10 pt from
        // the left and 20 pt from the top.
        let cocoa = NSRect::new(NSPoint::new(10.0, 900.0 - 20.0 - 50.0), NSSize::new(100.0, 50.0));
        let r = to_rect(cocoa, 900.0, 2.0);
        assert_eq!(r, Rect { x: 20, y: 40, w: 200, h: 100 });
        let back = from_rect(r, 900.0, 2.0);
        assert_eq!((back.origin.x, back.origin.y, back.size.width, back.size.height), (10.0, 830.0, 100.0, 50.0));
    }

    #[test]
    fn input_rects_decide_click_through() {
        assert!(takes_mouse(&None, 5, 5));
        assert!(!takes_mouse(&Some(Vec::new()), 5, 5));
        let rects = Some(vec![Rect { x: 10, y: 10, w: 20, h: 20 }]);
        assert!(takes_mouse(&rects, 15, 15));
        assert!(!takes_mouse(&rects, 5, 5));
    }
}
