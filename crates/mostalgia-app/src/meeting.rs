//! Meeting detail: summary, transcript (click to play, rename speakers,
//! edit lines) and an audio waveform.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::text::TextView;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::component::menu::DropdownMenu;
use schemars::JsonSchema;
use serde::Deserialize;
use mostalgia_core::config::{Config, SttChoice};
use mostalgia_core::playback::{self, Player};
use mostalgia_core::store::{Meeting, Store};
use mostalgia_core::i18n::{self, tr, trf};
use mostalgia_core::{Source, cloud_stt, finalize, paths, summary};

use crate::recording::SharedEngine;
use crate::ui;

/// Bars per waveform row.
const WAVE_BUCKETS: usize = 320;

/// Export the open meeting in a format (by file extension).
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = mostalgia, no_json)]
pub struct ExportAs(pub String);

pub enum MeetingEvent {
    /// Title, summary or transcript changed; the library should refresh.
    Changed,
    Deleted,
    /// Asks the app to delete this meeting (after its confirmation dialog).
    RequestDelete(i64),
}

impl EventEmitter<MeetingEvent> for MeetingView {}

enum Edit {
    /// Renaming the speaker currently labelled `.0`.
    Speaker(String, Entity<InputState>),
    /// Editing the text of segment `.0`.
    Line(i64, Entity<InputState>),
    Title(Entity<InputState>),
}

/// One engine to pick from in the "transcribe again" dialog.
struct EngineOption {
    choice: SttChoice,
    label: String,
    /// The connection's name for cloud engines (where the audio goes).
    cloud: Option<String>,
    cost: Option<f64>,
}

struct Retranscribe {
    options: Vec<EngineOption>,
    selected: usize,
}

pub struct MeetingView {
    pub id: i64,
    meeting: Option<Meeting>,
    tab: usize,
    player: Option<Player>,
    position: Option<f64>,
    peaks: Option<Vec<(Source, Vec<f32>)>>,
    summarizing: bool,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    edit: Option<Edit>,
    scroll: ScrollHandle,
    /// A jump target (segment id) to scroll into view once it's laid out;
    /// the flag is set when that happened.
    scroll_target: Option<(i64, std::rc::Rc<std::cell::Cell<bool>>)>,
    /// The line a jump landed on, highlighted for a moment.
    flash: Option<(i64, std::time::Instant)>,
    /// The app's cached local Whisper, reused for transcribing again.
    engine: Option<SharedEngine>,
    /// The "transcribe again" dialog, while open.
    retranscribe: Option<Retranscribe>,
    /// Engine label while transcribing again.
    retranscribing: Option<SharedString>,
    /// Why the last "transcribe again" failed.
    transcribe_error: Option<SharedString>,
    /// The "Chat with Mori" section is folded away.
    chat_folded: bool,
    /// Notes tab: grouped by kind (else by time).
    notes_grouped: bool,
    _subscriptions: Vec<Subscription>,
}

impl MeetingView {
    pub fn new(id: i64, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            id,
            meeting: None,
            tab: 0,
            player: None,
            position: None,
            peaks: None,
            summarizing: false,
            error: None,
            notice: None,
            edit: None,
            scroll: ScrollHandle::new(),
            scroll_target: None,
            flash: None,
            engine: None,
            retranscribe: None,
            retranscribing: None,
            transcribe_error: None,
            chat_folded: false,
            notes_grouped: true,
            _subscriptions: Vec::new(),
        };
        this.reload(cx);
        if this.meeting.as_ref().is_some_and(|m| m.summary.is_none() && !m.segments.is_empty()) {
            this.tab = 1;
        }
        this
    }

    /// Shares the app's cached Whisper engine (so transcribing again doesn't
    /// load a second copy).
    pub fn with_engine(mut self, engine: SharedEngine) -> Self {
        self.engine = Some(engine);
        self
    }

    pub fn set_tab(&mut self, tab: usize, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        match Store::open(&paths::database()).and_then(|s| s.get(self.id)) {
            Ok(m) => self.meeting = m,
            Err(e) => self.error = Some(format!("{e:#}").into()),
        }
        cx.notify();
    }

    /// Summarize now if that's configured and possible (after a recording).
    pub fn auto_summarize(&mut self, cx: &mut Context<Self>) {
        let config = Config::load();
        let ready = self.meeting.as_ref().is_some_and(|m| m.summary.is_none() && !m.segments.is_empty());
        if config.auto_summary && ready && config.summary_connection().is_ok_and(|c| c.is_configured()) {
            self.summarize(cx);
        }
    }

    fn summarize(&mut self, cx: &mut Context<Self>) {
        if self.summarizing {
            return;
        }
        let req = match Config::load().summary_request().and_then(|req| req.connection.require_key().map(|_| req)) {
            Ok(req) => req,
            Err(e) => {
                // Errors from here already name the connection.
                self.error = Some(format!("{e:#}").into());
                cx.notify();
                return;
            }
        };
        self.summarizing = true;
        self.error = None;
        self.tab = 0;
        cx.notify();
        let id = self.id;
        let provider = req.connection.name.clone();
        let task = cx.background_spawn(async move {
            let store = Store::open(&paths::database())?;
            let meeting = store.get(id)?.ok_or_else(|| anyhow::anyhow!(tr("Besprechung nicht gefunden")))?;
            let s = summary::summarize(&req, &meeting)?;
            store.save_summary(id, &s.provider, &s.model, s.effort.as_deref(), &s.markdown)?;
            if let Some(title) = &s.title {
                store.set_title(id, title)?;
            }
            // Summary sections become searchable too.
            if let Err(e) = mostalgia_core::ask::index_meeting(id) {
                eprintln!("[mostalgia] search index: {e:#}");
            }
            anyhow::Ok(())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.summarizing = false;
                if let Err(e) = result {
                    this.error = Some(trf("Zusammenfassung mit {} fehlgeschlagen: {}", &[&provider, &format!("{e:#}")]).into());
                }
                this.reload(cx);
                cx.emit(MeetingEvent::Changed);
            })
            .ok();
        })
        .detach();
    }

    /// Opens the "transcribe again" dialog with the local engine and every
    /// configured cloud model, preselecting the final-pass engine.
    pub fn open_retranscribe(&mut self, cx: &mut Context<Self>) {
        let Some(m) = &self.meeting else { return };
        if self.retranscribing.is_some() || m.tracks().is_empty() {
            return;
        }
        let config = Config::load();
        let hours = m.duration_secs / 3600.;
        let mut options = vec![EngineOption {
            choice: SttChoice::local(),
            label: config.engine_label(&SttChoice::local()),
            cloud: None,
            cost: None,
        }];
        for conn in config.connections.iter().filter(|c| !c.stt_models().is_empty() && c.is_configured()) {
            for model in conn.stt_models() {
                options.push(EngineOption {
                    choice: SttChoice { connection: conn.id.clone(), model: model.id.to_string() },
                    label: format!("{} · {}", conn.name, model.id),
                    cloud: Some(conn.name.clone()),
                    cost: cloud_stt::estimated_cost(conn, model.id, hours),
                });
            }
        }
        let wanted = &config.final_engine;
        let selected = match options.iter().position(|o| o.choice == *wanted || o.choice.is_local() && wanted.is_local()) {
            Some(i) => i,
            // The configured engine isn't usable; list it anyway so starting
            // it shows why instead of quietly picking another.
            None => {
                options.push(EngineOption {
                    choice: wanted.clone(),
                    label: config.engine_label(wanted),
                    cloud: Some(config.connection(&wanted.connection).map_or(wanted.connection.clone(), |c| c.name.clone())),
                    cost: None,
                });
                options.len() - 1
            }
        };
        self.retranscribe = Some(Retranscribe { options, selected });
        cx.notify();
    }

    fn start_retranscribe(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.retranscribe.take() else { return };
        let Some(m) = &self.meeting else { return };
        let option = &dialog.options[dialog.selected];
        let choice = option.choice.clone();
        let label: SharedString = option.label.clone().into();
        let language = m.language.clone();
        self.player = None;
        self.position = None;
        self.edit = None;
        self.transcribe_error = None;
        self.retranscribing = Some(label.clone());
        cx.notify();

        let id = self.id;
        let slot = self.engine.clone();
        let task = cx.background_spawn(async move {
            let config = Config::load();
            let vad = config.vad_model_path();
            let opts = finalize::FinalPassOptions {
                vad_model: vad.exists().then_some(vad),
                diarize: config.diarize,
                denoise_mic: config.denoise,
            };
            let cached = slot.as_ref().filter(|_| choice.is_local()).and_then(|s| s.lock().unwrap().take());
            let mut engine = match cached {
                Some(engine) => engine,
                None => finalize::engine_for(&config, &choice)?,
            };
            let result = engine.set_language(&language).and_then(|_| finalize::retranscribe(id, &mut engine, &opts));
            if let Some(slot) = slot.filter(|_| !engine.is_cloud()) {
                *slot.lock().unwrap() = Some(engine);
            }
            result
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.retranscribing = None;
                match result {
                    Ok(lines) => {
                        this.notice = Some(trf("Neu transkribiert mit {}: {} Zeilen", &[&label, &lines]).into());
                        this.tab = 1;
                    }
                    Err(e) => {
                        this.transcribe_error = Some(trf("Transkription mit {} fehlgeschlagen: {}", &[&label, &format!("{e:#}")]).into());
                    }
                }
                this.reload(cx);
                cx.emit(MeetingEvent::Changed);
            })
            .ok();
        })
        .detach();
    }

    /// Progress of "transcribe again", or the transcript's engine error with
    /// a way to transcribe again; shown above every tab.
    fn render_transcript_status(&mut self, m: &Meeting, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        if let Some(label) = &self.retranscribing {
            return Some(
                h_flex()
                    .mx_8()
                    .mb_3()
                    .p_3()
                    .gap_3()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .text_sm()
                    .child(gpui_kit::component::spinner::Spinner::new().small())
                    .child(div().font_medium().child(trf("Transkribiere mit {} …", &[label])))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Bei langen Aufnahmen kann das einige Minuten dauern.")))
                    .into_any_element(),
            );
        }
        let transient = self.transcribe_error.is_some();
        let error: SharedString = self.transcribe_error.clone().or_else(|| m.transcript_error.clone().map(Into::into))?;
        let has_audio = !m.tracks().is_empty();
        Some(
            h_flex()
                .mx_8()
                .mb_3()
                .p_3()
                .gap_3()
                .items_start()
                .rounded(theme.radius)
                .bg(theme.danger.opacity(0.1))
                .text_sm()
                .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.danger).mt(px(2.)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_color(theme.danger).font_medium().child(error))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(if transient {
                            tr("Das bisherige Transkript bleibt unverändert.")
                        } else {
                            tr("Die Aufnahme ist sicher gespeichert. Mit „Erneut transkribieren“ wählst du ein Modell und versuchst es nochmal.")
                        })),
                )
                .child(
                    Button::new("retranscribe-banner")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(tr("Erneut transkribieren"))
                        .disabled(!has_audio)
                        .when(!has_audio, |b| b.tooltip(tr("Audiodateien gelöscht")))
                        .on_click(cx.listener(|this, _, _, cx| this.open_retranscribe(cx))),
                )
                .when(transient, |el| {
                    el.child(Button::new("dismiss-transcribe-err").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                        this.transcribe_error = None;
                        cx.notify();
                    })))
                })
                .into_any_element(),
        )
    }

    fn render_retranscribe_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.retranscribe.as_ref()?;
        let theme = cx.theme().clone();
        let rows = dialog.options.iter().enumerate().map(|(i, o)| {
            let selected = i == dialog.selected;
            h_flex()
                .id(("engine-option", i))
                .gap_3()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .border(ui::border_width(cx))
                .border_color(if selected { theme.primary } else { theme.border })
                .when(selected, |el| el.bg(theme.primary.opacity(0.06)))
                .cursor_pointer()
                .hover(|s| s.bg(theme.muted.opacity(0.5)))
                .child(
                    div()
                        .flex_none()
                        .size(px(16.))
                        .rounded_full()
                        .border_2()
                        .border_color(if selected { theme.primary } else { theme.muted_foreground })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |el| el.child(div().size(px(6.)).rounded_full().bg(theme.primary))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_medium().child(o.label.clone()))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(match &o.cloud {
                            Some(name) => trf("Audio geht an {}", &[name]),
                            None => tr("Auf diesem Rechner, nichts verlässt ihn").to_string(),
                        })),
                )
                .when_some(o.cost, |el, usd| el.child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(cost_label(usd))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(d) = &mut this.retranscribe {
                        d.selected = i;
                        cx.notify();
                    }
                }))
        });
        let selected = &dialog.options[dialog.selected];
        let warning = selected.cloud.as_ref().map(|name| trf("Audio geht an {}", &[name]));
        Some(
            div()
                .id("retranscribe-overlay")
                .absolute()
                .inset_0()
                .bg(gpui_kit::black().opacity(0.45))
                .flex()
                .items_center()
                .justify_center()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.retranscribe = None;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("retranscribe-dialog")
                        .w(px(480.))
                        .max_h(relative(0.9))
                        .p_5()
                        .gap_3()
                        .rounded(theme.radius_lg)
                        .border(ui::border_width(cx))
                        .border_color(theme.border)
                        .bg(theme.background)
                        .shadow_lg()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Icon::new(IconName::RefreshCw).text_color(theme.primary))
                                .child(div().text_lg().font_semibold().child(tr("Erneut transkribieren"))),
                        )
                        .child(div().text_sm().text_color(theme.muted_foreground).child(tr("Mit welchem Modell soll die Aufnahme neu transkribiert werden?")))
                        .child(v_flex().id("engine-options").gap_2().min_h_0().overflow_y_scroll().children(rows))
                        .child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.warning)
                                .child(Icon::new(IconName::TriangleAlert).xsmall())
                                .child(tr("Ersetzt das Transkript. Umbenannte Sprecher werden zurückgesetzt.")),
                        )
                        .when_some(warning, |el, w| {
                            el.child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(Icon::new(IconName::Globe).xsmall())
                                    .child(w),
                            )
                        })
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("retranscribe-cancel").ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| {
                                    this.retranscribe = None;
                                    cx.notify();
                                })))
                                .child(
                                    Button::new("retranscribe-start")
                                        .primary()
                                        .icon(IconName::RefreshCw)
                                        .label(tr("Transkribieren"))
                                        .on_click(cx.listener(|this, _, _, cx| this.start_retranscribe(cx))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Shows the transcript at `secs` and plays from there (for links
    /// from search and "ask your meetings" answers).
    /// Opens the transcript at `secs`: scrolls that line into view,
    /// highlights it and plays from there.
    pub fn jump_to(&mut self, secs: f64, cx: &mut Context<Self>) {
        self.tab = 1;
        if let Some(seg) = self.meeting.as_ref().and_then(|m| {
            m.segments.iter().rev().find(|s| s.start_secs <= secs + 0.5).or(m.segments.first())
        }) {
            self.scroll_target = Some((seg.id, Default::default()));
            self.flash = Some((seg.id, std::time::Instant::now()));
            // Fade the highlight out again.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(2200)).await;
                this.update(cx, |this, cx| {
                    this.flash = None;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        self.play_from(secs, cx);
    }

    fn play_from(&mut self, secs: f64, cx: &mut Context<Self>) {
        let Some(m) = &self.meeting else { return };
        let tracks: Vec<_> = m.tracks().into_iter().map(|(_, p)| p).collect();
        self.player = None;
        match Player::play(&tracks, secs) {
            Ok(p) => {
                self.player = Some(p);
                self.position = Some(secs);
                cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_millis(100)).await;
                        let playing = this
                            .update(cx, |this, cx| {
                                let pos = this.player.as_ref().filter(|p| !p.is_finished()).map(|p| p.position_secs());
                                if pos.is_none() {
                                    this.player = None;
                                }
                                this.position = pos;
                                cx.notify();
                                pos.is_some()
                            })
                            .unwrap_or(false);
                        if !playing {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(e) => self.error = Some(trf("Wiedergabe: {}", &[&format!("{e:#}")]).into()),
        }
        cx.notify();
    }

    fn stop_playback(&mut self, cx: &mut Context<Self>) {
        self.player = None;
        self.position = None;
        cx.notify();
    }

    fn load_peaks(&mut self, m: &Meeting, cx: &mut Context<Self>) {
        if self.peaks.is_some() {
            return;
        }
        self.peaks = Some(Vec::new());
        let tracks = m.tracks();
        let task = cx.background_spawn(async move {
            tracks
                .into_iter()
                .filter_map(|(s, p)| playback::peaks(&p, WAVE_BUCKETS).ok().map(|v| (s, v)))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let peaks = task.await;
            this.update(cx, |this, cx| {
                this.peaks = Some(peaks);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn begin_edit(&mut self, edit: impl FnOnce(Entity<InputState>) -> Edit, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |state, cx| state.set_value(value, window, cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => this.commit_edit(cx),
            _ => {}
        });
        self._subscriptions = vec![sub];
        self.edit = Some(edit(input));
        cx.notify();
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.take() else { return };
        let result = Store::open(&paths::database()).and_then(|store| match &edit {
            Edit::Speaker(current, input) => store.rename_speaker(self.id, current, &input.read(cx).value()).map(|_| ()),
            Edit::Line(seg, input) => store.update_segment_text(*seg, &input.read(cx).value()),
            Edit::Title(input) => {
                let title = input.read(cx).value();
                if title.trim().is_empty() { Ok(()) } else { store.set_title(self.id, title.trim()) }
            }
        });
        if let Err(e) = result {
            self.error = Some(format!("{e:#}").into());
        }
        self._subscriptions.clear();
        self.reload(cx);
        cx.emit(MeetingEvent::Changed);
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.edit = None;
        self._subscriptions.clear();
        cx.notify();
    }

    fn export(&mut self, action: &ExportAs, _: &mut Window, cx: &mut Context<Self>) {
        use mostalgia_core::export::{self, Format};
        let (Some(m), Some(format)) = (&self.meeting, Format::from_extension(&action.0)) else { return };
        let dir = dirs_documents();
        let suggested = export::suggested_name(m, format);
        let picked = cx.prompt_for_new_path(&dir, Some(&suggested));
        let id = self.id;
        cx.spawn(async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
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
            let result = Store::open(&paths::database()).and_then(|s| {
                let m = s.get(id)?.ok_or_else(|| anyhow::anyhow!(tr("Besprechung nicht gefunden")))?;
                export::write(&m, format, &path)
            });
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.notice = Some(trf("Exportiert: {}", &[&path.display()]).into()),
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        // The app asks for confirmation (and whether to delete the audio).
        self.player = None;
        cx.emit(MeetingEvent::RequestDelete(self.id));
        cx.notify();
    }

    fn render_header(&mut self, m: &Meeting, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let title: AnyElement = match &self.edit {
            Some(Edit::Title(input)) => h_flex()
                .gap_2()
                .child(Input::new(input).w(px(420.)))
                .child(Button::new("title-save").small().primary().label(tr("Speichern")).on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))))
                .child(Button::new("title-cancel").small().ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))))
                .into_any_element(),
            _ => {
                let current = m.title.clone();
                h_flex()
                    .id("title")
                    .gap_2()
                    .group("title")
                    .child(
                        div()
                            .min_w_0()
                            .text_2xl()
                            .font_semibold()
                            .when(ui::is_rtl(&m.title), |el| el.flex_1())
                            .child(ui::bidi_text(m.title.clone(), true)),
                    )
                    .child(
                        Button::new("title-edit")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Pencil)
                            .tooltip(tr("Titel ändern"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_edit(Edit::Title, current.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            }
        };
        let speakers: Vec<String> = {
            let mut v: Vec<String> = Vec::new();
            for s in &m.segments {
                let l = s.speaker_label().to_string();
                if !v.contains(&l) {
                    v.push(l);
                }
            }
            v
        };
        h_flex()
            .px_8()
            .pt_6()
            .pb_4()
            .gap_4()
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(title)
                    .child(
                        h_flex()
                            .gap_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(h_flex().gap_1().child(Icon::new(IconName::Calendar).xsmall()).child(i18n::date_time(&m.started_at)))
                            .child(h_flex().gap_1().child(Icon::new(IconName::Clock).xsmall()).child(ui::minutes(m.duration_secs)))
                            .child(h_flex().gap_1().child(Icon::new(IconName::Users).xsmall()).child(trf("{} Sprecher", &[&speakers.len()]))),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("export")
                            .small()
                            .ghost()
                            .icon(IconName::Download)
                            .label(tr("Exportieren"))
                            .dropdown_menu(|menu, _, _| {
                                let mut menu = menu;
                                for f in mostalgia_core::export::Format::ALL {
                                    menu = menu.menu(tr(f.label()), Box::new(ExportAs(f.extension().into())));
                                }
                                menu
                            }),
                    )
                    .when(self.player.is_some(), |el| {
                        el.child(
                            Button::new("stop-play")
                                .icon(IconName::Square)
                                .label(tr("Wiedergabe stoppen"))
                                .outline()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.stop_playback(cx))),
                        )
                    })
                    .when(self.player.is_none(), |el| {
                        el.child(
                            Button::new("play")
                                .icon(IconName::Play)
                                .label(tr("Abspielen"))
                                .outline()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.play_from(0.0, cx))),
                        )
                    })
                    .child({
                        let has_audio = !m.tracks().is_empty();
                        Button::new("retranscribe")
                            .small()
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .tooltip(if has_audio { tr("Erneut transkribieren") } else { tr("Audiodateien gelöscht") })
                            .disabled(!has_audio || self.retranscribing.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.open_retranscribe(cx)))
                    })
                    .child(
                        Button::new("delete")
                            .small()
                            .ghost()
                            .icon(IconName::Trash)
                            .tooltip(tr("Löschen…"))
                            .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
                    )
            )
    }

    fn render_summary(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        self.render_summary_body(m, cx)
    }

    /// Everything Mori produced: her notes (grouped by kind or by time,
    /// pinned first within a group) and the chat from the recording.
    fn render_notes_tab(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if m.notes.is_empty() && m.chat.is_empty() {
            return v_flex()
                .pt_16()
                .items_center()
                .gap_2()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::FileText).large())
                .child(div().font_medium().child(tr("Keine Notizen für diese Besprechung")))
                .child(div().text_sm().child(tr("Mori war bei dieser Aufnahme nicht dabei. Schalte sie unter Einstellungen → Darstellung → Mori ein.")))
                .into_any_element();
        }
        let grouped = self.notes_grouped;
        let toggle = h_flex()
            .gap_1()
            .child(
                Button::new("notes-by-kind")
                    .xsmall()
                    .label(tr("Nach Art"))
                    .when(grouped, |b| b.primary())
                    .when(!grouped, |b| b.ghost())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.notes_grouped = true;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("notes-by-time")
                    .xsmall()
                    .label(tr("Nach Zeit"))
                    .when(!grouped, |b| b.primary())
                    .when(grouped, |b| b.ghost())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.notes_grouped = false;
                        cx.notify();
                    })),
            );
        v_flex()
            .gap_6()
            .when(!m.notes.is_empty(), |el| el.child(h_flex().justify_end().child(toggle)).child(self.render_notes(m, cx)))
            .when(!m.chat.is_empty(), |el| el.child(self.render_chat(m, cx)))
            .into_any_element()
    }

    /// The questions asked to Mori during the recording, with her answers;
    /// a cited time plays the meeting from there.
    fn render_chat(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let folded = self.chat_folded;
        let header = h_flex()
            .id("mori-chat-toggle")
            .gap_1p5()
            .cursor_pointer()
            .child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall().text_color(theme.muted_foreground))
            .child(ui::section_title(tr("CHAT MIT MORI"), cx))
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!("({})", m.chat.len())))
            .on_click(cx.listener(|this, _, _, cx| {
                this.chat_folded = !this.chat_folded;
                cx.notify();
            }));
        let weak = cx.entity().downgrade();
        let turns = m.chat.iter().enumerate().map(|(i, t)| {
            let weak = weak.clone();
            v_flex()
                .gap_1()
                .pl_5()
                .child(ui::bidi_text(t.question.clone(), true).text_sm().font_medium())
                .child(
                    crate::ask_view::cited_answer(&t.answer, &format!("meeting-chat-{i}"), cx, move |target, _, cx| {
                        weak.update(cx, |this, cx| this.jump_to(target.secs(), cx)).ok();
                    })
                    .text_sm()
                    .text_color(theme.foreground.opacity(0.9)),
                )
        });
        v_flex()
            .gap_4()
            .child(header)
            .when(!folded, |el| el.children(turns))
            .into_any_element()
    }

    /// Mori's notes; a click plays the meeting from there.
    fn render_notes(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        use mostalgia_core::companion::PointKind;
        let theme = cx.theme().clone();
        let row = |n: &mostalgia_core::store::Note, cx: &mut Context<Self>| {
            let (id, at, pinned) = (n.id, n.at_secs, n.pinned);
            let kind = PointKind::parse(&n.kind);
            let color = match kind {
                Some(PointKind::Decision) => theme.success,
                Some(PointKind::Action) => theme.primary,
                Some(PointKind::Question) => theme.warning,
                _ => theme.info,
            };
            h_flex()
                .id(("mori-note", id as usize))
                .gap_3()
                .items_start()
                .px_2()
                .py_1p5()
                .rounded(theme.radius)
                .cursor_pointer()
                .hover(|s| s.bg(theme.muted))
                .child(
                    div()
                        .w(px(52.))
                        .flex_none()
                        .pt_0p5()
                        .text_xs()
                        .text_color(theme.primary)
                        .font_family(theme.mono_font_family.clone())
                        .child(ui::hms(at)),
                )
                .when(!self.notes_grouped, |el| {
                    el.child(
                        h_flex()
                            .flex_none()
                            .w(px(120.))
                            .pt_0p5()
                            .gap_1p5()
                            .text_xs()
                            .font_semibold()
                            .text_color(color)
                            .child(div().size(px(6.)).rounded_full().bg(color))
                            .child(kind.map_or(n.kind.clone(), |k| k.label().to_string())),
                    )
                })
                .child(ui::bidi_text(n.text.clone(), true).flex_1().min_w_0().text_sm())
                .child(
                    Button::new(("mori-note-pin", id as usize))
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(IconName::Bookmark).text_color(if pinned { color } else { theme.muted_foreground.opacity(0.6) }))
                        .tooltip(if pinned { tr("Angeheftet – lösen") } else { tr("Anheften") })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            match Store::open(&paths::database()).and_then(|s| s.set_note_pinned(id, !pinned)) {
                                Ok(()) => this.reload(cx),
                                Err(e) => {
                                    this.error = Some(format!("{e:#}").into());
                                    cx.notify();
                                }
                            }
                        })),
                )
                .child(
                    Button::new(("mori-note-delete", id as usize))
                        .xsmall()
                        .ghost()
                        .icon(IconName::X)
                        .tooltip(tr("Notiz entfernen"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            match Store::open(&paths::database()).and_then(|s| s.delete_note(id)) {
                                Ok(_) => this.reload(cx),
                                Err(e) => {
                                    this.error = Some(format!("{e:#}").into());
                                    cx.notify();
                                }
                            }
                        })),
                )
                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr("Ab hier abspielen")).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| this.jump_to(at, cx)))
                .into_any_element()
        };
        let mut out = v_flex().gap_1();
        if self.notes_grouped {
            let groups = [
                (PointKind::Decision, tr("ENTSCHEIDUNGEN")),
                (PointKind::Action, tr("AUFGABEN")),
                (PointKind::Question, tr("OFFENE FRAGEN")),
                (PointKind::Fact, tr("ZAHLEN & FAKTEN")),
            ];
            for (kind, title) in groups {
                let mut notes: Vec<&mostalgia_core::store::Note> =
                    m.notes.iter().filter(|n| PointKind::parse(&n.kind).unwrap_or(PointKind::Fact) == kind).collect();
                if notes.is_empty() {
                    continue;
                }
                notes.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(a.at_secs.total_cmp(&b.at_secs)));
                out = out.child(ui::section_title(format!("{title} ({})", notes.len()), cx).pt_3().pb_1());
                for n in notes {
                    out = out.child(row(n, cx));
                }
            }
        } else {
            out = out.child(ui::section_title(tr("NOTIZEN VON MORI"), cx).pb_1());
            for n in &m.notes {
                out = out.child(row(n, cx));
            }
        }
        out.into_any_element()
    }

    fn render_summary_body(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if self.summarizing {
            return v_flex()
                .py_16()
                .items_center()
                .gap_3()
                .child(gpui_kit::component::spinner::Spinner::new().large())
                .child(div().text_color(theme.muted_foreground).child(tr("Die Besprechung wird zusammengefasst … (bei hoher Denktiefe bis zu einigen Minuten)")))
                .into_any_element();
        }
        match &m.summary {
            Some(s) => v_flex()
                .gap_4()
                .child(if ui::is_rtl(&s.markdown) {
                    ui::rtl_markdown(&summary_body(&s.markdown), cx).into_any_element()
                } else {
                    TextView::markdown("summary", summary_body(&s.markdown)).selectable(true).into_any_element()
                })
                .child(
                    h_flex()
                        .gap_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(trf(
                            "Erstellt von {} · {}{} am {}",
                            &[
                                &summary::provider_display(&s.provider),
                                &s.model,
                                &s.effort.as_deref().map(|e| trf(" · Denktiefe {}", &[&e])).unwrap_or_default(),
                                &i18n::date_time(&s.created_at),
                            ],
                        ) + &summary_language(m))
                        .child(
                            Button::new("resummarize")
                                .xsmall()
                                .ghost()
                                .icon(IconName::RefreshCw)
                                .label(tr("Neu erstellen"))
                                .on_click(cx.listener(|this, _, _, cx| this.summarize(cx))),
                        ),
                )
                .into_any_element(),
            None => v_flex()
                .py_16()
                .items_center()
                .gap_3()
                .child(Icon::new(IconName::Sparkles).large().text_color(theme.primary))
                .child(div().font_medium().child(tr("Noch keine Zusammenfassung")))
                .child(
                    div()
                        .max_w(px(420.))
                        .text_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(tr("Kernpunkte, Entscheidungen und Aufgaben mit Verantwortlichen, erstellt vom gewählten Sprachmodell. Es wird nur der Text gesendet, nie Audio.")),
                )
                .child(
                    Button::new("summarize")
                        .primary()
                        .icon(IconName::Sparkles)
                        .label(tr("Zusammenfassung erstellen"))
                        .disabled(m.segments.is_empty())
                        .on_click(cx.listener(|this, _, _, cx| this.summarize(cx))),
                )
                .into_any_element(),
        }
    }

    fn render_transcript(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let has_audio = !m.tracks().is_empty();
        let engine_line = h_flex()
            .mb_3()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .when_some(m.transcript_engine.clone(), |el, engine| {
                el.child(Icon::new(if engine.contains("Whisper") { IconName::Cpu } else { IconName::Globe }).xsmall())
                    .child(div().min_w_0().child(if m.transcript_error.is_some() {
                        engine
                    } else {
                        trf("Transkribiert mit {}", &[&engine])
                    }))
            })
            .child(div().flex_1())
            .child(
                Button::new("retranscribe-tab")
                    .xsmall()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .label(tr("Erneut transkribieren"))
                    .disabled(!has_audio || self.retranscribing.is_some())
                    .when(!has_audio, |b| b.tooltip(tr("Audiodateien gelöscht")))
                    .on_click(cx.listener(|this, _, _, cx| this.open_retranscribe(cx))),
            );
        if m.segments.is_empty() {
            return v_flex()
                .child(engine_line)
                .child(
                    div()
                        .py_16()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .child(tr("In dieser Aufnahme wurde keine Sprache erkannt.")),
                )
                .into_any_element();
        }
        let position = self.position;
        let mut rows: Vec<AnyElement> = vec![engine_line.into_any_element()];

        if let Some(Edit::Speaker(current, input)) = &self.edit {
            rows.push(
                h_flex()
                    .mb_3()
                    .p_3()
                    .gap_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .child(div().text_sm().child(trf("„{}“ umbenennen in", &[&ui::speaker_display(current)])))
                    .child(Input::new(input).w(px(220.)).small())
                    .child(Button::new("spk-save").small().primary().label(tr("Speichern")).on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))))
                    .child(Button::new("spk-cancel").small().ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))))
                    .into_any_element(),
            );
        }

        let mut marks = m.bookmarks.iter().peekable();
        for (ix, seg) in m.segments.iter().enumerate() {
            while let Some(b) = marks.next_if(|b| b.at_secs <= seg.start_secs) {
                rows.push(
                    h_flex()
                        .gap_3()
                        .py_1()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(div().w(px(52.)).font_family(theme.mono_font_family.clone()).child(ui::hms(b.at_secs)))
                        .child(Icon::new(IconName::Bookmark).xsmall())
                        .child(b.label.clone())
                        .into_any_element(),
                );
            }
            let playing = position.is_some_and(|p| p >= seg.start_secs && p < seg.end_secs.max(seg.start_secs + 0.5));
            let label = seg.speaker_label().to_string();
            let start = seg.start_secs;
            let seg_id = seg.id;
            let text = seg.text.clone();
            let editing = matches!(&self.edit, Some(Edit::Line(id, _)) if *id == seg_id);
            let body: AnyElement = match &self.edit {
                Some(Edit::Line(id, input)) if *id == seg_id => h_flex()
                    .flex_1()
                    .gap_2()
                    .child(Input::new(input).flex_1())
                    .child(Button::new("line-save").small().primary().icon(IconName::Check).on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))))
                    .child(Button::new("line-cancel").small().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))))
                    .into_any_element(),
                _ => div()
                    .flex_1()
                    .min_w_0()
                    .line_height(rems(1.55))
                    .child(ui::bidi_text(seg.text.clone(), true))
                    .into_any_element(),
            };
            let group = SharedString::from(format!("seg-{ix}"));
            let flashing = self.flash.is_some_and(|(id, _)| id == seg_id);
            // The jump target scrolls itself into view once it has bounds.
            let scroll_here = self.scroll_target.as_ref().filter(|(id, done)| *id == seg_id && !done.get()).map(|(_, done)| {
                let (done, scroll) = (done.clone(), self.scroll.clone());
                canvas(
                    move |bounds, window, _| {
                        let view = scroll.bounds();
                        if view.size.height <= px(0.) {
                            return;
                        }
                        let offset = scroll.offset();
                        let top_in_content = bounds.top() - view.top() - offset.y;
                        let wanted = -(top_in_content - view.size.height * 0.3);
                        let max = scroll.max_offset().y.abs();
                        scroll.set_offset(point(offset.x, wanted.clamp(-max, px(0.))));
                        done.set(true);
                        window.refresh();
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            });
            rows.push(
                h_flex()
                    .id(("seg", ix))
                    .relative()
                    .group(group.clone())
                    .children(scroll_here)
                    .when(flashing, |el| el.bg(theme.warning.opacity(0.18)))
                    .items_start()
                    .gap_3()
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .when(playing, |el| el.bg(theme.primary.opacity(0.08)))
                    .hover(|s| s.bg(theme.muted.opacity(0.5)))
                    .child(
                        div()
                            .id(("ts", ix))
                            .w(px(52.))
                            .flex_none()
                            .pt_0p5()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(if playing { theme.primary } else { theme.muted_foreground })
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.primary))
                            .child(ui::hms(start))
                            .on_click(cx.listener(move |this, _, _, cx| this.play_from(start, cx))),
                    )
                    .child(
                        div()
                            .id(("spk", ix))
                            .cursor_pointer()
                            .child(ui::speaker_chip(&label, cx))
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr("Sprecher umbenennen")).build(window, cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let current = label.clone();
                                let value = if current == "Ich" || current == "Andere" { String::new() } else { current.clone() };
                                this.begin_edit(move |input| Edit::Speaker(current, input), value, window, cx);
                            })),
                    )
                    .child(body)
                    .when(!editing, |el| {
                        el.child(
                            div().invisible().group_hover(group, |s| s.visible()).child(
                                Button::new(("edit", ix))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Pencil)
                                    .tooltip(tr("Text korrigieren"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.begin_edit(move |input| Edit::Line(seg_id, input), text.clone(), window, cx)
                                    })),
                            ),
                        )
                    })
                    .into_any_element(),
            );
        }
        v_flex().children(rows).into_any_element()
    }

    fn render_audio(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        self.load_peaks(m, cx);
        let theme = cx.theme();
        let duration = m.duration_secs.max(0.1);
        let Some(peaks) = &self.peaks else {
            return div().into_any_element();
        };
        if peaks.is_empty() {
            return v_flex()
                .py_16()
                .items_center()
                .child(gpui_kit::component::spinner::Spinner::new())
                .into_any_element();
        }
        let playhead = self.position.map(|p| (p / duration).clamp(0.0, 1.0) as f32);
        let rows = peaks.iter().enumerate().map(|(row, (source, values))| {
            let color = ui::speaker_color(source.speaker_label(), cx);
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(ui::speaker_chip(source.speaker_label(), cx))
                        .child(div().text_color(theme.muted_foreground).child(ui::source_label(*source))),
                )
                .child(
                    h_flex()
                        .id(("wave", row))
                        .relative()
                        .h(px(72.))
                        .items_center()
                        .gap(px(1.))
                        .children(values.iter().enumerate().map(|(i, v)| {
                            let at = i as f64 / values.len() as f64 * duration;
                            let h = (ui::meter_level(*v * 0.5) * 72.).max(2.);
                            let played = playhead.is_some_and(|p| (i as f32 / values.len() as f32) <= p);
                            div()
                                .id(("bar", row * WAVE_BUCKETS + i))
                                .flex_1()
                                .h(px(h))
                                .rounded(px(1.))
                                .bg(if played { color } else { color.opacity(0.45) })
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| this.play_from(at, cx)))
                        })),
                )
        });
        let ticks = (0..=4).map(|i| ui::hms(duration * i as f64 / 4.0));
        v_flex()
            .gap_6()
            .children(rows)
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .font_family(theme.mono_font_family.clone())
                    .children(ticks.map(|t| div().child(t))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(tr("Auf die Wellenform klicken, um ab dieser Stelle abzuspielen.")),
            )
            .into_any_element()
    }
}

impl Render for MeetingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(m) = self.meeting.take() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(self.error.clone().unwrap_or_else(|| tr("Besprechung nicht gefunden.").into()))
                .into_any_element();
        };
        let header = self.render_header(&m, cx);
        let status = self.render_transcript_status(&m, cx);
        let dialog = self.render_retranscribe_dialog(cx);
        let body = match self.tab {
            0 => self.render_summary(&m, cx),
            1 => self.render_transcript(&m, cx),
            2 => self.render_notes_tab(&m, cx),
            _ => self.render_audio(&m, cx),
        };
        let note_count = m.notes.len();
        let lines = m.segments.len();
        self.meeting = Some(m);
        let theme = cx.theme();

        v_flex()
            .size_full()
            .relative()
            .key_context("Meeting")
            .on_action(cx.listener(Self::export))
            .child(header)
            .children(status)
            .child(
                div().px_8().child(
                    TabBar::new("meeting-tabs")
                        .underline()
                        .selected_index(self.tab)
                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                            this.tab = *ix;
                            cx.notify();
                        }))
                        .child(Tab::new().prefix(Icon::new(IconName::Sparkles).small()).label(tr("Zusammenfassung")))
                        .child(Tab::new().prefix(Icon::new(IconName::FileText).small()).label(trf("Transkript · {}", &[&lines])))
                        .child(Tab::new().prefix(Icon::new(IconName::Bookmark).small()).label(if note_count > 0 {
                            trf("Notizen · {}", &[&note_count])
                        } else {
                            tr("Notizen").to_string()
                        }))
                        .child(Tab::new().prefix(Icon::new(IconName::AudioLines).small()).label(tr("Audio"))),
                ),
            )
            .when_some(self.notice.clone(), |el, note| {
                el.child(
                    h_flex()
                        .mx_8()
                        .mt_3()
                        .p_3()
                        .gap_2()
                        .rounded(theme.radius)
                        .bg(theme.success.opacity(0.12))
                        .text_sm()
                        .child(Icon::new(IconName::Check).small().text_color(theme.success))
                        .child(div().flex_1().child(note))
                        .child(Button::new("dismiss-note").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        }))),
                )
            })
            .when_some(self.error.clone(), |el, err| {
                el.child(
                    h_flex()
                        .mx_8()
                        .mt_3()
                        .p_3()
                        .gap_2()
                        .rounded(theme.radius)
                        .bg(theme.danger.opacity(0.1))
                        .text_color(theme.danger)
                        .text_sm()
                        .child(div().flex_1().child(err))
                        .child(Button::new("dismiss-err").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        }))),
                )
            })
            .child(
                div().flex_1().min_h_0().child(
                    div()
                        .id("meeting-body")
                        .size_full()
                        .track_scroll(&self.scroll)
                        .overflow_y_scroll()
                        .child(div().max_w(px(860.)).px_8().py_6().child(body)),
                ),
            )
            .children(dialog)
            .into_any_element()
    }
}

/// The summary without its leading "# Title" line, which the header shows.
fn summary_body(markdown: &str) -> String {
    let trimmed = markdown.trim_start();
    match trimmed.strip_prefix("# ") {
        Some(rest) => rest.split_once('\n').map_or("", |(_, body)| body).trim_start().to_string(),
        None => trimmed.to_string(),
    }
}

/// Estimated price in US dollars, e.g. "≈ 0,02 $" / "≈ $0.02".
fn cost_label(usd: f64) -> String {
    let de = i18n::lang() == i18n::Lang::De;
    if usd < 0.01 {
        return if de { "< 0,01 $".into() } else { "< $0.01".into() };
    }
    if de { format!("≈ {:.2} $", usd).replace('.', ",") } else { format!("≈ ${usd:.2}") }
}

/// ~/Documents (or the home folder) as the export dialog's starting point.
fn dirs_documents() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| ".".into());
    let docs = home.join("Documents");
    if docs.is_dir() { docs } else { home }
}

/// " · in English": the language the summary is written in (the configured
/// summary language, else the meeting's), or nothing when unknown.
fn summary_language(m: &Meeting) -> String {
    let code = mostalgia_core::config::Config::load().summary_language_code().unwrap_or_else(|| m.language.clone());
    if code.is_empty() || code == "auto" {
        return String::new();
    }
    trf(" · auf {}", &[&summary::language_name(&code)])
}
