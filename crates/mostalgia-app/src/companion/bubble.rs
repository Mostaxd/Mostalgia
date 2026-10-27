//! Mori's chat bubble: questions about the meeting being recorded (from
//! the live transcript) or about all meetings (the "Fragen" pipeline).

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::ask::{self, AskEvent};
use mostalgia_core::companion::{self, AnswerEvent};
use mostalgia_core::config::Config;
use mostalgia_core::connections;
use mostalgia_core::i18n::tr;

use super::{ChatRx, Companion, MoriError, MoriEvent, Point2, Scope, Setup, Turn};
use crate::ui;

const BUBBLE_W: f32 = 380.;
const BUBBLE_MAX_H: f32 = 500.;

impl Companion {
    pub(super) fn load_setup(&mut self) {
        let config = Config::load();
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
        if self.open {
            self.load_setup();
            self.chat_scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// The conversation a question goes to: the user's pick, else this
    /// meeting while recording and all meetings otherwise.
    pub(super) fn scope(&self, cx: &App) -> Scope {
        let recording = self.recording.read(cx).is_recording();
        match self.scope_choice {
            Some(Scope::Meeting) if recording => Scope::Meeting,
            Some(Scope::Library) => Scope::Library,
            _ if recording => Scope::Meeting,
            _ => Scope::Library,
        }
    }

    pub(super) fn ask(&mut self, question: String, cx: &mut Context<Self>) {
        let question = question.trim().to_string();
        if question.is_empty() || self.chat_rx.is_some() {
            return;
        }
        let scope = self.scope(cx);
        let history: Vec<(String, String)> = self
            .turns
            .iter()
            .filter(|t| t.scope == scope && t.done && t.error.is_none())
            .map(|t| (t.question.clone(), t.answer.clone()))
            .collect();
        self.turns.push(Turn {
            scope,
            question: question.clone(),
            answer: String::new(),
            status: Some(match scope {
                Scope::Meeting => tr("Mori liest das Live-Transkript …").into(),
                Scope::Library => tr("Durchsuche Besprechungen …").into(),
            }),
            error: None,
            done: false,
        });
        match scope {
            Scope::Meeting => {
                let lines = self.recording.read(cx).transcript_lines(0.);
                let transcript = companion::transcript_context(&lines, 200_000);
                let (tx, rx) = crossbeam_channel::unbounded();
                std::thread::spawn(move || companion::ask_live(&question, &history, &transcript, &tx));
                self.chat_rx = Some(ChatRx::Live(rx));
            }
            Scope::Library => {
                let (tx, rx) = crossbeam_channel::unbounded();
                std::thread::spawn(move || {
                    let _ = ask::index_pending();
                    ask::ask(&question, &history, &tx);
                });
                self.chat_rx = Some(ChatRx::Library(rx));
            }
        }
        self.idle_since = Instant::now();
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
        let scope = if meeting { Scope::Meeting } else { Scope::Library };
        self.scope_choice = Some(scope);
        self.turns.push(Turn {
            scope,
            question: question.into(),
            answer: answer.into(),
            status: None,
            error: None,
            done: true,
        });
        self.open = true;
        self.load_setup();
        cx.notify();
    }

    fn render_answer(&self, ti: usize, turn: &Turn, cx: &mut Context<Self>) -> Div {
        let mut out = v_flex().gap_1p5().w_full().text_sm();
        for (li, line) in turn.answer.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            out = out.child(match turn.scope {
                Scope::Library => {
                    let weak = cx.entity().downgrade();
                    crate::ask_view::answer_line(line, &format!("mori-{ti}-{li}"), cx, move |meeting_id, secs, _, cx| {
                        weak.update(cx, |_, cx| cx.emit(MoriEvent::OpenCitation { meeting_id, secs })).ok();
                    })
                }
                Scope::Meeting => {
                    let text = line.trim().replace("**", "");
                    match text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
                        Some(item) => h_flex()
                            .w_full()
                            .items_start()
                            .gap_2()
                            .child(div().flex_none().mt(px(8.)).size(px(4.)).rounded_full().bg(cx.theme().muted_foreground))
                            .child(ui::bidi_text(item.to_string(), true).flex_1().min_w_0())
                            .into_any_element(),
                        None => ui::bidi_text(text, true).w_full().into_any_element(),
                    }
                }
            });
        }
        out
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

        let header = h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(div().font_semibold().child("Mori"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when(!setup.name.is_empty(), |el| el.child(format!("· {} · {}", setup.name, setup.model))),
            )
            .when(!setup.name.is_empty(), |el| {
                let (color, label) = match setup.local {
                    Some(l) => (theme.success, l.split(" · ").next().unwrap_or(l).to_string()),
                    None => (theme.warning, tr("Cloud").to_string()),
                };
                el.child(
                    div()
                        .flex_none()
                        .px_2()
                        .rounded_full()
                        .bg(color.opacity(0.15))
                        .text_color(color)
                        .text_xs()
                        .font_medium()
                        .child(label)
                        .id("mori-privacy")
                        .tooltip({
                            let tip: SharedString = match setup.local {
                                Some(l) => l.into(),
                                None => mostalgia_core::i18n::trf("Fragen und Transkripttext gehen an {}", &[&setup.name]).into(),
                            };
                            move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        }),
                )
            })
            .child(Button::new("mori-close").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                this.open = false;
                cx.notify();
            })));

        let scope_toggle = h_flex()
            .gap_1()
            .px_3()
            .pt_2()
            .child(
                Button::new("mori-scope-meeting")
                    .xsmall()
                    .label(tr("Diese Besprechung"))
                    .selected(scope == Scope::Meeting)
                    .when(scope == Scope::Meeting, |b| b.primary())
                    .when(scope != Scope::Meeting, |b| b.ghost())
                    .disabled(!recording)
                    .tooltip(if recording { tr("Fragt das Live-Transkript der laufenden Aufnahme") } else { tr("Nur während einer Aufnahme") })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scope_choice = Some(Scope::Meeting);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("mori-scope-library")
                    .xsmall()
                    .label(tr("Alle Besprechungen"))
                    .selected(scope == Scope::Library)
                    .when(scope == Scope::Library, |b| b.primary())
                    .when(scope != Scope::Library, |b| b.ghost())
                    .tooltip(tr("Durchsucht alle gespeicherten Besprechungen, mit Quellen"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scope_choice = Some(Scope::Library);
                        cx.notify();
                    })),
            );

        let mut body: Vec<AnyElement> = Vec::new();
        if let Some(problem) = &setup.problem {
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
            body.push(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(match scope {
                        Scope::Meeting => tr("Frag mich zu dieser Besprechung, z. B. „Was wurde gerade entschieden?“ Ich lese das Live-Transkript mit."),
                        Scope::Library => tr("Frag mich zu deinen gespeicherten Besprechungen. Ich suche passende Stellen und zeige die Quellen."),
                    })
                    .into_any_element(),
            );
        }
        for (ti, turn) in self.turns.iter().enumerate() {
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
                Scope::Meeting => tr("Diese Besprechung"),
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
                .child(
                    div().flex_1().min_h_0().child(
                        v_flex()
                            .id("mori-chat")
                            .size_full()
                            .px_3()
                            .py_2()
                            .gap_3()
                            .track_scroll(&self.chat_scroll)
                            .overflow_y_scroll()
                            .children(body),
                    ),
                )
                .child(input_row)
                .into_any_element(),
        )
    }
}
