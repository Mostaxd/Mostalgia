//! Live captions: `mostalgia-app --captions` shows the live transcript in a
//! small always-on-top window during a recording (X11 through XWayland,
//! like Mori on the desktop), to follow and flag lines over a call without
//! switching to Mostalgia. It never takes keyboard focus. The app starts
//! it when a recording starts (if it's switched on) and ends it after.
//!
//! The last lines with who spoke, following the newest; scrolling up stops
//! that ("Live" brings it back). A line's star flags it: the app adds a
//! bookmark there. Mic and pause switches, "open Mostalgia", opacity; it
//! moves by its top bar and resizes at the corner; place and size are
//! remembered. It moves and resizes itself, following the pointer, like
//! Mori on the desktop: window managers ignore a move request from a
//! window that never takes focus (and Windows has none).
//!
//! Two looks (Settings): a box with the last lines, or subtitles: only the
//! newest lines, centred over a transparent window, the bar showing on
//! hover.

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::config::Config;
use mostalgia_core::i18n::{tr, trf};

use super::desktop::{send, spawn_stdin_reader, window_id};
use super::ipc::{FromMori, ToMori, WireLine, WireLive};
use super::x11::{Rect, X11};
use crate::{AppAssets, load_fonts, ui};

/// Default size (logical px): about four lines.
pub const DEFAULT_SIZE: (f32, f32) = (560., 140.);
/// Opacity steps of its button.
const OPACITIES: [f32; 4] = [1.0, 0.92, 0.8, 0.65];
/// Smallest size (logical px).
const MIN_SIZE: (f32, f32) = (300., 110.);
/// Lines shown as subtitles (the newest, counting the one being spoken).
const SUBTITLE_LINES: usize = 2;

/// A move or resize by hand: pointer and window (root pixels) when it began.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Move { pointer: (i32, i32), origin: (i32, i32) },
    Resize { pointer: (i32, i32), origin: (i32, i32), size: (i32, i32) },
}

/// Where the window goes for the pointer at `now` during `drag`: its
/// rectangle, never smaller than `min` (root pixels).
fn drag_rect(drag: Drag, now: (i32, i32), min: (i32, i32)) -> Rect {
    match drag {
        Drag::Move { pointer, origin } => {
            Rect { x: origin.0 + now.0 - pointer.0, y: origin.1 + now.1 - pointer.1, w: 0, h: 0 }
        }
        Drag::Resize { pointer, origin, size } => Rect {
            x: origin.0,
            y: origin.1,
            w: (size.0 + now.0 - pointer.0).max(min.0),
            h: (size.1 + now.1 - pointer.1).max(min.1),
        },
    }
}

/// How it looks (from the config).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    subtitles: bool,
    text: f32,
    speakers: bool,
    translation_only: bool,
}

impl Look {
    fn of(config: &Config) -> Self {
        Self {
            subtitles: config.captions_subtitles(),
            text: config.captions_text_size(),
            speakers: config.live_captions_speakers,
            translation_only: config.live_captions_translation_only,
        }
    }
}

/// Where it goes by default: bottom middle of the monitor, above the
/// panel, away from Mori (who starts top right).
pub fn default_origin(work: Rect, size: (i32, i32)) -> (i32, i32) {
    (work.x + (work.w - size.0) / 2, work.y + work.h - size.1 - work.h / 12)
}

/// The next opacity step after `now`.
pub fn next_opacity(now: f32) -> f32 {
    let i = OPACITIES.iter().position(|o| (o - now).abs() < 0.01).map_or(0, |i| (i + 1) % OPACITIES.len());
    OPACITIES[i]
}

/// Entry point of `mostalgia-app --captions [--demo]`.
pub fn run(demo: bool) {
    #[cfg(target_os = "linux")]
    let env = GraphicalEnvironment { wayland_display: None, ..GraphicalEnvironment::detect() };
    #[cfg(not(target_os = "linux"))]
    let env = GraphicalEnvironment::detect();
    let rx = spawn_stdin_reader(demo);
    gpui_kit::application().with_assets(AppAssets).with_windowing(WindowingRequest::Windowed(env)).run(move |cx: &mut App| {
        gpui_kit::init(cx);
        let config = Config::load();
        mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&config.ui_language));
        load_fonts(cx);
        let (w, h) = config.live_captions_size.unwrap_or(DEFAULT_SIZE);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(w), px(h)) })),
            titlebar: None,
            focus: false,
            show: true,
            // macOS: a non-activating panel, so clicking it leaves the call
            // app in front with the keyboard.
            kind: if cfg!(target_os = "macos") { WindowKind::PopUp } else { WindowKind::Normal },
            is_movable: true,
            is_resizable: true,
            is_minimizable: false,
            window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: Some(WindowDecorations::Server),
            app_id: Some("io.github.mostaxd.Mostalgia.Captions".into()),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            ui::apply_theme(window, cx);
            window.set_window_title(&tr("Live-Untertitel"));
            let view = cx.new(|cx| Captions::new(rx, demo, window, cx));
            cx.new(|cx| gpui_kit::component::Root::new(view, window, cx).bg(transparent_black()))
        });
        if let Err(e) = opened {
            eprintln!("[captions] opening the window: {e:#}");
            cx.quit();
        }
    });
}

pub struct Captions {
    rx: Receiver<ToMori>,
    x: Option<X11>,
    live: Option<WireLive>,
    session: Option<u64>,
    lines: Vec<WireLine>,
    interim: Vec<WireLine>,
    /// Live translations (line start, text), when translation is on.
    translations: Vec<(f64, String)>,
    /// Times of the lines flagged here.
    flagged: Vec<f64>,
    stalled: Option<u64>,
    scroll: ScrollHandle,
    /// Following the newest line (off once scrolled up).
    follow: bool,
    opacity: f32,
    /// Last saved place and size.
    saved: Option<((i32, i32), (f32, f32))>,
    /// Being moved or resized by hand.
    drag: Option<Drag>,
    /// The app saw the pointer move during the drag (from then on a button
    /// found up ends it, should the release itself be missed).
    drag_moved: bool,
    look: Look,
    checked_at: Instant,
    demo: Option<Instant>,
}

impl Captions {
    fn new(rx: Receiver<ToMori>, demo: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = Config::load();
        let x = window_id(window).and_then(|id| X11::connect(id).ok());
        match &x {
            Some(x) => {
                if let Err(e) = x.setup_window().and_then(|_| x.no_input_focus()) {
                    eprintln!("[captions] window setup: {e:#}");
                }
                #[cfg(target_os = "macos")]
                x.allow_resize();
                let scale = window.scale_factor();
                let (w, h) = config.live_captions_size.unwrap_or(DEFAULT_SIZE);
                let size = ((w * scale) as i32, (h * scale) as i32);
                let monitors = x.monitors();
                // The saved place, unless its monitor is gone (unplugged).
                let on_screen = |p: &(i32, i32)| monitors.iter().any(|m| m.rect.contains(p.0 + size.0 / 2, p.1 + size.1 / 2));
                let origin = config.live_captions_pos.filter(|p| monitors.is_empty() || on_screen(p)).unwrap_or_else(|| {
                    let m = monitors.iter().find(|m| m.primary).or(monitors.first()).map(|m| m.rect).unwrap_or(Rect { x: 0, y: 0, w: 1920, h: 1080 });
                    default_origin(x.work_area_on(&m), size)
                });
                let _ = x.move_resize(Rect { x: origin.0, y: origin.1, w: size.0, h: size.1 });
            }
            None => eprintln!("[captions] not an X11 window; it can't stay on top"),
        }
        cx.observe_window_appearance(window, |_, window, cx| ui::apply_theme(window, cx)).detach();
        cx.spawn_in(window, async move |this, cx| {
            let mut dragging = false;
            loop {
                // Following the pointer smoothly while it's dragged.
                cx.background_executor().timer(Duration::from_millis(if dragging { 8 } else { 60 })).await;
                match this.update_in(cx, |this, window, cx| {
                    this.poll(window, cx);
                    this.drag_step(false, window, cx);
                    this.drag.is_some()
                }) {
                    Ok(d) => dragging = d,
                    Err(_) => break,
                }
            }
        })
        .detach();
        send(&FromMori::Ready);
        Self {
            rx,
            x,
            live: None,
            session: None,
            lines: Vec::new(),
            interim: Vec::new(),
            translations: Vec::new(),
            flagged: Vec::new(),
            stalled: None,
            scroll: ScrollHandle::new(),
            follow: true,
            opacity: config.live_captions_opacity.clamp(0.4, 1.0),
            saved: None,
            drag: None,
            drag_moved: false,
            look: Look::of(&config),
            checked_at: Instant::now(),
            demo: demo.then(Instant::now),
        }
    }

    /// Screenshots: a recording a while in, one line flagged, stalled
    /// for `stalled` seconds if given.
    #[doc(hidden)]
    pub fn demo(stalled: Option<u64>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (_tx, rx) = crossbeam_channel::unbounded();
        let mut this = Self::new(rx, true, window, cx);
        this.demo = Some(Instant::now() - Duration::from_secs(20));
        this.demo_step(Instant::now() - Duration::from_secs(20));
        this.flagged.push(this.lines[1].at);
        this.interim = vec![WireLine { at: 760., speaker: "Felix".into(), text: "And the database migration …".into() }];
        this.stalled = stalled;
        this
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            match msg {
                ToMori::Live { live } => {
                    if live.as_ref().map(|l| l.session) != self.session && live.is_some() {
                        self.session = live.as_ref().map(|l| l.session);
                        self.lines.clear();
                        self.flagged.clear();
                        self.translations.clear();
                    }
                    self.live = live;
                }
                ToMori::Transcript { session, from, lines, interim } => {
                    if Some(session) == self.session && from <= self.lines.len() {
                        self.lines.truncate(from);
                        self.lines.extend(lines);
                        self.interim = interim;
                    }
                }
                ToMori::Stalled { secs } => self.stalled = secs,
                ToMori::Translations { session, from, items } => {
                    if Some(session) == self.session && from <= self.translations.len() {
                        self.translations.truncate(from);
                        self.translations.extend(items);
                    }
                }
                ToMori::ReloadConfig => {
                    let config = Config::load();
                    mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&config.ui_language));
                    ui::apply_theme(window, cx);
                    self.opacity = config.live_captions_opacity.clamp(0.4, 1.0);
                    self.look = Look::of(&config);
                }
                ToMori::Quit => {
                    self.save_place(window);
                    cx.quit();
                    return;
                }
                _ => {}
            }
        }
        if let Some(start) = self.demo {
            changed |= self.demo_step(start);
        }
        // Following: stay at the newest line.
        if changed && self.follow {
            self.scroll.scroll_to_bottom();
        }
        // Scrolled up by hand: stop following until "Live".
        let at_bottom = {
            let max = self.scroll.max_offset().y;
            max <= px(1.) || -self.scroll.offset().y >= max - px(24.)
        };
        if !changed && self.follow && !at_bottom && self.scroll.max_offset().y > px(1.) {
            self.follow = false;
            cx.notify();
        }
        if self.drag.is_none() && self.checked_at.elapsed() > Duration::from_secs(2) {
            self.checked_at = Instant::now();
            self.save_place(window);
        }
        if changed {
            cx.notify();
        }
    }

    /// A press on the bar (move) or the corner (resize): from now on the
    /// window follows the pointer until the button is up. Without a handle
    /// on the window, the window manager is asked to do it.
    fn start_drag(&mut self, resize: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(x) = &self.x else {
            if resize {
                window.start_window_resize(ResizeEdge::BottomRight);
            } else {
                window.start_window_move();
            }
            return;
        };
        let (pointer, position) = (x.pointer(), x.position());
        eprintln!("[captions] drag start (resize {resize}): pointer {pointer:?}, window {position:?}");
        let (Ok((px_, py_, _)), Ok(origin)) = (pointer, position) else { return };
        let scale = window.scale_factor();
        let b = window.bounds().size;
        let size = ((f32::from(b.width) * scale).round() as i32, (f32::from(b.height) * scale).round() as i32);
        self.drag_moved = false;
        self.drag = Some(if resize {
            Drag::Resize { pointer: (px_, py_), origin, size }
        } else {
            Drag::Move { pointer: (px_, py_), origin }
        });
        cx.notify();
    }

    /// Moves or resizes to the pointer while dragged (on each mouse move,
    /// and between them); ends at the release, or once the pointer moved
    /// and the button is found up.
    fn drag_step(&mut self, moved: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(drag), Some(x)) = (self.drag, &self.x) else { return };
        self.drag_moved |= moved;
        let Ok((px_, py_, down)) = x.pointer() else {
            self.drag = None;
            return;
        };
        let scale = window.scale_factor();
        let min = ((MIN_SIZE.0 * scale) as i32, (MIN_SIZE.1 * scale) as i32);
        let mut r = drag_rect(drag, (px_, py_), min);
        if let Drag::Move { .. } = drag {
            let b = window.bounds().size;
            r.w = (f32::from(b.width) * scale).round() as i32;
            r.h = (f32::from(b.height) * scale).round() as i32;
        }
        if let Err(e) = x.move_resize(r) {
            eprintln!("[captions] moving: {e:#}");
        }
        if !down && self.drag_moved {
            self.end_drag(window, cx);
        }
    }

    /// The button is up: the drag is over, and the new place saved.
    fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.take().is_none() {
            return;
        }
        if let Some(x) = &self.x {
            eprintln!("[captions] drag end: pointer {:?}, window {:?}", x.pointer(), x.position());
        }
        self.save_place(window);
        cx.notify();
    }

    /// Remembers where it is and how big (when that changed).
    fn save_place(&mut self, window: &Window) {
        let Some(x) = &self.x else { return };
        let Ok(pos) = x.position() else { return };
        let size = window.bounds().size;
        let place = (pos, (f32::from(size.width), f32::from(size.height)));
        if self.saved.is_none() {
            // The first look is where it was put; nothing to save.
            self.saved = Some(place);
            return;
        }
        if self.saved != Some(place) {
            self.saved = Some(place);
            let mut config = Config::load();
            config.live_captions_pos = Some(place.0);
            config.live_captions_size = Some(place.1);
            if let Err(e) = config.save() {
                eprintln!("[captions] saving its place: {e:#}");
            }
        }
    }

    /// `--demo`: lines arriving every 2.5 s, without the app.
    fn demo_step(&mut self, start: Instant) -> bool {
        const DEMO: &[(&str, &str)] = &[
            ("Felix", "Okay, let's look at the release plan for next week."),
            ("Nora", "The frontend build is green, I only need the backend container."),
            ("Me", "I'll push the API changes tonight."),
            ("Clara", "Then we keep Tuesday as the review date."),
            ("Felix", "Good. Who takes the pipeline?"),
            ("Nora", "I can do the frontend part, Felix the backend one."),
        ];
        let t = start.elapsed().as_secs_f64();
        let due = ((t / 2.5) as usize + 1).min(DEMO.len());
        self.live = Some(WireLive { session: 1, paused: false, speech_ago_ms: Some(0), speech_since_ago_ms: Some(0), speech_secs: t, last_line_end: t, lines: due, elapsed_secs: 754. + t, sources: Some([true, true]) });
        self.session = Some(1);
        if self.lines.len() >= due {
            return false;
        }
        self.lines = DEMO[..due].iter().enumerate().map(|(i, (who, text))| WireLine { at: 700. + i as f64 * 9., speaker: who.to_string(), text: text.to_string() }).collect();
        if std::env::var_os("CAPTIONS_DEMO_FLAG").is_some() && due >= 3 && self.flagged.is_empty() {
            self.flagged.push(self.lines[1].at);
        }
        true
    }

    fn flag(&mut self, at: f64, cx: &mut Context<Self>) {
        if let Some(i) = self.flagged.iter().position(|f| (*f - at).abs() < 0.01) {
            // Already flagged: the bookmark stays (removing it is in the app).
            let _ = i;
            return;
        }
        self.flagged.push(at);
        send(&FromMori::Flag { secs: at });
        cx.notify();
    }

    fn render_line(&self, ix: usize, l: &WireLine, interim: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let at = l.at;
        let flagged = self.flagged.iter().any(|f| (*f - at).abs() < 0.01);
        let translation = if interim { None } else { self.translations.iter().rev().find(|(t, _)| (*t - at).abs() < 0.01).map(|(_, t)| t.clone()) };
        let look = self.look;
        // Only the translation, once it's there.
        let (text, translation) = match translation {
            Some(t) if look.translation_only => (t, None),
            t => (l.text.clone(), t),
        };
        let group = SharedString::from(format!("caption-line-{ix}"));
        h_flex()
            .id(("caption", ix))
            .group(group.clone())
            .w_full()
            .gap_2()
            .items_start()
            .px_1()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.muted.opacity(0.5)))
            .when(look.speakers, |el| {
                el.child(
                    div()
                        .flex_none()
                        .min_w(px(56.))
                        .text_size(ui::transcript_rems(0.8 * look.text))
                        .font_semibold()
                        .text_color(ui::speaker_color(&l.speaker, cx))
                        .child(ui::speaker_display(&l.speaker)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui::transcript_rems(0.95 * look.text))
                    .when(interim, |el| el.text_color(theme.muted_foreground).italic())
                    .child(ui::bidi_text(text, true))
                    .when_some(translation, |el, t| {
                        el.child(ui::bidi_text(t, true).italic().text_color(theme.muted_foreground).text_size(ui::transcript_rems(0.9 * look.text)))
                    }),
            )
            .when(!interim, |el| {
                el.child(
                    div()
                        .flex_none()
                        .when(!flagged, |d| d.invisible().group_hover(group.clone(), |s| s.visible()))
                        .child(
                            Button::new(("flag", ix))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::Bookmark).text_color(if flagged { theme.warning } else { theme.muted_foreground }))
                                .tooltip(if flagged { tr("Markiert") } else { tr("Diese Stelle markieren") })
                                .on_click(cx.listener(move |this, _, _, cx| this.flag(at, cx))),
                        ),
                )
            })
            .into_any_element()
    }
}

impl Render for Captions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.look.subtitles {
            return self.render_subtitles(window, cx).into_any_element();
        }
        let theme = cx.theme().clone();
        let lines = self.lines.clone();
        let interim = self.interim.clone();
        let n = lines.len();
        let header = self.render_bar(cx);
        v_flex()
            .id("captions")
            .size_full()
            .relative()
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, cx| this.drag_step(true, window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, window, cx| this.end_drag(window, cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, window, cx| this.end_drag(window, cx)))
            .font(ui::ui_font(cx))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background.opacity(self.opacity))
            .text_color(theme.foreground)
            .overflow_hidden()
            .child(header)
            .when_some(self.stalled, |el, secs| el.child(stalled_banner(secs, cx)))
            .child(
                v_flex()
                    .id("captions-lines")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_2()
                    .py_1()
                    .gap_1()
                    .when(lines.is_empty() && interim.is_empty(), |el| {
                        el.child(div().py_4().text_center().text_sm().text_color(theme.muted_foreground).child(tr("Warte auf die ersten Worte …")))
                    })
                    .children(lines.iter().enumerate().map(|(i, l)| self.render_line(i, l, false, cx)))
                    .children(interim.iter().enumerate().map(|(i, l)| self.render_line(n + i, l, true, cx))),
            )
            .child(self.resize_grip(cx))
            .into_any_element()
    }
}

/// "Live transcription stuck for N s".
fn stalled_banner(secs: u64, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .flex_none()
        .px_2()
        .py_1()
        .gap_1p5()
        .bg(theme.warning.opacity(0.15))
        .text_xs()
        .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning))
        .child(trf("Die Live-Transkription hängt seit {} s", &[&secs]))
}

impl Captions {
    /// The top bar: moves the window; timer, mic, pause, opacity, app.
    fn render_bar(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let live = self.live.clone();
        let paused = live.as_ref().is_some_and(|l| l.paused);
        let mic_on = live.as_ref().and_then(|l| l.sources).is_none_or(|s| s[0]);
        let elapsed = live.as_ref().map_or(0., |l| l.elapsed_secs);
        h_flex()
            .id("captions-bar")
            .flex_none()
            .h(px(30.))
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .cursor_move()
            // Moving it by its bar (buttons keep their clicks).
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.start_drag(false, window, cx)))
            .child(super::controls::sparkles(theme.primary, !Config::load().reduce_motion, paused))
            .child(div().text_xs().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(ui::hms(elapsed)))
            .child(div().flex_1())
            .when(!self.follow, |el| {
                el.child(Button::new("captions-live").xsmall().primary().icon(IconName::ArrowDown).label(tr("Live")).on_click(cx.listener(|this, _, _, cx| {
                    this.follow = true;
                    this.scroll.scroll_to_bottom();
                    cx.notify();
                })))
            })
            .child(
                Button::new("captions-mic")
                    .xsmall()
                    .ghost()
                    .icon(if mic_on { Icon::new(IconName::Mic) } else { Icon::new(IconName::MicOff).text_color(theme.muted_foreground) })
                    .tooltip(if mic_on { tr("Mikrofon ausschalten") } else { tr("Mikrofon einschalten") })
                    .on_click(move |_, _, _| send(&FromMori::SetSource { source: "mic".into(), on: !mic_on })),
            )
            .child(
                Button::new("captions-pause")
                    .xsmall()
                    .ghost()
                    .icon(if paused { IconName::Play } else { IconName::Pause })
                    .tooltip(if paused { tr("Fortsetzen") } else { tr("Pause") })
                    .on_click(move |_, _, _| send(&FromMori::PauseRecording { paused: !paused })),
            )
            .child(
                Button::new("captions-opacity")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Eye)
                    .tooltip(trf("Deckkraft {} %", &[&((self.opacity * 100.).round() as u32)]))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.opacity = next_opacity(this.opacity);
                        let mut config = Config::load();
                        config.live_captions_opacity = this.opacity;
                        if let Err(e) = config.save() {
                            eprintln!("[captions] {e:#}");
                        }
                        cx.notify();
                    })),
            )
            .child(Button::new("captions-app").xsmall().ghost().icon(IconName::AppWindow).tooltip(tr("Mostalgia öffnen")).on_click(|_, _, _| send(&FromMori::ShowApp)))
    }

    /// The corner grip: resizes the window.
    fn resize_grip(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("captions-resize")
            .absolute()
            .right_0()
            .bottom_0()
            .size(px(14.))
            .cursor(CursorStyle::ResizeUpLeftDownRight)
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.start_drag(true, window, cx)))
    }

    /// Subtitles: the newest lines centred at the bottom of a transparent
    /// window, each on a dark band; the bar and the grip show on hover.
    fn render_subtitles(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let look = self.look;
        let mut shown: Vec<(WireLine, bool)> = self.lines.iter().map(|l| (l.clone(), false)).chain(self.interim.iter().map(|l| (l.clone(), true))).collect();
        let skip = shown.len().saturating_sub(SUBTITLE_LINES);
        shown.drain(..skip);
        // Bar and grip stay while dragged or hovered.
        let pinned = self.drag.is_some();
        let bar = self.render_bar(cx).rounded(theme.radius).border_1().border_color(theme.border).bg(theme.background.opacity(0.92));
        let grip = self.resize_grip(cx);
        let band = gpui_kit::hsla(0., 0., 0.05, (self.opacity * 0.8).clamp(0.35, 0.85));
        let lines = shown.into_iter().map(|(l, interim)| {
            let translation = if interim { None } else { self.translations.iter().rev().find(|(t, _)| (*t - l.at).abs() < 0.01).map(|(_, t)| t.clone()) };
            let (text, translation) = match translation {
                Some(t) if look.translation_only => (t, None),
                t => (l.text.clone(), t),
            };
            v_flex()
                .max_w_full()
                .items_center()
                .px_3()
                .py_1()
                .rounded(theme.radius)
                .bg(band)
                .text_center()
                .text_color(gpui_kit::white())
                .text_size(ui::transcript_rems(1.25 * look.text))
                .when(interim, |el| el.text_color(gpui_kit::white().opacity(0.75)))
                .child(
                    h_flex()
                        .gap_2()
                        .items_baseline()
                        .flex_wrap()
                        .justify_center()
                        .when(look.speakers, |el| {
                            el.child(div().flex_none().font_semibold().text_color(ui::speaker_color(&l.speaker, cx)).child(ui::speaker_display(&l.speaker)))
                        })
                        .child(ui::bidi_text(text, true)),
                )
                .when_some(translation, |el, t| el.child(ui::bidi_text(t, true).italic().text_color(gpui_kit::white().opacity(0.85)).text_size(ui::transcript_rems(1.1 * look.text))))
        }).collect::<Vec<_>>();
        v_flex()
            .id("captions-subtitles")
            .group("captions")
            .size_full()
            .relative()
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, cx| this.drag_step(true, window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, window, cx| this.end_drag(window, cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, window, cx| this.end_drag(window, cx)))
            .font(ui::ui_font(cx))
            .text_color(theme.foreground)
            .child(div().when(!pinned, |d| d.invisible().group_hover("captions", |s| s.visible())).child(bar))
            .when_some(self.stalled, |el, secs| el.child(stalled_banner(secs, cx).rounded(theme.radius)))
            .child(v_flex().flex_1().min_h_0().justify_end().items_center().gap_1().pb_2().overflow_hidden().children(lines))
            .child(div().when(!pinned, |d| d.invisible().group_hover("captions", |s| s.visible())).child(grip))
    }
}

#[cfg(test)]
mod tests {
    use super::{Drag, Rect, default_origin, drag_rect, next_opacity};

    #[test]
    fn a_drag_follows_the_pointer() {
        let moved = drag_rect(Drag::Move { pointer: (100, 100), origin: (500, 800) }, (130, 90), (300, 110));
        assert_eq!((moved.x, moved.y), (530, 790));
        let resized = drag_rect(Drag::Resize { pointer: (100, 100), origin: (500, 800), size: (560, 140) }, (140, 120), (300, 110));
        assert_eq!(resized, Rect { x: 500, y: 800, w: 600, h: 160 });
        let small = drag_rect(Drag::Resize { pointer: (100, 100), origin: (500, 800), size: (560, 140) }, (-400, 0), (300, 110));
        assert_eq!((small.w, small.h), (300, 110), "never below the smallest size");
    }

    #[test]
    fn opacity_cycles() {
        assert_eq!(next_opacity(1.0), 0.92);
        assert_eq!(next_opacity(0.65), 1.0);
        assert_eq!(next_opacity(0.5), 1.0, "an odd value starts over");
    }

    #[test]
    fn starts_bottom_middle_clear_of_mori() {
        let work = Rect { x: 0, y: 32, w: 1920, h: 1048 };
        let (x, y) = default_origin(work, (560, 140));
        assert_eq!(x, 680);
        assert!(y + 140 < 32 + 1048 && y > 32 + 1048 / 2, "lower half, above the panel");
        // Mori's default spot (top right) stays clear.
        assert!(x + 560 < 1920 - 48 - 200 || y > 140 + 400);
    }
}
