//! Mori on the desktop: `mostalgia-app --mori` runs her in a small,
//! borderless, transparent X11 window (through XWayland, so GNOME lets it
//! stay above other windows and sit anywhere on any monitor). The app
//! starts this process and talks to it over stdin / stdout (see `ipc`).
//!
//! The window is just big enough for her, and grows towards the middle of
//! her monitor while the chat bubble or point cards are shown, so she
//! stays where she was put. Only her and those cards take mouse input;
//! clicks on the transparent rest go to the windows below.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use gpui_kit::*;
use mostalgia_core::config::Config;

use super::feed::{Feed, RemoteFeed};
use super::ipc::{self, FromMori, ToMori};
use super::x11::{Rect, X11};
use super::{Companion, MoriEvent};
use crate::{AppAssets, load_fonts, ui};

/// Room around her in the small window (logical px).
pub(super) const PAD: f32 = 16.;
/// Extra width for the chat bubble / cards beside her.
pub(super) const SIDE_W: f32 = 404.;
/// Minimum height of the grown window (the bubble is up to 500 px).
pub(super) const GROWN_MIN_H: f32 = 580.;
/// Room for the cards above or below her.
pub(super) const CARDS_H: f32 = 300.;
/// The point cards' width (as in `points.rs`).
pub(super) const CARD_W: f32 = 280.;

/// Where her window is and how it's laid out around her.
pub(crate) struct Placement {
    pub x: X11,
    pub scale: f32,
    /// Her top-left on screen (root pixels).
    pub mori: (i32, i32),
    /// The bubble and cards go left of her (she's on the right half of her
    /// monitor) / above her (lower half).
    pub right: bool,
    pub bottom: bool,
    applied: Option<Rect>,
    applied_input: Vec<Rect>,
    /// A drag in progress: pointer and window position when it began, and
    /// whether the window manager took over the move.
    pub drag: Option<DesktopDrag>,
}

#[derive(Clone, Copy)]
pub(crate) struct DesktopDrag {
    pub pointer: (i32, i32),
    pub window: (i32, i32),
    pub wm_move: bool,
    pub since: Instant,
}

impl Placement {
    pub fn new(x: X11, scale: f32) -> Self {
        Self { x, scale, mori: (0, 0), right: true, bottom: false, applied: None, applied_input: Vec::new(), drag: None }
    }

    /// The window size and her top-left in it (logical px), small or grown.
    pub fn layout(&self, (w, h): (f32, f32), grown: bool) -> ((f32, f32), (f32, f32)) {
        if !grown {
            return ((w + 2. * PAD, h + 2. * PAD), (PAD, PAD));
        }
        let size = (w + 2. * PAD + SIDE_W, (h + 2. * PAD + CARDS_H).max(GROWN_MIN_H));
        let ox = if self.right { size.0 - w - PAD } else { PAD };
        let oy = if self.bottom { size.1 - h - PAD } else { PAD };
        (size, (ox, oy))
    }

    fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    /// Moves / resizes the window so she stays at `self.mori`.
    pub fn apply_window(&mut self, mori_size: (f32, f32), grown: bool) {
        let (size, o) = self.layout(mori_size, grown);
        let rect = Rect { x: self.mori.0 - self.px(o.0), y: self.mori.1 - self.px(o.1), w: self.px(size.0), h: self.px(size.1) };
        if self.applied != Some(rect) {
            if let Err(e) = self.x.move_resize(rect) {
                eprintln!("[mori] moving the window: {e:#}");
            }
            self.applied = Some(rect);
        }
    }

    /// `apply_window`, and only `input` (window rectangles, logical px)
    /// takes the mouse.
    pub fn apply(&mut self, mori_size: (f32, f32), grown: bool, input: &[(f32, f32, f32, f32)]) {
        self.apply_window(mori_size, grown);
        let input: Vec<Rect> = input
            .iter()
            .map(|&(x, y, w, h)| Rect { x: self.px(x), y: self.px(y), w: self.px(w), h: self.px(h) })
            .collect();
        if input != self.applied_input {
            if let Err(e) = self.x.set_input_rects(&input) {
                eprintln!("[mori] input region: {e:#}");
            }
            self.applied_input = input;
        }
    }

    /// Her top-left from where the window is now (after a move).
    pub fn read_back(&mut self, mori_size: (f32, f32), grown: bool) {
        let (_, o) = self.layout(mori_size, grown);
        if let Ok((x, y)) = self.x.position() {
            self.mori = (x + self.px(o.0), y + self.px(o.1));
            self.applied = None;
        }
    }

    /// Keeps her fully on a monitor (the one her middle is on, else the
    /// primary one) and picks the side the bubble opens to.
    pub fn settle(&mut self, (w, h): (f32, f32)) {
        let monitors = self.x.monitors();
        let (pw, ph) = (self.px(w), self.px(h));
        let center = (self.mori.0 + pw / 2, self.mori.1 + ph / 2);
        let monitor = monitors
            .iter()
            .find(|m| m.rect.contains(center.0, center.1))
            .or_else(|| monitors.iter().find(|m| m.primary))
            .or(monitors.first())
            .map(|m| m.rect);
        let Some(m) = monitor else { return };
        if !m.contains(center.0, center.1) {
            // Her monitor is gone: top right of the primary one.
            self.mori = (m.x + m.w - pw - self.px(48.), m.y + self.px(140.));
        }
        self.mori.0 = self.mori.0.clamp(m.x, (m.x + m.w - pw).max(m.x));
        self.mori.1 = self.mori.1.clamp(m.y, (m.y + m.h - ph).max(m.y));
        let center = (self.mori.0 + pw / 2, self.mori.1 + ph / 2);
        self.right = center.0 > m.x + m.w / 2;
        self.bottom = center.1 > m.y + m.h / 2;
    }

    pub fn save(&self) {
        let mut config = Config::load();
        config.companion_desktop_pos = Some(self.mori);
        if let Err(e) = config.save() {
            eprintln!("[mori] saving her position: {e:#}");
        }
    }
}

/// Writes one message to the app (stdout).
pub(crate) fn send(msg: &FromMori) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(ipc::encode(msg).as_bytes());
    let _ = out.flush();
}

/// Entry point of `mostalgia-app --mori [--demo]`.
pub fn run(demo: bool) {
    // XWayland, not Wayland: only X11 windows may stay on top and be
    // placed on GNOME.
    let env = GraphicalEnvironment { wayland_display: None, ..GraphicalEnvironment::detect() };
    let rx = spawn_stdin_reader(demo);
    gpui_kit::application().with_assets(AppAssets).with_windowing(WindowingRequest::Windowed(env)).run(move |cx: &mut App| {
        gpui_kit::init(cx);
        let config = Config::load();
        mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&config.ui_language));
        load_fonts(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(200.), px(300.)) })),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::Normal,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            // "Server" so GPUI draws no client border or shadow; `X11::setup_window`
            // then tells the window manager to draw no decorations either.
            window_decorations: Some(WindowDecorations::Server),
            app_id: Some("io.github.mostaxd.Mostalgia.Mori".into()),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            ui::apply_theme(window, cx);
            window.set_window_title("Mori");
            let view = cx.new(|cx| DesktopMori::new(rx, demo, window, cx));
            cx.new(|cx| gpui_kit::component::Root::new(view, window, cx).bg(transparent_black()))
        });
        if let Err(e) = opened {
            eprintln!("[mori] opening the window: {e:#}");
            cx.quit();
        }
    });
}

fn spawn_stdin_reader(demo: bool) -> Receiver<ToMori> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ToMori>(&line) {
                Ok(msg) => {
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
                Err(e) => eprintln!("[mori] bad message: {e}"),
            }
        }
        // The app is gone (or closed the pipe): so is she. The demo runs
        // without an app.
        if !demo {
            let _ = tx.send(ToMori::Quit);
        }
    });
    rx
}

pub struct DesktopMori {
    companion: Entity<Companion>,
    feed: Entity<RemoteFeed>,
    rx: Receiver<ToMori>,
    demo: Option<Instant>,
    _subscriptions: Vec<Subscription>,
}

impl DesktopMori {
    fn new(rx: Receiver<ToMori>, demo: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let feed = cx.new(|_| RemoteFeed::default());
        let placement = window_id(window).and_then(|id| X11::connect(id).ok()).map(|x| {
            if let Err(e) = x.setup_window() {
                eprintln!("[mori] window setup: {e:#}");
            }
            Placement::new(x, window.scale_factor())
        });
        if placement.is_none() {
            eprintln!("[mori] not an X11 window; she can't stay on top");
        }
        let companion = cx.new(|cx| Companion::new_desktop(Feed::Remote(feed.clone()), placement, window, cx));
        let sub = cx.subscribe(&companion, |_, _, event: &MoriEvent, _| match event {
            MoriEvent::OpenCitation { meeting_id, secs } => send(&FromMori::OpenMeeting { meeting_id: *meeting_id, secs: *secs }),
            MoriEvent::OpenSettings => send(&FromMori::OpenSettings),
        });
        let moment = cx.subscribe(&companion, |_, _, event: &super::ShowLiveMoment, _| send(&FromMori::ShowLiveMoment { secs: event.secs }));
        cx.observe_window_appearance(window, |_, window, cx| ui::apply_theme(window, cx)).detach();
        let this = Self { companion, feed, rx, demo: demo.then(Instant::now), _subscriptions: vec![sub, moment] };
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                if this.update_in(cx, |this, window, cx| this.poll(window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        send(&FromMori::Ready);
        this
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                ToMori::Live { live } => self.feed.update(cx, |f, _| f.set_live(live)),
                ToMori::Transcript { session, from, lines, interim } => {
                    self.feed.update(cx, |f, _| f.set_transcript(session, from, lines, interim))
                }
                ToMori::Saved { meeting_id } => {
                    self.companion.update(cx, |c, cx| c.recording_saved(meeting_id, cx));
                    send(&FromMori::NotesSaved { meeting_id });
                }
                ToMori::ReloadConfig => {
                    let config = Config::load();
                    mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&config.ui_language));
                    ui::apply_theme(window, cx);
                    self.companion.update(cx, |c, cx| c.reload_config(cx));
                    cx.refresh_windows();
                }
                ToMori::Quit => {
                    cx.quit();
                    return;
                }
            }
        }
        if let Some(start) = self.demo {
            self.demo_step(start, cx);
        }
    }

    /// `--demo`: a fake recording with speech now and then and a few point
    /// cards, without the app or a network.
    fn demo_step(&mut self, start: Instant, cx: &mut Context<Self>) {
        use super::ipc::WireLive;
        let t = start.elapsed().as_secs_f64();
        let speaking = t % 7. < 4.;
        let since = (t % 7.) * 1000.;
        let live = WireLive {
            session: 1,
            paused: false,
            speech_ago_ms: speaking.then_some(0),
            speech_since_ago_ms: speaking.then_some(since as u64),
            speech_secs: 0.,
            last_line_end: t,
        };
        self.feed.update(cx, |f, _| f.set_live(Some(live)));
        if t > 6. && self.demo.is_some_and(|_| !DEMO_POINTS_SHOWN.with(|s| s.replace(true))) {
            use mostalgia_core::companion::PointKind;
            self.companion.update(cx, |c, cx| {
                c.demo_points(
                    &[
                        (PointKind::Decision, "Release moves to Friday.", 62.),
                        (PointKind::Action, "Anna sends the updated plan by Thursday.", 95.),
                        (PointKind::Question, "Who reviews the migration?", 130.),
                    ],
                    &[1],
                    cx,
                )
            });
        }
    }
}

thread_local! {
    static DEMO_POINTS_SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The X11 window id behind a GPUI window.
fn window_id(window: &Window) -> Option<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        RawWindowHandle::Xlib(h) => Some(h.window as u32),
        _ => None,
    }
}

impl Render for DesktopMori {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().relative().font(ui::ui_font(_cx)).child(self.companion.clone())
    }
}
