//! Mori, the optional note-taking companion: an animated character that
//! writes along while people speak, answers live questions and pops up
//! points she noticed. She is drawn over the window's content, can be
//! dragged anywhere inside it, and opens a chat bubble when clicked.

pub mod art;
mod bubble;
pub use bubble::ShowLiveMoment;
mod points;
pub mod desktop;
pub mod feed;
pub mod host;
pub mod ipc;
mod x11;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::ask::AskEvent;
use mostalgia_core::companion::{AnswerEvent, Point};
use mostalgia_core::config::Config;
use mostalgia_core::i18n::{self, tr};

use crate::recording::{LiveState, RecordingView};
use feed::Feed;
use art::Pose;

pub enum MoriEvent {
    /// Open a meeting at this moment (a citation in an answer).
    OpenCitation { meeting_id: i64, secs: f64 },
    /// Open Settings → AI & transcription (no connection set up).
    OpenSettings,
    /// She was dragged out of the app window: move her to the desktop.
    PopOut,
    /// "Back into the app" on the desktop.
    BackToApp,
}

impl EventEmitter<MoriEvent> for Companion {}

/// Her height at scale 1.
const BASE_HEIGHT: f32 = 220.;
/// Distance she keeps from the window edges.
const MARGIN: f32 = 16.;
/// Default distance from the window top: below the title bar and the
/// recording header, so she doesn't cover its buttons.
const DEFAULT_TOP: f32 = 132.;
/// How far past the window edge a drag pops her out to the desktop.
const POP_OUT_MARGIN: f32 = 12.;
/// Mouse travel (px) after which a press is a drag, not a click.
const DRAG_THRESHOLD: f32 = 4.;
/// Upper bound for the animation timer, whatever the art asks for.
const MAX_FPS: u32 = 24;
/// Speech counts as ongoing this long after it was last heard.
const SPEECH_HOLD: Duration = Duration::from_millis(1500);
/// How long she looks up when someone starts speaking.
const LISTEN_FOR: Duration = Duration::from_millis(900);
/// Idle this long outside a recording, she dozes off.
const DOZE_AFTER: Duration = Duration::from_secs(300);

/// Which conversation a question goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The live transcript of the recording in progress.
    Meeting,
    /// All saved meetings (the "Fragen" search pipeline).
    Library,
}

pub(crate) struct Turn {
    scope: Scope,
    question: String,
    /// Answer so far (library answers carry [[m:ID@SECS]] markers).
    answer: String,
    status: Option<String>,
    error: Option<String>,
    done: bool,
}

enum ChatRx {
    Live(Receiver<AnswerEvent>),
    Library(Receiver<AskEvent>),
}

/// Which connection answers, for the bubble's header.
#[derive(Clone)]
pub(crate) struct Setup {
    name: String,
    model: String,
    /// The local label ("Local · nothing leaves this computer") for local
    /// connections; `None` for cloud ones.
    local: Option<&'static str>,
    /// Why she can't answer yet (no connection or key).
    problem: Option<String>,
}

pub(crate) struct LivePoint {
    id: usize,
    point: Point,
    pinned: bool,
    dismissed: bool,
    shown_at: Instant,
}

/// The last failure, shown next to her with her Error pose.
#[derive(Clone)]
pub(crate) struct MoriError {
    message: String,
    /// From a live points request (they pause until dismissed or retried).
    points: bool,
}

struct Drag {
    mouse: Point2,
    origin: Point2,
    moved: bool,
}

#[derive(Clone, Copy)]
struct Point2 {
    x: f32,
    y: f32,
}

pub struct Companion {
    /// The recording she follows (this window's, or the app's over IPC).
    recording: Feed,
    /// She runs in her own desktop window (`mostalgia-app --mori`).
    desktop_mode: bool,
    /// That window's X11 side (`None` in the app window, or without X11).
    desktop: Option<desktop::Placement>,
    /// The recording seen last (a new one starts a fresh chat).
    session_seen: Option<u64>,
    enabled: bool,
    scale: f32,
    points_interval: u32,
    /// Top-left as fractions of the free room (see `Config::companion_pos`).
    pos: Option<(f32, f32)>,
    reduce_motion: bool,
    /// Rendered frames for the current pixel height.
    frames: HashMap<(Pose, usize), Arc<Image>>,
    frames_px: u32,
    pose: Pose,
    frame: usize,
    pose_since: Instant,
    ticking: bool,
    /// Window content size at the last render.
    viewport: (f32, f32),
    drag: Option<Drag>,
    was_recording: bool,
    idle_since: Instant,
    /// Show the answer/point pose a little longer after it arrived.
    talking_until: Option<Instant>,
    error: Option<MoriError>,
    // Chat bubble
    open: bool,
    input: Entity<InputState>,
    turns: Vec<Turn>,
    chat_rx: Option<ChatRx>,
    scope_choice: Option<Scope>,
    chat_scroll: ScrollHandle,
    setup: Option<Setup>,
    bubble: bubble::BubbleState,
    lang_seen: Option<i18n::Lang>,
    // Live points
    points: Vec<LivePoint>,
    next_point: usize,
    points_rx: Option<Receiver<Result<Vec<Point>, String>>>,
    /// Finalized speech (secs) at the last points request.
    points_mark: f64,
    /// Transcript time the next points request starts from.
    points_from: f64,
    expanded: bool,
    visible_seen: usize,
    /// Screenshots: a fixed pose.
    demo_pose: Option<Pose>,
    _subscriptions: Vec<Subscription>,
}

impl Companion {
    /// Mori over the app window's content, following its recording screen.
    pub fn new(recording: Entity<RecordingView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(Feed::Window(recording), false, None, window, cx)
    }

    /// Mori in her own desktop window.
    pub(crate) fn new_desktop(feed: Feed, placement: Option<desktop::Placement>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(feed, true, placement, window, cx)
    }

    fn build(recording: Feed, desktop_mode: bool, placement: Option<desktop::Placement>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Frag Mori …")));
        let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let q = input.read(cx).value().to_string();
                input.update(cx, |s, cx| s.set_value("", window, cx));
                this.ask(q, cx);
            }
        });
        let mut this = Self {
            recording,
            desktop_mode,
            desktop: placement,
            session_seen: None,
            enabled: false,
            scale: 1.,
            points_interval: 0,
            pos: None,
            reduce_motion: false,
            frames: HashMap::new(),
            frames_px: 0,
            pose: Pose::Idle,
            frame: 0,
            pose_since: Instant::now(),
            ticking: false,
            viewport: (1200., 800.),
            drag: None,
            was_recording: false,
            idle_since: Instant::now(),
            talking_until: None,
            error: None,
            open: false,
            input,
            turns: Vec::new(),
            chat_rx: None,
            scope_choice: None,
            chat_scroll: ScrollHandle::new(),
            setup: None,
            bubble: Default::default(),
            lang_seen: None,
            points: Vec::new(),
            next_point: 0,
            points_rx: None,
            points_mark: 0.,
            points_from: 0.,
            expanded: false,
            visible_seen: 0,
            demo_pose: None,
            _subscriptions: vec![sub],
        };
        this.reload_config(cx);
        this
    }

    /// Re-reads her settings (after the settings screen changed them).
    pub fn reload_config(&mut self, cx: &mut Context<Self>) {
        let config = Config::load();
        // On the desktop she lives in her own window, not over the app's
        // (whose process only runs while she's turned on).
        self.enabled = self.desktop_mode || config.companion && !config.companion_desktop;
        self.scale = config.companion_scale.clamp(0.5, 2.);
        self.points_interval = config.companion_points_interval_secs;
        self.pos = config.companion_pos;
        let size = self.size();
        if let Some(p) = &mut self.desktop
            && p.drag.is_none()
        {
            // No position yet (or reset): `settle` puts her top right.
            p.mori = config.companion_desktop_pos.unwrap_or((i32::MIN / 2, i32::MIN / 2));
            p.settle(size);
        }
        self.reduce_motion = config.reduce_motion;
        if !self.enabled {
            self.open = false;
            self.drag = None;
        }
        if self.setup.is_some() {
            self.load_setup();
        }
        cx.notify();
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn size(&self) -> (f32, f32) {
        let h = BASE_HEIGHT * self.scale;
        (h * art::FRAME_W / art::FRAME_H, h)
    }

    /// Free room for her top-left corner inside the window.
    fn room(&self) -> (f32, f32) {
        let (w, h) = self.size();
        ((self.viewport.0 - w - 2. * MARGIN).max(0.), (self.viewport.1 - h - 2. * MARGIN).max(0.))
    }

    /// On the desktop: whether her window is grown for the bubble or cards.
    fn grown(&self) -> bool {
        self.open || self.expanded || self.error.is_some() || !self.visible_points().is_empty()
    }

    /// Her top-left in window coordinates.
    fn origin(&self) -> Point2 {
        if let Some(p) = &self.desktop {
            let (_, (x, y)) = p.layout(self.size(), self.grown());
            return Point2 { x, y };
        }
        let (rx, ry) = self.room();
        match self.pos {
            Some((fx, fy)) => Point2 { x: MARGIN + fx.clamp(0., 1.) * rx, y: MARGIN + fy.clamp(0., 1.) * ry },
            None => Point2 { x: MARGIN + rx, y: DEFAULT_TOP.min(MARGIN + ry) },
        }
    }

    fn set_origin(&mut self, x: f32, y: f32) {
        let (rx, ry) = self.room();
        let fx = if rx > 0. { ((x - MARGIN) / rx).clamp(0., 1.) } else { 0. };
        let fy = if ry > 0. { ((y - MARGIN) / ry).clamp(0., 1.) } else { 0. };
        self.pos = Some((fx, fy));
    }

    fn save_pos(&self) {
        let mut config = Config::load();
        config.companion_pos = self.pos;
        if let Err(e) = config.save() {
            eprintln!("[mostalgia] saving Mori's position: {e:#}");
        }
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let o = self.origin();
        if let Some(p) = &mut self.desktop {
            let pointer = p.x.pointer().map(|(x, y, _)| (x, y)).unwrap_or_default();
            p.drag = Some(desktop::DesktopDrag { pointer, window: p.mori, wm_move: false, since: Instant::now() });
        }
        self.drag = Some(Drag {
            mouse: Point2 { x: ev.position.x.into(), y: ev.position.y.into() },
            origin: o,
            moved: false,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.drag else { return };
        let (x, y): (f32, f32) = (ev.position.x.into(), ev.position.y.into());
        let (dx, dy) = (x - drag.mouse.x, y - drag.mouse.y);
        if !drag.moved && dx.hypot(dy) < DRAG_THRESHOLD {
            return;
        }
        let first = !drag.moved;
        drag.moved = true;
        if let Some(p) = &mut self.desktop {
            let Some(d) = &mut p.drag else { return };
            if first {
                // The window manager moves the window (across monitors,
                // with its snapping); `tick` notices when it's dropped.
                d.wm_move = true;
                d.since = Instant::now();
                window.start_window_move();
            } else if d.wm_move && d.since.elapsed() > Duration::from_millis(250) && ev.pressed_button == Some(MouseButton::Left) {
                // Still getting the motion: the window manager didn't take
                // over, so move the window ourselves.
                d.wm_move = false;
            }
            if !d.wm_move {
                let d = *d;
                if let Ok((px_, py_, _)) = p.x.pointer() {
                    p.mori = (d.window.0 + px_ - d.pointer.0, d.window.1 + py_ - d.pointer.1);
                    let (size, grown) = (self.size(), self.grown());
                    if let Some(p) = &mut self.desktop {
                        p.apply_window(size, grown);
                    }
                }
            }
            return;
        }
        // Dragged past the window edge: she moves to the desktop.
        let (vw, vh) = self.viewport;
        let outside = x < -POP_OUT_MARGIN || y < -POP_OUT_MARGIN || x > vw + POP_OUT_MARGIN || y > vh + POP_OUT_MARGIN;
        // Not every compositor reports motion outside the window: reaching
        // the very edge counts as well.
        let at_edge = x <= 1. || y <= 1. || x >= vw - 1. || y >= vh - 1.;
        if vw > 0. && (outside || at_edge) {
            self.drag = None;
            cx.emit(MoriEvent::PopOut);
            cx.notify();
            return;
        }
        let (ox, oy) = (drag.origin.x + dx, drag.origin.y + dy);
        self.set_origin(ox, oy);
        cx.notify();
    }

    /// Whether she runs in her own desktop window.
    pub fn is_desktop(&self) -> bool {
        self.desktop.is_some()
    }

    fn mouse_up(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        if self.desktop.is_some() {
            if drag.moved {
                self.end_desktop_drag(false);
            } else {
                if let Some(p) = &mut self.desktop {
                    p.drag = None;
                }
                self.toggle_bubble(cx);
            }
            self.idle_since = Instant::now();
            cx.notify();
            return;
        }
        if drag.moved {
            self.save_pos();
        } else {
            self.toggle_bubble(cx);
        }
        self.idle_since = Instant::now();
        cx.notify();
    }

    /// A drag of her desktop window ended: where is she now? Keeps her on
    /// a monitor, turns the bubble towards its middle and remembers it.
    fn end_desktop_drag(&mut self, wm_moved: bool) {
        let (size, grown) = (self.size(), self.grown());
        let Some(p) = &mut self.desktop else { return };
        if wm_moved {
            p.read_back(size, grown);
        }
        p.drag = None;
        p.settle(size);
        p.save();
        self.drag = None;
    }

    /// Sizes and moves her desktop window for what's shown, keeping her in
    /// place, and lets only her, the bubble and the cards take the mouse.
    fn place_desktop_window(&mut self, o: Point2) {
        let (w, h) = self.size();
        let grown = self.grown();
        let cards = self.expanded || self.error.is_some() || !self.visible_points().is_empty();
        let open = self.open;
        let Some(p) = &mut self.desktop else { return };
        let ((vw, vh), _) = p.layout((w, h), grown);
        self.viewport = (vw, vh);
        // While the window manager moves the window, leave it alone.
        if p.drag.is_some_and(|d| d.wm_move) && self.drag.as_ref().is_some_and(|d| d.moved) {
            return;
        }
        let pad = desktop::PAD;
        let mut input = vec![(o.x - pad, o.y - pad, w + 2. * pad, h + 2. * pad)];
        if open {
            // The bubble's side, full height.
            input.push(if p.right { (0., 0., (o.x - 4.).max(0.), vh) } else { (o.x + w + 4., 0., vw - (o.x + w + 4.), vh) });
        }
        if cards {
            let (x0, x1) = if p.right { ((o.x + w - desktop::CARD_W - 8.).max(0.), o.x + w + pad) } else { ((o.x - pad).max(0.), o.x + desktop::CARD_W + 8.) };
            let (y0, y1) = if self.expanded {
                (0., vh)
            } else if p.bottom {
                (0., o.y)
            } else {
                (o.y + h, vh)
            };
            input.push((x0, y0, x1 - x0, y1 - y0));
        }
        p.apply((w, h), grown, &input);
    }

    /// The frame image for her current pose, rendered at the pixel size
    /// she is drawn at (cached per pose and frame).
    fn frame_image(&mut self, px_h: u32) -> Arc<Image> {
        if px_h != self.frames_px {
            self.frames.clear();
            self.frames_px = px_h;
        }
        let (pose, frame) = (self.pose, self.frame % art::frame_count(self.pose).max(1));
        self.frames
            .entry((pose, frame))
            .or_insert_with(|| Arc::new(Image::from_bytes(ImageFormat::Svg, scaled_svg(pose, frame, px_h).into_bytes())))
            .clone()
    }

    fn thinking(&self) -> bool {
        self.points_rx.is_some() || self.chat_rx.is_some() && self.turns.last().is_some_and(|t| t.answer.is_empty())
    }

    fn talking(&self) -> bool {
        self.chat_rx.is_some() || self.talking_until.is_some_and(|t| Instant::now() < t)
    }

    fn wanted_pose(&self, live: Option<LiveState>) -> Pose {
        if let Some(p) = self.demo_pose {
            return p;
        }
        if self.error.is_some() {
            return Pose::Error;
        }
        if self.thinking() {
            return Pose::Thinking;
        }
        if self.talking() {
            return Pose::Talking;
        }
        let now = Instant::now();
        match live {
            Some(l) if l.paused => Pose::Sleeping,
            Some(l) => {
                let speaking = l.speech_at.is_some_and(|t| now - t < SPEECH_HOLD);
                if speaking && l.speech_since.is_some_and(|t| now - t < LISTEN_FOR) {
                    Pose::Listening
                } else if speaking {
                    Pose::Writing
                } else {
                    Pose::Idle
                }
            }
            None if self.idle_since.elapsed() > DOZE_AFTER => Pose::Sleeping,
            None => Pose::Idle,
        }
    }

    /// A new recording clears what's left of the last one; the bubble's
    /// default conversation follows whether one is running.
    /// Returns whether a new recording began (the chat starts fresh).
    fn track_recording(&mut self, recording: bool, session: Option<u64>) -> bool {
        let new_session = session.is_some() && session != self.session_seen;
        if session.is_some() {
            self.session_seen = session;
        }
        if recording == self.was_recording && !new_session {
            return false;
        }
        let mut reset = false;
        self.was_recording = recording;
        self.scope_choice = None;
        self.idle_since = Instant::now();
        if recording {
            self.points.clear();
            self.points_mark = 0.;
            self.points_from = 0.;
            self.expanded = false;
            reset = true;
        }
        reset
    }

    fn start_ticking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ticking = true;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let delay = this.update_in(cx, |this, window, cx| this.tick(window, cx)).ok().flatten();
                let Some(delay) = delay else { break };
                cx.background_executor().timer(delay).await;
            }
        })
        .detach();
    }

    /// One animation step plus the bookkeeping (poses, answers, points).
    /// Returns when to run again, `None` to stop (she's turned off).
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Duration> {
        if !self.enabled {
            self.ticking = false;
            return None;
        }
        let (live, session) = {
            let feed = self.recording.read(cx);
            (feed.live_state(), feed.session_id())
        };
        if self.track_recording(live.is_some(), session) {
            self.reset(cx);
        }
        // The window manager dropped her window somewhere.
        let wm_moving = self.drag.as_ref().is_some_and(|d| d.moved) && self.desktop.as_ref().and_then(|p| p.drag).is_some_and(|d| d.wm_move);
        if wm_moving && self.desktop.as_ref().and_then(|p| p.x.pointer().ok()).is_some_and(|(_, _, down)| !down) {
            self.end_desktop_drag(true);
            self.idle_since = Instant::now();
            cx.notify();
        }
        let moving = self.drag.as_ref().is_some_and(|d| d.moved) && self.desktop.is_some();
        let mut changed = self.poll_chat(cx);
        changed |= self.poll_points();
        if let Some(l) = live {
            self.maybe_request_points(l, cx);
        }
        let visible = self.visible_points().len();
        changed |= visible != self.visible_seen;
        self.visible_seen = visible;

        let pose = self.wanted_pose(live);
        // With reduced motion she holds each pose a while (errors show at once).
        let hold = self.reduce_motion && pose != Pose::Error && self.pose_since.elapsed() < Duration::from_secs(2);
        if pose != self.pose && !hold {
            self.pose = pose;
            self.frame = 0;
            self.pose_since = Instant::now();
            changed = true;
        }
        let busy = self.chat_rx.is_some() || self.points_rx.is_some();
        // On the desktop she's always in view, not only in an active window.
        let animate = !self.reduce_motion && (self.desktop_mode || window.is_window_active()) && self.demo_pose.is_none();
        if animate && art::frame_count(self.pose) > 1 {
            self.frame = (self.frame + 1) % art::frame_count(self.pose);
            changed = true;
        }
        if changed {
            cx.notify();
        }
        Some(if moving {
            Duration::from_millis(50)
        } else if animate {
            Duration::from_secs_f32(1. / art::FPS.clamp(1, MAX_FPS) as f32)
        } else if busy {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(400)
        })
    }

    /// Stores the pinned points with the meeting just saved, then clears them.
    pub fn save_pinned(&mut self, meeting_id: i64, cx: &mut Context<Self>) {
        let pinned: Vec<&LivePoint> = self.points.iter().filter(|p| p.pinned && !p.dismissed).collect();
        if !pinned.is_empty() {
            let result = mostalgia_core::store::Store::open(&mostalgia_core::paths::database()).and_then(|store| {
                for p in &pinned {
                    store.add_note(meeting_id, p.point.kind.as_str(), &p.point.text, p.point.at_secs)?;
                }
                Ok(())
            });
            if let Err(e) = result {
                self.error = Some(MoriError {
                    message: mostalgia_core::i18n::trf("Moris Notizen konnten nicht gespeichert werden: {}", &[&format!("{e:#}")]),
                    points: false,
                });
            }
        }
        self.points.clear();
        self.expanded = false;
        cx.notify();
    }

    /// A recording was saved as `meeting_id`: her pinned points (and the
    /// chat) go with it.
    pub fn recording_saved(&mut self, meeting_id: i64, cx: &mut Context<Self>) {
        self.save_pinned(meeting_id, cx);
        self.save_session_chat(meeting_id, cx);
    }

    /// Back to normal after the screenshot demos.
    #[doc(hidden)]
    pub fn demo_reset(&mut self, cx: &mut Context<Self>) {
        self.demo_pose = None;
        self.open = false;
        self.turns.clear();
        self.points.clear();
        self.scope_choice = None;
        cx.notify();
    }

    /// Fixes her pose (screenshots).
    #[doc(hidden)]
    pub fn demo_pose(&mut self, pose: Option<Pose>, cx: &mut Context<Self>) {
        self.demo_pose = pose;
        if let Some(p) = pose {
            self.pose = p;
            self.frame = 0;
        }
        cx.notify();
    }
}

/// `art::frame_svg` wrapped in an outer SVG of the size it is drawn at, so
/// it is rasterized sharp instead of scaled from 200 × 320.
fn scaled_svg(pose: Pose, frame: usize, px_h: u32) -> String {
    let inner = art::frame_svg(pose, frame);
    let inner = match inner.trim_start().strip_prefix("<?xml") {
        Some(rest) => rest.split_once("?>").map_or("", |(_, r)| r).to_string(),
        None => inner,
    };
    let h = px_h.max(1);
    let w = ((h as f32) * art::FRAME_W / art::FRAME_H).round() as u32;
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{w}" height="{h}" viewBox="0 0 {} {}">{inner}</svg>"#,
        art::FRAME_W,
        art::FRAME_H
    )
}

impl Render for Companion {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.enabled {
            return div().into_any_element();
        }
        if self.lang_seen != Some(i18n::lang()) {
            self.lang_seen = Some(i18n::lang());
            self.input.update(cx, |s, cx| s.set_placeholder(tr("Frag Mori …"), window, cx));
        }
        let vp = window.viewport_size();
        self.viewport = (vp.width.into(), vp.height.into());
        if !self.ticking {
            self.start_ticking(window, cx);
        }
        let (w, h) = self.size();
        let o = self.origin();
        if self.desktop.is_some() {
            self.place_desktop_window(o);
        }
        let image = self.frame_image((h * window.scale_factor()).round() as u32);
        let dragging = self.drag.as_ref().is_some_and(|d| d.moved);

        let character = div()
            .id("mori")
            .absolute()
            .left(px(o.x))
            .top(px(o.y))
            .w(px(w))
            .h(px(h))
            .when(!dragging, |el| el.cursor_pointer())
            .when(dragging, |el| el.cursor_grabbing())
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| this.mouse_down(ev, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.mouse_up(cx)))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| this.mouse_move(ev, window, cx)))
            // (Her desktop window is too small for the tooltip.)
            .when(!self.desktop_mode, |el| {
                el.tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr("Mori · klicken zum Fragen, ziehen zum Verschieben")).build(window, cx))
            })
            .child(img(image).size_full());

        let bubble = self.render_bubble(o, (w, h), cx);
        let stack = self.render_points(o, (w, h), cx);
        let badge = self.render_badge(o, (w, h), cx);
        let theme = cx.theme();
        let name_tag = (self.pose == Pose::Error).then(|| {
            h_flex()
                .absolute()
                .left(px(o.x))
                .top(px(o.y + h - 6.))
                .w(px(w))
                .justify_center()
                .child(div().px_2().rounded_full().bg(theme.danger).text_color(theme.danger_foreground).text_xs().font_semibold().child(tr("Fehler")))
        });

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .children(stack)
            .child(character)
            .children(name_tag)
            .children(badge)
            .children(bubble)
            // While dragging, a layer over everything keeps the mouse even
            // when it outruns her.
            .when(self.drag.is_some(), |el| {
                el.child(
                    div()
                        .id("mori-drag")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .when(dragging, |el| el.cursor_grabbing())
                        .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| this.mouse_move(ev, window, cx)))
                        .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.mouse_up(cx)))
                        .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.mouse_up(cx)))
                        // Pointer motion outside the window (during the
                        // press) doesn't hit any element; listen window-wide.
                        .when(self.desktop.is_none(), |el| {
                            let this = cx.entity().downgrade();
                            el.child(canvas(|_, _, _| {}, move |_, _, window, _| {
                                let this = this.clone();
                                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                                    if phase == DispatchPhase::Capture {
                                        this.update(cx, |this, cx| this.mouse_move(ev, window, cx)).ok();
                                    }
                                });
                            }).absolute().size_full())
                        }),
                )
            })
            .into_any_element()
    }
}
