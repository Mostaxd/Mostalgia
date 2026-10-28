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
/// Room for the cards above or below her without a known work area.
pub(super) const CARDS_H: f32 = 300.;
/// The most room the cards' list takes above or below her (it scrolls).
pub(super) const CARDS_MAX_H: f32 = 560.;
/// The least room it gets, however close she is to the screen edge.
const CARDS_MIN_H: f32 = 140.;

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
    /// Her monitor's work area (root pixels), from `settle`.
    work: Option<Rect>,
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
        Self { x, scale, mori: (0, 0), right: true, bottom: false, work: None, applied: None, applied_input: Vec::new(), drag: None }
    }

    /// Room (logical px) for the cards' stack above her (`bottom`) or
    /// below her, up to her monitor's work area edge.
    pub fn cards_room(&self, (_, h): (f32, f32)) -> f32 {
        let Some(m) = self.work else { return CARDS_H };
        let space = if self.bottom { self.mori.1 - m.y } else { m.y + m.h - (self.mori.1 + self.px(h)) };
        (space as f32 / self.scale - super::points::STACK_GAP - super::points::STACK_EDGE).clamp(CARDS_MIN_H, CARDS_MAX_H)
    }

    /// The window size and her top-left in it (logical px), small or grown.
    /// Grown, it has room beside her for the bubble and above / below her
    /// for the cards (as much as her monitor has, up to `CARDS_MAX_H`).
    pub fn layout(&self, (w, h): (f32, f32), grown: bool) -> ((f32, f32), (f32, f32)) {
        if !grown {
            return ((w + 2. * PAD, h + 2. * PAD), (PAD, PAD));
        }
        let cards = PAD + h + super::points::STACK_GAP + self.cards_room((w, h)) + super::points::STACK_EDGE;
        let size = (w + 2. * PAD + SIDE_W, cards.max(GROWN_MIN_H));
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
        self.work = Some(self.x.work_area().and_then(|w| w.intersect(&m)).unwrap_or(m));
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
            window.set_window_title(Config::load().companion_character().name());
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
    demo_next: usize,
    demo_expanded: bool,
    demo_scrolled: bool,
    demo_menu: bool,
    replay: Option<Replay>,
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
            MoriEvent::BackToApp => send(&FromMori::BackToApp),
            MoriEvent::NotesSaved { meeting_id } => send(&FromMori::NotesSaved { meeting_id: *meeting_id }),
            MoriEvent::StartRecording { mic, system } => send(&FromMori::StartRecording { mic: *mic, system: *system }),
            MoriEvent::SetPaused { paused } => send(&FromMori::PauseRecording { paused: *paused }),
            MoriEvent::StopRecording => send(&FromMori::StopRecording),
            MoriEvent::SetSource { source, on } => send(&FromMori::SetSource { source: source.as_str().into(), on: *on }),
            MoriEvent::OpenMoriSettings => send(&FromMori::OpenMoriSettings),
            MoriEvent::PopOut => {}
        });
        let moment = cx.subscribe(&companion, |_, _, event: &super::ShowLiveMoment, _| send(&FromMori::ShowLiveMoment { secs: event.secs }));
        cx.observe_window_appearance(window, |_, window, cx| ui::apply_theme(window, cx)).detach();
        let this = Self { companion, feed, rx, demo: demo.then(Instant::now), demo_next: 0, demo_expanded: false, demo_scrolled: false, demo_menu: false, replay: demo.then(Replay::load).flatten(), _subscriptions: vec![sub, moment] };
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
                    // (She says `NotesSaved` herself, also for points that
                    // arrive after this.)
                    self.companion.update(cx, |c, cx| c.recording_saved(meeting_id, cx));
                }
                ToMori::OpenMeeting { meeting } => self.companion.update(cx, |c, cx| c.set_open_meeting(meeting, cx)),
                ToMori::StartFailed { message } => self.companion.update(cx, |c, cx| c.recording_start_failed(message, cx)),
                ToMori::ReloadConfig => {
                    let config = Config::load();
                    mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&config.ui_language));
                    ui::apply_theme(window, cx);
                    self.companion.update(cx, |c, cx| c.reload_config(cx));
                    window.set_window_title(config.companion_character().name());
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

    /// `--demo`: a fake recording with speech now and then, without the
    /// app or a network: a new point card every few seconds. With
    /// `MORI_DEMO_TRANSCRIPT=<file>` (lines "Speaker: text") it replays that
    /// transcript instead, a line every 2.5 s (or at its own time, for lines
    /// "[mm:ss-mm:ss] Speaker: text": finalized 1.2 s after it ends), through
    /// the same feed the app uses (transcript every half second), and the
    /// points come from the real requests to the configured connection.
    fn demo_step(&mut self, start: Instant, cx: &mut Context<Self>) {
        use super::ipc::{WireLine, WireLive};
        let t = start.elapsed().as_secs_f64();
        if let Some(replay) = &mut self.replay {
            const EVERY: f64 = 2.5;
            const FINALIZE: f64 = 1.2;
            let n = replay.lines.len();
            let due = replay.lines.iter().take_while(|l| l.end + FINALIZE <= t).count();
            let speaking = replay.lines.iter().any(|l| l.start <= t && t < l.end) || !replay.timed && due < n;
            let since = replay.lines.iter().find(|l| l.start <= t && t < l.end).map_or(t % EVERY, |l| t - l.start);
            let live = WireLive {
                session: 1,
                paused: false,
                speech_ago_ms: speaking.then_some(0),
                speech_since_ago_ms: speaking.then_some((since * 1000.) as u64),
                speech_secs: replay.lines[..due].iter().map(|l| l.end - l.start).sum(),
                last_line_end: replay.lines[..due].last().map_or(0., |l| l.end),
                lines: due,
                elapsed_secs: t,
                sources: None,
            };
            if t > replay.lines.last().map_or(0., |l| l.end) + 8. * EVERY && !replay.ended {
                // The recording ends a while after the last line.
                replay.ended = true;
                eprintln!("[mori] demo: the replayed recording ended");
                self.feed.update(cx, |f, _| f.set_live(None));
                return;
            }
            if replay.ended {
                return;
            }
            self.feed.update(cx, |f, _| f.set_live(Some(live)));
            // The transcript lags like the app's (every 500 ms).
            if replay.sent < due && replay.sent_at.elapsed() >= Duration::from_millis(500) {
                replay.sent_at = Instant::now();
                let lines: Vec<WireLine> = (replay.sent..due)
                    .map(|i| {
                        let l = &replay.lines[i];
                        WireLine { at: l.start, speaker: l.speaker.clone(), text: l.text.clone() }
                    })
                    .collect();
                let from = replay.sent;
                replay.sent = due;
                self.feed.update(cx, |f, _| f.set_transcript(1, from, lines, Vec::new()));
            }
            return;
        }
        // MORI_DEMO_PAUSED=1: the recording is paused; MORI_DEMO_POINTS=0:
        // no cards.
        let paused = std::env::var_os("MORI_DEMO_PAUSED").is_some_and(|v| v == "1");
        let speaking = !paused && t % 7. < 4.;
        let since = (t % 7.) * 1000.;
        let live = WireLive {
            session: 1,
            paused,
            speech_ago_ms: speaking.then_some(0),
            speech_since_ago_ms: speaking.then_some(since as u64),
            speech_secs: 0.,
            last_line_end: t,
            lines: 0,
            elapsed_secs: t,
            // MORI_DEMO_MIC_OFF=1: the mic is switched off.
            sources: std::env::var_os("MORI_DEMO_MIC_OFF").is_some_and(|v| v == "1").then_some([false, true]),
        };
        self.feed.update(cx, |f, _| f.set_live(Some(live)));
        // A new card every 3 s from 4 s on.
        let due = if t < 4. || std::env::var_os("MORI_DEMO_POINTS").is_some_and(|v| v == "0") {
            0
        } else {
            (((t - 4.) / 3.) as usize + 1).min(DEMO_POINTS.len())
        };
        if self.demo_next < due {
            let batch: Vec<(mostalgia_core::companion::PointKind, &str, f64)> = DEMO_POINTS[self.demo_next..due].to_vec();
            let first = self.demo_next;
            self.demo_next = due;
            // The second one gets pinned.
            let pinned: Vec<usize> = (first..due).filter(|&i| i == 1).map(|i| i - first).collect();
            eprintln!("[mori] demo: point {} of {}", due, DEMO_POINTS.len());
            self.companion.update(cx, |c, cx| c.demo_points(&batch, &pinned, cx));
        }
        // MORI_DEMO_EXPAND_AT=<s>: the full list from then on;
        // MORI_DEMO_SCROLL_AT=<s>: scrolled away from the newest then.
        let at = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<f64>().ok());
        if let Some(e) = at("MORI_DEMO_EXPAND_AT")
            && t >= e
            && !self.demo_expanded
        {
            self.demo_expanded = true;
            eprintln!("[mori] demo: expanded");
            self.companion.update(cx, |c, cx| c.demo_expand(true, cx));
        }
        // MORI_DEMO_MENU_AT=<s>: her menu opens then (below the bar / by her).
        if let Some(e) = at("MORI_DEMO_MENU_AT")
            && t >= e
            && !self.demo_menu
        {
            self.demo_menu = true;
            eprintln!("[mori] demo: menu");
            self.companion.update(cx, |c, cx| c.demo_menu(Some((54., 44.)), cx));
        }
        if let Some(e) = at("MORI_DEMO_SCROLL_AT")
            && t >= e
            && !self.demo_scrolled
        {
            self.demo_scrolled = true;
            eprintln!("[mori] demo: scrolled away");
            self.companion.update(cx, |c, cx| c.demo_scroll_away(260., cx));
        }
    }
}

/// The canned points of `--demo`, one every few seconds.
const DEMO_POINTS: &[(mostalgia_core::companion::PointKind, &str, f64)] = {
    use mostalgia_core::companion::PointKind::*;
    &[
        (Decision, "Release moves to Friday.", 62.),
        (Action, "Anna sends the updated plan by Thursday.", 95.),
        (Question, "Who reviews the migration?", 130.),
        (Fact, "Churn went from 4.1% to 3.2% after the onboarding change in March, mostly in the small-business segment.", 151.),
        (Action, "Ben: book the room for the design review next Tuesday afternoon.", 178.),
        (Decision, "The beta stays invite-only until the load tests pass.", 203.),
        (Fact, "Budget: 48,000 EUR left for Q3.", 231.),
        (Question, "Do we need legal to sign off on the new data retention policy before the launch, or is the existing agreement enough?", 254.),
        (Action, "Chris updates the onboarding emails.", 280.),
        (Fact, "Support tickets are down 18% week over week.", 302.),
        (Decision, "We drop the Windows 7 installer.", 327.),
        (Action, "Dana: write up the incident report from Monday's outage, including the timeline and the follow-ups, and share it with the whole team by Wednesday.", 351.),
        (Question, "When does the new pricing take effect?", 377.),
        (Fact, "Two of five pilot customers renewed already.", 402.),
    ]
};

/// `MORI_DEMO_TRANSCRIPT`: the lines to replay and how far it got.
struct Replay {
    lines: Vec<ReplayLine>,
    /// The lines came with their own times.
    timed: bool,
    sent: usize,
    sent_at: Instant,
    ended: bool,
}

impl Replay {
    fn load() -> Option<Self> {
        let path = std::env::var_os("MORI_DEMO_TRANSCRIPT")?;
        let text = std::fs::read_to_string(&path).map_err(|e| eprintln!("[mori] demo transcript: {e}")).ok()?;
        let mut timed = false;
        let lines: Vec<ReplayLine> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, l)| {
                // Without times: a line every 2.5 s, each 2.4 s long.
                let (mut start, mut end) = (i as f64 * 2.5, i as f64 * 2.5 + 2.4);
                let mut l = l.trim();
                if let Some((stamp, rest)) = l.strip_prefix('[').and_then(|r| r.split_once("] "))
                    && let Some((a, b)) = stamp.split_once('-')
                    && let (Some(a), Some(b)) = (mmss(a), mmss(b))
                {
                    (start, end, l, timed) = (a, b, rest, true);
                }
                let (speaker, text) = match l.split_once(':') {
                    Some((who, said)) if who.len() < 24 => (who.trim().to_string(), said.trim().to_string()),
                    _ => ("Others".to_string(), l.to_string()),
                };
                ReplayLine { start, end, speaker, text }
            })
            .collect();
        eprintln!("[mori] demo: replaying {} transcript lines", lines.len());
        Some(Self { lines, timed, sent: 0, sent_at: Instant::now(), ended: false })
    }
}

struct ReplayLine {
    start: f64,
    end: f64,
    speaker: String,
    text: String,
}

/// "mm:ss.s" in seconds.
fn mmss(s: &str) -> Option<f64> {
    let (m, s) = s.split_once(':')?;
    Some(m.trim().parse::<f64>().ok()? * 60. + s.trim().parse::<f64>().ok()?)
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
