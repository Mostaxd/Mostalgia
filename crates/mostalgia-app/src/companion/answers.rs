//! The live answer helper: when someone in the recording asks a question
//! (a finished line from someone else with a question mark), she looks it
//! up in past meetings and/or the documents drive and, if they hold the
//! answer, quietly shows a suggested answer with its sources next to her
//! notes (`Config::companion_answers`). One request at a time; a question
//! asked meanwhile waits, newer ones replace it.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::ask::cite::{self, Mark, Target};
use mostalgia_core::assist::{self, Sources, Suggestion};
use mostalgia_core::companion::{ContextLine, same_note};
use mostalgia_core::i18n::tr;

use super::{Companion, MoriEvent};
use crate::recording::LiveState;
use crate::ui;

/// No new request sooner than this after the previous one started.
const MIN_GAP: Duration = Duration::from_secs(6);
/// Transcript before a question sent along (recording seconds).
const CONTEXT_SECS: f64 = 120.;
/// Lines this far behind the newest one checked are looked at again (the
/// two tracks' lines don't arrive in time order).
const LOOK_BACK_SECS: f64 = 60.;
/// A suggestion shown longer than this (and not hovered) folds away.
const SHOW_FOR: Duration = Duration::from_secs(120);
/// Suggestions kept on screen at once (older ones fold away).
const MAX_SHOWN: usize = 2;

/// A suggested answer on screen.
pub(crate) struct LiveAnswer {
    id: usize,
    question: String,
    at_secs: f64,
    answer: String,
    dismissed: bool,
    shown_at: Instant,
}

struct Asking {
    question: String,
    at_secs: f64,
    before: Vec<ContextLine>,
}

type Reply = (String, f64, Instant, Result<Option<Suggestion>, String>);

#[derive(Default)]
pub(crate) struct AnswersState {
    enabled: bool,
    sources: Sources,
    /// Lines already looked at (recording second, text).
    seen: HashSet<(u64, String)>,
    /// Newest line looked at (recording seconds).
    newest: f64,
    /// Finalized lines at the last check.
    lines: usize,
    /// Someone other than "Me" was heard: then the user's own questions
    /// don't count.
    others_heard: bool,
    /// Questions already looked up (not asked twice).
    asked: Vec<String>,
    waiting: Option<Asking>,
    rx: Option<Receiver<Reply>>,
    last_start: Option<Instant>,
    pub(crate) items: Vec<LiveAnswer>,
    next_id: usize,
    hovered: Option<usize>,
}

impl AnswersState {
    pub(crate) fn configure(&mut self, enabled: bool, sources: Sources) {
        self.enabled = enabled;
        self.sources = sources;
        if !enabled {
            self.waiting = None;
        }
    }

    /// A new recording: nothing seen, nothing shown.
    pub(crate) fn reset(&mut self) {
        let (enabled, sources) = (self.enabled, self.sources);
        *self = Self { enabled, sources, ..Default::default() };
    }

    pub(crate) fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Suggestions shown now (oldest first).
    fn visible(&self) -> Vec<usize> {
        let mut out: Vec<usize> = (0..self.items.len())
            .filter(|&i| {
                let a = &self.items[i];
                !a.dismissed && (a.shown_at.elapsed() < SHOW_FOR || self.hovered == Some(a.id))
            })
            .collect();
        while out.len() > MAX_SHOWN {
            out.remove(0);
        }
        out
    }
}

/// The answer as plain text without its source markers (for the clipboard).
fn plain(answer: &str) -> String {
    let text: String = cite::parse(answer).pieces.iter().filter_map(|p| if let Mark::Text(t) = p { Some(t.as_str()) } else { None }).collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ").replace(" .", ".").replace(" ,", ",")
}

impl Companion {
    /// Looks at the lines finished since the last check for a question and
    /// starts a lookup (or queues it while one runs).
    pub(super) fn maybe_suggest(&mut self, live: LiveState, cx: &mut Context<Self>) {
        let a = &mut self.answers;
        if !a.enabled || live.paused || live.lines == a.lines {
            self.start_waiting_suggestion();
            return;
        }
        a.lines = live.lines;
        let lines = self.recording.read(cx).transcript_lines((a.newest - LOOK_BACK_SECS).max(0.));
        let a = &mut self.answers;
        for line in lines.iter().filter(|l| !l.interim) {
            if !a.seen.insert((line.at_secs.to_bits(), line.text.clone())) {
                continue;
            }
            a.newest = a.newest.max(line.at_secs);
            if line.speaker != "Me" {
                a.others_heard = true;
            }
            let Some(question) = assist::question_in(line, a.others_heard) else { continue };
            if a.asked.iter().any(|q| same_note(q, &question)) {
                continue;
            }
            a.asked.push(question.clone());
            let before: Vec<ContextLine> = lines.iter().filter(|l| !l.interim && l.at_secs < line.at_secs && l.at_secs >= line.at_secs - CONTEXT_SECS).cloned().collect();
            eprintln!("[mori] {} live answer: question at {}: {question}", chrono::Local::now().format("%H:%M:%S"), ui::hms(line.at_secs));
            // The newest question wins over one still waiting.
            a.waiting = Some(Asking { question, at_secs: line.at_secs, before });
        }
        self.start_waiting_suggestion();
    }

    fn start_waiting_suggestion(&mut self) {
        let a = &mut self.answers;
        if a.rx.is_some() || a.last_start.is_some_and(|t| t.elapsed() < MIN_GAP) {
            return;
        }
        let Some(Asking { question, at_secs, before }) = a.waiting.take() else { return };
        let sources = a.sources;
        let (tx, rx) = crossbeam_channel::bounded(1);
        a.rx = Some(rx);
        let started = Instant::now();
        a.last_start = Some(started);
        std::thread::spawn(move || {
            let result = assist::suggest(&question, &before, sources).map_err(|e| format!("{e:#}"));
            let _ = tx.send((question, at_secs, started, result));
        });
    }

    /// Takes a finished lookup; returns whether anything changed.
    pub(super) fn poll_answers(&mut self) -> bool {
        let Some(rx) = &self.answers.rx else { return false };
        let (question, at_secs, started, result) = match rx.try_recv() {
            Ok(r) => r,
            Err(crossbeam_channel::TryRecvError::Empty) => return false,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.answers.rx = None;
                return false;
            }
        };
        self.answers.rx = None;
        let took = started.elapsed().as_secs_f32();
        let clock = chrono::Local::now().format("%H:%M:%S");
        match result {
            Ok(Some(s)) => {
                let usage = s.usage.map_or(String::new(), |u| format!(", {} tokens in, {} out", u.input, u.output));
                eprintln!("[mori] {clock} live answer ({took:.1}s, {} hits, {}{usage}): {}", s.hits, s.model, s.answer);
                let a = &mut self.answers;
                a.items.push(LiveAnswer { id: a.next_id, question, at_secs, answer: s.answer, dismissed: false, shown_at: Instant::now() });
                a.next_id += 1;
                self.talking_until = Some(Instant::now() + Duration::from_millis(2000));
                true
            }
            Ok(None) => {
                eprintln!("[mori] {clock} live answer ({took:.1}s): nothing to suggest for {question:?}");
                false
            }
            Err(e) => {
                eprintln!("[mori] {clock} live answer FAILED ({took:.1}s): {e}");
                false
            }
        }
    }

    /// Whether any suggestion is on screen (it keeps the stack up).
    pub(super) fn has_answers(&self) -> bool {
        self.answers_shown() > 0
    }

    /// Suggestions on screen now.
    pub(super) fn answers_shown(&self) -> usize {
        if self.open { 0 } else { self.answers.visible().len() }
    }

    /// The suggestion cards, oldest first.
    pub(super) fn render_answers(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.open {
            return Vec::new();
        }
        self.answers.visible().into_iter().map(|i| self.render_answer_card(i, cx)).collect()
    }

    fn render_answer_card(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let a = &self.answers.items[i];
        let id = a.id;
        let weak = cx.entity().downgrade();
        let body = crate::ask_view::cited_answer(&a.answer, &format!("mori-answer-{id}"), cx, move |target, _, cx| {
            weak.update(cx, |_, cx| match target {
                Target::Meeting { meeting_id, secs } => cx.emit(MoriEvent::OpenCitation { meeting_id, secs }),
                Target::Document { doc_id, loc } => cx.emit(MoriEvent::OpenDocument { doc_id, loc }),
                Target::Time { .. } => {}
            })
            .ok();
        });
        let copy = plain(&a.answer);
        let hover = cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered {
                this.answers.hovered = Some(id);
            } else if this.answers.hovered == Some(id) {
                this.answers.hovered = None;
            }
            cx.notify();
        });
        v_flex()
            .id(("mori-answer", id))
            .flex_shrink_0()
            .w_full()
            .px_2()
            .pt_1()
            .pb_1p5()
            .gap_1()
            .rounded(theme.radius_lg)
            .border(ui::border_width(cx))
            .border_color(theme.primary.opacity(0.6))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_sm()
            .block_mouse_except_scroll()
            .on_hover(hover)
            .child(
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .child(Icon::new(IconName::Lightbulb).xsmall().text_color(theme.primary))
                    .child(div().text_color(theme.primary).font_semibold().child(tr("Antwortvorschlag")))
                    .child(div().text_color(theme.muted_foreground).font_family(theme.mono_font_family.clone()).child(ui::hms(a.at_secs)))
                    .child(div().flex_1())
                    .child(
                        Button::new(("mori-answer-copy", id))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Copy)
                            .tooltip(tr("Antwort kopieren"))
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
                    )
                    .child(
                        Button::new(("mori-answer-dismiss", id))
                            .xsmall()
                            .ghost()
                            .icon(IconName::X)
                            .tooltip(tr("Ausblenden"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(a) = this.answers.items.iter_mut().find(|a| a.id == id) {
                                    a.dismissed = true;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(ui::bidi_text(format!("„{}“", a.question), true).text_xs().text_color(theme.muted_foreground))
            .child(body.text_sm())
            .into_any_element()
    }

    /// Shows a suggestion as if it had just arrived (screenshots, demo).
    #[doc(hidden)]
    pub fn demo_answer(&mut self, question: &str, at_secs: f64, answer: &str, cx: &mut Context<Self>) {
        let a = &mut self.answers;
        a.items.push(LiveAnswer { id: a.next_id, question: question.into(), at_secs, answer: answer.into(), dismissed: false, shown_at: Instant::now() });
        a.next_id += 1;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn plain_drops_markers() {
        assert_eq!(plain("It ships in May [[m:4@120]]. The VPN rotates [[d:2@3]]."), "It ships in May. The VPN rotates.");
    }
}
