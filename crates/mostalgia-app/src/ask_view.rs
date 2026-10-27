//! "Fragen": ask questions about all meetings; answers stream in with
//! clickable sources that open the meeting at that moment.

use std::time::Duration;

use crossbeam_channel::Receiver;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::ask::{self, AskEvent, Piece};
use mostalgia_core::config::Config;
use mostalgia_core::i18n::{self, tr, trf};
use mostalgia_core::summary;

use crate::ui;

pub enum AskViewEvent {
    /// Open a meeting and play from this moment.
    OpenCitation { meeting_id: i64, secs: f64 },
}

impl EventEmitter<AskViewEvent> for AskView {}

struct Turn {
    question: String,
    /// Answer so far (with [[m:ID@SECS]] markers).
    answer: String,
    status: Option<String>,
    error: Option<String>,
    done: bool,
}

pub struct AskView {
    turns: Vec<Turn>,
    input: Entity<InputState>,
    rx: Option<Receiver<AskEvent>>,
    scroll: ScrollHandle,
    /// Language the input placeholder was last set in.
    lang_seen: Option<i18n::Lang>,
    _subscriptions: Vec<Subscription>,
}

const SUGGESTIONS: [&str; 4] = [
    "Was wurde in der letzten Besprechung entschieden?",
    "Welche Aufgaben sind noch offen und wer ist verantwortlich?",
    "Was wurde diese Woche über das Budget gesagt?",
    "Fasse meine Besprechungen von heute zusammen.",
];

impl AskView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Frage zu deinen Besprechungen …")));
        let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let q = input.read(cx).value().to_string();
                input.update(cx, |s, cx| s.set_value("", window, cx));
                this.ask(q, cx);
            }
        });
        Self {
            turns: Vec::new(),
            input,
            rx: None,
            scroll: ScrollHandle::new(),
            lang_seen: None,
            _subscriptions: vec![sub],
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    /// Shows a finished question/answer without asking (screenshots).
    #[doc(hidden)]
    pub fn demo(&mut self, question: &str, answer: &str, cx: &mut Context<Self>) {
        self.turns.push(Turn {
            question: question.into(),
            answer: answer.into(),
            status: None,
            error: None,
            done: true,
        });
        cx.notify();
    }

    fn busy(&self) -> bool {
        self.rx.is_some()
    }

    pub fn ask(&mut self, question: String, cx: &mut Context<Self>) {
        let question = question.trim().to_string();
        if question.is_empty() || self.busy() {
            return;
        }
        let history: Vec<(String, String)> = self
            .turns
            .iter()
            .filter(|t| t.done && t.error.is_none())
            .map(|t| (t.question.clone(), t.answer.clone()))
            .collect();
        self.turns.push(Turn {
            question: question.clone(),
            answer: String::new(),
            status: Some(tr("Durchsuche Besprechungen …").into()),
            error: None,
            done: false,
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            // Make sure recent meetings are searchable (cheap if nothing changed).
            let _ = ask::index_pending();
            ask::ask(&question, &history, &tx);
        });
        self.scroll.scroll_to_bottom();
        cx.notify();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                let keep = this.update(cx, |this, cx| this.poll(cx)).unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(rx) = &self.rx else { return false };
        let Some(turn) = self.turns.last_mut() else { return false };
        let mut finished = false;
        let mut changed = false;
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
                    turn.status = None;
                    turn.done = true;
                    finished = true;
                }
                AskEvent::Error(e) => {
                    turn.error = Some(e);
                    turn.status = None;
                    turn.done = true;
                    finished = true;
                }
            }
        }
        if finished {
            self.rx = None;
        }
        if changed {
            self.scroll.scroll_to_bottom();
            cx.notify();
        }
        !finished
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.busy() {
            self.turns.clear();
            cx.notify();
        }
    }

    /// One answer line: words and citation chips in a wrapping flow
    /// (right-to-left for Arabic).
    fn render_line(&self, line: &str, key: (usize, usize), cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut line = line.trim_end().replace("**", "");
        let mut bullet = false;
        if let Some(rest) = line.trim_start().strip_prefix("- ").or_else(|| line.trim_start().strip_prefix("* ")) {
            line = rest.to_string();
            bullet = true;
        }
        let heading = line.starts_with('#');
        let line = line.trim_start_matches('#').trim().to_string();
        let rtl = ui::is_rtl(&line);
        let mut items: Vec<AnyElement> = Vec::new();
        for (i, piece) in ask::parse_answer(&line).into_iter().enumerate() {
            match piece {
                Piece::Text(t) => {
                    for w in t.split_whitespace() {
                        items.push(div().flex_none().child(w.to_string()).into_any_element());
                    }
                }
                Piece::Cite(c) => {
                    let (id, secs) = (c.meeting_id, c.secs);
                    // "Titel · dd.mm. · hh:mm:ss" with the title shortened.
                    let parts: Vec<&str> = c.label.split(" · ").collect();
                    let title = parts.first().copied().unwrap_or_default();
                    let short = if title.chars().count() > 22 {
                        format!("{}…", title.chars().take(21).collect::<String>().trim_end())
                    } else {
                        title.to_string()
                    };
                    let chip = std::iter::once(short).chain(parts.iter().skip(1).map(|p| p.to_string())).collect::<Vec<_>>().join(" · ");
                    items.push(
                        h_flex()
                            .id(SharedString::from(format!("cite-{}-{}-{i}", key.0, key.1)))
                            .flex_none()
                            .gap_1()
                            .px_1p5()
                            .rounded(theme.radius)
                            .bg(theme.primary.opacity(0.12))
                            .text_color(theme.primary)
                            .text_xs()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.primary.opacity(0.22)))
                            .child(Icon::new(IconName::Play).xsmall())
                            .child(chip)
                            .tooltip({
                                let full: SharedString = c.label.clone().into();
                                move |window, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(AskViewEvent::OpenCitation { meeting_id: id, secs })))
                            .into_any_element(),
                    );
                }
            }
        }
        h_flex()
            .w_full()
            .items_start()
            .gap_2()
            .when(rtl, |el| el.flex_row_reverse())
            .when(bullet, |el| el.child(div().flex_none().mt(px(9.)).size(px(5.)).rounded_full().bg(theme.muted_foreground)))
            .child(
                h_flex()
                    .flex_1()
                    .flex_wrap()
                    .items_center()
                    .gap_x(px(4.))
                    .gap_y(px(4.))
                    .when(rtl, |el| el.flex_row_reverse())
                    .when(heading, |el| el.font_semibold())
                    .children(items),
            )
            .into_any_element()
    }
}

impl Render for AskView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.lang_seen != Some(i18n::lang()) {
            self.lang_seen = Some(i18n::lang());
            self.input.update(cx, |s, cx| s.set_placeholder(tr("Frage zu deinen Besprechungen …"), window, cx));
        }
        let theme = cx.theme().clone();
        let config = Config::load();
        let provider = config.summary_provider;
        let has_key = matches!(summary::api_key(provider), Ok(Some(_)));

        let mut body: Vec<AnyElement> = Vec::new();
        if self.turns.is_empty() {
            body.push(
                v_flex()
                    .pt_16()
                    .items_center()
                    .gap_3()
                    .child(Icon::new(IconName::Sparkles).large().text_color(theme.primary))
                    .child(div().text_xl().font_semibold().child(tr("Frag deine Besprechungen")))
                    .child(
                        div()
                            .max_w(px(520.))
                            .text_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(tr("Die Suche läuft lokal über alle Transkripte und Zusammenfassungen. An das Sprachmodell gehen nur die gefundenen Abschnitte, nie Audio. Jede Aussage verweist auf die Stelle in der Aufnahme.")),
                    )
                    .child(
                        v_flex().pt_2().gap_2().children(SUGGESTIONS.iter().enumerate().map(|(i, s)| {
                            let q = tr(s).to_string();
                            div()
                                .id(("suggest", i))
                                .px_3()
                                .py_1p5()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .text_sm()
                                .cursor_pointer()
                                .hover(|st| st.bg(theme.muted))
                                .child(tr(s))
                                .on_click(cx.listener(move |this, _, _, cx| this.ask(q.clone(), cx)))
                        })),
                    )
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
                            .max_w(px(560.))
                            .px_3()
                            .py_2()
                            .rounded(theme.radius_lg)
                            .bg(theme.primary.opacity(0.12))
                            .child(ui::bidi_text(turn.question.clone(), true)),
                    )
                    .into_any_element(),
            );
            let mut answer = v_flex().gap_2().w_full();
            for (li, line) in turn.answer.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                answer = answer.child(self.render_line(line, (ti, li), cx));
            }
            body.push(
                v_flex()
                    .gap_2()
                    .w_full()
                    .child(answer)
                    .when_some(turn.status.clone(), |el, s| {
                        el.child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(gpui_kit::component::spinner::Spinner::new().small())
                                .child(s),
                        )
                    })
                    .when_some(turn.error.clone(), |el, e| {
                        el.child(
                            div()
                                .p_3()
                                .rounded(theme.radius)
                                .bg(theme.danger.opacity(0.1))
                                .text_color(theme.danger)
                                .text_sm()
                                .child(e),
                        )
                    })
                    .into_any_element(),
            );
        }

        let model = config.summary_model.clone();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_8()
                    .py_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_xl().font_semibold().child(tr("Fragen")))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} · {model}", provider.name())))
                    .child(div().flex_1())
                    .when(!self.turns.is_empty(), |el| {
                        el.child(
                            Button::new("ask-clear")
                                .small()
                                .ghost()
                                .icon(IconName::Plus)
                                .label(tr("Neue Unterhaltung"))
                                .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                        )
                    }),
            )
            .child(
                div().flex_1().min_h_0().child(
                    div()
                        .id("ask-body")
                        .size_full()
                        .track_scroll(&self.scroll)
                        .overflow_y_scroll()
                        .child(v_flex().max_w(px(860.)).px_8().py_6().gap_5().children(body)),
                ),
            )
            .child(
                v_flex()
                    .px_8()
                    .py_3()
                    .gap_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .max_w(px(860.))
                            .child(Input::new(&self.input).flex_1())
                            .child(
                                Button::new("ask-send")
                                    .primary()
                                    .icon(IconName::ArrowDown)
                                    .label(tr("Fragen"))
                                    .loading(self.busy())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let q = this.input.read(cx).value().to_string();
                                        this.input.update(cx, |s, cx| s.set_value("", window, cx));
                                        this.ask(q, cx);
                                    })),
                            ),
                    )
                    .when(!has_key, |el| {
                        el.child(div().text_xs().text_color(theme.warning).child(trf(
                            "Kein {}-API-Schlüssel — in Einstellungen → KI & Zusammenfassung hinterlegen.",
                            &[&provider.name()],
                        )))
                    }),
            )
    }
}
