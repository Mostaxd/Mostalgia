//! The decision log screen (optional, Settings → AI): every decision
//! across meetings, newest first, with search and filters; each opens its
//! meeting at the line. Entries can be edited, deleted or marked
//! superseded; the list copies as Markdown.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::decisions::{self, Entry, Filter};
use mostalgia_core::i18n::{self, tr, trf};
use mostalgia_core::paths;
use mostalgia_core::store::Store;

use crate::ui;

pub enum DecisionsEvent {
    /// Open a meeting's transcript at a moment (`None`: its start).
    Open { meeting_id: i64, secs: Option<f64> },
}

impl EventEmitter<DecisionsEvent> for DecisionsView {}

pub struct DecisionsView {
    query: Entity<InputState>,
    from: Entity<InputState>,
    to: Entity<InputState>,
    meeting: Option<(i64, String)>,
    person: Option<String>,
    entries: Vec<Entry>,
    /// Being edited: the entry and its text.
    editing: Option<(i64, Entity<InputState>)>,
    /// "Build the log from existing meetings": (done, of) while it runs.
    building: Option<(usize, usize)>,
    notice: Option<SharedString>,
    error: Option<SharedString>,
    _subs: Vec<Subscription>,
}

impl DecisionsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Entscheidungen durchsuchen")));
        let from = cx.new(|cx| InputState::new(window, cx).placeholder(tr("von (2026-10-01)")));
        let to = cx.new(|cx| InputState::new(window, cx).placeholder(tr("bis (2026-10-31)")));
        let subs = [&query, &from, &to]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        this.reload(cx);
                    }
                })
            })
            .collect();
        let mut this = Self { query, from, to, meeting: None, person: None, entries: Vec::new(), editing: None, building: None, notice: None, error: None, _subs: subs };
        this.reload(cx);
        this
    }

    fn filter(&self, cx: &App) -> Filter {
        let date = |input: &Entity<InputState>, end: bool| {
            let v = input.read(cx).value().trim().to_string();
            mostalgia_core::media_import::parse_date(&v).map(|d| if end && v.len() <= 10 { d + chrono::Duration::hours(12) } else { d - chrono::Duration::hours(if v.len() <= 10 { 12 } else { 0 }) })
        };
        Filter {
            query: self.query.read(cx).value().to_string(),
            from: date(&self.from, false),
            to: date(&self.to, true),
            meeting: self.meeting.as_ref().map(|m| m.0),
            person: self.person.clone(),
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let filter = self.filter(cx);
        match Store::open(&paths::database()).and_then(|s| decisions::list(&s, &filter)) {
            Ok(entries) => self.entries = entries,
            Err(e) => self.error = Some(format!("{e:#}").into()),
        }
        cx.notify();
    }

    /// Fills the log from every summarized meeting, with progress.
    pub fn build(&mut self, cx: &mut Context<Self>) {
        if self.building.is_some() {
            return;
        }
        let ids = match Store::open(&paths::database()).and_then(|s| decisions::summarized_meetings(&s)) {
            Ok(ids) => ids,
            Err(e) => {
                self.error = Some(format!("{e:#}").into());
                cx.notify();
                return;
            }
        };
        self.building = Some((0, ids.len()));
        self.error = None;
        cx.notify();
        let (tx, rx) = crossbeam_channel::unbounded::<Result<usize, String>>();
        std::thread::spawn(move || {
            let run = || -> anyhow::Result<()> {
                let store = Store::open(&paths::database())?;
                for id in ids {
                    let added = match store.get(id)? {
                        Some(m) => decisions::add_meeting(&store, &m)?,
                        None => 0,
                    };
                    let _ = tx.send(Ok(added));
                }
                Ok(())
            };
            if let Err(e) = run() {
                let _ = tx.send(Err(format!("{e:#}")));
            }
        });
        cx.spawn(async move |this, cx| {
            let mut added = 0;
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
                let mut done = false;
                let mut failed = None;
                let mut steps = 0;
                loop {
                    match rx.try_recv() {
                        Ok(Ok(n)) => {
                            added += n;
                            steps += 1;
                        }
                        Ok(Err(e)) => failed = Some(e),
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            done = true;
                            break;
                        }
                    }
                }
                let alive = this
                    .update(cx, |this, cx| {
                        if let Some((d, _)) = this.building.as_mut() {
                            *d += steps;
                        }
                        if let Some(e) = failed {
                            this.error = Some(trf("Entscheidungslog: {}", &[&e]).into());
                        }
                        if done {
                            let (_, of) = this.building.take().unwrap_or_default();
                            let mut config = mostalgia_core::config::Config::load();
                            config.decision_log_built = true;
                            if let Err(e) = config.save() {
                                this.error = Some(format!("{e:#}").into());
                            }
                            this.notice = Some(trf("{} Entscheidungen aus {} Besprechungen.", &[&added, &of]).into());
                            this.reload(cx);
                        }
                        cx.notify();
                    })
                    .is_ok();
                if done || !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Screenshots: the log filled right away (no background thread).
    #[doc(hidden)]
    pub fn demo_build_now(&mut self, cx: &mut Context<Self>) {
        if let Ok(store) = Store::open(&paths::database()) {
            for id in decisions::summarized_meetings(&store).unwrap_or_default() {
                if let Ok(Some(m)) = store.get(id) {
                    let _ = decisions::add_meeting(&store, &m);
                }
            }
        }
        let mut config = mostalgia_core::config::Config::load();
        config.decision_log_built = true;
        let _ = config.save();
        self.reload(cx);
    }

    fn with_store(&mut self, f: impl FnOnce(&Store) -> anyhow::Result<()>, cx: &mut Context<Self>) {
        if let Err(e) = Store::open(&paths::database()).and_then(|s| f(&s)) {
            self.error = Some(format!("{e:#}").into());
        }
        self.reload(cx);
    }

    fn begin_edit(&mut self, e: &Entry, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |s, cx| s.set_value(e.text.clone(), window, cx));
        self.editing = Some((e.id, input));
        cx.notify();
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some((id, input)) = self.editing.take() else { return };
        let text = input.read(cx).value().trim().to_string();
        if !text.is_empty() {
            self.with_store(|s| decisions::edit(s, id, &text), cx);
        }
    }

    fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::notification::Notification;
        if self.entries.is_empty() {
            window.push_notification(Notification::info(tr("Keine Entscheidungen zum Kopieren.")), cx);
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(decisions::markdown(&self.entries)));
        window.push_notification(Notification::success(trf("{} Entscheidungen kopiert", &[&self.entries.len()])), cx);
    }

    fn render_entry(&self, ix: usize, e: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = e.id;
        let entry = e.clone();
        let editing = self.editing.as_ref().filter(|(eid, _)| *eid == id).map(|(_, i)| i.clone());
        let superseded_by = e.superseded_by.and_then(|by| self.entries.iter().find(|x| x.id == by)).map(|x| x.text.clone());
        // Newer decisions this one could have been replaced by.
        let newer: Vec<(i64, String)> = self.entries.iter().filter(|x| x.started_at > e.started_at || (x.meeting_id == e.meeting_id && x.at_secs > e.at_secs)).filter(|x| x.id != id).take(12).map(|x| (x.id, x.text.clone())).collect();
        let weak = cx.entity().downgrade();
        let (meeting_id, secs, is_superseded) = (e.meeting_id, e.at_secs, e.superseded);
        v_flex()
            .id(("decision", ix))
            .p_3()
            .gap_1p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .when(e.superseded, |el| el.opacity(0.6))
            .child(match editing {
                Some(input) => h_flex()
                    .gap_2()
                    .child(Input::new(&input).small().flex_1())
                    .child(Button::new(("save", ix)).small().primary().label(tr("Speichern")).on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))))
                    .child(Button::new(("cancel", ix)).small().ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| {
                        this.editing = None;
                        cx.notify();
                    })))
                    .into_any_element(),
                None => div()
                    .text_size(ui::transcript_rems(1.))
                    .when(e.superseded, |el| el.line_through())
                    .child(ui::bidi_text(e.text.clone(), true))
                    .into_any_element(),
            })
            .when_some(superseded_by.filter(|_| e.superseded), |el, by| {
                el.child(div().text_xs().text_color(theme.warning).child(trf("Ersetzt durch: {}", &[&by])))
            })
            .when(e.superseded && e.superseded_by.is_none(), |el| el.child(div().text_xs().text_color(theme.warning).child(tr("Ersetzt"))))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_3()
                    .gap_y_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        Button::new(("open", ix))
                            .xsmall()
                            .ghost()
                            .icon(IconName::ArrowUpRight)
                            .label(match secs {
                                Some(s) => format!("{} · {}", e.meeting_title, ui::hms(s)),
                                None => e.meeting_title.clone(),
                            })
                            .tooltip(tr("In der Besprechung zeigen (und abspielen, wenn das Audio noch da ist)"))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(DecisionsEvent::Open { meeting_id, secs }))),
                    )
                    .child(i18n::date_time(&e.started_at))
                    .when_some(e.speaker.clone(), |el, s| el.child(h_flex().gap_1().child(Icon::new(IconName::User).xsmall()).child(s)))
                    .when(e.edited, |el| el.child(tr("bearbeitet")))
                    .child(div().flex_1())
                    .child(Button::new(("edit", ix)).xsmall().ghost().icon(IconName::Pencil).tooltip(tr("Bearbeiten")).on_click(cx.listener(move |this, _, window, cx| this.begin_edit(&entry, window, cx))))
                    .child(
                        Button::new(("supersede", ix))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Replace)
                            .tooltip(tr("Als ersetzt markieren"))
                            .dropdown_menu(move |menu, _, _| {
                                let mut menu = menu;
                                if is_superseded {
                                    let w = weak.clone();
                                    menu = menu.item(PopupMenuItem::new(tr("Nicht mehr ersetzt")).on_click(move |_, _, cx| {
                                        w.update(cx, |this, cx| this.with_store(|s| decisions::set_superseded(s, id, false, None), cx)).ok();
                                    }));
                                }
                                let w = weak.clone();
                                menu = menu.item(PopupMenuItem::new(tr("Ersetzt (ohne Verweis)")).on_click(move |_, _, cx| {
                                    w.update(cx, |this, cx| this.with_store(|s| decisions::set_superseded(s, id, true, None), cx)).ok();
                                }));
                                if !newer.is_empty() {
                                    menu = menu.separator();
                                }
                                for (by, text) in &newer {
                                    let (w, by) = (weak.clone(), *by);
                                    let label: String = text.chars().take(70).collect();
                                    menu = menu.item(PopupMenuItem::new(trf("Ersetzt durch: {}", &[&label])).on_click(move |_, _, cx| {
                                        w.update(cx, |this, cx| this.with_store(|s| decisions::set_superseded(s, id, true, Some(by)), cx)).ok();
                                    }));
                                }
                                menu
                            }),
                    )
                    .child(Button::new(("delete", ix)).xsmall().ghost().icon(IconName::Trash).tooltip(tr("Löschen")).on_click(cx.listener(move |this, _, _, cx| {
                        this.with_store(|s| decisions::delete(s, id), cx);
                    }))),
            )
            .into_any_element()
    }
}

impl Render for DecisionsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let config = mostalgia_core::config::Config::load();
        // Meetings and people to filter by (from everything in the log).
        let all = Store::open(&paths::database()).and_then(|s| decisions::list(&s, &Filter::default())).unwrap_or_default();
        let mut meetings: Vec<(i64, String)> = Vec::new();
        let mut people: Vec<String> = Vec::new();
        for e in &all {
            if !meetings.iter().any(|(id, _)| *id == e.meeting_id) {
                meetings.push((e.meeting_id, e.meeting_title.clone()));
            }
            if let Some(s) = &e.speaker
                && !people.contains(s)
            {
                people.push(s.clone());
            }
        }
        let weak = cx.entity().downgrade();
        let meeting_label = self.meeting.as_ref().map_or_else(|| tr("Alle Besprechungen").to_string(), |m| m.1.clone());
        let meeting_menu = {
            let weak = weak.clone();
            Button::new("filter-meeting").small().outline().icon(IconName::Calendar).label(meeting_label).dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.));
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(tr("Alle Besprechungen")).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| {
                        this.meeting = None;
                        this.reload(cx);
                    })
                    .ok();
                }));
                for m in &meetings {
                    let (w, m) = (weak.clone(), m.clone());
                    menu = menu.item(PopupMenuItem::new(m.1.clone()).on_click(move |_, _, cx| {
                        let m = m.clone();
                        w.update(cx, |this, cx| {
                            this.meeting = Some(m);
                            this.reload(cx);
                        })
                        .ok();
                    }));
                }
                menu
            })
        };
        let person_menu = {
            let weak = weak.clone();
            Button::new("filter-person").small().outline().icon(IconName::User).label(self.person.clone().unwrap_or_else(|| tr("Alle Personen").to_string())).dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(tr("Alle Personen")).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| {
                        this.person = None;
                        this.reload(cx);
                    })
                    .ok();
                }));
                for p in &people {
                    let (w, p) = (weak.clone(), p.clone());
                    menu = menu.item(PopupMenuItem::new(p.clone()).on_click(move |_, _, cx| {
                        let p = p.clone();
                        w.update(cx, |this, cx| {
                            this.person = Some(p);
                            this.reload(cx);
                        })
                        .ok();
                    }));
                }
                menu
            })
        };
        let pending = (!config.decision_log_built).then(|| Store::open(&paths::database()).and_then(|s| decisions::summarized_meetings(&s)).map(|v| v.len()).unwrap_or(0)).filter(|n| *n > 0);
        let entries: Vec<Entry> = self.entries.clone();
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .px_8()
                    .pt_6()
                    .pb_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().flex_1().text_2xl().font_semibold().child(tr("Entscheidungen")))
                            .child(Button::new("copy-decisions").small().ghost().icon(IconName::Copy).label(tr("Als Markdown kopieren")).on_click(cx.listener(|this, _, window, cx| this.copy(window, cx)))),
                    )
                    .child(div().text_sm().text_color(theme.muted_foreground).child(tr("Alle Entscheidungen aus den Zusammenfassungen deiner Besprechungen, neueste zuerst.")))
                    .child(Input::new(&self.query).small().prefix(Icon::new(IconName::Search).small()))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .child(meeting_menu)
                            .child(person_menu)
                            .child(div().w(px(150.)).child(Input::new(&self.from).small()))
                            .child(div().w(px(150.)).child(Input::new(&self.to).small())),
                    ),
            )
            .when_some(self.building, |el, (done, of)| {
                el.child(
                    v_flex()
                        .mx_8()
                        .mt_4()
                        .p_3()
                        .gap_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .child(h_flex().gap_2().text_sm().child(gpui_kit::component::spinner::Spinner::new().small()).child(trf("Log wird erstellt … {} von {} Besprechungen", &[&done, &of])))
                        .child(ui::progress_bar(Some(done as f32 / of.max(1) as f32), cx)),
                )
            })
            .when_some(pending.filter(|_| self.building.is_none()), |el, n| {
                el.child(
                    h_flex()
                        .mx_8()
                        .mt_4()
                        .p_3()
                        .gap_3()
                        .rounded(theme.radius)
                        .bg(theme.primary.opacity(0.08))
                        .text_sm()
                        .child(div().flex_1().child(tr("Das Log füllt sich mit neuen Besprechungen. Frühere Besprechungen einmalig übernehmen?")))
                        .child(Button::new("build-log").small().primary().icon(IconName::ListChecks).label(trf("Log aus vorhandenen Besprechungen erstellen ({})", &[&n])).on_click(cx.listener(|this, _, _, cx| this.build(cx)))),
                )
            })
            .when_some(self.notice.clone(), |el, n| el.child(div().mx_8().mt_3().text_sm().text_color(theme.success).child(n)))
            .when_some(self.error.clone(), |el, e| el.child(div().mx_8().mt_3().p_2().rounded(theme.radius).bg(theme.danger.opacity(0.1)).text_color(theme.danger).text_sm().child(e)))
            .child(
                v_flex()
                    .id("decisions-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_8()
                    .py_4()
                    .gap_2()
                    .when(entries.is_empty() && self.building.is_none(), |el| {
                        el.child(div().py_16().text_center().text_color(theme.muted_foreground).child(if all.is_empty() {
                            tr("Noch keine Entscheidungen. Sie kommen aus den Zusammenfassungen – neue Besprechungen landen automatisch hier.")
                        } else {
                            tr("Keine Entscheidung passt zu diesen Filtern.")
                        }))
                    })
                    .children(entries.iter().enumerate().map(|(ix, e)| self.render_entry(ix, e, cx))),
            )
    }
}
