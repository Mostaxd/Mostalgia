//! The optional note-taking companion (Mori, Tsumugi or Hoshi, see
//! `character`): an animated character that writes along while people
//! speak, answers live questions and pops up points she noticed. She is
//! drawn over the window's content, can be dragged anywhere inside it, and
//! opens a chat bubble when clicked. (The code calls her Mori, the first.)

mod answers;
pub mod art;
pub mod captions;
pub mod cast;
pub mod character;
mod bubble;
pub use bubble::ShowLiveMoment;
mod controls;
pub use controls::BarIndicator;
mod points;
pub mod desktop;
pub mod feed;
pub mod host;
pub mod ipc;
mod x11;
#[cfg(target_os = "macos")]
mod macos;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::ask::AskEvent;
use mostalgia_core::companion::{AnswerEvent, NoteStyle, Point, PointKind, PointsReply};
use mostalgia_core::config::{Config, NotesMode};
use mostalgia_core::i18n::{self, tr, trf};

use crate::recording::{LiveState, RecordingView};
use feed::Feed;
use art::Pose;
use character::Character;

pub enum MoriEvent {
    /// Open a meeting at this moment (a citation in an answer).
    OpenCitation { meeting_id: i64, secs: f64 },
    /// Open a document at a page or section (a citation).
    OpenDocument { doc_id: i64, loc: u32 },
    /// Open Settings → AI & transcription (no connection set up).
    OpenSettings,
    /// She was dragged out of the app window: move her to the desktop.
    PopOut,
    /// "Back into the app" on the desktop.
    BackToApp,
    /// Her points were stored with this meeting (or its notes changed).
    NotesSaved { meeting_id: i64 },
    /// Her record button: start a recording from exactly these sources,
    /// with the people in the call if picked (0 = automatic).
    StartRecording { mic: bool, system: bool, people: u8 },
    /// Pause / resume the recording in progress.
    SetPaused { paused: bool },
    /// Stop (and save) the recording in progress.
    StopRecording,
    /// Switch a source of the recording in progress off or on.
    SetSource { source: mostalgia_core::Source, on: bool },
    /// "Settings" in her menu: Settings → Companion.
    OpenMoriSettings,
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
    Live,
    /// The saved meeting open in the app.
    Open,
    /// All saved meetings (the "Fragen" search pipeline).
    Library,
}

pub(crate) struct Turn {
    scope: Scope,
    /// For `Scope::Open`: the meeting asked about.
    meeting: Option<i64>,
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
    /// Hidden from the stack (dismissed or cleared); still saved.
    dismissed: bool,
    /// When the card was first on screen (its fold timer).
    shown_at: Option<Instant>,
    /// When it may appear: cards that arrive together come one by one.
    release_at: Instant,
    /// Its arrival was announced (her talking, the "N new" pill, the log).
    announced: bool,
    /// When the line that triggered its request was finalized (for the log).
    line_at: Option<Instant>,
    /// When its request returned (for the log).
    arrived: Instant,
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
    notes_mode: NotesMode,
    /// As it happens: least seconds between two checks.
    notes_gap: f64,
    /// A card shrinks to a line this long after it appeared (`None` = never).
    card_collapse: Option<Duration>,
    /// Cards that arrive together appear this far apart.
    card_stagger: Duration,
    /// The focus hint from the settings (`Config::companion_notes_focus`).
    default_focus: String,
    /// The note style from the settings (`Config::companion_notes_style`)…
    default_style: NoteStyle,
    /// …and this recording's choice from her bubble (`None` = the default).
    style_override: Option<NoteStyle>,
    /// This recording's focus hint (empty = the default), set in her bubble
    /// or with the record button; saved with the meeting.
    focus_input: Entity<InputState>,
    /// The bubble's focus field is open.
    focus_editing: bool,
    /// Empty the focus field at the next render (the recording was saved).
    focus_clear: bool,
    /// The focus field's placeholder as last set.
    focus_placeholder: String,
    /// Top-left as fractions of the free room (see `Config::companion_pos`).
    pos: Option<(f32, f32)>,
    reduce_motion: bool,
    /// Who she is (`Config::companion_character`).
    character: Character,
    /// Rendered frames for the current pixel height.
    frames: HashMap<(Character, Pose, usize), Arc<Image>>,
    frames_px: u32,
    /// The "a note arrived" animation playing (Tsumugi's and Hoshi's), and
    /// its rendered frames.
    pop: Option<(PointKind, Instant)>,
    pop_frames: HashMap<(Character, PointKind, usize, u32), Arc<Image>>,
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
    /// The meeting open in the app (id, title), for the "This meeting" scope.
    open_meeting: Option<(i64, String)>,
    chat_scroll: ScrollHandle,
    setup: Option<Setup>,
    bubble: bubble::BubbleState,
    lang_seen: Option<i18n::Lang>,
    // Live points
    points: Vec<LivePoint>,
    next_point: usize,
    points_rx: Option<Receiver<Result<PointsReply, String>>>,
    points_run: Option<points::PointsRun>,
    points_runs: usize,
    /// Since when a due request waits for the transcript to catch up.
    points_wait_since: Option<Instant>,
    /// The recording as last seen (for a last request when it ends).
    last_live: Option<LiveState>,
    /// The recording was saved as this meeting while a request ran.
    late_save: Option<i64>,
    points_scroll: ScrollHandle,
    /// The list follows new cards (it's scrolled to her end).
    points_follow: bool,
    points_user_scrolled: bool,
    points_scroll_to_end: bool,
    /// New cards that arrived while scrolled away.
    points_unseen: usize,
    points_above: bool,
    /// The stack's bounds at the last paint, and as last given to the
    /// desktop window's input region.
    stack_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    stack_applied: Option<Bounds<Pixels>>,
    /// Finalized speech (secs) at the last points request.
    points_mark: f64,
    /// When to ask next and what to send (see `points::NotesState`).
    notes: points::NotesState,
    /// The compact card under the mouse (shown in full).
    hovered_point: Option<usize>,
    expanded: bool,
    /// Cards shown and how many of them compact, at the last tick.
    visible_seen: (usize, usize),
    /// Screenshots: a fixed pose.
    demo_pose: Option<Pose>,
    /// Her figure is hidden: the collapsed bar stands in for her
    /// (`Config::companion_hidden`).
    hidden: bool,
    /// Record controls, the Notes tab and her menu.
    ctl: controls::ControlsState,
    /// Suggested answers to questions asked in the recording.
    answers: answers::AnswersState,
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
        let input = cx.new(|cx| InputState::new(window, cx));
        let focus_input = cx.new(|cx| InputState::new(window, cx));
        let focus_sub = cx.subscribe_in(&focus_input, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.focus_editing = false;
                cx.notify();
            }
        });
        let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let q = input.read(cx).value().to_string();
                input.update(cx, |s, cx| s.set_value("", window, cx));
                this.ask(q, cx);
            }
        });
        // Her menu closes on Escape and when her window loses focus (on the
        // desktop: a click anywhere else).
        let escape = cx.observe_keystrokes(|this: &mut Self, ev, _, cx| {
            if ev.keystroke.key == "escape" && this.ctl.menu.is_some() {
                this.ctl.menu = None;
                cx.notify();
            }
        });
        let unfocus = cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            if !window.is_window_active() && this.ctl.menu.is_some() {
                this.ctl.menu = None;
                cx.notify();
            }
        });
        let mut this = Self {
            recording,
            desktop_mode,
            desktop: placement,
            session_seen: None,
            enabled: false,
            scale: 1.,
            notes_mode: NotesMode::Off,
            notes_gap: mostalgia_core::companion::LIVE_NOTES_GAP,
            card_collapse: None,
            card_stagger: Duration::ZERO,
            default_focus: String::new(),
            default_style: NoteStyle::Default,
            style_override: None,
            focus_input,
            focus_editing: false,
            focus_clear: false,
            focus_placeholder: String::new(),
            pos: None,
            reduce_motion: false,
            character: Character::Mori,
            frames: HashMap::new(),
            frames_px: 0,
            pop: None,
            pop_frames: HashMap::new(),
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
            open_meeting: None,
            chat_scroll: ScrollHandle::new(),
            setup: None,
            bubble: Default::default(),
            lang_seen: None,
            points: Vec::new(),
            next_point: 0,
            points_rx: None,
            points_run: None,
            points_runs: 0,
            points_wait_since: None,
            last_live: None,
            late_save: None,
            points_scroll: ScrollHandle::new(),
            points_follow: true,
            points_user_scrolled: false,
            points_scroll_to_end: false,
            points_unseen: 0,
            points_above: false,
            stack_bounds: Rc::new(Cell::new(None)),
            stack_applied: None,
            points_mark: 0.,
            notes: Default::default(),
            hovered_point: None,
            expanded: false,
            visible_seen: (0, 0),
            demo_pose: None,
            hidden: false,
            ctl: Default::default(),
            answers: Default::default(),
            _subscriptions: vec![sub, focus_sub, escape, unfocus],
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
        self.notes_mode = config.notes_mode();
        self.notes_gap = config.notes_gap() as f64;
        self.card_collapse = config.card_collapse().map(Duration::from_secs_f32);
        self.card_stagger = Duration::from_secs_f32(config.card_stagger());
        self.default_focus = config.companion_notes_focus.trim().to_string();
        self.default_style = config.notes_style();
        self.pos = config.companion_pos;
        self.hidden = config.companion_hidden;
        self.answers.configure(config.companion_answers, config.answer_sources());
        let character = config.companion_character();
        if character != self.character {
            self.character = character;
            self.frames.clear();
            self.pop = None;
            self.frame = 0;
            // Her name in the placeholders.
            self.lang_seen = None;
        }
        let size = self.size();
        if let Some(p) = &mut self.desktop
            && p.drag.is_none()
        {
            // No position yet (or reset): `settle` puts her top right.
            p.mori = config.companion_desktop_pos.unwrap_or((i32::MIN / 2, i32::MIN / 2));
            p.settle(size);
        }
        self.reduce_motion = config.reduce_motion;
        self.ctl.bar_indicator = controls::BarIndicator::parse(&config.companion_bar_indicator);
        if !self.enabled {
            self.open = false;
            self.drag = None;
        }
        if self.setup.is_some() {
            self.load_setup();
        }
        cx.notify();
    }

    /// What her notes focus on now: this recording's hint, else the
    /// default from the settings ("" = nothing special).
    fn notes_focus_in(&self, cx: &App) -> String {
        let own = self.focus_input.read(cx).value().trim().to_string();
        if own.is_empty() { self.default_focus.clone() } else { own }
    }

    /// The note style for this recording: chosen in her bubble, else the
    /// default from the settings.
    fn notes_style_now(&self) -> NoteStyle {
        self.style_override.unwrap_or(self.default_style)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Her size, or the collapsed bar's while she's hidden.
    fn size(&self) -> (f32, f32) {
        if self.hidden {
            return (controls::bar_w(), controls::bar_h());
        }
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
        self.open || self.expanded || self.error.is_some() || self.ctl.menu.is_some() || !self.visible_points().is_empty()
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
            eprintln!("[mostalgia] saving the companion's position: {e:#}");
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
        let Some(p) = &self.desktop else { return };
        let ((vw, vh), _) = p.layout((w, h), grown);
        self.viewport = (vw, vh);
        let menu = self.menu_rect(o);
        // While the window manager moves the window, leave it alone.
        if p.drag.is_some_and(|d| d.wm_move) && self.drag.as_ref().is_some_and(|d| d.moved) {
            return;
        }
        // (The bar takes the mouse only on itself.)
        let pad = if self.hidden { 2. } else { desktop::PAD };
        let mut input = vec![(o.x - pad, o.y - pad, w + 2. * pad, h + 2. * pad)];
        if let Some(m) = menu {
            input.push(m);
        }
        if open {
            // The bubble's side, full height.
            input.push(if p.right { (0., 0., (o.x - 4.).max(0.), vh) } else { (o.x + w + 4., 0., vw - (o.x + w + 4.), vh) });
        }
        if cards {
            // The stack as painted last (a frame behind: `tick` re-renders
            // when it changes), else its whole region.
            let b = self.stack_bounds.get();
            self.stack_applied = b;
            let rect = match b {
                Some(b) => (f32::from(b.origin.x) - 2., f32::from(b.origin.y) - 2., f32::from(b.size.width) + 4., f32::from(b.size.height) + 4.),
                None => {
                    let r = self.points_region(o, (w, h));
                    let y0 = if r.above { r.anchor - r.max_h } else { r.anchor };
                    (r.left - 3., y0, r.width + 6., r.max_h)
                }
            };
            input.push(rect);
        }
        if let Some(p) = &mut self.desktop {
            p.apply((w, h), grown, &input);
        }
    }

    /// Her name, for the UI.
    pub(crate) fn name(&self) -> &'static str {
        self.character.name()
    }

    /// The frame image for her current pose, rendered at the pixel size
    /// she is drawn at (cached per character, pose and frame).
    fn frame_image(&mut self, px_h: u32) -> Arc<Image> {
        if px_h != self.frames_px {
            self.frames.clear();
            self.frames_px = px_h;
        }
        let c = self.character;
        let (pose, frame) = (self.pose, self.frame % character::frame_count(c, self.pose).max(1));
        self.frames
            .entry((c, pose, frame))
            .or_insert_with(|| Arc::new(Image::from_bytes(ImageFormat::Svg, character::scaled(character::frame_svg(c, pose, frame), character::frame_size(c), px_h).into_bytes())))
            .clone()
    }

    /// How long a pop stays: played once, then held a moment.
    fn pop_len(&self) -> Option<Duration> {
        let frames = character::pop_frames(self.character)?;
        Some(Duration::from_secs_f32(frames as f32 / character::fps(self.character).max(1) as f32) + Duration::from_millis(600))
    }

    /// A note arrived: her own little animation of it, if she has one
    /// (not with reduced motion, or while she's hidden).
    pub(crate) fn start_pop(&mut self, kind: PointKind) {
        if self.reduce_motion || self.hidden || character::pop_frames(self.character).is_none() {
            return;
        }
        self.pop = Some((kind, Instant::now()));
    }

    /// The pop over her, while it plays.
    fn render_pop(&mut self, o: Point2, (w, h): (f32, f32), scale: f32) -> Option<AnyElement> {
        let (kind, since) = self.pop?;
        let len = self.pop_len()?;
        if since.elapsed() >= len || self.hidden {
            self.pop = None;
            return None;
        }
        let c = self.character;
        let frames = character::pop_frames(c)?;
        let frame = ((since.elapsed().as_secs_f32() * character::fps(c) as f32) as usize).min(frames - 1);
        let size = (w * 0.34).round();
        let px_ = (size * scale).round().max(1.) as u32;
        let image = match self.pop_frames.get(&(c, kind, frame, px_)) {
            Some(i) => i.clone(),
            None => {
                let (svg, side) = character::pop_svg(c, kind, frame)?;
                let image = Arc::new(Image::from_bytes(ImageFormat::Svg, character::scaled(svg, (side, side), px_).into_bytes()));
                if self.pop_frames.len() > 256 {
                    self.pop_frames.clear();
                }
                self.pop_frames.insert((c, kind, frame, px_), image.clone());
                image
            }
        };
        // Up by her head, on the side towards her cards.
        Some(div().absolute().left(px(o.x + w * 0.6)).top(px(o.y + h * 0.02 - size * 0.25)).w(px(size)).h(px(size)).child(img(image).size_full()).into_any_element())
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
            self.notes = Default::default();
            self.points_wait_since = None;
            self.points_follow = true;
            self.points_unseen = 0;
            self.expanded = false;
            self.answers.reset();
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
        let ended = self.was_recording && live.is_none();
        if self.track_recording(live.is_some(), session) {
            self.reset(cx);
        }
        if ended {
            self.final_points(cx);
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
        changed |= self.poll_points(cx);
        changed |= self.poll_answers();
        self.poll_memory();
        changed |= self.release_points();
        if let Some(l) = live {
            self.maybe_request_points(l, cx);
            self.maybe_suggest(l, cx);
        }
        changed |= self.track_record_controls(live);
        let visible = self.visible_points();
        let visible = (visible.len() + self.answers_shown(), visible.iter().filter(|&&i| self.is_compact(i)).count());
        changed |= visible != self.visible_seen;
        self.visible_seen = visible;
        // The stack's size changed: her window's input region follows.
        if self.desktop.is_some() && self.stack_bounds.get() != self.stack_applied {
            changed = true;
        }

        let pose = self.wanted_pose(live);
        // With reduced motion she holds each pose a while (errors show at once).
        let hold = self.reduce_motion && pose != Pose::Error && self.pose_since.elapsed() < Duration::from_secs(2);
        if pose != self.pose && !hold {
            self.pose = pose;
            self.frame = 0;
            self.pose_since = Instant::now();
            changed = true;
        }
        if let Some((_, since)) = self.pop {
            // (Its frames follow the clock; the last render clears it.)
            changed = true;
            if self.pop_len().is_none_or(|len| since.elapsed() > len + Duration::from_millis(200)) {
                self.pop = None;
            }
        }
        let busy = self.chat_rx.is_some() || self.points_rx.is_some() || self.answers.busy() || self.pop.is_some();
        // On the desktop she's always in view, not only in an active window.
        let animate = !self.reduce_motion && !self.hidden && (self.desktop_mode || window.is_window_active()) && self.demo_pose.is_none();
        let count = character::frame_count(self.character, self.pose);
        if animate && count > 1 {
            self.frame = (self.frame + 1) % count;
            changed = true;
        }
        if changed {
            cx.notify();
        }
        Some(if moving {
            Duration::from_millis(50)
        } else if animate {
            Duration::from_secs_f32(1. / character::fps(self.character).clamp(1, MAX_FPS) as f32)
        } else if self.pop.is_some() {
            Duration::from_millis(1000 / character::fps(self.character).clamp(1, MAX_FPS) as u64)
        } else if busy || live.is_some() || self.ctl.starting.is_some() {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(400)
        })
    }

    /// Stores every point of the recording with the meeting just saved
    /// (dismissed and cleared ones too; pinned ones marked), then clears
    /// them. A request still running saves its points there when it returns.
    pub fn save_points(&mut self, meeting_id: i64, cx: &mut Context<Self>) {
        let points: Vec<(Point, bool)> = self.points.iter().map(|p| (p.point.clone(), p.pinned)).collect();
        // Who took them, for the meeting's Notes tab and exports.
        let had_chat = self.turns.iter().any(|t| t.done);
        if !points.is_empty() || had_chat {
            let character = self.character.id();
            if let Err(e) = mostalgia_core::store::Store::open(&mostalgia_core::paths::database()).and_then(|s| s.set_companion(meeting_id, character)) {
                eprintln!("[mori] saving the companion with meeting {meeting_id}: {e:#}");
            }
        }
        self.store_points(meeting_id, points.iter().map(|(p, pinned)| (p, *pinned)));
        // The focus her notes were taken with goes with them (the meeting's
        // "Regenerate notes" starts from it); the next recording starts
        // from the default again.
        let focus = self.notes_focus_in(cx);
        if self.notes_mode != NotesMode::Off && !focus.is_empty() {
            let result = mostalgia_core::store::Store::open(&mostalgia_core::paths::database()).and_then(|s| s.set_notes_focus(meeting_id, Some(&focus)));
            if let Err(e) = result {
                eprintln!("[mori] saving the notes focus with meeting {meeting_id}: {e:#}");
            }
        }
        self.focus_clear = true;
        self.style_override = None;
        self.focus_editing = false;
        if self.points_rx.is_some() {
            self.late_save = Some(meeting_id);
        }
        self.points.clear();
        self.expanded = false;
        self.points_unseen = 0;
        // (The app saves them before it opens the meeting; on the desktop it
        // waits to hear it to reload the meeting.)
        if self.desktop_mode {
            cx.emit(MoriEvent::NotesSaved { meeting_id });
        }
        cx.notify();
    }

    /// Adds points to a meeting's notes; a failure shows on her. Returns
    /// whether it worked.
    fn store_points<'a>(&mut self, meeting_id: i64, points: impl Iterator<Item = (&'a Point, bool)>) -> bool {
        let points: Vec<(&Point, bool)> = points.collect();
        if points.is_empty() {
            return true;
        }
        let result = mostalgia_core::store::Store::open(&mostalgia_core::paths::database()).and_then(|store| {
            for (p, pinned) in &points {
                store.add_note_pinned(meeting_id, p.kind.as_str(), &p.text, p.at_secs, *pinned)?;
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                eprintln!("[mori] saved {} points with meeting {meeting_id}", points.len());
                true
            }
            Err(e) => {
                eprintln!("[mori] saving points with meeting {meeting_id}: {e:#}");
                self.error = Some(MoriError {
                    message: mostalgia_core::i18n::trf("Die Notizen von {} konnten nicht gespeichert werden: {}", &[&self.name(), &format!("{e:#}")]),
                    points: false,
                });
                false
            }
        }
    }

    /// A recording was saved as `meeting_id`: her points (and the chat) go
    /// with it.
    pub fn recording_saved(&mut self, meeting_id: i64, cx: &mut Context<Self>) {
        self.invalidate_notes();
        self.save_points(meeting_id, cx);
        self.save_session_chat(meeting_id, cx);
    }

    /// Back to normal after the screenshot demos.
    #[doc(hidden)]
    pub fn demo_reset(&mut self, cx: &mut Context<Self>) {
        self.demo_pose = None;
        self.open = false;
        self.turns.clear();
        self.points.clear();
        self.expanded = false;
        self.points_follow = true;
        self.points_unseen = 0;
        self.scope_choice = None;
        self.hidden = false;
        self.pop = None;
        self.ctl = Default::default();
        cx.notify();
    }

    /// Shows this companion instead of the configured one (screenshots).
    #[doc(hidden)]
    pub fn demo_character(&mut self, character: Character, cx: &mut Context<Self>) {
        self.character = character;
        self.frames.clear();
        self.lang_seen = None;
        cx.notify();
    }

    /// Holds the "a note arrived" animation at this frame (screenshots).
    #[doc(hidden)]
    pub fn demo_pop(&mut self, kind: Option<PointKind>, frame: usize, cx: &mut Context<Self>) {
        self.pop = kind.and_then(|k| {
            let at = Duration::from_secs_f32(frame as f32 / character::fps(self.character).max(1) as f32);
            Instant::now().checked_sub(at).map(|t| (k, t))
        });
        cx.notify();
    }

    /// Puts her at this spot (fractions of the free room, `None` = the
    /// default top right; screenshots).
    #[doc(hidden)]
    pub fn demo_pos(&mut self, pos: Option<(f32, f32)>, cx: &mut Context<Self>) {
        self.pos = pos;
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

impl Render for Companion {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.enabled {
            return div().into_any_element();
        }
        if self.lang_seen != Some(i18n::lang()) {
            self.lang_seen = Some(i18n::lang());
            let ask = trf("Frag {} …", &[&self.name()]);
            self.input.update(cx, |s, cx| s.set_placeholder(ask, window, cx));
            self.focus_placeholder.clear();
        }
        let placeholder = if self.default_focus.is_empty() {
            tr("z. B. Aufgaben für mich, Preise, Fristen").to_string()
        } else {
            trf("Standard: {}", &[&self.default_focus])
        };
        if placeholder != self.focus_placeholder {
            self.focus_input.update(cx, |s, cx| s.set_placeholder(placeholder.clone(), window, cx));
            self.focus_placeholder = placeholder;
        }
        if std::mem::take(&mut self.focus_clear) {
            self.focus_input.update(cx, |s, cx| s.set_value("", window, cx));
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
        let dragging = self.drag.as_ref().is_some_and(|d| d.moved);
        let character = if self.hidden {
            self.render_bar(o, dragging, cx)
        } else {
            let image = self.frame_image((h * window.scale_factor()).round() as u32);
            div()
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
            .on_mouse_down(MouseButton::Right, cx.listener(|this, ev: &MouseDownEvent, _, cx| this.open_menu(ev, cx)))
            // (Her desktop window is too small for the tooltip.)
            .when(!self.desktop_mode, |el| {
                let tip: SharedString = trf("{} · klicken zum Fragen, ziehen zum Verschieben, Rechtsklick für das Menü", &[&self.name()]).into();
                el.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
            })
            .child(img(image).size_full())
            .into_any_element()
        };

        let bubble = self.render_bubble(o, (w, h), cx);
        let stack = self.render_points(o, (w, h), cx);
        let badge = if self.hidden { None } else { self.render_badge(o, (w, h), cx) };
        let pop = self.render_pop(o, (w, h), window.scale_factor());
        let menu = self.render_menu(o, cx);
        let theme = cx.theme();
        let name_tag = (self.pose == Pose::Error && !self.hidden).then(|| {
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
            .children(pop)
            .children(name_tag)
            .children(badge)
            .children(bubble)
            .children(menu)
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
