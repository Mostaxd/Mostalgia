//! Settings → Microsoft Teams: the app registration, signing in, and what
//! Mostalgia does with the calendar and with finished meetings. Saves only
//! the `teams` part of the config (the rest of the settings screen keeps
//! its own copy and leaves this part alone).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Local;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, h_flex, switch::Switch, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::config::Config;
use mostalgia_core::i18n::{tr, trf};
use mostalgia_core::teams::{self, Event, TeamsConfig, auth, ops};

use crate::ui;

/// Where the registration steps are explained.
pub const GUIDE_URL: &str = "https://github.com/Mostaxd/Mostalgia/blob/main/docs/teams-integration.md";

pub struct TeamsSettings {
    teams: TeamsConfig,
    client_id: Entity<InputState>,
    tenant: Entity<InputState>,
    todo_list: Entity<InputState>,
    /// Set while the browser sign-in runs; storing true cancels it.
    signing_in: Option<Arc<AtomicBool>>,
    error: Option<SharedString>,
    upcoming: Option<Result<Vec<Event>, String>>,
    loading: bool,
    _subscriptions: Vec<Subscription>,
}

impl TeamsSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let teams = Config::load().teams;
        let input = |value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let (value, placeholder) = (value.to_string(), placeholder.to_string());
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            })
        };
        let client_id = input(&teams.client_id, teams::CLIENT_ID_PLACEHOLDER, window, cx);
        let tenant = input(&teams.tenant, teams::DEFAULT_TENANT, window, cx);
        let todo_list = input(&teams.todo_list, teams::DEFAULT_TODO_LIST, window, cx);
        let on_change = |field: fn(&mut TeamsConfig, String)| {
            move |this: &mut Self, input: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>| {
                if let InputEvent::Change = event {
                    let value = input.read(cx).value().trim().to_string();
                    field(&mut this.teams, value);
                    this.save(cx);
                }
            }
        };
        let subs = vec![
            cx.subscribe_in(&client_id, window, on_change(|t, v| t.client_id = v)),
            cx.subscribe_in(&tenant, window, on_change(|t, v| t.tenant = v)),
            cx.subscribe_in(&todo_list, window, on_change(|t, v| t.todo_list = v)),
        ];
        let mut this = Self {
            teams,
            client_id,
            tenant,
            todo_list,
            signing_in: None,
            error: None,
            upcoming: None,
            loading: false,
            _subscriptions: subs,
        };
        this.load_upcoming(cx);
        this
    }

    /// Writes the Teams settings into the config on disk.
    fn save(&mut self, cx: &mut Context<Self>) {
        let mut config = Config::load();
        config.teams = self.teams.clone();
        if let Err(e) = config.save() {
            eprintln!("[mostalgia] saving settings: {e:#}");
        }
        cx.notify();
    }

    fn set(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut TeamsConfig)) {
        f(&mut self.teams);
        self.save(cx);
    }

    fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.signing_in.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.signing_in = Some(cancel.clone());
        self.error = None;
        cx.notify();
        let cfg = self.teams.clone();
        let task = cx.background_spawn(async move { auth::login(&cfg, &auth::open_browser, &cancel).map_err(|e| format!("{e:#}")) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.signing_in = None;
                match result {
                    Ok(account) => {
                        this.teams.account_name = account.name;
                        this.teams.account_email = account.email;
                        this.save(cx);
                        this.load_upcoming(cx);
                    }
                    Err(e) => this.error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = &self.signing_in {
            c.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = auth::logout() {
            self.error = Some(format!("{e:#}").into());
        }
        self.teams.account_name.clear();
        self.teams.account_email.clear();
        self.upcoming = None;
        self.save(cx);
    }

    fn load_upcoming(&mut self, cx: &mut Context<Self>) {
        if !ops::calendar_ready(&self.teams) || self.loading {
            return;
        }
        self.loading = true;
        let cfg = self.teams.clone();
        let task = cx.background_spawn(async move { ops::refresh_events(&cfg, 24 * 7).map_err(|e| format!("{e:#}")) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                this.upcoming = Some(result.map(|events| events.into_iter().filter(|e| e.is_teams()).take(5).collect()));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn switch(&self, id: &'static str, on: bool, cx: &mut Context<Self>, f: fn(&mut TeamsConfig, bool)) -> AnyElement {
        Switch::new(id)
            .checked(on)
            .on_change(cx.listener(move |this, v: &bool, _, cx| this.set(cx, |t| f(t, *v))))
            .into_any_element()
    }

    fn render_account(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let row = |label: &str, hint: Option<&str>, control: AnyElement| crate::settings::settings_row(label, hint, control, &theme);
        let has_id = self.teams.client_id().is_some();
        let valid_id = self.teams.client_id_valid();
        let signed_in = self.teams.signed_in();
        let status: SharedString = if self.signing_in.is_some() {
            tr("Warte auf die Anmeldung im Browser …").into()
        } else if signed_in {
            trf("Angemeldet als {}", &[&self.teams.account_label()]).into()
        } else if !has_id {
            tr("Erst die App bei Microsoft registrieren und ihre Client-ID eintragen.").into()
        } else {
            tr("Nicht angemeldet").into()
        };
        let action: AnyElement = if self.signing_in.is_some() {
            Button::new("teams-cancel").small().ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| this.cancel_sign_in(cx))).into_any_element()
        } else if signed_in {
            Button::new("teams-sign-out").small().ghost().label(tr("Abmelden")).on_click(cx.listener(|this, _, _, cx| this.sign_out(cx))).into_any_element()
        } else {
            Button::new("teams-sign-in")
                .small()
                .primary()
                .label(tr("Mit Microsoft anmelden"))
                .disabled(!valid_id)
                .on_click(cx.listener(|this, _, _, cx| this.sign_in(cx)))
                .into_any_element()
        };
        ui::card(cx)
            .child(div().font_semibold().child(tr("Microsoft-Konto")))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(if signed_in { IconName::Check } else { IconName::User }).small().text_color(if signed_in { theme.success } else { theme.muted_foreground }))
                    .child(div().flex_1().text_sm().child(status))
                    .child(action),
            )
            .when_some(self.error.clone(), |el, e| el.child(div().text_xs().text_color(theme.danger).child(e)))
            .child(row(
                tr("Client-ID"),
                Some(tr("Die Application (client) ID der App-Registrierung in Microsoft Entra ID.")),
                div().w(px(300.)).child(Input::new(&self.client_id).small()).into_any_element(),
            ))
            .when(has_id && !valid_id, |el| el.child(div().text_xs().text_color(theme.warning).child(tr("Die Client-ID ist eine GUID (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)"))))
            .child(row(
                tr("Mandant"),
                Some(tr("common für alle Konten, organizations nur Firmenkonten, oder die ID bzw. Domain deiner Organisation.")),
                div().w(px(300.)).child(Input::new(&self.tenant).small()).into_any_element(),
            ))
            .child(
                h_flex().child(
                    Button::new("teams-guide")
                        .small()
                        .ghost()
                        .icon(IconName::ExternalLink)
                        .label(tr("Anleitung: App bei Microsoft registrieren"))
                        .on_click(|_, _, cx| cx.open_url(GUIDE_URL)),
                ),
            )
    }

    fn render_calendar(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let row = |label: &str, hint: Option<&str>, control: AnyElement| crate::settings::settings_row(label, hint, control, &theme);
        let t = self.teams.clone();
        let mut card = ui::card(cx)
            .child(div().font_semibold().child(tr("Kalender & Aufnahmen")))
            .child(row(
                tr("Kalender lesen"),
                Some(tr("Erkennt, zu welcher Teams-Besprechung eine Aufnahme gehört.")),
                self.switch("teams-calendar", t.calendar_sync, cx, |t, v| t.calendar_sync = v),
            ))
            .child(row(
                tr("Nach der Besprechung benennen"),
                Some(tr("Die Aufnahme heißt wie der Termin, auch nach der Zusammenfassung.")),
                self.switch("teams-auto-name", t.auto_name, cx, |t, v| t.auto_name = v),
            ))
            .child(row(
                tr("Teilnehmende als Hinweise"),
                Some(tr("Namen und Begriffe aus der Einladung helfen Whisper; im Zweiergespräch bekommt „Andere“ den Namen.")),
                self.switch("teams-hints", t.attendee_hints, cx, |t, v| t.attendee_hints = v),
            ));
        if ops::calendar_ready(&t) {
            let list: AnyElement = match &self.upcoming {
                None => div().text_xs().text_color(theme.muted_foreground).child(tr("Lese den Kalender …")).into_any_element(),
                Some(Err(e)) => div().text_xs().text_color(theme.danger).child(e.clone()).into_any_element(),
                Some(Ok(events)) if events.is_empty() => {
                    div().text_xs().text_color(theme.muted_foreground).child(tr("Keine Teams-Besprechungen in den nächsten 7 Tagen")).into_any_element()
                }
                Some(Ok(events)) => v_flex()
                    .gap_1()
                    .children(events.iter().map(|e| {
                        let start = e.start.with_timezone(&Local);
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(Icon::new(IconName::Calendar).small().text_color(theme.muted_foreground))
                            .child(div().w(px(150.)).text_color(theme.muted_foreground).child(mostalgia_core::i18n::date_time(&start)))
                            .child(div().flex_1().min_w_0().truncate().child(e.recording_title()))
                    }))
                    .into_any_element(),
            };
            card = card.child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(div().text_sm().child(tr("Nächste Teams-Besprechungen")))
                            .child(
                                Button::new("teams-refresh")
                                    .small()
                                    .ghost()
                                    .icon(IconName::RefreshCw)
                                    .disabled(self.loading)
                                    .on_click(cx.listener(|this, _, _, cx| this.load_upcoming(cx))),
                            ),
                    )
                    .child(list),
            );
        }
        card
    }

    fn render_results(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let row = |label: &str, hint: Option<&str>, control: AnyElement| crate::settings::settings_row(label, hint, control, &theme);
        let t = self.teams.clone();
        ui::card(cx)
            .child(div().font_semibold().child(tr("Ergebnisse an Teams")))
            .child(row(
                tr("Zusammenfassung automatisch posten"),
                Some(tr("Sobald sie geschrieben ist, im Chat der Besprechung (nur Geschäfts- oder Schulkonten).")),
                self.switch("teams-auto-post", t.auto_post_summary, cx, |t, v| t.auto_post_summary = v),
            ))
            .child(row(
                tr("Aufgaben mitposten"),
                None,
                self.switch("teams-post-actions", t.post_action_items, cx, |t, v| t.post_action_items = v),
            ))
            .child(row(
                tr("To-Do-Liste"),
                Some(tr("Aufgaben aus Zusammenfassungen landen in dieser Liste von Microsoft To Do.")),
                div().w(px(220.)).child(Input::new(&self.todo_list).small()).into_any_element(),
            ))
            .child(row(
                tr("Nur meine Aufgaben"),
                Some(tr("Ohne Aufgaben, die jemand anderes übernommen hat.")),
                self.switch("teams-todo-mine", t.todo_only_mine, cx, |t, v| t.todo_only_mine = v),
            ))
            .child(row(
                tr("Teams-Transkripte abrufen"),
                Some(tr("Gleicht Sprechernamen mit dem Transkript von Teams ab. Braucht die Zustimmung eines Admins; danach neu anmelden.")),
                self.switch("teams-transcripts", t.transcripts, cx, |t, v| t.transcripts = v),
            ))
            .child(row(
                tr("Briefing vor Besprechungen"),
                Some(tr("Minuten vorher: Agenda, wer kommt und was vom letzten Mal offen ist (0 = aus).")),
                h_flex()
                    .gap_1()
                    .children([0u32, 5, 10, 15].into_iter().map(|m| {
                        Button::new(SharedString::from(format!("teams-brief-{m}")))
                            .small()
                            .label(if m == 0 { tr("Aus").to_string() } else { format!("{m}") })
                            .when(t.brief_minutes == m, |b| b.primary())
                            .when(t.brief_minutes != m, |b| b.ghost())
                            .on_click(cx.listener(move |this, _, _, cx| this.set(cx, |t| t.brief_minutes = m)))
                    }))
                    .into_any_element(),
            ))
    }
}

impl Render for TeamsSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_4().child(self.render_account(cx)).child(self.render_calendar(cx)).child(self.render_results(cx))
    }
}
