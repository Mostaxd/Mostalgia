//! Live points: every so often during a recording Mori asks the model for
//! new decisions, action items, open questions and facts, and shows each
//! as a note card popping out of her notepad.
//!
//! The cards form a chat-like list next to her: the newest one nearest to
//! her, older ones further away; it scrolls when it doesn't fit, follows
//! new cards unless scrolled away (then a "N new" pill jumps back), and
//! never covers her or runs past the window (or her monitor's work area).
//!
//! Every point she produces is stored with the meeting when it's saved
//! (see `Companion::save_points`); dismissing or clearing only hides cards.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::companion::{self, Point, PointKind, PointsReply};
use mostalgia_core::i18n::{tr, trf};

use super::{Companion, LivePoint, MoriError, Point2};
use crate::recording::LiveState;
use crate::ui;

/// Cards shown at once; older ones fold into the counter.
const MAX_VISIBLE: usize = 3;
/// A card folds into the counter this long after it was first shown.
const CARD_FOR: Duration = Duration::from_secs(45);
/// A card stays at least this long before newer ones push it into the
/// counter (so a batch of several points all pop up).
const MIN_SHOW: Duration = Duration::from_secs(8);
const CARD_W: f32 = 280.;
/// Space between her and the stack.
pub(super) const STACK_GAP: f32 = 8.;
/// Space between the stack and the window (or work area) edge.
pub(super) const STACK_EDGE: f32 = 8.;
/// Room kept free above the stack in the app window (title bar and the
/// recording header with its buttons).
const APP_TOP: f32 = super::DEFAULT_TOP - 8.;
/// Height of the stack's header row (count, Clear, Collapse).
const HEADER_H: f32 = 30.;
/// Characters of recent transcript sent with a points request.
const POINTS_CONTEXT_CHARS: usize = 24_000;
/// How long a points request waits for her copy of the transcript (desktop)
/// to catch up with the speech counter before asking anyway.
const CATCH_UP_WAIT: Duration = Duration::from_secs(3);
/// Unanalysed speech at the end of a recording worth a last request.
const FINAL_MIN_SPEECH: f64 = 10.;

pub(super) fn kind_color(kind: PointKind, theme: &gpui_kit::component::Theme) -> Hsla {
    match kind {
        PointKind::Decision => theme.success,
        PointKind::Action => theme.primary,
        PointKind::Question => theme.warning,
        PointKind::Fact => theme.info,
    }
}

/// A live points request in flight (for the log line when it returns).
pub(crate) struct PointsRun {
    n: usize,
    started: Instant,
    new_speech: f64,
    chars: usize,
    lines: usize,
    rec_secs: f64,
    session: Option<u64>,
    /// The last request after the recording ended.
    last: bool,
}

/// Where the stack goes: `anchor` is the y of its edge nearest to her
/// (its bottom when `above`, else its top); it may grow `max_h` from there.
#[derive(Clone, Copy, Debug)]
pub(super) struct Region {
    pub left: f32,
    pub width: f32,
    pub above: bool,
    pub anchor: f32,
    pub max_h: f32,
}

fn clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

impl Companion {
    pub(super) fn maybe_request_points(&mut self, live: LiveState, cx: &mut Context<Self>) {
        self.last_live = Some(live);
        let paused_by_error = self.error.as_ref().is_some_and(|e| e.points);
        if self.points_interval == 0 || self.points_rx.is_some() || paused_by_error || live.paused {
            return;
        }
        if live.speech_secs - self.points_mark < self.points_interval as f64 {
            return;
        }
        // On the desktop the speech counter can be ahead of her transcript
        // copy (it comes every half second): wait for the line that pushed
        // it over, else that line would only be read an interval later.
        let caught_up = self.recording.read(cx).caught_up(&live);
        if !caught_up {
            let since = *self.points_wait_since.get_or_insert_with(Instant::now);
            if since.elapsed() < CATCH_UP_WAIT {
                return;
            }
            eprintln!("[mori] {} live points: the transcript copy is still behind the speech counter ({} lines expected); asking with what's there", clock(), live.lines);
        }
        self.points_wait_since = None;
        self.request_points(live, false, cx);
    }

    /// The recording ended (or the window lost it): a last request for the
    /// speech since the previous one, so its points aren't lost.
    pub(super) fn final_points(&mut self, cx: &mut Context<Self>) {
        let Some(live) = self.last_live.take() else { return };
        let paused_by_error = self.error.as_ref().is_some_and(|e| e.points);
        if self.points_interval == 0 || self.points_rx.is_some() || paused_by_error {
            return;
        }
        if live.speech_secs - self.points_mark >= FINAL_MIN_SPEECH {
            self.request_points(live, true, cx);
        }
    }

    fn request_points(&mut self, live: LiveState, last: bool, cx: &mut Context<Self>) {
        let new_speech = live.speech_secs - self.points_mark;
        self.points_mark = live.speech_secs;
        // A little overlap so a point made across the boundary isn't lost.
        let (lines, session) = {
            let feed = self.recording.read(cx);
            (feed.transcript_lines((self.points_from - 30.).max(0.)), feed.session_id().or(self.session_seen))
        };
        self.points_from = live.last_line_end;
        let transcript = companion::points_context(&lines, POINTS_CONTEXT_CHARS);
        let previous: Vec<Point> = self.points.iter().map(|p| p.point.clone()).collect();
        self.points_runs += 1;
        self.points_run = Some(PointsRun {
            n: self.points_runs,
            started: Instant::now(),
            new_speech,
            chars: transcript.len(),
            lines: lines.iter().filter(|l| !l.interim).count(),
            rec_secs: live.last_line_end,
            session,
            last,
        });
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(companion::live_points(&transcript, &previous).map_err(|e| format!("{e:#}")));
        });
        self.points_rx = Some(rx);
    }

    /// Takes a finished points request; returns whether anything changed.
    pub(super) fn poll_points(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(rx) = &self.points_rx else { return false };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(crossbeam_channel::TryRecvError::Empty) => return false,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(tr("Die Anfrage für Live-Punkte ist abgebrochen").to_string()),
        };
        self.points_rx = None;
        let run = self.points_run.take();
        let head = match &run {
            Some(r) => format!(
                "[mori] {} live points #{}{} (rec {}): {:.0}s new speech, {} lines, {} chars, took {:.1}s",
                clock(),
                r.n,
                if r.last { " (after the recording)" } else { "" },
                ui::hms(r.rec_secs),
                r.new_speech,
                r.lines,
                r.chars,
                r.started.elapsed().as_secs_f32()
            ),
            None => format!("[mori] {} live points", clock()),
        };
        match result {
            Ok(PointsReply { points, duplicates }) => {
                if self.error.as_ref().is_some_and(|e| e.points) {
                    self.error = None;
                }
                let returned = points.len();
                // The recording was saved while the request ran: the
                // points go to that meeting.
                if let Some(meeting_id) = self.late_save.take() {
                    let saved = self.store_points(meeting_id, points.iter().map(|p| (p, false)));
                    eprintln!("{head} → {returned} returned ({duplicates} repeated), saved to meeting {meeting_id} (recording already saved){}", if saved { "" } else { ", saving FAILED" });
                    if saved && returned > 0 {
                        cx.emit(super::MoriEvent::NotesSaved { meeting_id });
                    }
                    return true;
                }
                if run.as_ref().is_some_and(|r| r.session.is_some() && r.session != self.session_seen) {
                    eprintln!("{head} → {returned} returned, dropped: a new recording started meanwhile");
                    return true;
                }
                let first = self.points.len();
                self.add_points(points);
                let visible = self.visible_points();
                let shown = visible.iter().filter(|&&i| i >= first).count();
                let reason = if returned == 0 && duplicates > 0 {
                    format!(" — nothing new: all {duplicates} repeated earlier points")
                } else if returned == 0 {
                    " — nothing new: the model returned []".to_string()
                } else if shown < returned && self.open {
                    " — the chat bubble is open: they're in the counter and pop up when it closes".to_string()
                } else if shown < returned {
                    format!(" — {} not shown (folded into the counter)", returned - shown)
                } else {
                    String::new()
                };
                eprintln!("{head} → {returned} returned ({duplicates} repeated), {shown} shown{reason}");
                if returned > 0 {
                    self.talking_until = Some(Instant::now() + Duration::from_millis(2500));
                    if self.points_follow {
                        self.points_scroll_to_end = true;
                    } else {
                        self.points_unseen += returned;
                    }
                }
            }
            // Shown with her Error pose and a card; no new request until
            // dismissed or retried.
            Err(message) => {
                eprintln!("{head} → FAILED: {message}");
                self.late_save = None;
                self.error = Some(MoriError { message, points: true });
                self.points_scroll_to_end = true;
            }
        }
        true
    }

    fn add_points(&mut self, points: Vec<Point>) {
        for point in points {
            self.points.push(LivePoint { id: self.next_point, point, pinned: false, dismissed: false, shown_at: None });
            self.next_point += 1;
        }
    }

    /// Indices of the cards shown now (oldest first).
    /// While the chat bubble is open they all fold into the counter (and
    /// their time starts when they're first shown).
    pub(super) fn visible_points(&self) -> Vec<usize> {
        if self.open {
            return Vec::new();
        }
        let fresh = |p: &LivePoint| p.shown_at.is_none_or(|t| t.elapsed() < CARD_FOR);
        let mut out: Vec<usize> = (0..self.points.len()).filter(|&i| !self.points[i].dismissed && (self.expanded || fresh(&self.points[i]))).collect();
        if !self.expanded {
            while out.len() > MAX_VISIBLE && self.points[out[0]].shown_at.is_some_and(|t| t.elapsed() >= MIN_SHOW) {
                out.remove(0);
            }
        }
        out
    }

    /// Starts the fold timer of the cards on screen now.
    pub(super) fn mark_shown(&mut self) {
        let now = Instant::now();
        for i in self.visible_points() {
            self.points[i].shown_at.get_or_insert(now);
        }
    }

    fn retry_points(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        if self.points_rx.is_none()
            && let Some(live) = self.recording.read(cx).live_state()
        {
            self.points_from = (self.points_from - self.points_interval as f64).max(0.);
            self.request_points(live, false, cx);
        }
        cx.notify();
    }

    /// Hides the cards that aren't pinned (they stay in the meeting's notes).
    fn clear_points(&mut self, cx: &mut Context<Self>) {
        for p in &mut self.points {
            if !p.pinned {
                p.dismissed = true;
            }
        }
        self.points_unseen = 0;
        if self.points.iter().all(|p| p.dismissed) {
            self.expanded = false;
        }
        cx.notify();
    }

    fn scroll_points_to_end(&mut self, cx: &mut Context<Self>) {
        self.points_follow = true;
        self.points_unseen = 0;
        self.points_scroll_to_end = true;
        cx.notify();
    }

    /// Shows the full list ("N more" on her notepad).
    pub(super) fn expand_points(&mut self, cx: &mut Context<Self>) {
        self.expanded = true;
        self.open = false;
        self.scroll_points_to_end(cx);
    }

    /// Shows points as if they had just arrived (screenshots, demo).
    #[doc(hidden)]
    pub fn demo_points(&mut self, points: &[(PointKind, &str, f64)], pinned: &[usize], cx: &mut Context<Self>) {
        let first = self.points.len();
        self.add_points(points.iter().map(|(kind, text, at)| Point { kind: *kind, text: text.to_string(), at_secs: *at }).collect());
        for i in pinned {
            if let Some(p) = self.points.get_mut(first + i) {
                p.pinned = true;
            }
        }
        if self.points_follow {
            self.points_scroll_to_end = true;
        } else {
            self.points_unseen += points.len();
        }
        self.talking_until = Some(Instant::now() + Duration::from_millis(2500));
        cx.notify();
    }

    /// Shows the full list (screenshots, demo).
    #[doc(hidden)]
    pub fn demo_expand(&mut self, expanded: bool, cx: &mut Context<Self>) {
        if expanded {
            self.expand_points(cx);
        } else {
            self.expanded = false;
            cx.notify();
        }
    }

    /// Scrolls the list away from her end by `px` (screenshots, demo), as
    /// a user would with the wheel.
    #[doc(hidden)]
    pub fn demo_scroll_away(&mut self, by: f32, cx: &mut Context<Self>) {
        let off = self.points_scroll.offset();
        let y = if self.points_above { f32::from(off.y) + by } else { f32::from(off.y) - by };
        self.points_scroll.set_offset(point(off.x, px(y)));
        self.points_user_scrolled = true;
        cx.notify();
    }

    /// Where the stack goes: above or below her (desktop: towards the
    /// middle of her monitor, within its work area; in the app: where
    /// there's more room), on her side, never over her.
    pub(super) fn points_region(&self, o: Point2, (w, h): (f32, f32)) -> Region {
        let (vw, vh) = self.viewport;
        let width = CARD_W.min(vw - 16.).max(120.);
        let right_side = match &self.desktop {
            Some(p) => p.right,
            None => o.x + w / 2. > vw / 2.,
        };
        let left = if right_side { (o.x + w - width).max(8.) } else { o.x.min(vw - width - 8.).max(8.) };
        let below_room = vh - (o.y + h) - STACK_GAP;
        let (above, room) = match &self.desktop {
            Some(p) => {
                let monitor = p.cards_room(self.size());
                if p.bottom {
                    (true, (o.y - STACK_GAP - STACK_EDGE).min(monitor))
                } else {
                    (false, (below_room - STACK_EDGE).min(monitor))
                }
            }
            None => {
                let below = below_room - super::MARGIN;
                let up = o.y - STACK_GAP - APP_TOP;
                if below >= 240. || below >= up { (false, below) } else { (true, up) }
            }
        };
        let anchor = if above { o.y - STACK_GAP } else { o.y + h + STACK_GAP };
        Region { left, width, above, anchor, max_h: room.max(60.) }
    }

    fn render_card(&self, i: usize, motion: bool, above: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let p = &self.points[i];
        let id = p.id;
        let color = kind_color(p.point.kind, &theme);
        let card = v_flex()
            .id(("mori-point", id))
            .relative()
            .flex_shrink_0()
            .w_full()
            .px_2()
            .pt_1()
            .pb_1p5()
            .gap_0p5()
            .rounded(theme.radius_lg)
            .border(ui::border_width(cx))
            .border_color(if p.pinned { color } else { theme.border })
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_sm()
            // Clicks stay on the card, the wheel scrolls the list.
            .block_mouse_except_scroll()
            .child(
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .child(div().flex_shrink_0().size(px(7.)).rounded_full().bg(color))
                    .child(div().text_color(color).font_semibold().child(p.point.kind.label()))
                    .child(div().text_color(theme.muted_foreground).font_family(theme.mono_font_family.clone()).child(ui::hms(p.point.at_secs)))
                    .child(div().flex_1())
                    .child(
                        Button::new(("mori-pin", id))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(IconName::Bookmark).text_color(if p.pinned { color } else { theme.muted_foreground }))
                            .tooltip(if p.pinned { tr("Angeheftet – in den Notizen der Besprechung hervorgehoben") } else { tr("Anheften – in den Notizen der Besprechung hervorheben") })
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
                            .tooltip(tr("Blendet aus – bleibt in den Notizen der Besprechung gespeichert"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(p) = this.points.iter_mut().find(|p| p.id == id) {
                                    p.dismissed = true;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(ui::bidi_text(p.point.text.clone(), true).text_sm().line_height(relative(1.3)));
        if motion {
            // Pops out of her notepad: from her side of the stack.
            let from = if above { 12. } else { -12. };
            card.with_animation(("mori-pop", id), Animation::new(Duration::from_millis(380)).with_easing(ease_out_quint()), move |el, d| {
                el.opacity(d).top(px((1. - d) * from))
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
            .flex_shrink_0()
            .w_full()
            .p_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border(ui::border_width(cx))
            .border_color(theme.danger.opacity(0.5))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_sm()
            .block_mouse_except_scroll()
            .child(
                h_flex()
                    .gap_1p5()
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

    /// Whether the list is scrolled to her end (the newest card).
    fn points_at_end(&self, above: bool) -> bool {
        let max = f32::from(self.points_scroll.max_offset().y);
        let off = -f32::from(self.points_scroll.offset().y);
        if above { off >= max - 6. } else { off <= 6. }
    }

    /// The cards (and the last error) next to her, see [`Region`].
    pub(super) fn render_points(&mut self, o: Point2, (w, h): (f32, f32), cx: &mut Context<Self>) -> Option<AnyElement> {
        let spec = crate::themes::active(cx);
        let motion = spec.animations && !self.reduce_motion;
        let error = self.error.clone().filter(|e| e.points || !self.open);
        self.mark_shown();
        let visible = self.visible_points();
        if visible.is_empty() && error.is_none() {
            self.stack_bounds.set(None);
            return None;
        }
        let r = self.points_region(o, (w, h));
        // She moved to where the stack flips over: start at the newest.
        if r.above != self.points_above {
            self.points_above = r.above;
            self.points_follow = true;
            self.points_unseen = 0;
            self.points_scroll_to_end = true;
        }
        // The user scrolled: following new cards again only at her end.
        if self.points_user_scrolled {
            self.points_user_scrolled = false;
            self.points_follow = self.points_at_end(r.above);
            if self.points_follow {
                self.points_unseen = 0;
            }
        }
        if std::mem::take(&mut self.points_scroll_to_end) {
            if r.above {
                self.points_scroll.scroll_to_bottom();
            } else {
                self.points_scroll.set_offset(point(px(0.), px(0.)));
            }
        }
        // Oldest first, the newest (and an error) nearest to her.
        let mut items: Vec<AnyElement> = visible.iter().map(|&i| self.render_card(i, motion, r.above, cx)).collect();
        if let Some(e) = &error {
            items.push(self.render_error_card(e, cx));
        }
        if !r.above {
            items.reverse();
        }
        let theme = cx.theme().clone();
        let total = self.points.iter().filter(|p| !p.dismissed).count();
        let clearable = self.points.iter().any(|p| !p.dismissed && !p.pinned);
        let header = (self.expanded || visible.len() >= 2).then(|| {
            h_flex()
                .flex_shrink_0()
                .h(px(HEADER_H - 4.))
                .gap_1()
                .pl_2()
                .pr_0p5()
                .rounded(theme.radius_lg)
                .bg(theme.popover)
                .border(ui::border_width(cx))
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .block_mouse_except_scroll()
                .child(trf("{} Punkte", &[&total]))
                .child(div().flex_1())
                .when(clearable, |el| {
                    el.child(
                        Button::new("mori-clear")
                            .xsmall()
                            .ghost()
                            .label(tr("Leeren"))
                            .tooltip(tr("Blendet alle nicht angehefteten Karten aus – sie bleiben in den Notizen der Besprechung gespeichert"))
                            .on_click(cx.listener(|this, _, _, cx| this.clear_points(cx))),
                    )
                })
                .when(self.expanded, |el| {
                    el.child(Button::new("mori-collapse").xsmall().ghost().label(tr("Einklappen")).on_click(cx.listener(|this, _, _, cx| {
                        this.expanded = false;
                        this.scroll_points_to_end(cx);
                    })))
                })
        });
        let list_max = (r.max_h - if header.is_some() { HEADER_H } else { 0. }).max(48.);
        let list = v_flex()
            .id("mori-points-list")
            .track_scroll(&self.points_scroll)
            .overflow_y_scroll()
            .max_h(px(list_max))
            .w_full()
            .gap_1p5()
            // Room for the cards' shadows inside the clipped list.
            .p(px(3.))
            .on_scroll(cx.listener(|this, _: &ScrollEvent, _, cx| {
                this.points_user_scrolled = true;
                cx.notify();
            }))
            .children(items);
        let pill = (self.points_unseen > 0 && !self.points_follow).then(|| {
            h_flex()
                .absolute()
                .left_0()
                .right_0()
                .when(r.above, |el| el.bottom(px(8.)))
                .when(!r.above, |el| el.top(px(8.)))
                .justify_center()
                .child(
                    h_flex()
                        .id("mori-new")
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
                        .block_mouse_except_scroll()
                        .child(Icon::new(if r.above { IconName::ArrowDown } else { IconName::ArrowUp }).xsmall())
                        .child(trf("{} neu", &[&self.points_unseen]))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| this.scroll_points_to_end(cx))),
                )
        });
        let list = div().relative().w_full().child(list).children(pill).into_any_element();
        let header = header.map(|h| h.mx(px(3.)).into_any_element());
        // The header at the far end from her.
        let parts: Vec<AnyElement> = if r.above { header.into_iter().chain([list]).collect() } else { [list].into_iter().chain(header).collect() };
        let bounds = self.stack_bounds.clone();
        let vh = self.viewport.1;
        let stack = v_flex()
            .id("mori-points")
            .absolute()
            .left(px(r.left - 3.))
            .w(px(r.width + 6.))
            .max_h(px(r.max_h))
            .gap_1()
            .map(|el| if r.above { el.bottom(px(vh - r.anchor)) } else { el.top(px(r.anchor)) })
            .children(parts)
            // Measures the stack (her desktop window lets only it take the mouse).
            .child(canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        Some(stack.into_any_element())
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
                .on_click(cx.listener(|this, _, _, cx| this.expand_points(cx)))
                .into_any_element(),
        )
    }
}
