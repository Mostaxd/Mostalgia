//! "Fragen": ask questions about all meetings; answers stream in with
//! clickable sources that open the meeting at that moment. A question can
//! also be about one collection of documents or one document only (picked
//! next to the input; coming from "Dokumente", the selected one comes first).

use std::time::Duration;

use crossbeam_channel::Receiver;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::ask::cite::{self, Mark, Target};
use mostalgia_core::ask::{self, AskEvent};
use mostalgia_core::paths;
use mostalgia_core::store::Store;
use mostalgia_core::config::Config;
use mostalgia_core::docs::{self, Within};
use mostalgia_core::i18n::{self, tr, trf};

use crate::ui;

pub enum AskViewEvent {
    /// Open a meeting and play from this moment.
    OpenCitation { meeting_id: i64, secs: f64 },
    /// Open a document at a page or section.
    OpenDocument { doc_id: i64, loc: u32 },
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
    /// What questions are about (`None`: all meetings and documents).
    within: Option<Within>,
    /// Scopes offered first (from the selection in "Dokumente").
    offers: Vec<Within>,
    /// The document collections (read when the screen opens).
    collections: Vec<docs::Collection>,
    /// Language and scope the input placeholder was last set for.
    lang_seen: Option<(i18n::Lang, Option<Within>)>,
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
            within: None,
            offers: Vec::new(),
            collections: docs::collections().unwrap_or_default(),
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

    /// Offers these scopes first and picks the first one (or, with none,
    /// all meetings and documents); a conversation keeps its scope.
    pub fn offer(&mut self, offers: Vec<Within>, cx: &mut Context<Self>) {
        if self.turns.is_empty() {
            self.within = offers.first().copied();
        }
        self.offers = offers;
        self.collections = docs::collections().unwrap_or_default();
        if let Some(Within::Collection(id)) = self.within
            && !self.collections.iter().any(|k| k.id == id)
        {
            self.within = None;
        }
        cx.notify();
    }

    /// Asks about these documents from now on.
    pub fn set_within(&mut self, within: Option<Within>, cx: &mut Context<Self>) {
        self.within = within;
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
            status: Some(if self.within.is_some() { tr("Durchsuche Dokumente …") } else { tr("Durchsuche Besprechungen …") }.into()),
            error: None,
            done: false,
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        self.rx = Some(rx);
        let within = self.within;
        std::thread::spawn(move || {
            // Make sure recent meetings are searchable (cheap if nothing changed).
            if within.is_none() {
                let _ = ask::index_pending();
            }
            ask::ask_within(&question, &history, within, &tx);
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

    fn render_answer(&self, answer: &str, ti: usize, cx: &mut Context<Self>) -> Div {
        let weak = cx.entity().downgrade();
        cited_answer(answer, &format!("ask-{ti}"), cx, move |target, _, cx| {
            match target {
                Target::Meeting { meeting_id, secs } => {
                    weak.update(cx, |_, cx| cx.emit(AskViewEvent::OpenCitation { meeting_id, secs })).ok();
                }
                Target::Document { doc_id, loc } => {
                    weak.update(cx, |_, cx| cx.emit(AskViewEvent::OpenDocument { doc_id, loc })).ok();
                }
                Target::Time { .. } => {}
            }
        })
    }
}

/// "Sammlung „Projekt X“" / "Dokument „Spec“" / everything.
fn scope_label(within: Option<Within>, collections: &[docs::Collection]) -> String {
    match within {
        Some(Within::Collection(id)) => {
            let name = collections.iter().find(|k| k.id == id).map_or("", |k| k.name.as_str());
            trf("Sammlung „{}“", &[&name])
        }
        Some(Within::Document(id)) => trf("Dokument „{}“", &[&document_label(id).0]),
        _ => tr("Alle Besprechungen und Dokumente").to_string(),
    }
}

fn scope_icon(within: Option<Within>) -> IconName {
    match within {
        Some(Within::Collection(_)) => IconName::Folder,
        Some(Within::Document(_)) => IconName::FileText,
        _ => IconName::Library,
    }
}

const DOC_SUGGESTIONS: [&str; 3] = [
    "Fasse die wichtigsten Punkte zusammen.",
    "Welche offenen Fragen oder Risiken werden genannt?",
    "Erkläre den Aufbau in einfachen Worten.",
];

impl AskView {
    /// The scope picker next to the input.
    fn scope_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut choices: Vec<Option<Within>> = self.offers.iter().copied().map(Some).collect();
        choices.push(None);
        for k in &self.collections {
            let w = Some(Within::Collection(k.id));
            if !choices.contains(&w) {
                choices.push(w);
            }
        }
        let offered = self.offers.len();
        let choices: Vec<(Option<Within>, String)> = choices.into_iter().map(|w| (w, scope_label(w, &self.collections))).collect();
        let current = self.within;
        let weak = cx.entity().downgrade();
        Button::new("ask-scope")
            .outline()
            .icon(scope_icon(current))
            .label(shorten(&scope_label(current, &self.collections), 34))
            .tooltip(tr("Worauf sich die Frage bezieht"))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.));
                for (i, (w, label)) in choices.iter().enumerate() {
                    if i == offered && offered > 0 {
                        menu = menu.separator();
                    }
                    let (w, weak) = (*w, weak.clone());
                    menu = menu.item(PopupMenuItem::new(label.clone()).icon(scope_icon(w)).checked(w == current).on_click(move |_, _, cx| {
                        weak.update(cx, |this, cx| this.set_within(w, cx)).ok();
                    }));
                }
                menu
            })
    }
}

/// Title and short date of a meeting cited in an answer, cached for a
/// while so rendering doesn't hit the database every frame.
fn meeting_label(id: i64) -> (String, String) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<HashMap<i64, (String, String, Instant)>>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((t, d, at)) = cache.get(&id) {
        if at.elapsed() < Duration::from_secs(30) {
            return (t.clone(), d.clone());
        }
    }
    let found = Store::open(&paths::database()).ok().and_then(|s| s.title(id).ok().flatten());
    let (title, date) = match found {
        Some((t, start)) => (t, i18n::short_date(&start)),
        None => (trf("Besprechung #{}", &[&id]), String::new()),
    };
    cache.insert(id, (title.clone(), date.clone(), Instant::now()));
    (title, date)
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        format!("{}…", text.chars().take(max - 1).collect::<String>().trim_end())
    } else {
        text.to_string()
    }
}

/// Title and kind of a document cited in an answer, cached like
/// [`meeting_label`].
fn document_label(id: i64) -> (String, Option<mostalgia_core::docs::Kind>) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<HashMap<i64, (String, Option<mostalgia_core::docs::Kind>, Instant)>>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((t, k, at)) = cache.get(&id) {
        if at.elapsed() < Duration::from_secs(30) {
            return (t.clone(), *k);
        }
    }
    let (title, kind) = match mostalgia_core::docs::get(id).ok().flatten() {
        Some(d) => (d.title, Some(d.kind)),
        None => (trf("Dokument #{}", &[&id]), None),
    };
    cache.insert(id, (title.clone(), kind, Instant::now()));
    (title, kind)
}

/// "p. 4" / "§4" for a cited document location (the chip label).
fn loc_text(target: &Target) -> String {
    match *target {
        Target::Document { doc_id, loc } => match document_label(doc_id).1 {
            Some(kind) => mostalgia_core::docs::loc_label(kind, loc),
            None => format!("#{loc}"),
        },
        _ => target.time_label(),
    }
}

/// Hover text for a citation: "0:42" or "Budget review · Oct 27 · 0:42".
fn cite_tooltip(target: &Target) -> String {
    match *target {
        Target::Document { doc_id, .. } => format!("{} · {}", document_label(doc_id).0, loc_text(target)),
        Target::Meeting { meeting_id, .. } => {
            let (title, date) = meeting_label(meeting_id);
            [title, date, target.time_label()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
        }
        Target::Time { .. } => target.time_label(),
    }
}

fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
}

/// One word or citation of a line; words and the markers right after them
/// stay together when the line wraps.
enum Token {
    Word(String),
    Cite(usize, Target),
}

/// An answer as readable prose: each citation becomes a small number after
/// the word it belongs to, and a compact "Sources" row follows — time chips
/// for the current meeting, grouped by meeting for library answers.
/// Clicking a number or chip calls `on_cite`. `key` makes element ids unique.
pub fn cited_answer(answer: &str, key: &str, cx: &App, on_cite: impl Fn(Target, &mut Window, &mut App) + Clone + 'static) -> Div {
    let theme = cx.theme().clone();
    let cited = cite::parse(&answer.replace("**", ""));
    let mut out = v_flex().w_full().gap_1p5().line_height(relative(1.55));
    for (li, line) in cited.lines().into_iter().enumerate() {
        // Line kind from its leading text: bullet, heading or plain.
        let mut line = line;
        let mut bullet = false;
        let mut heading = false;
        if let Some(Mark::Text(first)) = line.first_mut() {
            let trimmed = first.trim_start();
            if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).or_else(|| trimmed.strip_prefix("• ")) {
                *first = rest.to_string();
                bullet = true;
            } else if trimmed.starts_with('#') {
                *first = trimmed.trim_start_matches('#').trim_start().to_string();
                heading = true;
            }
        }
        if line.iter().all(|m| matches!(m, Mark::Text(t) if t.trim().is_empty())) {
            continue;
        }
        let rtl = line.iter().any(|m| matches!(m, Mark::Text(t) if ui::is_rtl(t)));
        // Words grouped with the markers glued to them.
        let mut groups: Vec<Vec<Token>> = Vec::new();
        let mut glue = false;
        for mark in line {
            match mark {
                Mark::Text(t) => {
                    let mut first = true;
                    for w in t.split_whitespace() {
                        let attach = first && glue && !t.starts_with(char::is_whitespace);
                        match groups.last_mut() {
                            Some(g) if attach => g.push(Token::Word(w.to_string())),
                            _ => groups.push(vec![Token::Word(w.to_string())]),
                        }
                        first = false;
                    }
                    glue = !t.ends_with(char::is_whitespace);
                }
                Mark::Cite { index, target } => {
                    match groups.last_mut() {
                        Some(g) => g.push(Token::Cite(index, target)),
                        None => groups.push(vec![Token::Cite(index, target)]),
                    }
                    glue = true;
                }
            }
        }
        let items = groups.into_iter().enumerate().map(|(gi, group)| {
            let parts = group.into_iter().enumerate().map(|(ci, tok)| match tok {
                Token::Word(w) => div().flex_none().child(w).into_any_element(),
                Token::Cite(n, target) => {
                    let on_cite = on_cite.clone();
                    div()
                        .id(SharedString::from(format!("cite-{key}-{li}-{gi}-{ci}")))
                        .flex_none()
                        .ml(px(2.))
                        .mt(px(1.))
                        .min_w(px(15.))
                        .h(px(15.))
                        .px(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(theme.muted_foreground.opacity(0.14))
                        .text_color(theme.muted_foreground)
                        .text_size(px(10.))
                        .line_height(px(15.))
                        .font_medium()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.primary.opacity(0.18)).text_color(theme.primary))
                        .child(n.to_string())
                        .tooltip(tooltip(cite_tooltip(&target)))
                        .on_click(move |_, window, cx| on_cite(target, window, cx))
                        .into_any_element()
                }
            });
            h_flex().flex_none().items_start().when(rtl, |el| el.flex_row_reverse()).children(parts)
        });
        out = out.child(
            h_flex()
                .w_full()
                .items_start()
                .gap_2()
                .when(rtl, |el| el.flex_row_reverse())
                .when(bullet, |el| el.child(div().flex_none().mt(px(9.)).size(px(4.)).rounded_full().bg(theme.muted_foreground)))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .flex_wrap()
                        .items_start()
                        .gap_x(px(4.))
                        .when(rtl, |el| el.flex_row_reverse())
                        .when(heading, |el| el.font_semibold())
                        .children(items),
                ),
        );
    }
    if cited.sources.is_empty() {
        return out;
    }

    // Sources: "0:42  1:13" or "Budget review  0:42  1:13 · Kickoff  4:05".
    let chip = |n: usize, target: Target| {
        let on_cite = on_cite.clone();
        h_flex()
            .id(SharedString::from(format!("source-{key}-{n}")))
            .flex_none()
            .gap(px(4.))
            .h(px(20.))
            .px(px(7.))
            .rounded_full()
            .bg(theme.muted_foreground.opacity(0.1))
            .text_color(theme.muted_foreground)
            .line_height(px(20.))
            .cursor_pointer()
            .hover(|s| s.bg(theme.primary.opacity(0.15)).text_color(theme.primary))
            .child(div().text_size(px(10.)).opacity(0.7).child(n.to_string()))
            .child(loc_text(&target))
            .tooltip(tooltip(match target {
                Target::Meeting { .. } => trf("{} – öffnen und ab hier abspielen", &[&cite_tooltip(&target)]),
                Target::Document { .. } => trf("{} – im Dokument zeigen", &[&cite_tooltip(&target)]),
                Target::Time { .. } => trf("{} – zu dieser Stelle springen", &[&target.time_label()]),
            }))
            .on_click(move |_, window, cx| on_cite(target, window, cx))
            .into_any_element()
    };
    let mut row: Vec<AnyElement> = vec![div().flex_none().mr(px(2.)).child(tr("Quellen")).into_any_element()];
    let mut meetings: Vec<i64> = Vec::new();
    for t in &cited.sources {
        if let Target::Meeting { meeting_id, .. } = t {
            if !meetings.contains(meeting_id) {
                meetings.push(*meeting_id);
            }
        }
    }
    let numbered = cited.sources.iter().copied().enumerate().map(|(i, t)| (i + 1, t));
    for (n, t) in numbered.clone().filter(|(_, t)| matches!(t, Target::Time { .. })) {
        row.push(chip(n, t));
    }
    for (gi, id) in meetings.iter().enumerate() {
        let (title, date) = meeting_label(*id);
        let full = if date.is_empty() { title.clone() } else { format!("{title} · {date}") };
        row.push(
            div()
                .id(SharedString::from(format!("source-{key}-m{id}")))
                .flex_none()
                .when(gi > 0 || row.len() > 1, |el| el.ml(px(6.)))
                .max_w(px(220.))
                .truncate()
                .child(shorten(&title, 28))
                .tooltip(tooltip(full))
                .into_any_element(),
        );
        for (n, t) in numbered.clone().filter(|(_, t)| matches!(t, Target::Meeting { meeting_id, .. } if meeting_id == id)) {
            row.push(chip(n, t));
        }
    }
    let mut documents: Vec<i64> = Vec::new();
    for t in &cited.sources {
        if let Target::Document { doc_id, .. } = t {
            if !documents.contains(doc_id) {
                documents.push(*doc_id);
            }
        }
    }
    for id in &documents {
        let (title, _) = document_label(*id);
        row.push(
            h_flex()
                .id(SharedString::from(format!("source-{key}-d{id}")))
                .flex_none()
                .when(row.len() > 1, |el| el.ml(px(6.)))
                .gap(px(3.))
                .max_w(px(220.))
                .child(Icon::new(IconName::FileText).xsmall())
                .child(div().truncate().child(shorten(&title, 28)))
                .tooltip(tooltip(title.clone()))
                .into_any_element(),
        );
        for (n, t) in numbered.clone().filter(|(_, t)| matches!(t, Target::Document { doc_id, .. } if doc_id == id)) {
            row.push(chip(n, t));
        }
    }
    out.child(
        h_flex()
            .w_full()
            .mt_1()
            .flex_wrap()
            .items_center()
            .gap_x(px(4.))
            .gap_y(px(4.))
            .text_xs()
            .line_height(px(20.))
            .text_color(theme.muted_foreground)
            .children(row),
    )
}

impl Render for AskView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.lang_seen != Some((i18n::lang(), self.within)) {
            self.lang_seen = Some((i18n::lang(), self.within));
            let placeholder = match self.within {
                Some(Within::Collection(_)) => tr("Frage zu dieser Sammlung …"),
                Some(Within::Document(_)) => tr("Frage zu diesem Dokument …"),
                _ => tr("Frage zu deinen Besprechungen …"),
            };
            self.input.update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
        }
        let scoped = self.within.is_some();
        let suggestions: Vec<&str> = if scoped { DOC_SUGGESTIONS.to_vec() } else { SUGGESTIONS.to_vec() };
        let theme = cx.theme().clone();
        let config = Config::load();
        // Search embeddings from a cloud model (named in the intro).
        let cloud_embeddings = config.connection(&config.embedding.connection).filter(|c| !c.is_local()).map(|c| c.name.clone());
        // Which connection and model answer, shown in the header; a missing
        // or unconfigured connection is shown as an error, never swapped.
        let ask = config.ask_request();
        let (provider_name, model, ask_problem) = match &ask {
            Ok(req) if req.connection.is_configured() => (req.connection.name.clone(), req.model.clone(), None),
            Ok(req) => (
                req.connection.name.clone(),
                req.model.clone(),
                Some(trf("{} ist nicht eingerichtet — in Einstellungen → KI hinterlegen.", &[&req.connection.name])),
            ),
            Err(e) => (String::new(), String::new(), Some(format!("{e:#}"))),
        };

        let mut body: Vec<AnyElement> = Vec::new();
        if self.turns.is_empty() {
            body.push(
                v_flex()
                    .pt_16()
                    .items_center()
                    .gap_3()
                    .child(Icon::new(IconName::Sparkles).large().text_color(theme.primary))
                    .child(div().text_xl().font_semibold().child(if scoped { trf("Frag: {}", &[&scope_label(self.within, &self.collections)]) } else { tr("Frag deine Besprechungen").to_string() }))
                    .child(
                        div()
                            .max_w(px(520.))
                            .text_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(match (scoped, &cloud_embeddings) {
                                (true, None) => tr("Die Suche läuft lokal nur über diese Dokumente – auch wenn sie für Antworten ausgeschaltet sind. An das Sprachmodell gehen nur die gefundenen Abschnitte. Jede Aussage verweist auf die Stelle im Dokument.").to_string(),
                                (false, None) => tr("Die Suche läuft lokal über alle Transkripte und Zusammenfassungen. An das Sprachmodell gehen nur die gefundenen Abschnitte, nie Audio. Jede Aussage verweist auf die Stelle in der Aufnahme.").to_string(),
                                (true, Some(name)) => trf("Die Suche läuft nur über diese Dokumente – auch wenn sie für Antworten ausgeschaltet sind – mit Embeddings von {} (Cloud). An das Sprachmodell gehen nur die gefundenen Abschnitte. Jede Aussage verweist auf die Stelle im Dokument.", &[name]),
                                (false, Some(name)) => trf("Die Suche läuft über alle Transkripte und Zusammenfassungen, mit Embeddings von {} (Cloud). An das Sprachmodell gehen nur die gefundenen Abschnitte, nie Audio. Jede Aussage verweist auf die Stelle in der Aufnahme.", &[name]),
                            }),
                    )
                    .child(
                        v_flex().pt_2().gap_2().children(suggestions.into_iter().enumerate().map(|(i, s)| {
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
            let answer = self.render_answer(&turn.answer, ti, cx);
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
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{provider_name} · {model}")))
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
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("ask-body")
                            .size_full()
                            .track_scroll(&self.scroll)
                            .overflow_y_scroll()
                            .child(v_flex().w_full().max_w(px(860.)).mx_auto().px_8().py_6().gap_5().children(body)),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .child(
                v_flex()
                    .px_8()
                    .py_3()
                    .gap_1()
                    .border_t_1()
                    .border_color(theme.border)
                    // Lined up with the conversation's text (860 less its padding).
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(px(860. - 64.))
                            .mx_auto()
                            .gap_2()
                            .child(self.scope_button(cx))
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
                    .when_some(ask_problem, |el, problem| el.child(div().w_full().max_w(px(860. - 64.)).mx_auto().text_xs().text_color(theme.warning).child(problem))),
            )
    }
}
