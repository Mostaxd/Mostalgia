//! The window: title bar, library sidebar, and the current screen.

use std::sync::{Arc, Mutex};

use chrono::{Datelike, Local};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, StyledExt, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::paths;
use mostalgia_core::store::{MeetingSummary, SearchHit, Store};

use crate::meeting::{MeetingEvent, MeetingView};
use crate::recording::{RecordingEvent, RecordingView, SharedEngine};
use crate::settings::{SettingsEvent, SettingsView};
use crate::{control, ui};

enum Screen {
    Recording,
    Meeting(Entity<MeetingView>),
    Settings(Entity<SettingsView>),
}

pub struct MostalgiaApp {
    engine: SharedEngine,
    recording: Entity<RecordingView>,
    screen: Screen,
    meetings: Vec<MeetingSummary>,
    search: Entity<InputState>,
    query: String,
    hits: Vec<SearchHit>,
    error: Option<SharedString>,
    tray: Option<ksni::blocking::Handle<control::Tray>>,
    _subscriptions: Vec<Subscription>,
}

impl MostalgiaApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine: SharedEngine = Arc::new(Mutex::new(None));
        let recording = cx.new(|cx| RecordingView::new(engine.clone(), cx));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Transkripte durchsuchen"));
        let subs = vec![
            cx.subscribe_in(&recording, window, |this, _, event: &RecordingEvent, window, cx| match event {
                RecordingEvent::Saved(id) => {
                    this.reload(cx);
                    this.open_meeting(*id, window, cx);
                    if let Screen::Meeting(view) = &this.screen {
                        view.update(cx, |v, cx| v.auto_summarize(cx));
                    }
                }
            }),
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.query = input.read(cx).value().to_string();
                    this.run_search(cx);
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| ui::apply_theme(window, cx)),
        ];
        let mut this = Self {
            engine,
            recording,
            screen: Screen::Recording,
            meetings: Vec::new(),
            search,
            query: String::new(),
            hits: Vec::new(),
            error: None,
            tray: None,
            _subscriptions: subs,
        };
        this.reload(cx);
        this.start_control(window, cx);
        this
    }

    /// The control socket (`mostalgia toggle`) and the tray icon.
    fn start_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use ksni::blocking::TrayMethods;
        let (tx, rx) = std::sync::mpsc::channel();
        if let Err(e) = control::listen(tx.clone()) {
            eprintln!("[mostalgia] control socket: {e}");
        }
        match (control::Tray { tx, recording: false }).spawn() {
            Ok(handle) => self.tray = Some(handle),
            Err(e) => eprintln!("[mostalgia] tray icon unavailable: {e}"),
        }
        cx.on_app_quit(|_, _| async { control::cleanup() }).detach();
        cx.spawn_in(window, async move |this, cx| {
            let mut was_recording = false;
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
                let commands: Vec<_> = rx.try_iter().collect();
                let alive = this
                    .update_in(cx, |this, window, cx| {
                        for cmd in commands {
                            this.handle_command(cmd, window, cx);
                        }
                        let recording = this.recording.read(cx).is_recording();
                        if recording != was_recording {
                            was_recording = recording;
                            if let Some(tray) = &this.tray {
                                tray.update(|t| t.recording = recording);
                            }
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_command(&mut self, cmd: control::Command, window: &mut Window, cx: &mut Context<Self>) {
        use control::Command;
        let recording = self.recording.read(cx).is_recording();
        let active = self.recording.read(cx).is_active();
        match cmd {
            Command::Toggle if recording => self.recording.update(cx, |r, cx| r.stop(cx)),
            Command::Toggle | Command::Start if !active => self.start_recording(cx),
            Command::Stop if recording => self.recording.update(cx, |r, cx| r.stop(cx)),
            Command::Show => window.activate_window(),
            Command::Quit => cx.quit(),
            _ => {}
        }
    }

    /// Switches to the recording screen and starts recording.
    pub fn start_recording(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Recording;
        self.recording.update(cx, |r, cx| r.start(cx));
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        match Store::open(&paths::database()).and_then(|s| s.list(500)) {
            Ok(m) => {
                self.meetings = m;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}").into()),
        }
        self.run_search(cx);
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        self.hits = if self.query.trim().is_empty() {
            Vec::new()
        } else {
            Store::open(&paths::database())
                .and_then(|s| s.search(&self.query, 50))
                .unwrap_or_default()
        };
        cx.notify();
    }

    pub fn open_meeting(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if let Screen::Meeting(view) = &self.screen {
            if view.read(cx).id == id {
                return;
            }
        }
        let view = cx.new(|cx| MeetingView::new(id, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, event: &MeetingEvent, _, cx| match event {
            MeetingEvent::Changed => this.reload(cx),
            MeetingEvent::Deleted => {
                this.screen = Screen::Recording;
                this.reload(cx);
            }
        });
        self._subscriptions.push(sub);
        self.screen = Screen::Meeting(view);
        cx.notify();
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gpu = self
            .engine
            .lock()
            .ok()
            .and_then(|e| e.as_ref().and_then(|e| e.gpu().map(str::to_string)));
        let view = cx.new(|cx| SettingsView::new(gpu, window, cx));
        let recording = self.recording.clone();
        let sub = cx.subscribe(&view, move |_, _, _: &SettingsEvent, cx| {
            recording.update(cx, |r, cx| r.reload_config(cx));
        });
        self._subscriptions.push(sub);
        self.screen = Screen::Settings(view);
        cx.notify();
    }

    #[doc(hidden)]
    pub fn recording_view(&self) -> Entity<RecordingView> {
        self.recording.clone()
    }

    #[doc(hidden)]
    pub fn meeting_view(&self) -> Option<Entity<MeetingView>> {
        match &self.screen {
            Screen::Meeting(v) => Some(v.clone()),
            _ => None,
        }
    }

    #[doc(hidden)]
    pub fn settings_view(&self) -> Option<Entity<SettingsView>> {
        match &self.screen {
            Screen::Settings(v) => Some(v.clone()),
            _ => None,
        }
    }

    pub fn show_recording(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Recording;
        cx.notify();
    }

    fn selected_meeting(&self, cx: &App) -> Option<i64> {
        match &self.screen {
            Screen::Meeting(v) => Some(v.read(cx).id),
            _ => None,
        }
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let recording_active = self.recording.read(cx).is_active();
        let is_recording = self.recording.read(cx).is_recording();
        let on_record_screen = matches!(self.screen, Screen::Recording);
        let selected = self.selected_meeting(cx);

        let record_button = Button::new("new-recording")
            .w_full()
            .when(is_recording, |b| b.danger().icon(IconName::Radio).label("Aufnahme läuft"))
            .when(!is_recording, |b| {
                b.primary()
                    .icon(IconName::Plus)
                    .label(if recording_active { "Aufnahme" } else { "Neue Aufnahme" })
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.screen = Screen::Recording;
                cx.notify();
            }));

        let mut items: Vec<AnyElement> = Vec::new();
        if self.query.trim().is_empty() {
            let today = Local::now().date_naive();
            let mut group: Option<&'static str> = None;
            for (ix, m) in self.meetings.iter().enumerate() {
                let date = m.started_at.date_naive();
                let days = (today - date).num_days();
                let g = if days == 0 {
                    "HEUTE"
                } else if days == 1 {
                    "GESTERN"
                } else if days < 7 && date.weekday().num_days_from_monday() < today.weekday().num_days_from_monday() {
                    "DIESE WOCHE"
                } else if date.year() == today.year() && date.month() == today.month() {
                    "DIESER MONAT"
                } else {
                    "FRÜHER"
                };
                if group != Some(g) {
                    group = Some(g);
                    items.push(ui::section_title(g, cx).px_2().pt_4().pb_1().into_any_element());
                }
                let id = m.id;
                let active = selected == Some(id);
                let time = if days == 0 || days == 1 {
                    m.started_at.format("%H:%M").to_string()
                } else {
                    m.started_at.format("%d.%m. %H:%M").to_string()
                };
                items.push(
                    v_flex()
                        .id(("meeting", ix))
                        .px_2()
                        .py_1p5()
                        .gap_0p5()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .when(active, |el| el.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
                        .when(!active, |el| el.hover(|s| s.bg(theme.sidebar_accent.opacity(0.6))))
                        .child(
                            div()
                                .w_full()
                                .text_sm()
                                .font_medium()
                                .when(!ui::is_rtl(&m.title), |el| el.truncate().child(m.title.clone()))
                                .when(ui::is_rtl(&m.title), |el| el.child(ui::bidi_text(m.title.clone(), false))),
                        )
                        .child(
                            h_flex()
                                .gap_1p5()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{time} · {}", ui::minutes(m.duration_secs)))
                                .when(m.has_summary, |el| el.child(Icon::new(IconName::Sparkles).xsmall().text_color(theme.primary))),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.open_meeting(id, window, cx)))
                        .into_any_element(),
                );
            }
            if self.meetings.is_empty() {
                items.push(
                    div()
                        .px_2()
                        .py_6()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Noch keine Besprechungen. Starte deine erste Aufnahme.")
                        .into_any_element(),
                );
            }
        } else {
            items.push(ui::section_title(format!("{} TREFFER", self.hits.len()), cx).px_2().pt_3().pb_1().into_any_element());
            for (ix, h) in self.hits.iter().enumerate() {
                let id = h.meeting_id;
                let snippet = h.snippet.replace(['[', ']'], "");
                items.push(
                    v_flex()
                        .id(("hit", ix))
                        .px_2()
                        .py_1p5()
                        .gap_0p5()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.sidebar_accent.opacity(0.6)))
                        .child(div().w_full().text_sm().line_clamp(2).child(ui::bidi_text(snippet, true)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(format!("{} · {} · {}", h.title, h.started_at.format("%d.%m."), ui::hms(h.start_secs))),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.open_meeting(id, window, cx)))
                        .into_any_element(),
                );
            }
        }

        let settings_active = matches!(self.screen, Screen::Settings(_));
        v_flex()
            .w(px(272.))
            .h_full()
            .flex_none()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                v_flex()
                    .p_3()
                    .gap_3()
                    .child(record_button.when(on_record_screen && !is_recording, |b| b.outline()))
                    .child(
                        Input::new(&self.search)
                            .small()
                            .prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground))
                            .cleanable(true),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    v_flex()
                        .id("library")
                        .size_full()
                        .px_2()
                        .pb_2()
                        .overflow_y_scroll()
                        .children(items),
                ),
            )
            .when_some(self.error.clone(), |el, e| el.child(div().p_3().text_xs().text_color(theme.danger).child(e)))
            .child(
                div().p_2().border_t_1().border_color(theme.sidebar_border).child(
                    Button::new("settings")
                        .w_full()
                        .ghost()
                        .when(settings_active, |b| b.selected(true))
                        .icon(IconName::Settings)
                        .label("Einstellungen")
                        .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
                ),
            )
    }
}

impl Render for MostalgiaApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self.render_sidebar(cx);
        let content: AnyView = match &self.screen {
            Screen::Recording => self.recording.clone().into(),
            Screen::Meeting(v) => v.clone().into(),
            Screen::Settings(v) => v.clone().into(),
        };
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(ui::ui_font(cx))
            .child(
                TitleBar::new().child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .font_semibold()
                        .child(Icon::new(IconName::AudioLines).small().text_color(theme.primary))
                        .child("mostalgia"),
                ),
            )
            .child(h_flex().flex_1().min_h_0().child(sidebar).child(div().flex_1().min_w_0().h_full().child(content)))
    }
}
