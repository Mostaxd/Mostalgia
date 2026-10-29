//! "Besprechung aus Text": a meeting nobody recorded, from its transcript
//! as text (pasted or opened from a file), then summarized and given notes
//! like a recording (see `mostalgia_core::text_import`).

use chrono::Local;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::companion::NoteStyle;
use mostalgia_core::config::Config;
use mostalgia_core::i18n::{tr, trf};
use mostalgia_core::store::Store;
use mostalgia_core::text_import::{self, Format};
use mostalgia_core::paths;

/// Date format of the "when" field.
const WHEN_FORMAT: &str = "%Y-%m-%d %H:%M";

pub enum TextImportEvent {
    /// Saved as this meeting; the summary and notes are still to be made.
    Imported { meeting_id: i64, keep_title: bool, focus: String, style: NoteStyle },
    Cancel,
}

impl EventEmitter<TextImportEvent> for TextImportView {}

/// What the pasted text was read as.
struct Preview {
    format: Format,
    lines: usize,
    speakers: usize,
    timed: bool,
    duration_secs: f64,
    language: Option<&'static str>,
}

pub struct TextImportView {
    title: Entity<InputState>,
    when: Entity<InputState>,
    duration: Entity<InputState>,
    text: Entity<TextareaState>,
    focus: Entity<InputState>,
    style: NoteStyle,
    /// A language code, or `None` to detect it from the text.
    language: Option<String>,
    preview: Option<Preview>,
    /// The file the text came from.
    file: Option<String>,
    error: Option<SharedString>,
    importing: bool,
    /// The duration field's placeholder as last set.
    estimate_shown: String,
    _subscriptions: Vec<Subscription>,
}

impl TextImportView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = Config::load();
        let title = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Optional – sonst aus der Zusammenfassung")));
        let when = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("2026-10-27 14:00");
            s.set_value(Local::now().format(WHEN_FORMAT).to_string(), window, cx);
            s
        });
        let duration = cx.new(|cx| InputState::new(window, cx).placeholder(tr("geschätzt")));
        let text = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder(tr("Transkript oder Mitschrift hier einfügen …\n\nZum Beispiel:\nAnna: Wollen wir mit dem Budget anfangen?\nBen: Gern. Wir liegen zehn Prozent unter Plan."))
        });
        let focus = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(tr("z. B. Aufgaben für mich, Preise, Fristen"));
            s.set_value(config.companion_notes_focus.clone(), window, cx);
            s
        });
        let subs = vec![
            cx.subscribe(&text, |this: &mut Self, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.reparse(cx);
                }
            }),
            cx.subscribe(&when, |this: &mut Self, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.error = None;
                    cx.notify();
                }
            }),
            cx.subscribe(&duration, |this: &mut Self, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.error = None;
                    cx.notify();
                }
            }),
        ];
        text.update(cx, |s, cx| s.focus(window, cx));
        Self {
            title,
            when,
            duration,
            text,
            focus,
            style: config.notes_style(),
            language: None,
            preview: None,
            file: None,
            error: None,
            importing: false,
            estimate_shown: String::new(),
            _subscriptions: subs,
        }
    }

    /// Reads the text again for the "recognized as" line and the estimates.
    fn reparse(&mut self, cx: &mut Context<Self>) {
        let text = self.text.read(cx).value().to_string();
        self.error = None;
        self.preview = if text.trim().is_empty() {
            None
        } else {
            let parsed = text_import::parse(&text);
            Some(Preview {
                format: parsed.format,
                lines: parsed.lines.len(),
                speakers: parsed.speakers().len(),
                timed: parsed.timed(),
                duration_secs: parsed.estimated_duration(),
                language: text_import::detect_language(&text),
            })
        };
        cx.notify();
    }

    /// Screenshots: the dialog filled in with `text`.
    #[doc(hidden)]
    pub fn demo(&mut self, title: &str, when: &str, file: Option<&str>, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.title.update(cx, |s, cx| s.set_value(title.to_string(), window, cx));
        self.when.update(cx, |s, cx| s.set_value(when.to_string(), window, cx));
        self.text.update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
        self.file = file.map(str::to_string);
        self.reparse(cx);
    }

    fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("Öffnen").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(p) => p,
                    None => return,
                },
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(e)) => {
                    this.update(cx, |this, cx| {
                        this.error = Some(trf("Dateiauswahl: {}", &[&format!("{e:#}")]).into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let read = std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| text_import::decode(&b));
            this.update_in(cx, |this, window, cx| {
                match read {
                    Ok(text) => {
                        this.text.update(cx, |s, cx| s.set_value(text, window, cx));
                        this.file = path.file_name().map(|n| n.to_string_lossy().into_owned());
                        this.reparse(cx);
                    }
                    Err(e) => {
                        this.error = Some(trf("{} lässt sich nicht lesen: {}", &[&path.display(), &format!("{e:#}")]).into());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Saves the meeting (in the background) and hands it to the app.
    fn import(&mut self, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        let text = self.text.read(cx).value().to_string();
        if text.trim().is_empty() {
            self.error = Some(tr("Füge zuerst den Text der Besprechung ein.").into());
            cx.notify();
            return;
        }
        let when = self.when.read(cx).value().to_string();
        let Some(started_at) = text_import::parse_date_time(&when, Local::now()) else {
            self.error = Some(trf("Unbekanntes Datum {} (z. B. \"2026-10-27 14:00\")", &[&format!("{when:?}")]).into());
            cx.notify();
            return;
        };
        let duration = self.duration.read(cx).value().to_string();
        let duration_secs = if duration.trim().is_empty() {
            None
        } else {
            match text_import::parse_duration(&duration) {
                Some(d) => Some(d),
                None => {
                    self.error = Some(trf("Unbekannte Dauer {} (z. B. 45, 1h30 oder 1:30:00)", &[&format!("{duration:?}")]).into());
                    cx.notify();
                    return;
                }
            }
        };
        let title = self.title.read(cx).value().trim().to_string();
        let keep_title = !title.is_empty();
        let language = self.language.clone();
        let focus = self.focus.read(cx).value().trim().to_string();
        let style = self.style;
        self.importing = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            let mut store = Store::open(&paths::database())?;
            let done = text_import::import(
                &mut store,
                &text_import::Request { text: &text, title: Some(title.as_str()).filter(|t| !t.is_empty()), started_at, duration_secs, language: language.as_deref() },
            )?;
            drop(store);
            if let Err(e) = mostalgia_core::ask::index_meeting(done.meeting_id) {
                eprintln!("[mostalgia] search index: {e:#}");
            }
            anyhow::Ok(done.meeting_id)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.importing = false;
                match result {
                    Ok(meeting_id) => cx.emit(TextImportEvent::Imported { meeting_id, keep_title, focus, style }),
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Where the text goes: the summary's and the notes' connections.
    fn privacy_line(&self) -> String {
        let config = Config::load();
        let summary = config.auto_summary.then(|| config.summary_connection().ok().filter(|c| c.is_configured())).flatten().map(|c| c.name);
        let notes = config.notes_request().ok().filter(|r| r.connection.is_configured()).map(|r| r.connection.name);
        match (summary, notes) {
            (Some(s), Some(n)) if s == n => trf("Nur der Text geht an {} – für Zusammenfassung und Notizen. Kein Audio.", &[&s]),
            (Some(s), Some(n)) => trf("Nur der Text geht an {} (Zusammenfassung) und {} (Notizen).", &[&s, &n]),
            (Some(s), None) => trf("Nur der Text geht an {} für die Zusammenfassung.", &[&s]),
            (None, Some(n)) => trf("Nur der Text geht an {} für die Notizen.", &[&n]),
            (None, None) => tr("Der Text bleibt auf diesem Rechner (keine KI-Verbindung für Zusammenfassung oder Notizen eingerichtet).").to_string(),
        }
    }

    fn language_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let detected = self.preview.as_ref().and_then(|p| p.language);
        let label = match (&self.language, detected) {
            (Some(code), _) => text_import::language_display(code),
            (None, Some(code)) => trf("Automatisch ({})", &[&text_import::language_display(code)]),
            (None, None) => tr("Automatisch erkennen").to_string(),
        };
        let weak = cx.entity().downgrade();
        let current = self.language.clone();
        Button::new("ti-language")
            .small()
            .outline()
            .icon(IconName::Globe)
            .label(label)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.));
                let choices = std::iter::once((None, tr("Automatisch erkennen").to_string()))
                    .chain(text_import::LANGUAGES.iter().map(|(code, name)| (Some(code.to_string()), name.to_string())));
                for (code, name) in choices {
                    let weak = weak.clone();
                    let checked = current == code;
                    menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(move |_, _, cx| {
                        let code = code.clone();
                        weak.update(cx, |this, cx| {
                            this.language = code;
                            cx.notify();
                        })
                        .ok();
                    }));
                }
                menu
            })
    }
}

fn field(label: &'static str, input: impl IntoElement) -> Div {
    v_flex().gap_1().child(div().text_sm().font_medium().child(tr(label))).child(input)
}

impl Render for TextImportView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let empty = self.preview.is_none();
        // The duration field shows the estimate until one is typed.
        let estimate = match &self.preview {
            Some(p) => trf("≈ {} min (geschätzt)", &[&(p.duration_secs / 60.).round().max(1.)]),
            None => tr("geschätzt").to_string(),
        };
        if self.estimate_shown != estimate {
            self.estimate_shown = estimate.clone();
            self.duration.update(cx, |s, cx| s.set_placeholder(estimate, window, cx));
        }
        let recognized = match &self.preview {
            Some(p) => {
                let mut parts = vec![p.format.label().to_string(), trf("{} Zeilen", &[&p.lines])];
                if p.speakers > 0 {
                    parts.push(trf("{} Sprecher", &[&p.speakers]));
                }
                parts.push(if p.timed { tr("mit Zeiten").to_string() } else { tr("ohne Zeiten – die Zeilen werden ohne Uhrzeit angezeigt").to_string() });
                trf("Erkannt: {}", &[&parts.join(" · ")])
            }
            None => tr("Versteht Fließtext, „Name: Text“-Zeilen, Zeitstempel, Exporte aus Teams, Zoom und Meet sowie WebVTT- und SRT-Untertitel.").to_string(),
        };
        let header = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::FileText).text_color(theme.primary))
                    .child(div().text_2xl().font_semibold().child(tr("Besprechung aus Text"))),
            )
            .child(div().text_color(theme.muted_foreground).child(tr(
                "Für Besprechungen, die nicht aufgenommen wurden: Transkript oder Mitschrift einfügen – daraus wird eine Besprechung mit Zusammenfassung und Notizen.",
            )));
        let meta = h_flex()
            .gap_3()
            .items_end()
            .child(field("Titel", Input::new(&self.title).small()).flex_1().min_w(px(200.)))
            .child(field("Wann", Input::new(&self.when).small()).w(px(170.)))
            .child(field("Dauer", Input::new(&self.duration).small()).w(px(180.)));
        let source = h_flex()
            .gap_2()
            .child(
                Button::new("ti-open")
                    .small()
                    .outline()
                    .icon(IconName::FolderOpen)
                    .label(tr("Datei öffnen…"))
                    .tooltip(tr("Text, Markdown, WebVTT oder SRT"))
                    .on_click(cx.listener(|this, _, window, cx| this.open_file(window, cx))),
            )
            .when_some(self.file.clone(), |el, f| {
                el.child(h_flex().gap_1().text_xs().text_color(theme.muted_foreground).child(Icon::new(IconName::File).xsmall()).child(f))
            })
            .child(div().flex_1())
            .child(div().text_sm().text_color(theme.muted_foreground).child(tr("Sprache")))
            .child(self.language_button(cx));
        let notes = v_flex()
            .gap_2()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::WandSparkles).small().text_color(theme.primary))
                    .child(div().font_semibold().child(trf("Notizen von {}", &[&Config::load().companion_character().name()]))),
            )
            .child(field("Worauf achten? (optional)", Input::new(&self.focus).small()))
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child(tr("Notizstil")))
                    .child(h_flex().flex_wrap().gap_1().children(NoteStyle::ALL.map(|style| {
                        let on = style == self.style;
                        Button::new(SharedString::from(format!("ti-style-{}", style.id())))
                            .small()
                            .label(style.label())
                            .when(on, |b| b.primary())
                            .when(!on, |b| b.ghost())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.style = style;
                                cx.notify();
                            }))
                    })))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(self.style.hint())),
            );
        let footer = h_flex()
            .gap_3()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Info).xsmall())
                    .child(div().min_w_0().child(self.privacy_line())),
            )
            .child(Button::new("ti-cancel").ghost().label(tr("Abbrechen")).on_click(cx.listener(|_, _, _, cx| cx.emit(TextImportEvent::Cancel))))
            .child(
                Button::new("ti-import")
                    .primary()
                    .icon(IconName::Plus)
                    .label(if self.importing { tr("Wird angelegt …") } else { tr("Besprechung anlegen") })
                    .disabled(empty || self.importing)
                    .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
            );
        div().id("text-import").size_full().overflow_y_scroll().child(
            v_flex()
                .w_full()
                .max_w(px(820.))
                .mx_auto()
                .px_8()
                .py_6()
                .gap_4()
                .child(header)
                .child(meta)
                .child(source)
                .child(Textarea::new(&self.text).h(px(300.)))
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(if empty { theme.muted_foreground } else { theme.foreground })
                        .child(Icon::new(if empty { IconName::Info } else { IconName::Check }).xsmall().text_color(if empty { theme.muted_foreground } else { theme.success }))
                        .child(recognized),
                )
                .child(notes)
                .when_some(self.error.clone(), |el, e| {
                    el.child(div().p_3().rounded(theme.radius).bg(theme.danger.opacity(0.1)).text_sm().text_color(theme.danger).child(e))
                })
                .child(footer),
        )
    }
}
