//! Live points: every so often during a recording Mori asks the model for
//! new decisions, action items, open questions and facts, and shows each
//! as a note card popping out of her notepad.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::companion::{self, Point, PointKind};
use mostalgia_core::i18n::{tr, trf};

use super::{Companion, LivePoint, MoriError, Point2};
use crate::recording::LiveState;
use crate::ui;

/// Cards shown at once; older ones fold into the counter.
const MAX_VISIBLE: usize = 3;
/// A card folds into the counter after this long.
const CARD_FOR: Duration = Duration::from_secs(45);
const CARD_W: f32 = 280.;
/// Characters of recent transcript sent with a points request.
const POINTS_CONTEXT_CHARS: usize = 24_000;

pub(super) fn kind_color(kind: PointKind, theme: &gpui_kit::component::Theme) -> Hsla {
    match kind {
        PointKind::Decision => theme.success,
        PointKind::Action => theme.primary,
        PointKind::Question => theme.warning,
        PointKind::Fact => theme.info,
    }
}

impl Companion {
    pub(super) fn maybe_request_points(&mut self, live: LiveState, cx: &mut Context<Self>) {
        let paused_by_error = self.error.as_ref().is_some_and(|e| e.points);
        if self.points_interval == 0 || self.points_rx.is_some() || paused_by_error || live.paused {
            return;
        }
        if live.speech_secs - self.points_mark >= self.points_interval as f64 {
            self.request_points(live, cx);
        }
    }

    fn request_points(&mut self, live: LiveState, cx: &mut Context<Self>) {
        self.points_mark = live.speech_secs;
        // A little overlap so a point made across the boundary isn't lost.
        let lines = self.recording.read(cx).transcript_lines((self.points_from - 30.).max(0.));
        self.points_from = live.last_line_end;
        let transcript = companion::points_context(&lines, POINTS_CONTEXT_CHARS);
        let previous: Vec<Point> = self.points.iter().map(|p| p.point.clone()).collect();
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(companion::live_points(&transcript, &previous).map_err(|e| format!("{e:#}")));
        });
        self.points_rx = Some(rx);
    }

    /// Takes a finished points request; returns whether anything changed.
    pub(super) fn poll_points(&mut self) -> bool {
        let Some(rx) = &self.points_rx else { return false };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(crossbeam_channel::TryRecvError::Empty) => return false,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(tr("Die Anfrage für Live-Punkte ist abgebrochen").to_string()),
        };
        self.points_rx = None;
        match result {
            Ok(points) => {
                if self.error.as_ref().is_some_and(|e| e.points) {
                    self.error = None;
                }
                if !points.is_empty() {
                    self.talking_until = Some(Instant::now() + Duration::from_millis(2500));
                }
                self.add_points(points);
            }
            // Shown with her Error pose; no new request until dismissed or retried.
            Err(message) => self.error = Some(MoriError { message, points: true }),
        }
        true
    }

    fn add_points(&mut self, points: Vec<Point>) {
        for point in points {
            self.points.push(LivePoint {
                id: self.next_point,
                point,
                pinned: false,
                dismissed: false,
                shown_at: Instant::now(),
            });
            self.next_point += 1;
        }
    }

    /// Indices of the cards shown now (newest last).
    /// While the chat bubble is open they all fold into the counter.
    pub(super) fn visible_points(&self) -> Vec<usize> {
        if self.open {
            return Vec::new();
        }
        let mut out: Vec<usize> = (0..self.points.len())
            .filter(|&i| !self.points[i].dismissed)
            .filter(|&i| self.expanded || self.points[i].shown_at.elapsed() < CARD_FOR)
            .collect();
        if !self.expanded && out.len() > MAX_VISIBLE {
            out.drain(..out.len() - MAX_VISIBLE);
        }
        out
    }

    fn retry_points(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        if let Some(live) = self.recording.read(cx).live_state() {
            self.points_from = (self.points_from - self.points_interval as f64).max(0.);
            self.request_points(live, cx);
        }
        cx.notify();
    }

    /// Shows points as if they had just arrived (screenshots).
    #[doc(hidden)]
    pub fn demo_points(&mut self, points: &[(PointKind, &str, f64)], pinned: &[usize], cx: &mut Context<Self>) {
        let first = self.points.len();
        self.add_points(points.iter().map(|(kind, text, at)| Point { kind: *kind, text: text.to_string(), at_secs: *at }).collect());
        for i in pinned {
            if let Some(p) = self.points.get_mut(first + i) {
                p.pinned = true;
            }
        }
        cx.notify();
    }

    fn render_card(&self, i: usize, motion: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let p = &self.points[i];
        let id = p.id;
        let color = kind_color(p.point.kind, &theme);
        let card = v_flex()
            .id(("mori-point", id))
            .relative()
            .w_full()
            .p_2p5()
            .gap_1()
            .rounded(theme.radius_lg)
            .border(ui::border_width(cx))
            .border_color(if p.pinned { color } else { theme.border })
            .border_l_4()
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_md()
            .occlude()
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_color(color)
                            .font_semibold()
                            .child(div().size(px(6.)).rounded_full().bg(color))
                            .child(p.point.kind.label()),
                    )
                    .child(div().text_color(theme.muted_foreground).font_family(theme.mono_font_family.clone()).child(ui::hms(p.point.at_secs)))
                    .child(div().flex_1())
                    .child(
                        Button::new(("mori-pin", id))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(IconName::Bookmark).text_color(if p.pinned { color } else { theme.muted_foreground }))
                            .tooltip(if p.pinned { tr("Angeheftet – wird mit der Besprechung gespeichert") } else { tr("Anheften – wird mit der Besprechung gespeichert") })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(p) = this.points.iter_mut().find(|p| p.id == id) {
                                    p.pinned = !p.pinned;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("mori-dismiss", id))
                            .xsmall()
                            .ghost()
                            .icon(IconName::X)
                            .tooltip(tr("Verwerfen"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(p) = this.points.iter_mut().find(|p| p.id == id) {
                                    p.dismissed = true;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(ui::bidi_text(p.point.text.clone(), true).text_sm());
        if motion {
            card.with_animation(("mori-pop", id), Animation::new(Duration::from_millis(380)).with_easing(ease_out_quint()), |el, d| {
                el.opacity(d).top(px((1. - d) * -14.))
            })
            .into_any_element()
        } else {
            card.into_any_element()
        }
    }

    fn render_error_card(&self, err: &MoriError, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .id("mori-error")
            .w_full()
            .p_2p5()
            .gap_1p5()
            .rounded(theme.radius_lg)
            .border(ui::border_width(cx))
            .border_color(theme.danger.opacity(0.5))
            .bg(theme.popover)
            .shadow_md()
            .occlude()
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.danger)
                    .child(Icon::new(IconName::TriangleAlert).xsmall())
                    .child(if err.points { tr("Live-Punkte fehlgeschlagen") } else { tr("Mori konnte nicht antworten") }),
            )
            .child(div().text_xs().child(err.message.clone()))
            .when(err.points, |el| {
                el.child(div().text_xs().text_color(theme.muted_foreground).child(tr("Live-Punkte sind pausiert, bis du es erneut versuchst.")))
            })
            .child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .when(err.points, |el| {
                        el.child(
                            Button::new("mori-retry")
                                .xsmall()
                                .outline()
                                .icon(IconName::RefreshCw)
                                .label(tr("Erneut versuchen"))
                                .on_click(cx.listener(|this, _, _, cx| this.retry_points(cx))),
                        )
                    })
                    .child(Button::new("mori-error-dismiss").xsmall().ghost().label(tr("Schließen")).on_click(cx.listener(|this, _, _, cx| {
                        this.error = None;
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }

    /// The cards (and the last error) next to her: below when there's
    /// room, else above; on her side of the window.
    pub(super) fn render_points(&mut self, o: Point2, (w, h): (f32, f32), cx: &mut Context<Self>) -> Option<AnyElement> {
        let spec = crate::themes::active(cx);
        let motion = spec.animations && !self.reduce_motion;
        let error = self.error.clone().filter(|e| e.points || !self.open);
        let visible = self.visible_points();
        if visible.is_empty() && error.is_none() {
            return None;
        }
        let (vw, vh) = self.viewport;
        let width = CARD_W.min(vw - 16.);
        let right_side = o.x + w / 2. > vw / 2.;
        let left = if right_side { (o.x + w - width).max(8.) } else { o.x.min(vw - width - 8.) };
        let below = vh - (o.y + h);
        let mut cards: Vec<AnyElement> = Vec::new();
        if let Some(e) = &error {
            cards.push(self.render_error_card(e, cx));
        }
        for i in visible.into_iter().rev() {
            cards.push(self.render_card(i, motion, cx));
        }
        let theme = cx.theme().clone();
        let stack = v_flex()
            .id("mori-points")
            .absolute()
            .left(px(left))
            .w(px(width))
            .gap_2()
            .when(self.expanded, |el| el.max_h(px((vh * 0.6).max(200.))).overflow_y_scroll())
            .when(self.expanded, |el| {
                el.child(
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(trf("{} Punkte", &[&self.points.iter().filter(|p| !p.dismissed).count()]))
                        .child(Button::new("mori-collapse").xsmall().ghost().label(tr("Einklappen")).on_click(cx.listener(|this, _, _, cx| {
                            this.expanded = false;
                            cx.notify();
                        }))),
                )
            })
            .children(cards);
        Some(
            if below >= 220. {
                stack.top(px(o.y + h + 6.))
            } else {
                stack.top(px((o.y - 6. - 3. * 96.).max(48.)))
            }
            .into_any_element(),
        )
    }

    /// The counter on her notepad for points not shown as cards.
    pub(super) fn render_badge(&mut self, o: Point2, (w, h): (f32, f32), cx: &mut Context<Self>) -> Option<AnyElement> {
        let shown = self.visible_points().len();
        let total = self.points.iter().filter(|p| !p.dismissed).count();
        if total == 0 || self.expanded && !self.open || total == shown {
            return None;
        }
        let theme = cx.theme();
        let pinned = self.points.iter().filter(|p| p.pinned && !p.dismissed).count();
        Some(
            h_flex()
                .id("mori-badge")
                .absolute()
                .left(px(o.x + w * 0.62))
                .top(px(o.y + h * 0.58))
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_full()
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .text_xs()
                .font_semibold()
                .shadow_md()
                .cursor_pointer()
                .child(Icon::new(IconName::FileText).xsmall())
                .child(format!("{}", total - shown))
                .tooltip({
                    let tip: SharedString = trf("{} weitere Punkte ({} angeheftet) – klicken zum Anzeigen", &[&(total - shown), &pinned]).into();
                    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.expanded = true;
                    this.open = false;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }
}
