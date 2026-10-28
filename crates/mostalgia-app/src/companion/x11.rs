//! The X11 side of Mori's desktop window. GNOME on Wayland lets no Wayland
//! client stay on top or pick its position, but X11 clients (through
//! XWayland) can: `_NET_WM_STATE_ABOVE`, ConfigureWindow positions, an
//! input shape so clicks on her transparent surroundings reach the windows
//! below. All coordinates here are X11 root-window pixels.

#[cfg(target_os = "linux")]
use anyhow::{Context, Result};
#[cfg(target_os = "linux")]
use x11rb::connection::{Connection, RequestConnection};
#[cfg(target_os = "linux")]
use x11rb::protocol::randr::ConnectionExt as _;
#[cfg(target_os = "linux")]
use x11rb::protocol::shape::{self, ConnectionExt as _};
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{self, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, PropMode};
#[cfg(target_os = "linux")]
use x11rb::rust_connection::RustConnection;
#[cfg(target_os = "linux")]
use x11rb::wrapper::ConnectionExt as _;

#[cfg(target_os = "linux")]
x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_STATE_STICKY,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_UTILITY,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _MOTIF_WM_HINTS,
        _NET_WORKAREA,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = ((self.x + self.w).min(o.x + o.w), (self.y + self.h).min(o.y + o.h));
        (x1 > x0 && y1 > y0).then(|| Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }
}

/// A Monitor as RandR reports it.
#[derive(Clone, Copy, Debug)]
pub struct Monitor {
    pub rect: Rect,
    pub primary: bool,
}

#[cfg(target_os = "linux")]
pub struct X11 {
    conn: RustConnection,
    root: xproto::Window,
    atoms: Atoms,
    pub window: xproto::Window,
    shape: bool,
}

#[cfg(target_os = "linux")]
impl X11 {
    /// Connects to `$DISPLAY` (the XWayland server Mori's window lives on).
    pub fn connect(window: u32) -> Result<Self> {
        let (conn, screen) = x11rb::connect(None).context("connecting to the X server")?;
        let root = conn.setup().roots[screen].root;
        let atoms = Atoms::new(&conn)?.reply()?;
        let shape = conn.extension_information(shape::X11_EXTENSION_NAME).ok().flatten().is_some();
        Ok(Self { conn, root, atoms, window, shape })
    }

    /// Keeps the window above others, out of the taskbar, overview and
    /// Alt+Tab, and on every workspace.
    pub fn setup_window(&self) -> Result<()> {
        let a = &self.atoms;
        // A utility window: GNOME keeps it out of Alt+Tab and the overview,
        // and it still moves and takes keyboard focus for the chat input.
        self.conn.change_property32(
            PropMode::REPLACE,
            self.window,
            a._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[a._NET_WM_WINDOW_TYPE_UTILITY, a._NET_WM_WINDOW_TYPE_NORMAL],
        )?;
        // No title bar or frame (flags = decorations, decorations = none).
        self.conn.change_property32(PropMode::REPLACE, self.window, a._MOTIF_WM_HINTS, a._MOTIF_WM_HINTS, &[2, 0, 0, 0, 0])?;
        // _NET_WM_STATE_ADD, two properties per message.
        for pair in [
            [a._NET_WM_STATE_ABOVE, a._NET_WM_STATE_STICKY],
            [a._NET_WM_STATE_SKIP_TASKBAR, a._NET_WM_STATE_SKIP_PAGER],
        ] {
            let event = ClientMessageEvent::new(32, self.window, a._NET_WM_STATE, [1, pair[0], pair[1], 1, 0]);
            self.conn.send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Never takes keyboard focus (clicks still work): the live captions
    /// mustn't take it from the call window (ICCCM WM_HINTS input = false).
    pub fn no_input_focus(&self) -> Result<()> {
        self.conn.change_property32(PropMode::REPLACE, self.window, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, &[1, 0, 0, 0, 0, 0, 0, 0, 0])?;
        self.conn.flush()?;
        Ok(())
    }

    /// The window's top-left on the root window.
    pub fn position(&self) -> Result<(i32, i32)> {
        let r = self.conn.translate_coordinates(self.window, self.root, 0, 0)?.reply()?;
        Ok((r.dst_x as i32, r.dst_y as i32))
    }

    pub fn move_resize(&self, r: Rect) -> Result<()> {
        self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new().x(r.x).y(r.y).width(r.w.max(1) as u32).height(r.h.max(1) as u32),
        )?;
        self.conn.flush()?;
        Ok(())
    }

    /// Only these rectangles (window pixels) take mouse input; clicks
    /// elsewhere go to the window below.
    pub fn set_input_rects(&self, rects: &[Rect]) -> Result<()> {
        if !self.shape {
            return Ok(());
        }
        let rects: Vec<xproto::Rectangle> = rects
            .iter()
            .map(|r| xproto::Rectangle { x: r.x as i16, y: r.y as i16, width: r.w.max(0) as u16, height: r.h.max(0) as u16 })
            .collect();
        self.conn.shape_rectangles(shape::SO::SET, shape::SK::INPUT, xproto::ClipOrdering::UNSORTED, self.window, 0, 0, &rects)?;
        self.conn.flush()?;
        Ok(())
    }

    /// The pointer on the root window and whether the left button is down.
    pub fn pointer(&self) -> Result<(i32, i32, bool)> {
        let r = self.conn.query_pointer(self.root)?.reply()?;
        Ok((r.root_x as i32, r.root_y as i32, r.mask.contains(xproto::KeyButMask::BUTTON1)))
    }

    /// The desktop's work area (without panels), if the window manager
    /// says (`_NET_WORKAREA` of the current workspace).
    pub fn work_area(&self) -> Option<Rect> {
        let reply = self.conn.get_property(false, self.root, self.atoms._NET_WORKAREA, AtomEnum::CARDINAL, 0, 4).ok()?.reply().ok()?;
        let v: Vec<u32> = reply.value32()?.collect();
        (v.len() >= 4 && v[2] > 0 && v[3] > 0).then(|| Rect { x: v[0] as i32, y: v[1] as i32, w: v[2] as i32, h: v[3] as i32 })
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        let reply = self.conn.randr_get_monitors(self.root, true).ok().and_then(|c| c.reply().ok());
        let mut out: Vec<Monitor> = reply
            .map(|r| {
                r.monitors
                    .iter()
                    .map(|m| Monitor { rect: Rect { x: m.x as i32, y: m.y as i32, w: m.width as i32, h: m.height as i32 }, primary: m.primary })
                    .collect()
            })
            .unwrap_or_default();
        if out.is_empty() {
            let s = &self.conn.setup().roots[0];
            out.push(Monitor { rect: Rect { x: 0, y: 0, w: s.width_in_pixels as i32, h: s.height_in_pixels as i32 }, primary: true });
        }
        out
    }
}

/// No X11 here: `connect` always fails, so callers take their "not an X11
/// window" path (the window stays an ordinary one).
#[cfg(not(target_os = "linux"))]
pub struct X11 {
    never: std::convert::Infallible,
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::unused_self)]
impl X11 {
    pub fn connect(_window: u32) -> anyhow::Result<Self> {
        anyhow::bail!("X11 is not available on this system")
    }
    pub fn setup_window(&self) -> anyhow::Result<()> {
        match self.never {}
    }
    pub fn no_input_focus(&self) -> anyhow::Result<()> {
        match self.never {}
    }
    pub fn position(&self) -> anyhow::Result<(i32, i32)> {
        match self.never {}
    }
    pub fn move_resize(&self, _r: Rect) -> anyhow::Result<()> {
        match self.never {}
    }
    pub fn set_input_rects(&self, _rects: &[Rect]) -> anyhow::Result<()> {
        match self.never {}
    }
    pub fn pointer(&self) -> anyhow::Result<(i32, i32, bool)> {
        match self.never {}
    }
    pub fn work_area(&self) -> Option<Rect> {
        match self.never {}
    }
    pub fn monitors(&self) -> Vec<Monitor> {
        match self.never {}
    }
}
