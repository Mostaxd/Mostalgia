//! Mori's chat bubble: questions about the meeting being recorded (from
//! the live transcript) or about all meetings (the "Fragen" pipeline).

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::ask::{self, AskEvent};
use mostalgia_core::companion::{self, AnswerEvent};
use mostalgia_core::config::Config;
use mostalgia_core::connections;
use mostalgia_core::i18n::{tr, trf};

use mostalgia_core::ask::cite::Target;

use super::{ChatRx, Companion, MoriError, MoriEvent, Point2, Scope, Setup, Turn};
use crate::ui;

const BUBBLE_W: f32 = 400.;
const BUBBLE_MAX_H: f32 = 520.;
/// How long "Chat cleared · Undo" stays.
const UNDO_FOR: Duration = Duration::from_secs(6);

/// Scroll the live transcript to this moment (a time cited in a "This
/// meeting" answer). The app subscribes and forwards it to the recording view.
#[derive(Clone, Copy, Debug)]
pub struct ShowLiveMoment {
    pub secs: f64,
}

impl EventEmitter<ShowLiveMoment> for Companion {}

/// The bubble's own state.
#[derive(Default)]
pub(crate) struct BubbleState {
    /// The conversation just cleared with "New chat", restorable for a moment.
    undo: Option<Undo>,
    /// Show the "New chat" button (`Config::companion_clear_button`).
    clear_button: bool,
}

struct Undo {
    turns: Vec<Turn>,
    scope_choice: Option<Scope>,
    since: Instant,
}

impl Companion {
    pub(super) fn load_setup(&mut self) {
        let config = Config::load();
        self.bubble.clear_button = config.companion_clear_button;
        self.setup = Some(match config.ask_request() {
            Ok(req) => Setup {
                name: req.connection.name.clone(),
                model: req.model.clone(),
                local: req.connection.is_local().then(|| connections::local_label(req.connection.base())),
                problem: companion::setup_problem(&config),
            },
            Err(e) => Setup { name: String::new(), model: String::new(), local: None, problem: Some(format!("{e:#}")) },
        });
    }

    pub(super) fn toggle_bubble(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        self.ctl.choice = None;
        if self.open {
            self.load_setup();
            self.chat_scroll.scroll_to_bottom();
            self.invalidate_notes();
        }
        cx.notify();
    }

    /// Starts a fresh conversation (no earlier questions or answers, an
    /// answer still coming is dropped). Call when a recording starts.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.turns.clear();
        self.chat_rx = None;
        self.scope_choice = None;
        self.bubble.undo = None;
        self.chat_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Takes the finished "This meeting" questions and answers of the
    /// recording (question, answer with `[[t:…]]` citations) out of the
    /// bubble, to save with the meeting (`Store::add_companion_chat`).
    pub fn take_session_chat(&mut self) -> Vec<(String, String)> {
        let (session, rest): (Vec<Turn>, Vec<Turn>) = std::mem::take(&mut self.turns).into_iter().partition(|t| t.scope == Scope::Live);
        self.turns = rest;
        if self.bubble.undo.as_ref().is_some_and(|u| u.turns.iter().any(|t| t.scope == Scope::Live)) {
            self.bubble.undo = None;
        }
        session
            .into_iter()
            .filter(|t| t.done && t.error.is_none() && !t.answer.trim().is_empty())
            .map(|t| (t.question, t.answer))
            .collect()
    }

    /// [`Self::take_session_chat`] and saves it with `meeting_id`; a failure
    /// shows next to Mori.
    pub fn save_session_chat(&mut self, meeting_id: i64, cx: &mut Context<Self>) {
        let chat = self.take_session_chat();
        if !chat.is_empty() {
            let result = mostalgia_core::store::Store::open(&mostalgia_core::paths::database()).and_then(|mut s| s.add_companion_chat(meeting_id, &chat));
            if let Err(e) = result {
                self.error = Some(MoriError {
                    message: mostalgia_core::i18n::trf("Der Chat mit {} konnte nicht gespeichert werden: {}", &[&self.name(), &format!("{e:#}")]),
                    points: false,
                });
            }
        }
        cx.notify();
    }

    /// "New chat": clears the conversation, undoable for a few seconds.
    fn new_chat(&mut self, cx: &mut Context<Self>) {
        if self.turns.is_empty() {
            return;
        }
        self.chat_rx = None;
        let since = Instant::now();
        self.bubble.undo = Some(Undo { turns: std::mem::take(&mut self.turns), scope_choice: self.scope_choice, since });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_FOR).await;
            this.update(cx, |this, cx| {
                if this.bubble.undo.as_ref().is_some_and(|u| u.since == since) {
                    this.bubble.undo = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn undo_new_chat(&mut self, cx: &mut Context<Self>) {
        if let Some(undo) = self.bubble.undo.take() {
            // Anything asked in the meantime stays after the restored chat.
            let newer = std::mem::replace(&mut self.turns, undo.turns);
            self.turns.extend(newer);
            self.scope_choice = undo.scope_choice;
            self.chat_scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    /// The conversation a question goes to: the user's pick, else this
    /// meeting while recording and all meetings otherwise.
    pub(super) fn scope(&self, cx: &App) -> Scope {
        let recording = self.recording.read(cx).is_recording();
        let open = self.open_meeting.is_some();
        match self.scope_choice {
            Some(Scope::Live) if recording => Scope::Live,
            Some(Scope::Open) if open => Scope::Open,
            Some(Scope::Library) => Scope::Library,
            _ if recording => Scope::Live,
            _ if open => Scope::Open,
            _ => Scope::Library,
        }
    }

    /// The meeting open in the app changed (`None`: no meeting screen).
    pub fn set_open_meeting(&mut self, meeting: Option<(i64, String)>, cx: &mut Context<Self>) {
        if self.open_meeting != meeting {
            self.open_meeting = meeting;
            self.invalidate_notes();
            cx.notify();
        }
    }

    pub(super) fn ask(&mut self, question: String, cx: &mut Context<Self>) {
        let question = question.trim().to_string();
        if question.is_empty() || self.chat_rx.is_some() {
            return;
        }
        let scope = self.scope(cx);
        let meeting = (scope == Scope::Open).then(|| self.open_meeting.as_ref().map(|(id, _)| *id)).flatten();
        let history: Vec<(String, String)> = self
            .turns
            .iter()
            .filter(|t| t.scope == scope && t.meeting == meeting && t.done && t.error.is_none())
            .map(|t| (t.question.clone(), t.answer.clone()))
            .collect();
        self.turns.push(Turn {
            scope,
            meeting,
            question: question.clone(),
            answer: String::new(),
            status: Some(match scope {
                Scope::Live => trf("{} liest das Live-Transkript …", &[&self.name()]),
                Scope::Open => trf("{} liest die Besprechung …", &[&self.name()]),
                Scope::Library => tr("Durchsuche Besprechungen …").into(),
            }),
            error: None,
            done: false,
        });
        match scope {
            Scope::Live => {
                let lines = self.recording.read(cx).transcript_lines(0.);
                let transcript = companion::transcript_context(&lines, 200_000);
                let (tx, rx) = crossbeam_channel::unbounded();
                std::thread::spawn(move || companion::ask_live(&question, &history, &transcript, &tx));
                self.chat_rx = Some(ChatRx::Live(rx));
            }
            Scope::Open => {
                let id = meeting.unwrap_or_default();
                let (tx, rx) = crossbeam_channel::unbounded();
                std::thread::spawn(move || companion::ask_meeting(&question, &history, id, &tx));
                self.chat_rx = Some(ChatRx::Live(rx));
            }
            Scope::Library => {
                let (tx, rx) = crossbeam_channel::unbounded();
                std::thread::spawn(move || {
                    let _ = ask::index_pending();
                    ask::ask_as_mori(&question, &history, &tx);
                });
                self.chat_rx = Some(ChatRx::Library(rx));
            }
        }
        self.idle_since = Instant::now();
        self.bubble.undo = None;
        self.chat_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Takes streamed answer text; returns whether anything changed.
    pub(super) fn poll_chat(&mut self, _cx: &mut Context<Self>) -> bool {
        let Some(rx) = &self.chat_rx else { return false };
        let Some(turn) = self.turns.last_mut() else {
            self.chat_rx = None;
            return true;
        };
        let mut finished: Option<Result<(), String>> = None;
        let mut changed = false;
        match rx {
            ChatRx::Live(rx) => {
                for ev in rx.try_iter() {
                    changed = true;
                    match ev {
                        AnswerEvent::Delta(d) => {
                            turn.status = None;
                            turn.answer.push_str(&d);
                        }
                        AnswerEvent::Done(answer) => {
                            turn.answer = answer;
                            finished = Some(Ok(()));
                        }
                        AnswerEvent::Error(e) => finished = Some(Err(e)),
                    }
                }
            }
            ChatRx::Library(rx) => {
                for ev in rx.try_iter() {
                    changed = true;
                    match ev {
                        AskEvent::Status(s) => turn.status = Some(s),
                        AskEvent::TextDelta(t) => {
                            turn.status = None;
                            turn.answer.push_str(&t);
                        }
                        AskEvent::ClearText => turn.answer.clear(),
                        AskEvent::Done { answer, .. } => {
                            turn.answer = answer;
                            finished = Some(Ok(()));
                        }
                        AskEvent::Error(e) => finished = Some(Err(e)),
                    }
                }
            }
        }
        if let Some(result) = finished {
            turn.status = None;
            turn.done = true;
            self.chat_rx = None;
            match result {
                Ok(()) => {
                    self.error = None;
                    self.talking_until = Some(Instant::now() + Duration::from_secs(3));
                    // "This meeting" chats are kept with that meeting right away.
                    if let (Scope::Open, Some(id)) = (turn.scope, turn.meeting) {
                        let chat = [(turn.question.clone(), turn.answer.clone())];
                        match mostalgia_core::store::Store::open(&mostalgia_core::paths::database()).and_then(|mut s| s.add_companion_chat(id, &chat)) {
                            Ok(()) => _cx.emit(MoriEvent::NotesSaved { meeting_id: id }),
                            Err(e) => {
                                self.error = Some(MoriError {
                                    message: mostalgia_core::i18n::trf("Der Chat konnte nicht gespeichert werden: {}", &[&format!("{e:#}")]),
                                    points: false,
                                })
                            }
                        }
                    }
                }
                Err(e) => {
                    turn.error = Some(e.clone());
                    self.error = Some(MoriError { message: e, points: false });
                }
            }
        }
        if changed {
            self.chat_scroll.scroll_to_bottom();
        }
        changed
    }

    /// Shows a finished question and answer without asking (screenshots).
    #[doc(hidden)]
    pub fn demo_chat(&mut self, meeting: bool, question: &str, answer: &str, cx: &mut Context<Self>) {
        let scope = if meeting { Scope::Live } else { Scope::Library };
        self.scope_choice = Some(scope);
        self.turns.push(Turn {
            scope,
            meeting: None,
            question: question.into(),
            answer: answer.into(),
            status: None,
            error: None,
            done: true,
        });
        self.open = true;
        self.load_setup();
        self.chat_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn render_answer(&self, ti: usize, turn: &Turn, cx: &mut Context<Self>) -> Div {
        let weak = cx.entity().downgrade();
        let live = turn.scope == Scope::Live;
        let saved = turn.meeting;
        crate::ask_view::cited_answer(&turn.answer, &format!("mori-{ti}"), cx, move |target, _, cx| {
            weak.update(cx, |_, cx| match target {
                Target::Meeting { meeting_id, secs } => cx.emit(MoriEvent::OpenCitation { meeting_id, secs }),
                Target::Time { secs, .. } if live => cx.emit(ShowLiveMoment { secs }),
                Target::Time { secs, .. } => {
                    if let Some(meeting_id) = saved {
                        cx.emit(MoriEvent::OpenCitation { meeting_id, secs });
                    }
                }
            })
            .ok();
        })
        .text_sm()
    }

    /// This recording's focus hint for her notes: a chip that opens a field.
    fn render_focus_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let own = self.focus_input.read(cx).value().trim().to_string();
        let row = h_flex().gap_1p5().px_3().pb_2().text_xs();
        if self.focus_editing {
            return row
                .child(Icon::new(IconName::Target).xsmall().text_color(theme.primary))
                .child(Input::new(&self.focus_input).xsmall().flex_1())
                .child(Button::new("mori-focus-done").xsmall().ghost().icon(IconName::Check).tooltip(tr("Fertig")).on_click(cx.listener(|this, _, _, cx| {
                    this.focus_editing = false;
                    cx.notify();
                })))
                .into_any_element();
        }
        let (text, custom) = if !own.is_empty() {
            (own, true)
        } else if !self.default_focus.is_empty() {
            (self.default_focus.clone(), false)
        } else {
            (tr("nichts Bestimmtes").to_string(), false)
        };
        row.child(
            h_flex()
                .id("mori-focus")
                .min_w_0()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_full()
                .cursor_pointer()
                .bg(if custom { theme.primary.opacity(0.15) } else { theme.muted })
                .text_color(if custom { theme.primary } else { theme.muted_foreground })
                .hover(|s| s.bg(theme.primary.opacity(0.2)))
                .child(Icon::new(IconName::Target).xsmall())
                .child(div().min_w_0().truncate().child(trf("Fokus: {}", &[&text])))
                .tooltip({
                    let tip: SharedString = trf("Worauf {} bei den Live-Notizen besonders achtet – gilt für diese Aufnahme und wird mit ihr gespeichert. Den Standard legst du in den Einstellungen fest.", &[&self.name()]).into();
                    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_editing = true;
                    this.focus_input.update(cx, |s, cx| s.focus(window, cx));
                    cx.notify();
                })),
        )
        .child(self.render_style_chip(cx))
        .into_any_element()
    }

    /// This recording's note style: a chip that switches between the two
    /// (the default comes from the settings).
    fn render_style_chip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        use mostalgia_core::companion::NoteStyle;
        let theme = cx.theme().clone();
        let style = self.notes_style_now();
        let custom = self.style_override.is_some_and(|s| s != self.default_style);
        let tip: SharedString = trf("Notizstil für diese Aufnahme: {} – {} Klicken zum Wechseln; den Standard legst du in den Einstellungen fest.", &[&style.label(), &style.hint()]).into();
        h_flex()
            .id("mori-style")
            .flex_none()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_full()
            .cursor_pointer()
            .bg(if custom { theme.primary.opacity(0.15) } else { theme.muted })
            .text_color(if custom { theme.primary } else { theme.muted_foreground })
            .hover(|s| s.bg(theme.primary.opacity(0.2)))
            .child(Icon::new(IconName::StickyNote).xsmall())
            .child(style.label())
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| {
                let next = match this.notes_style_now() {
                    NoteStyle::Default => NoteStyle::Modern,
                    NoteStyle::Modern => NoteStyle::Default,
                };
                this.style_override = (next != this.default_style).then_some(next);
                cx.notify();
            }))
            .into_any_element()
    }

    /// The bubble shows the Notes tab.
    fn bubble_notes(&self) -> bool {
        self.ctl.notes_tab
    }

    pub(super) fn render_bubble(&mut self, o: Point2, (w, h): (f32, f32), cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.open {
            return None;
        }
        if self.setup.is_none() {
            self.load_setup();
        }
        let setup = self.setup.clone()?;
        let theme = cx.theme().clone();
        let (vw, vh) = self.viewport;
        let width = BUBBLE_W.min(vw - 16.);
        // Beside her, on the side with room; over her on very narrow windows.
        let right_room = vw - (o.x + w);
        let left = if o.x >= width + 12. && (o.x + w / 2. > vw / 2. || right_room < width + 12.) {
            o.x - width - 8.
        } else if right_room >= width + 12. {
            o.x + w + 8.
        } else {
            (vw - width - 8.).max(8.)
        };
        let max_h = BUBBLE_MAX_H.min(vh - 56.);
        let top = (o.y + h * 0.1).min(vh - max_h - 8.).max(48.);
        let recording = self.recording.read(cx).is_recording();
        let scope = self.scope(cx);
        let busy = self.chat_rx.is_some();
        let notes_tab = self.bubble_notes();
        let record_controls = self.render_record_controls(super::controls::ChoiceIn::Bubble, cx);

        let header = h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(div().font_semibold().child(self.name()))
            .when(!setup.name.is_empty(), |el| {
                let (color, label) = match setup.local {
                    Some(l) => (theme.success, l.split(" · ").next().unwrap_or(l).to_string()),
                    None => (theme.warning, tr("Cloud").to_string()),
                };
                let which = format!("{} · {}", setup.name, setup.model);
                let tip: SharedString = match setup.local {
                    Some(l) => format!("{which}\n{l}").into(),
                    None => format!("{which}\n{}", mostalgia_core::i18n::trf("Fragen und Transkripttext gehen an {}", &[&setup.name])).into(),
                };
                el.child(
                    div()
                        .id("mori-privacy")
                        .flex_none()
                        .px_2()
                        .rounded_full()
                        .bg(color.opacity(0.15))
                        .text_color(color)
                        .text_xs()
                        .font_medium()
                        .child(label)
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)),
                )
            })
            .child(div().flex_1())
            .child(record_controls)
            .when(self.bubble.clear_button && !self.turns.is_empty() && !self.bubble_notes(), |el| {
                el.child(
                    Button::new("mori-new-chat")
                        .xsmall()
                        .ghost()
                        .icon(IconName::Eraser)
                        // (Icon only next to the recording controls.)
                        .when(!recording, |b| b.label(tr("Neuer Chat")))
                        .tooltip(tr("Leert diese Unterhaltung (rückgängig machbar)"))
                        .on_click(cx.listener(|this, _, _, cx| this.new_chat(cx))),
                )
            })
            .child(Button::new("mori-close").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                this.open = false;
                cx.notify();
            })));

        let open_title = self.open_meeting.as_ref().map(|(_, t)| t.clone());
        let scope_button = |id: &'static str, label: &'static str, tip: String, this_scope: Scope, enabled: bool, cx: &mut Context<Self>| {
            let on = scope == this_scope && !notes_tab;
            Button::new(id)
                .xsmall()
                .label(tr(label))
                .selected(on)
                .when(on, |b| b.primary())
                .when(!on, |b| b.ghost())
                .disabled(!enabled)
                .tooltip(tip)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.scope_choice = Some(this_scope);
                    this.ctl.notes_tab = false;
                    this.chat_scroll.scroll_to_bottom();
                    cx.notify();
                }))
        };
        let notes_count = self.notes_count(cx);
        let scope_toggle = h_flex()
            .gap_1()
            .px_3()
            .py_2()
            .flex_wrap()
            .when(recording, |el| {
                el.child(scope_button("mori-scope-live", "Live-Besprechung", tr("Fragt das Live-Transkript der laufenden Aufnahme").to_string(), Scope::Live, true, cx))
            })
            .when_some(open_title.clone(), |el, title| {
                el.child(scope_button("mori-scope-open", "Diese Besprechung", trf("Fragt die geöffnete Besprechung: {}", &[&title]), Scope::Open, true, cx))
            })
            .child(scope_button("mori-scope-library", "Alle Besprechungen", tr("Durchsucht alle gespeicherten Besprechungen, mit Quellen").to_string(), Scope::Library, true, cx))
            .child(
                Button::new("mori-tab-notes")
                    .xsmall()
                    .icon(IconName::StickyNote)
                    .label(if notes_count > 0 { format!("{} · {notes_count}", tr("Notizen")) } else { tr("Notizen").to_string() })
                    .selected(notes_tab)
                    .when(notes_tab, |b| b.primary())
                    .when(!notes_tab, |b| b.ghost())
                    .tooltip(tr("Die Notizen der laufenden Aufnahme oder der geöffneten Besprechung"))
                    .on_click(cx.listener(|this, _, _, cx| this.show_notes_tab(cx))),
            )
            .when(scope == Scope::Open && !notes_tab, |el| {
                el.when_some(open_title, |el, title| {
                    el.child(div().w_full().text_xs().text_color(theme.muted_foreground).truncate().child(title))
                })
            });

        let focus_row = (recording && self.notes_mode != mostalgia_core::config::NotesMode::Off).then(|| self.render_focus_row(cx));

        let mut body: Vec<AnyElement> = Vec::new();
        let rec_error = self.ctl.error.clone().map(|e| {
            h_flex()
                .items_start()
                .gap_2()
                .mx_3()
                .mt_2()
                .p_2()
                .rounded(theme.radius)
                .bg(theme.danger.opacity(0.1))
                .text_color(theme.danger)
                .text_xs()
                .child(Icon::new(IconName::TriangleAlert).xsmall().mt_0p5())
                .child(v_flex().flex_1().min_w_0().child(div().font_semibold().child(tr("Die Aufnahme konnte nicht starten"))).child(e))
                .child(Button::new("mori-rec-error-close").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                    this.ctl.error = None;
                    cx.notify();
                })))
        });
        if notes_tab {
        } else if let Some(problem) = &setup.problem {
            body.push(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.warning.opacity(0.12))
                    .text_sm()
                    .child(div().font_medium().child(tr("Richte in Einstellungen → KI & Transkription eine Verbindung ein.")))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(problem.clone()))
                    .child(
                        Button::new("mori-open-settings")
                            .small()
                            .primary()
                            .icon(IconName::Settings)
                            .label(tr("Einstellungen öffnen"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(MoriEvent::OpenSettings))),
                    )
                    .into_any_element(),
            );
        } else if self.turns.is_empty() {
            if self.bubble.undo.as_ref().is_some_and(|u| u.since.elapsed() < UNDO_FOR) {
                body.push(
                    h_flex()
                        .gap_2()
                        .px_2p5()
                        .py_1p5()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(div().flex_1().child(tr("Chat geleert")))
                        .child(
                            Button::new("mori-undo-clear")
                                .xsmall()
                                .ghost()
                                .icon(IconName::Undo2)
                                .label(tr("Rückgängig"))
                                .on_click(cx.listener(|this, _, _, cx| this.undo_new_chat(cx))),
                        )
                        .into_any_element(),
                );
            }
            body.push(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(match scope {
                        Scope::Live => tr("Frag mich zu dieser Besprechung, z. B. „Was wurde gerade entschieden?“ Ich lese das Live-Transkript mit."),
                        Scope::Open => tr("Frag mich zur geöffneten Besprechung – ich kenne Transkript, Zusammenfassung und Notizen und zeige dir die Stellen."),
                        Scope::Library => tr("Frag mich zu deinen gespeicherten Besprechungen. Ich suche passende Stellen und zeige die Quellen."),
                    })
                    .into_any_element(),
            );
        }
        for (ti, turn) in self.turns.iter().enumerate().filter(|_| !notes_tab) {
            body.push(
                h_flex()
                    .w_full()
                    .justify_end()
                    .child(
                        div()
                            .max_w(px(width * 0.8))
                            .px_2p5()
                            .py_1p5()
                            .rounded(theme.radius_lg)
                            .bg(theme.primary.opacity(0.12))
                            .text_sm()
                            .child(ui::bidi_text(turn.question.clone(), true)),
                    )
                    .into_any_element(),
            );
            let label = match turn.scope {
                Scope::Live => tr("Live-Besprechung"),
                Scope::Open => tr("Diese Besprechung"),
                Scope::Library => tr("Alle Besprechungen"),
            };
            body.push(
                v_flex()
                    .gap_1()
                    .w_full()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(label))
                    .child(self.render_answer(ti, turn, cx))
                    .when_some(turn.status.clone(), |el, s| {
                        el.child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(gpui_kit::component::spinner::Spinner::new().small())
                                .child(s),
                        )
                    })
                    .when_some(turn.error.clone(), |el, e| {
                        el.child(div().p_2().rounded(theme.radius).bg(theme.danger.opacity(0.1)).text_color(theme.danger).text_xs().child(e))
                    })
                    .into_any_element(),
            );
        }

        let notes = if notes_tab { self.render_notes_tab(cx) } else { div().into_any_element() };
        let choice = self.render_choice_popup(cx);
        let input_row = h_flex()
            .gap_2()
            .p_3()
            .border_t_1()
            .border_color(theme.border)
            .child(Input::new(&self.input).small().flex_1().disabled(setup.problem.is_some()))
            .child(
                Button::new("mori-send")
                    .small()
                    .primary()
                    .icon(IconName::ArrowUp)
                    .loading(busy)
                    .disabled(setup.problem.is_some())
                    .on_click(cx.listener(|this, _, window, cx| {
                        let q = this.input.read(cx).value().to_string();
                        this.input.update(cx, |s, cx| s.set_value("", window, cx));
                        this.ask(q, cx);
                    })),
            );

        Some(
            v_flex()
                .id("mori-bubble")
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .max_h(px(max_h))
                .rounded(theme.radius_lg)
                .border(ui::border_width(cx))
                .border_color(theme.border)
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(header)
                .child(scope_toggle)
                .children(focus_row)
                .children(rec_error)
                .child(div().flex_1().min_h_0().child(if notes_tab {
                    notes
                } else {
                    v_flex()
                        .id("mori-chat")
                        .size_full()
                        .px_3()
                        .py_2()
                        .gap_3()
                        .track_scroll(&self.chat_scroll)
                        .overflow_y_scroll()
                        .children(body)
                        .into_any_element()
                }))
                .when(!notes_tab, |el| el.child(input_row))
                .children(choice)
                .into_any_element(),
        )
    }
}
