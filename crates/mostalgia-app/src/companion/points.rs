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
use crossbeam_channel::Receiver;
use mostalgia_core::companion::{self, ContextLine, NotesClock, NotesInput, Point, PointKind, PointsReply};
use mostalgia_core::config::NotesMode;
use mostalgia_core::i18n::{tr, trf};

use super::{Companion, LivePoint, MoriError, Point2};
use crate::recording::LiveState;
use crate::ui;

/// Cards shown at once; older ones fold into the counter.
const MAX_VISIBLE: usize = 4;
/// A card folds into the counter this long after it was first shown (or
/// a while after it shrank to a line, if that's later; see
/// `Config::card_collapse`, and `Config::card_stagger` for how far apart
/// cards that arrive together appear).
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
/// Characters of transcript sent with a points request (beyond them the
/// meeting memory takes over, see `companion::memory_due`).
const POINTS_CONTEXT_CHARS: usize = 24_000;
/// How long a points request waits for her copy of the transcript (desktop)
/// to catch up with the speech counter before asking anyway.
const CATCH_UP_WAIT: Duration = Duration::from_secs(3);
/// Unanalysed speech at the end of a recording worth a last request.
const FINAL_MIN_SPEECH: f64 = 4.;

pub(super) fn kind_color(kind: PointKind, theme: &gpui_kit::component::Theme) -> Hsla {
    match kind {
        PointKind::Decision => theme.success,
        PointKind::Action => theme.primary,
        PointKind::Question => theme.warning,
        PointKind::Fact => theme.info,
        PointKind::Point => theme.magenta,
    }
}

/// A live points request in flight (for the log line when it returns).
pub(crate) struct PointsRun {
    n: usize,
    started: Instant,
    new_speech: f64,
    chars: usize,
    /// Of `chars`: the start that stays the same between requests (cacheable).
    stable: usize,
    lines: usize,
    rec_secs: f64,
    session: Option<u64>,
    /// When the newest line it looks at was finalized (arrived here).
    line_at: Option<Instant>,
    /// The last request after the recording ended.
    last: bool,
}

/// When to ask for live notes next and what to send: the lines already
/// looked at, the running meeting memory, the cards' release times.
#[derive(Default)]
pub(crate) struct NotesState {
    /// Finalized lines (count) at the last request.
    seen_lines: usize,
    /// Lines up to this recording second were looked at (`None`: none yet).
    new_after: Option<f64>,
    /// When the last request started.
    last_start: Option<Instant>,
    /// Finalized lines (count) at the last tick.
    lines_now: usize,
    /// When the finalized line count last grew (the newest line arrived).
    line_at: Option<Instant>,
    /// When the first line not looked at yet arrived.
    waiting_since: Option<Instant>,
    /// Words in the lines not looked at yet: (line count they're for, words).
    words: (usize, usize),
    /// What happened before `memory_until`, in a few sentences.
    memory: Option<String>,
    /// Lines from this recording second on go verbatim.
    memory_until: f64,
    memory_rx: Option<Receiver<(f64, Result<String, String>)>>,
    /// When the latest card appears (or appeared).
    last_release: Option<Instant>,
    /// The style and focus logged last (logged again when they change).
    style_seen: Option<(companion::NoteStyle, String)>,
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
        let now = Instant::now();
        if live.lines != self.notes.lines_now {
            if live.lines > self.notes.lines_now {
                self.notes.line_at = Some(now);
            }
            self.notes.lines_now = live.lines;
        }
        if live.lines > self.notes.seen_lines {
            self.notes.waiting_since.get_or_insert(now);
        } else {
            self.notes.waiting_since = None;
        }
        let paused_by_error = self.error.as_ref().is_some_and(|e| e.points);
        if self.notes_mode == NotesMode::Off || paused_by_error || live.paused {
            return;
        }
        // Words in the lines not looked at yet (counted when one arrives).
        if self.notes.words.0 != live.lines {
            let after = self.notes.new_after.unwrap_or(-1.);
            let words = self.recording.read(cx).transcript_lines(after.max(0.)).iter().filter(|l| !l.interim && l.at_secs > after).map(|l| companion::word_count(&l.text)).sum();
            self.notes.words = (live.lines, words);
        }
        let due = NotesClock {
            in_flight: self.points_rx.is_some(),
            since_last: self.notes.last_start.map(|t| t.elapsed().as_secs_f64()),
            new_lines: live.lines.saturating_sub(self.notes.seen_lines),
            new_words: self.notes.words.1,
            new_speech: live.speech_secs - self.points_mark,
            waiting: self.notes.waiting_since.map_or(0., |t| t.elapsed().as_secs_f64()),
        };
        if !companion::notes_due(self.notes_mode, self.notes_gap, &due) {
            return;
        }
        // On the desktop the speech counter can be ahead of her transcript
        // copy (it comes every half second): wait for the line that pushed
        // it over, else that line would only be read with the next request.
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
        if self.notes_mode == NotesMode::Off || self.points_rx.is_some() || paused_by_error {
            return;
        }
        if live.lines > self.notes.seen_lines && live.speech_secs - self.points_mark >= FINAL_MIN_SPEECH {
            self.request_points(live, true, cx);
        }
    }

    fn request_points(&mut self, live: LiveState, last: bool, cx: &mut Context<Self>) {
        let new_speech = live.speech_secs - self.points_mark;
        self.points_mark = live.speech_secs;
        let (lines, session) = {
            let feed = self.recording.read(cx);
            (feed.transcript_lines(self.notes.memory_until), feed.session_id().or(self.session_seen))
        };
        // Finalized lines from where the memory ends (the whole meeting
        // while it's short).
        let lines: Vec<ContextLine> = lines.into_iter().filter(|l| !l.interim && l.at_secs >= self.notes.memory_until).collect();
        let previous: Vec<Point> = self.points.iter().map(|p| p.point.clone()).collect();
        let input = NotesInput { memory: self.notes.memory.as_deref(), lines: &lines, new_after: self.notes.new_after.unwrap_or(-1.), notes: &previous };
        let (message, stable) = companion::notes_message(&input, POINTS_CONTEXT_CHARS);
        if let Some(newest) = lines.iter().map(|l| l.at_secs).reduce(f64::max) {
            self.notes.new_after = Some(newest.max(self.notes.new_after.unwrap_or(newest)));
        }
        self.notes.seen_lines = live.lines;
        self.notes.waiting_since = None;
        self.notes.last_start = Some(Instant::now());
        self.notes.words = (live.lines, 0);
        self.points_runs += 1;
        self.points_run = Some(PointsRun {
            n: self.points_runs,
            started: Instant::now(),
            new_speech,
            chars: message.len(),
            stable,
            lines: lines.len(),
            rec_secs: live.last_line_end,
            session,
            line_at: self.notes.line_at,
            last,
        });
        let mode = self.notes_mode;
        let (tx, rx) = crossbeam_channel::bounded(1);
        let notes = previous.clone();
        let focus = self.notes_focus_in(cx);
        let style = self.notes_style_now();
        if self.notes.style_seen != Some((style, focus.clone())) {
            let wide = if companion::focus_widens(&focus) { " (the focus asks for more: room for more notes per check)" } else { "" };
            eprintln!("[mori] {} live notes: style {}, focus {focus:?}{wide}", clock(), style.id());
            self.notes.style_seen = Some((style, focus.clone()));
        }
        let memory_focus = focus.clone();
        std::thread::spawn(move || {
            let _ = tx.send(companion::live_points(&message, stable, &notes, mode, style, &focus).map_err(|e| format!("{e:#}")));
        });
        self.points_rx = Some(rx);
        // Long meetings: older lines fold into the memory (in the
        // background; requests go on with the old one meanwhile).
        if self.notes.memory_rx.is_none()
            && let Some(cut) = companion::memory_due(&lines)
        {
            let fold: Vec<ContextLine> = lines.iter().filter(|l| l.at_secs < cut).cloned().collect();
            let message = companion::memory_message(self.notes.memory.as_deref(), &fold, &previous);
            eprintln!("[mori] {} meeting memory: folding {} lines up to {} into it", clock(), fold.len(), ui::hms(cut));
            let (tx, rx) = crossbeam_channel::bounded(1);
            let started = Instant::now();
            std::thread::spawn(move || {
                let result = companion::meeting_memory(&message, &memory_focus).map(|(m, _)| m).map_err(|e| format!("{e:#}"));
                eprintln!("[mori] meeting memory took {:.1}s", started.elapsed().as_secs_f32());
                let _ = tx.send((cut, result));
            });
            self.notes.memory_rx = Some(rx);
        }
    }

    /// Takes a finished memory update. A failure only goes to the log: the
    /// notes go on with the longer transcript (and it's tried again).
    pub(super) fn poll_memory(&mut self) {
        let Some(rx) = &self.notes.memory_rx else { return };
        let (cut, result) = match rx.try_recv() {
            Ok(r) => r,
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            Err(crossbeam_channel::TryRecvError::Disconnected) => (0., Err("the request thread ended".into())),
        };
        self.notes.memory_rx = None;
        match result {
            Ok(memory) => {
                eprintln!("[mori] {} meeting memory now covers up to {}: {memory}", clock(), ui::hms(cut));
                self.notes.memory = Some(memory);
                self.notes.memory_until = cut;
            }
            Err(e) => eprintln!("[mori] {} meeting memory FAILED (notes keep the full transcript): {e}", clock()),
        }
    }

    /// Takes a finished points request; returns whether anything changed.
    pub(super) fn poll_points(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(rx) = &self.points_rx else { return false };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(crossbeam_channel::TryRecvError::Empty) => return false,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(tr("Die Anfrage für Live-Notizen ist abgebrochen").to_string()),
        };
        self.points_rx = None;
        let run = self.points_run.take();
        let head = match &run {
            Some(r) => format!(
                "[mori] {} live points #{}{} (rec {}): {:.0}s new speech, {} lines, {} chars ({} stable), took {:.1}s{}",
                clock(),
                r.n,
                if r.last { " (after the recording)" } else { "" },
                ui::hms(r.rec_secs),
                r.new_speech,
                r.lines,
                r.chars,
                r.stable,
                r.started.elapsed().as_secs_f32(),
                r.line_at.map_or(String::new(), |t| format!(", {:.1}s after its newest line", t.elapsed().as_secs_f32())),
            ),
            None => format!("[mori] {} live points", clock()),
        };
        match result {
            Ok(PointsReply { points, duplicates, over_limit, usage, first_token_secs, model }) => {
                if self.error.as_ref().is_some_and(|e| e.points) {
                    self.error = None;
                }
                let returned = points.len();
                let cost = format!(
                    "{model}, first token {}{}",
                    first_token_secs.map_or("-".into(), |s| format!("{s:.1}s")),
                    usage.map_or(String::new(), |u| format!(", {} tokens in ({} cached), {} out", u.input, u.cached, u.output))
                );
                // The recording was saved while the request ran: the
                // points go to that meeting.
                if let Some(meeting_id) = self.late_save.take() {
                    let saved = self.store_points(meeting_id, points.iter().map(|p| (p, false)));
                    eprintln!("{head} → {returned} returned ({duplicates} repeated), saved to meeting {meeting_id} (recording already saved){} [{cost}]", if saved { "" } else { ", saving FAILED" });
                    if saved && returned > 0 {
                        cx.emit(super::MoriEvent::NotesSaved { meeting_id });
                    }
                    return true;
                }
                if run.as_ref().is_some_and(|r| r.session.is_some() && r.session != self.session_seen) {
                    eprintln!("{head} → {returned} returned, dropped: a new recording started meanwhile");
                    return true;
                }
                let reason = if returned == 0 && duplicates > 0 {
                    format!(" — nothing new: all {duplicates} repeated earlier points")
                } else if returned == 0 {
                    " — nothing new: the model returned no notes".to_string()
                } else if returned > 1 {
                    " — they appear one by one".to_string()
                } else {
                    String::new()
                };
                let over = if over_limit > 0 { format!(", {over_limit} over the limit left out") } else { String::new() };
                eprintln!("{head} → {returned} returned ({duplicates} repeated{over}){reason} [{cost}]");
                self.add_points(points, run.as_ref().and_then(|r| r.line_at));
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

    /// Queues new cards: each appears `card_stagger` after the one before.
    fn add_points(&mut self, points: Vec<Point>, line_at: Option<Instant>) {
        let now = Instant::now();
        let secs = |t: Instant| if t >= now { (t - now).as_secs_f64() } else { -(now - t).as_secs_f64() };
        let at = companion::stagger(0., self.notes.last_release.map(secs), points.len(), self.card_stagger.as_secs_f64());
        for (point, at) in points.into_iter().zip(at) {
            let release_at = now + Duration::from_secs_f64(at.max(0.));
            self.notes.last_release = Some(release_at);
            self.points.push(LivePoint { id: self.next_point, point, pinned: false, dismissed: false, shown_at: None, release_at, announced: false, line_at, arrived: now });
            self.next_point += 1;
        }
    }

    /// Cards whose time came: she talks, the list follows (or the "N new"
    /// pill counts them), and the log says how late they are. Returns
    /// whether any appeared.
    pub(super) fn release_points(&mut self) -> bool {
        let now = Instant::now();
        let mut released = 0;
        let mut newest = None;
        for p in &mut self.points {
            if p.announced || p.release_at > now {
                continue;
            }
            p.announced = true;
            released += 1;
            if !p.dismissed {
                newest = Some(p.point.kind);
            }
            let Some(line_at) = p.line_at else { continue };
            eprintln!(
                "[mori] {} note shown{}: {:.1}s after its newest line was finalized (request {:.1}s, queued {:.1}s) — [{}] {}",
                clock(),
                if p.dismissed { " (already dismissed)" } else { "" },
                (now - line_at).as_secs_f64(),
                p.arrived.saturating_duration_since(line_at).as_secs_f64(),
                (now - p.arrived).as_secs_f64(),
                p.point.kind.as_str(),
                p.point.text
            );
        }
        if released == 0 {
            return false;
        }
        self.talking_until = Some(now + Duration::from_millis(2000));
        if let Some(kind) = newest {
            self.start_pop(kind);
        }
        if self.points_follow {
            self.points_scroll_to_end = true;
        } else {
            self.points_unseen += released;
        }
        true
    }

    /// Indices of the cards shown now (oldest first): released, not
    /// dismissed, not folded into the counter yet. While the chat bubble is
    /// open they all fold into the counter (and their time starts when
    /// they're first shown).
    pub(super) fn visible_points(&self) -> Vec<usize> {
        if self.open {
            return Vec::new();
        }
        let now = Instant::now();
        let card_for = self.card_collapse.map_or(CARD_FOR, |c| CARD_FOR.max(c + Duration::from_secs(10)));
        let fresh = |p: &LivePoint| p.shown_at.is_none_or(|t| t.elapsed() < card_for);
        let mut out: Vec<usize> =
            (0..self.points.len()).filter(|&i| !self.points[i].dismissed && self.points[i].release_at <= now && (self.expanded || fresh(&self.points[i]))).collect();
        if !self.expanded {
            while out.len() > MAX_VISIBLE && self.points[out[0]].shown_at.is_some_and(|t| t.elapsed() >= MIN_SHOW) {
                out.remove(0);
            }
        }
        out
    }

    /// Whether a card shows as one quiet line: a while after it appeared,
    /// unless the full list is open or the mouse is on it.
    pub(super) fn is_compact(&self, i: usize) -> bool {
        let p = &self.points[i];
        !self.expanded && self.hovered_point != Some(p.id) && self.card_collapse.is_some_and(|after| p.shown_at.is_some_and(|t| t.elapsed() >= after))
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
        // All at once (no staggering), like a batch that arrived a while ago.
        self.notes.last_release = None;
        self.add_points(points.iter().map(|(kind, text, at)| Point { kind: *kind, text: text.to_string(), at_secs: *at }).collect(), None);
        for (i, p) in self.points[first..].iter_mut().enumerate() {
            p.release_at = p.arrived;
            p.pinned = pinned.contains(&i);
        }
        self.release_points();
        cx.notify();
    }

    /// Makes every card but the newest `fresh` look as if it had been on
    /// screen for `secs` (screenshots: compact older cards).
    #[doc(hidden)]
    pub fn demo_age_points(&mut self, fresh: usize, secs: f32, cx: &mut Context<Self>) {
        let then = Instant::now().checked_sub(Duration::from_secs_f32(secs)).unwrap_or_else(Instant::now);
        let n = self.points.len().saturating_sub(fresh);
        for p in &mut self.points[..n] {
            p.shown_at = Some(then);
        }
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
        let compact = self.is_compact(i);
        // The mouse on a compact card shows it in full (with its buttons).
        let hover = cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered {
                this.hovered_point = Some(id);
            } else if this.hovered_point == Some(id) {
                this.hovered_point = None;
            }
            cx.notify();
        });
        if compact {
            let card = h_flex()
                .id(("mori-point", id))
                .relative()
                .flex_shrink_0()
                .w_full()
                .px_2()
                .py_0p5()
                .gap_1p5()
                .rounded(theme.radius_lg)
                .border(ui::border_width(cx))
                .border_color(if p.pinned { color.opacity(0.6) } else { theme.border.opacity(0.6) })
                .bg(theme.popover.opacity(0.85))
                .text_color(theme.popover_foreground)
                .text_xs()
                .block_mouse_except_scroll()
                .on_hover(hover)
                .child(div().flex_shrink_0().size(px(6.)).rounded_full().bg(color))
                .child(div().flex_1().min_w_0().truncate().child(p.point.text.clone()))
                .when(p.pinned, |el| el.child(Icon::new(IconName::Bookmark).xsmall().text_color(color)));
            // Settles from full strength to the quiet line.
            return if motion {
                card.with_animation(("mori-settle", id), Animation::new(Duration::from_millis(600)).with_easing(ease_in_out), |el, d| el.opacity(1. - 0.35 * d))
                    .into_any_element()
            } else {
                card.opacity(0.65).into_any_element()
            };
        }
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
            .on_hover(hover)
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
                                if this.hovered_point == Some(id) {
                                    this.hovered_point = None;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(ui::bidi_text(p.point.text.clone(), true).text_sm().line_height(relative(1.3)));
        // A new card eases out of her notepad (from her side of the stack);
        // one shown in full again under the mouse just appears.
        let new = p.shown_at.is_none_or(|t| t.elapsed() < Duration::from_secs(1));
        if motion && new {
            let from = if above { 10. } else { -10. };
            card.with_animation(("mori-pop", id), Animation::new(Duration::from_millis(650)).with_easing(ease_out_quint()), move |el, d| {
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
                    .child(if err.points { tr("Live-Notizen fehlgeschlagen").to_string() } else { trf("{} konnte nicht antworten", &[&self.name()]) }),
            )
            .child(div().text_xs().child(err.message.clone()))
            .when(err.points, |el| {
                el.child(div().text_xs().text_color(theme.muted_foreground).child(tr("Live-Notizen sind pausiert, bis du es erneut versuchst.")))
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
                .child(trf("{} Notizen", &[&total]))
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
                    let tip: SharedString = trf("{} weitere Notizen ({} angeheftet) – klicken zum Anzeigen", &[&(total - shown), &pinned]).into();
                    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.expand_points(cx)))
                .into_any_element(),
        )
    }
}
