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
//! remembered.

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
            kind: WindowKind::Normal,
            is_movable: true,
            is_resizable: true,
            is_minimizable: false,
            window_min_size: Some(size(px(300.), px(110.))),
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
    /// Times of the lines flagged here.
    flagged: Vec<f64>,
    stalled: Option<u64>,
    scroll: ScrollHandle,
    /// Following the newest line (off once scrolled up).
    follow: bool,
    opacity: f32,
    /// Last saved place and size.
    saved: Option<((i32, i32), (f32, f32))>,
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
                let scale = window.scale_factor();
                let (w, h) = config.live_captions_size.unwrap_or(DEFAULT_SIZE);
                let size = ((w * scale) as i32, (h * scale) as i32);
                let origin = config.live_captions_pos.unwrap_or_else(|| {
                    let monitors = x.monitors();
                    let m = monitors.iter().find(|m| m.primary).or(monitors.first()).map(|m| m.rect).unwrap_or(Rect { x: 0, y: 0, w: 1920, h: 1080 });
                    default_origin(x.work_area().and_then(|w| w.intersect(&m)).unwrap_or(m), size)
                });
                let _ = x.move_resize(Rect { x: origin.0, y: origin.1, w: size.0, h: size.1 });
            }
            None => eprintln!("[captions] not an X11 window; it can't stay on top"),
        }
        cx.observe_window_appearance(window, |_, window, cx| ui::apply_theme(window, cx)).detach();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(60)).await;
                if this.update_in(cx, |this, window, cx| this.poll(window, cx)).is_err() {
                    break;
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
            flagged: Vec::new(),
            stalled: None,
            scroll: ScrollHandle::new(),
            follow: true,
            opacity: config.live_captions_opacity.clamp(0.4, 1.0),
            saved: None,
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
                ToMori::ReloadConfig => {
                    let config = Config::load();
                    mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&config.ui_language));
                    ui::apply_theme(window, cx);
                    self.opacity = config.live_captions_opacity.clamp(0.4, 1.0);
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
        if self.checked_at.elapsed() > Duration::from_secs(2) {
            self.checked_at = Instant::now();
            self.save_place(window);
        }
        if changed {
            cx.notify();
        }
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
            .child(
                div()
                    .flex_none()
                    .min_w(px(56.))
                    .text_size(ui::transcript_rems(0.8))
                    .font_semibold()
                    .text_color(ui::speaker_color(&l.speaker, cx))
                    .child(ui::speaker_display(&l.speaker)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui::transcript_rems(0.95))
                    .when(interim, |el| el.text_color(theme.muted_foreground).italic())
                    .child(ui::bidi_text(l.text.clone(), true)),
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let live = self.live.clone();
        let paused = live.as_ref().is_some_and(|l| l.paused);
        let mic_on = live.as_ref().and_then(|l| l.sources).is_none_or(|s| s[0]);
        let elapsed = live.as_ref().map_or(0., |l| l.elapsed_secs);
        let lines = self.lines.clone();
        let interim = self.interim.clone();
        let n = lines.len();
        let header = h_flex()
            .id("captions-bar")
            .flex_none()
            .h(px(30.))
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .cursor_move()
            // Moving it by its bar (the window manager does the move).
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
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
            .child(Button::new("captions-app").xsmall().ghost().icon(IconName::AppWindow).tooltip(tr("Mostalgia öffnen")).on_click(|_, _, _| send(&FromMori::ShowApp)));
        v_flex()
            .size_full()
            .relative()
            .font(ui::ui_font(cx))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background.opacity(self.opacity))
            .text_color(theme.foreground)
            .overflow_hidden()
            .child(header)
            .when_some(self.stalled, |el, secs| {
                el.child(
                    h_flex()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .gap_1p5()
                        .bg(theme.warning.opacity(0.15))
                        .text_xs()
                        .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning))
                        .child(trf("Die Live-Transkription hängt seit {} s", &[&secs])),
                )
            })
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
            .child(
                // The corner grip (the window manager resizes).
                div()
                    .id("captions-resize")
                    .absolute()
                    .right_0()
                    .bottom_0()
                    .size(px(14.))
                    .cursor(CursorStyle::ResizeUpLeftDownRight)
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_resize(ResizeEdge::BottomRight)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Rect, default_origin, next_opacity};

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
