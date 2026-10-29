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
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use schemars::JsonSchema;
use serde::Deserialize;
use mostalgia_core::config::{Config, SttChoice};
use mostalgia_core::playback::{self, Player};
use mostalgia_core::store::{Meeting, Store};
use mostalgia_core::i18n::{self, tr, trf};
use mostalgia_core::{Source, cloud_stt, companion, finalize, paths, recap, summary, voices};

use crate::recording::SharedEngine;
use crate::processing;
use crate::ui;

/// Bars per waveform row.
const WAVE_BUCKETS: usize = 320;

/// Export the open meeting in a format (by file extension).
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = mostalgia, no_json)]
pub struct ExportAs(pub String);

/// Copy the open meeting's recap: 0 short text, 1 short Markdown, 2 full
/// text, 3 full Markdown.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = mostalgia, no_json)]
pub struct CopyRecap(pub u8);

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
    /// Giving these lines (segment ids) to a newly typed speaker.
    Reassign(Vec<i64>, Entity<InputState>),
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
    /// People in the call, counting you (0 = automatic).
    people: u8,
}

pub struct MeetingView {
    pub id: i64,
    meeting: Option<Meeting>,
    tab: usize,
    player: Option<Player>,
    position: Option<f64>,
    peaks: Option<Vec<(Source, Vec<f32>)>>,
    summarizing: bool,
    /// Whether work on this meeting was running at the last look (see
    /// [`MeetingView::processing_changed`]).
    was_busy: bool,
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
    /// Transcript lines (segment ids) selected to give to another speaker.
    selected: Vec<i64>,
    /// The line Shift-click selects from.
    select_anchor: Option<usize>,
    /// The "Chat with Mori" section is folded away.
    chat_folded: bool,
    /// Notes tab: grouped by kind (else by time).
    notes_grouped: bool,
    /// "Regenerate notes": its focus field while the panel is open.
    regen_panel: Option<Entity<InputState>>,
    /// Regenerating: "provider · model", and (part, parts) being read.
    regenerating: Option<(SharedString, (usize, usize))>,
    regen_error: Option<SharedString>,
    /// The last regenerate's result line and the archived version it
    /// replaced (its Undo).
    regen_done: Option<(SharedString, Option<i64>)>,
    /// The focus used for this meeting in this session (prefills the panel).
    regen_focus: Option<String>,
    /// The note style chosen in the panel (from the settings when it opens,
    /// unless chosen this session).
    regen_style: Option<companion::NoteStyle>,
    /// The focus the meeting's notes were taken with (stored with it).
    notes_focus: Option<String>,
    /// Earlier sets of notes a regenerate replaced.
    note_versions: Vec<mostalgia_core::store::NoteVersion>,
    versions_open: bool,
    /// Imported from text with a title of its own: the summary keeps it.
    keep_title: bool,
    /// Notes to write once the summary is done (after a text import):
    /// focus and style.
    pending_notes: Option<(String, companion::NoteStyle)>,
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
            was_busy: false,
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
            selected: Vec::new(),
            select_anchor: None,
            chat_folded: false,
            notes_grouped: true,
            regen_panel: None,
            regenerating: None,
            regen_error: None,
            regen_done: None,
            regen_focus: None,
            regen_style: None,
            notes_focus: None,
            note_versions: Vec::new(),
            versions_open: false,
            keep_title: false,
            pending_notes: None,
            _subscriptions: Vec::new(),
        };
        this.reload(cx);
        this.take_failures();
        this.was_busy = processing::for_meeting(id).is_some();
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

    /// Errors of work on this meeting that ended while it wasn't open (or
    /// here, before the view saw them): shown where that work shows them,
    /// next to its retry.
    fn take_failures(&mut self) {
        for (key, message) in processing::take_failures(self.id) {
            let message: SharedString = message.into();
            match key {
                processing::Key::Transcript(_) => self.transcribe_error = Some(message),
                processing::Key::Notes(_) => self.regen_error = Some(message),
                _ => self.error = Some(message),
            }
        }
    }

    /// Work on meetings started or moved on: redraw, and load what work on
    /// this meeting (maybe started in another view of it) just finished.
    pub fn processing_changed(&mut self, cx: &mut Context<Self>) {
        let busy = processing::for_meeting(self.id).is_some();
        if self.was_busy && !busy {
            self.reload(cx);
        }
        self.was_busy = busy;
        self.take_failures();
        cx.notify();
    }

    /// Puts the meeting's recap on the clipboard (see `recap`) and says so.
    /// Without a summary nothing is copied: it says why and writes one if
    /// it can.
    fn copy_recap(&mut self, length: recap::Length, style: recap::Style, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::notification::Notification;
        let Some(m) = &self.meeting else { return };
        match recap::recap(m, length, style) {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                window.push_notification(Notification::success(tr("Recap kopiert")), cx);
            }
            None => {
                window.push_notification(Notification::info(tr("Noch keine Zusammenfassung – sie wird jetzt geschrieben; danach Recap kopieren.")), cx);
                self.summarize(cx);
            }
        }
    }

    /// Screenshots: "Copy recap" clicked.
    #[doc(hidden)]
    pub fn demo_copy_recap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_recap(recap::Length::Short, recap::Style::Plain, window, cx);
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let loaded = Store::open(&paths::database()).and_then(|s| Ok((s.get(self.id)?, s.notes_focus(self.id)?, s.note_versions(self.id)?)));
        match loaded {
            Ok((m, focus, versions)) => {
                self.meeting = m;
                self.notes_focus = focus;
                self.note_versions = versions;
            }
            Err(e) => self.error = Some(format!("{e:#}").into()),
        }
        cx.notify();
    }

    /// Opens the "Regenerate notes" panel, its focus prefilled with the one
    /// used last (this session, else saved with the meeting, else the
    /// default from the settings).
    fn open_regenerate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.regen_focus.clone().or_else(|| self.notes_focus.clone()).unwrap_or_else(|| Config::load().companion_notes_focus);
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(tr("z. B. Aufgaben für mich, Preise, Fristen"));
            s.set_value(focus, window, cx);
            s
        });
        let sub = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.start_regenerate(cx);
            }
        });
        self._subscriptions.push(sub);
        input.update(cx, |s, cx| s.focus(window, cx));
        self.regen_panel = Some(input);
        self.regen_error = None;
        cx.notify();
    }

    /// Just imported from text: the summary (if that's on, keeping a title
    /// the user gave), then the notes with the dialog's focus and style.
    pub fn after_text_import(&mut self, keep_title: bool, focus: String, style: companion::NoteStyle, cx: &mut Context<Self>) {
        self.keep_title = keep_title;
        let config = Config::load();
        let notes_ready = config.notes_request().is_ok_and(|r| r.connection.is_configured());
        let summary_ready = config.auto_summary && config.summary_connection().is_ok_and(|c| c.is_configured());
        if summary_ready && self.meeting.as_ref().is_some_and(|m| m.summary.is_none() && !m.segments.is_empty()) {
            self.pending_notes = notes_ready.then_some((focus, style));
            self.summarize(cx);
        } else if notes_ready {
            self.tab = 2;
            self.run_regenerate(focus, style, cx);
        } else {
            self.tab = 1;
            cx.notify();
        }
    }

    /// Writes the notes again from the saved transcript (see
    /// `companion::regenerate_notes`), in the background.
    fn start_regenerate(&mut self, cx: &mut Context<Self>) {
        if self.regenerating.is_some() {
            return;
        }
        let Some(input) = self.regen_panel.take() else { return };
        let focus = input.read(cx).value().trim().to_string();
        let style = self.regen_style.unwrap_or_else(|| Config::load().notes_style());
        self.run_regenerate(focus, style, cx);
    }

    fn run_regenerate(&mut self, focus: String, style: companion::NoteStyle, cx: &mut Context<Self>) {
        if self.regenerating.is_some() {
            return;
        }
        self.regen_panel = None;
        self.regen_focus = Some(focus.clone());
        self.regen_style = Some(style);
        let req = match Config::load().notes_request().and_then(|req| req.connection.require_key().map(|_| req)) {
            Ok(req) => req,
            Err(e) => {
                self.regen_error = Some(format!("{e:#}").into());
                cx.notify();
                return;
            }
        };
        let provider = req.connection.name.clone();
        self.regenerating = Some((format!("{} · {}", req.connection.name, req.model).into(), (1, 1)));
        self.regen_error = None;
        self.regen_done = None;
        cx.notify();
        let id = self.id;
        let key = processing::Key::Notes(id);
        let title = self.meeting.as_ref().map(|m| m.title.clone()).unwrap_or_default();
        processing::set(key, processing::Job { title, stage: trf("{} schreibt Notizen …", &[&companion_name()]), fraction: None });
        let (tx, rx) = crossbeam_channel::unbounded::<Result<(usize, usize), Result<companion::Regenerated, String>>>();
        let failed_provider = provider.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = companion::regenerate_notes(id, style, &focus, &mut |part, parts| {
                if parts > 1 {
                    processing::stage(key, trf("{} schreibt Notizen … (Teil {} von {})", &[&companion_name(), &part, &parts]), Some((part - 1) as f32 / parts as f32));
                }
                let _ = progress_tx.send(Ok((part, parts)));
            });
            match &result {
                Ok(_) => processing::clear(key),
                Err(e) => processing::fail(key, trf("Notizen mit {} neu erstellen fehlgeschlagen: {}", &[&failed_provider, &format!("{e:#}")])),
            }
            let _ = tx.send(Err(result.map_err(|e| format!("{e:#}"))));
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let mut finished = None;
                let mut progress = None;
                loop {
                    match rx.try_recv() {
                        Ok(Ok(p)) => progress = Some(p),
                        Ok(Err(result)) => finished = Some(result),
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            finished.get_or_insert(Err(tr("Die Anfrage ist abgebrochen").to_string()));
                            break;
                        }
                    }
                }
                let done = finished.is_some();
                let alive = this
                    .update(cx, |this, cx| {
                        if let (Some(p), Some((_, at))) = (progress, this.regenerating.as_mut()) {
                            *at = p;
                            cx.notify();
                        }
                        let Some(result) = finished else { return };
                        this.regenerating = None;
                        processing::take_failures(this.id);
                        match result {
                            Ok(r) => {
                                let line = if r.kept > 0 {
                                    trf("{} neue Notizen von {} ({} angeheftete behalten).", &[&r.added, &companion_name(), &r.kept])
                                } else {
                                    trf("{} neue Notizen von {}.", &[&r.added, &companion_name()])
                                };
                                eprintln!("[mostalgia] regenerated notes of meeting {}: {} new, {} kept, {} repeats left out, {} part(s), {} · {}", this.id, r.added, r.kept, r.repeats, r.parts, r.provider, r.model);
                                this.regen_done = Some((line.into(), r.version));
                            }
                            Err(e) => this.regen_error = Some(trf("Notizen mit {} neu erstellen fehlgeschlagen: {}", &[&provider, &e]).into()),
                        }
                        this.reload(cx);
                        cx.emit(MeetingEvent::Changed);
                    })
                    .is_ok();
                if done || !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Screenshots: the regenerate panel (0), its progress (1), or its
    /// result with Undo and the earlier versions open (2).
    #[doc(hidden)]
    pub fn demo_regenerate(&mut self, stage: u8, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = 2;
        match stage {
            0 => self.open_regenerate(window, cx),
            1 => {
                self.regen_panel = None;
                self.regenerating = Some(("DeepSeek · deepseek-flash".into(), (2, 3)));
            }
            _ => {
                self.regenerating = None;
                let version = self.note_versions.first().map(|v| v.id);
                let n = self.meeting.as_ref().map_or(0, |m| m.notes.iter().filter(|n| !n.pinned).count());
                let kept = self.meeting.as_ref().map_or(0, |m| m.notes.iter().filter(|n| n.pinned).count());
                self.regen_done = Some((trf("{} neue Notizen von {} ({} angeheftete behalten).", &[&n, &companion_name(), &kept]).into(), version));
                self.versions_open = true;
            }
        }
        cx.notify();
    }

    /// Brings back an earlier set of notes (the current unpinned ones are
    /// archived in its place).
    fn restore_notes(&mut self, version: i64, cx: &mut Context<Self>) {
        let id = self.id;
        match Store::open(&paths::database()).and_then(|mut s| s.restore_note_version(id, version)) {
            Ok(_) => {
                self.regen_done = None;
                self.notice = Some(tr("Frühere Notizen wiederhergestellt.").into());
                cx.background_spawn(async move {
                    if let Err(e) = mostalgia_core::ask::index_meeting(id) {
                        eprintln!("[mostalgia] search index: {e:#}");
                    }
                })
                .detach();
                self.reload(cx);
                cx.emit(MeetingEvent::Changed);
            }
            Err(e) => {
                self.error = Some(format!("{e:#}").into());
                cx.notify();
            }
        }
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
        let key = processing::Key::Summary(id);
        let title = self.meeting.as_ref().map(|m| m.title.clone()).unwrap_or_default();
        processing::set(key, processing::Job { title, stage: tr("Zusammenfassung wird geschrieben …").to_string(), fraction: None });
        let provider = req.connection.name.clone();
        let failed_provider = provider.clone();
        let keep_title = self.keep_title;
        let task = cx.background_spawn(async move {
            let result = summarize_meeting(id, &req, keep_title);
            match &result {
                Ok(()) => processing::clear(key),
                Err(e) => processing::fail(key, trf("Zusammenfassung mit {} fehlgeschlagen: {}", &[&failed_provider, &format!("{e:#}")])),
            }
            result
        });
        fn summarize_meeting(id: i64, req: &summary::SummaryRequest, keep_title: bool) -> anyhow::Result<()> {
            let store = Store::open(&paths::database())?;
            let meeting = store.get(id)?.ok_or_else(|| anyhow::anyhow!(tr("Besprechung nicht gefunden")))?;
            let s = summary::summarize(req, &meeting)?;
            store.save_summary(id, &s.provider, &s.model, s.effort.as_deref(), &s.markdown)?;
            if let Some(title) = s.title.as_ref().filter(|_| !keep_title) {
                store.set_title(id, title)?;
            }
            // Its decisions go into the log, if that's on.
            if Config::load().decision_log
                && let Some(m) = store.get(id)?
            {
                mostalgia_core::decisions::add_meeting(&store, &m)?;
            }
            // Summary sections become searchable too.
            if let Err(e) = mostalgia_core::ask::index_meeting(id) {
                eprintln!("[mostalgia] search index: {e:#}");
            }
            anyhow::Ok(())
        }
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.summarizing = false;
                processing::take_failures(this.id);
                if let Err(e) = result {
                    this.error = Some(trf("Zusammenfassung mit {} fehlgeschlagen: {}", &[&provider, &format!("{e:#}")]).into());
                }
                this.reload(cx);
                cx.emit(MeetingEvent::Changed);
                if let Some((focus, style)) = this.pending_notes.take() {
                    this.run_regenerate(focus, style, cx);
                }
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
        self.retranscribe = Some(Retranscribe { options, selected, people: 0 });
        cx.notify();
    }

    fn start_retranscribe(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.retranscribe.take() else { return };
        let Some(m) = &self.meeting else { return };
        let option = &dialog.options[dialog.selected];
        let choice = option.choice.clone();
        let label: SharedString = option.label.clone().into();
        let people = dialog.people;
        let language = m.language.clone();
        let title = m.title.clone();
        self.player = None;
        self.position = None;
        self.edit = None;
        self.transcribe_error = None;
        self.retranscribing = Some(label.clone());
        let id = self.id;
        let key = processing::Key::Transcript(id);
        processing::set(key, processing::Job { title, stage: trf("Transkribiere mit {} …", &[&label]), fraction: None });
        cx.notify();

        let slot = self.engine.clone();
        let failed_label = label.clone();
        let task = cx.background_spawn(async move {
            let config = Config::load();
            let vad = config.vad_model_path();
            let opts = finalize::FinalPassOptions {
                vad_model: vad.exists().then_some(vad),
                diarize: config.diarize,
                denoise_mic: config.denoise,
                people,
                progress: finalize::Progress::new(move |stage| processing::stage(key, stage.label(), stage.fraction())),
            };
            let cached = slot.as_ref().filter(|_| choice.is_local()).and_then(|s| s.lock().unwrap().take());
            let mut engine = match cached {
                Some(engine) => engine,
                None => finalize::engine_for(&config, &choice)?,
            };
            // With "auto", detect again: a stored language may itself have
            // been a wrong detection, and forcing it makes Whisper translate.
            let language = if config.language == "auto" { config.language.clone() } else { language };
            engine.set_meeting_languages(&config.meeting_languages);
            let result = engine.set_language(&language).and_then(|_| finalize::retranscribe(id, &mut engine, &opts));
            if let Some(slot) = slot.filter(|_| !engine.is_cloud()) {
                *slot.lock().unwrap() = Some(engine);
            }
            // Ends here, so it ends even when this meeting isn't open any more.
            match &result {
                Ok(_) => processing::clear(key),
                Err(e) => processing::fail(key, trf("Transkription mit {} fehlgeschlagen: {}", &[&failed_label, &format!("{e:#}")])),
            }
            result
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.retranscribing = None;
                // Shown right here instead.
                processing::take_failures(this.id);
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
        let job = processing::get(processing::Key::Transcript(self.id));
        if self.retranscribing.is_some() || job.is_some() {
            let label = self.retranscribing.clone().unwrap_or_default();
            let stage = job.as_ref().map(|j| j.stage.clone()).unwrap_or_else(|| trf("Transkribiere mit {} …", &[&label]));
            let fraction = job.and_then(|j| j.fraction);
            return Some(
                v_flex()
                    .mx_8()
                    .mb_3()
                    .p_3()
                    .gap_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .text_sm()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(gpui_kit::component::spinner::Spinner::new().small())
                            .child(div().font_medium().child(stage))
                            .when(!label.is_empty(), |el| el.child(div().text_xs().text_color(theme.muted_foreground).child(label)))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Bei langen Aufnahmen kann das einige Minuten dauern."))),
                    )
                    .child(ui::progress_bar(fraction, cx))
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
                        .when(!has_audio, |b| b.tooltip(audio_gone(m)))
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
        let picked = dialog.people;
        let has_system = self.meeting.as_ref().is_some_and(|m| m.tracks().iter().any(|(s, _)| *s == Source::System));
        let people_row = (Config::load().diarize && has_system).then(|| {
            h_flex()
                .flex_wrap()
                .gap_1()
                .child(div().text_sm().text_color(theme.muted_foreground).mr_2().child(tr("Personen im Gespräch (mit dir)")))
                .children(mostalgia_core::diarize::PEOPLE_CHOICES.into_iter().map(|n| {
                    Button::new(SharedString::from(format!("retranscribe-people-{n}")))
                        .label(ui::people_label(n))
                        .small()
                        .when(picked == n, |b| b.primary())
                        .when(picked != n, |b| b.ghost())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(d) = &mut this.retranscribe {
                                d.people = n;
                                cx.notify();
                            }
                        }))
                }))
        });
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
                        .w(px(480. * ui::ui_scale()))
                        .max_w(relative(0.9))
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
                        .when(people_row.is_some(), |el| el.children(people_row))
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
        // Imported from text: nothing to play (jumps only show the line).
        if m.is_text_import() {
            return;
        }
        let tracks: Vec<_> = m.tracks().into_iter().map(|(_, p)| p).collect();
        // Audio deleted: a jump only shows the line.
        if tracks.is_empty() {
            return;
        }
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
        if let Edit::Reassign(ids, input) = &edit {
            let name = input.read(cx).value().trim().to_string();
            self._subscriptions.clear();
            if !name.is_empty() {
                self.reassign(ids.clone(), name, cx);
            }
            cx.notify();
            return;
        }
        let result = Store::open(&paths::database()).and_then(|store| match &edit {
            Edit::Speaker(current, input) => store.rename_speaker(self.id, current, &input.read(cx).value()).map(|_| ()),
            Edit::Line(seg, input) => store.update_segment_text(*seg, &input.read(cx).value()),
            Edit::Reassign(..) => Ok(()),
            Edit::Title(input) => {
                let title = input.read(cx).value();
                if title.trim().is_empty() { Ok(()) } else { store.set_title(self.id, title.trim()) }
            }
        });
        if let Err(e) = result {
            self.error = Some(format!("{e:#}").into());
        } else if let Edit::Speaker(current, input) = &edit {
            self.learn_voice(current.clone(), input.read(cx).value().trim().to_string(), cx);
        }
        self._subscriptions.clear();
        self.reload(cx);
        cx.emit(MeetingEvent::Changed);
    }

    /// After a rename: remembers the named person's voice for later
    /// meetings (in the background; computing a voice print takes a moment).
    fn learn_voice(&mut self, previous: String, name: String, cx: &mut Context<Self>) {
        if !mostalgia_core::diarize::available() || previous == name || !Config::load().recognize_voices {
            return;
        }
        let id = self.id;
        let task = cx.background_spawn({
            let name = name.clone();
            async move { voices::learn_from_meeting(id, &previous, &name) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(voices::Learned::Voice { .. }) => {
                        this.notice = Some(trf("Stimme von {} gemerkt — wird in künftigen Besprechungen erkannt.", &[&name]).into())
                    }
                    Ok(voices::Learned::TooShort { .. }) => {
                        this.notice = Some(trf("{} spricht hier zu kurz, um die Stimme zu lernen.", &[&name]).into())
                    }
                    Ok(_) => {}
                    Err(e) => this.error = Some(trf("Stimme nicht gelernt: {}", &[&format!("{e:#}")]).into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Change speaker" / "Split speaker": gives these lines to `name`,
    /// then corrects the remembered voices (in the background): `name`
    /// learns from its lines here, and a name the lines had before forgets
    /// them.
    fn reassign(&mut self, ids: Vec<i64>, name: String, cx: &mut Context<Self>) {
        let previous = match Store::open(&paths::database()).and_then(|s| s.set_segments_speaker(self.id, &ids, &name)) {
            Ok(previous) => previous,
            Err(e) => {
                self.error = Some(format!("{e:#}").into());
                cx.notify();
                return;
            }
        };
        self.selected.clear();
        let who = ui::speaker_display(&name);
        self.notice = Some(if ids.len() == 1 { trf("Zeile jetzt bei {}", &[&who]) } else { trf("{} Zeilen jetzt bei {}", &[&ids.len(), &who]) }.into());
        self.reload(cx);
        cx.emit(MeetingEvent::Changed);
        let has_audio = self.meeting.as_ref().is_some_and(|m| m.tracks().iter().any(|(s, _)| *s == Source::System));
        if !has_audio || !mostalgia_core::diarize::available() || !Config::load().recognize_voices {
            return;
        }
        let id = self.id;
        let task = cx.background_spawn({
            let name = name.clone();
            async move { voices::correct_from_meeting(id, &previous, &name) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(voices::Learned::Voice { .. }) => {
                        this.notice = Some(trf("Korrigiert — die Stimme von {} ist jetzt genauer gemerkt.", &[&name]).into())
                    }
                    Ok(voices::Learned::TooShort { .. }) => {
                        this.notice = Some(trf("{} spricht hier zu kurz, um die Stimme zu lernen.", &[&name]).into())
                    }
                    Ok(_) => {}
                    Err(e) => this.error = Some(trf("Stimme nicht gelernt: {}", &[&format!("{e:#}")]).into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl-click toggles a line in the selection, Shift-click selects the
    /// lines from the last one clicked; a plain click clears it.
    fn click_line(&mut self, ix: usize, seg_id: i64, modifiers: Modifiers, cx: &mut Context<Self>) {
        if modifiers.shift
            && let (Some(anchor), Some(m)) = (self.select_anchor, &self.meeting)
        {
            let (a, b) = (anchor.min(ix), anchor.max(ix));
            for seg in &m.segments[a..=b.min(m.segments.len() - 1)] {
                if !self.selected.contains(&seg.id) {
                    self.selected.push(seg.id);
                }
            }
        } else if modifiers.secondary(cx) {
            self.toggle_selected(ix, seg_id);
        } else if !self.selected.is_empty() {
            self.selected.clear();
        } else {
            return;
        }
        cx.notify();
    }

    fn toggle_selected(&mut self, ix: usize, seg_id: i64) {
        match self.selected.iter().position(|id| *id == seg_id) {
            Some(i) => {
                self.selected.remove(i);
            }
            None => self.selected.push(seg_id),
        }
        self.select_anchor = Some(ix);
    }

    /// The speakers of this meeting, in order of appearance.
    fn meeting_speakers(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        if let Some(m) = &self.meeting {
            for seg in &m.segments {
                let label = seg.speaker_label();
                if !names.iter().any(|n| n == label) {
                    names.push(label.to_string());
                }
            }
        }
        names
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
                .child(Input::new(input).w(px(420. * ui::ui_scale())).max_w_full())
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
            // With large text in a narrow window the buttons go below.
            .flex_wrap()
            .child(
                v_flex()
                    .flex_1()
                    .min_w(rems(16.))
                    .gap_1()
                    .child(title)
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_x_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(h_flex().gap_1().child(Icon::new(IconName::Calendar).xsmall()).child(i18n::date_time(&m.started_at)))
                            .child(h_flex().gap_1().child(Icon::new(IconName::Clock).xsmall()).child(ui::minutes(m.duration_secs)))
                            .when(!(m.is_text_import() && m.segments.iter().all(|s| s.speaker.is_none())), |el| {
                                el.child(h_flex().gap_1().child(Icon::new(IconName::Users).xsmall()).child(trf("{} Sprecher", &[&speakers.len()])))
                            })
                            .when(m.is_text_import(), |el| el.child(h_flex().gap_1().child(Icon::new(IconName::FileText).xsmall()).child(tr("Aus Text importiert")))),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child({
                        let has_summary = m.summary.is_some();
                        h_flex()
                            .child(
                                Button::new("copy-recap")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Copy)
                                    .label(tr("Recap kopieren"))
                                    .tooltip(if has_summary {
                                        tr("Kurze Zusammenfassung in die Zwischenablage – für Teams, Mail oder Jira")
                                    } else {
                                        tr("Braucht eine Zusammenfassung – klicken, um sie zu schreiben")
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| this.copy_recap(recap::Length::Short, recap::Style::Plain, window, cx))),
                            )
                            .child(
                                Button::new("copy-recap-as")
                                    .small()
                                    .ghost()
                                    .icon(IconName::ChevronDown)
                                    .disabled(!has_summary)
                                    .tooltip(tr("Recap als …"))
                                    .dropdown_menu(|menu, _, _| {
                                        let mut menu = menu;
                                        for (label, action) in [
                                            (tr("Kurz · Text"), CopyRecap(0)),
                                            (tr("Kurz · Markdown"), CopyRecap(1)),
                                            (tr("Vollständig · Text"), CopyRecap(2)),
                                            (tr("Vollständig · Markdown"), CopyRecap(3)),
                                        ] {
                                            menu = menu.menu(label, Box::new(action));
                                        }
                                        menu
                                    }),
                            )
                    })
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
                    .when(!m.is_text_import() && !m.tracks().is_empty(), |el| {
                        let kept = mostalgia_core::cleanup::is_kept(&m.dir);
                        el.child(
                            Button::new("keep-audio")
                                .small()
                                .ghost()
                                .icon(if kept { IconName::PinOff } else { IconName::Pin })
                                .tooltip(if kept { tr("Audio wird behalten – klicken, um es wieder aufräumen zu lassen") } else { tr("Audio behalten (nie automatisch löschen)") })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let Some(m) = &this.meeting else { return };
                                    if let Err(e) = mostalgia_core::cleanup::set_kept(&m.dir, !kept) {
                                        this.error = Some(format!("{e:#}").into());
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .when(self.player.is_none() && !m.is_text_import() && !m.tracks().is_empty(), |el| {
                        el.child(
                            Button::new("play")
                                .icon(IconName::Play)
                                .label(tr("Abspielen"))
                                .outline()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.play_from(0.0, cx))),
                        )
                    })
                    .when(!m.is_text_import(), |el| el.child({
                        let has_audio = !m.tracks().is_empty();
                        Button::new("retranscribe")
                            .small()
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .tooltip(if has_audio { SharedString::from(tr("Erneut transkribieren")) } else { audio_gone(m) })
                            .disabled(!has_audio || self.retranscribing.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.open_retranscribe(cx)))
                    }))
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
    /// pinned first within a group), "Regenerate notes" with its earlier
    /// versions, and the chat from the recording.
    fn render_notes_tab(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let can_regenerate = !m.segments.is_empty() && self.retranscribing.is_none();
        let regen = self.render_regenerate(m, cx);
        let versions = self.render_note_versions(cx);
        if m.notes.is_empty() && m.chat.is_empty() {
            return v_flex()
                .gap_6()
                .children(regen)
                .when(self.regen_panel.is_none() && self.regenerating.is_none(), |el| {
                    el.child(
                        v_flex()
                            .pt_12()
                            .items_center()
                            .gap_2()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::FileText).large())
                            .child(div().font_medium().child(tr("Keine Notizen für diese Besprechung")))
                            .child(div().text_sm().text_center().child(if can_regenerate {
                                trf("{} war bei dieser Aufnahme nicht dabei – sie kann die Notizen aber jetzt aus dem Transkript schreiben.", &[&companion_name()])
                            } else {
                                tr("Bei dieser Aufnahme war keine Begleiterin dabei. Schalte eine unter Einstellungen → Begleiterin ein.").to_string()
                            }))
                            .when(can_regenerate, |el| {
                                el.child(
                                    Button::new("notes-regenerate-empty")
                                        .small()
                                        .primary()
                                        .icon(IconName::WandSparkles)
                                        .label(tr("Notizen erstellen"))
                                        .on_click(cx.listener(|this, _, window, cx| this.open_regenerate(window, cx))),
                                )
                            }),
                    )
                })
                .children(versions)
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
        let busy = self.regen_panel.is_some() || self.regenerating.is_some();
        let toolbar = h_flex()
            .gap_2()
            .when(can_regenerate, |el| {
                el.child(
                    Button::new("notes-regenerate")
                        .xsmall()
                        .outline()
                        .icon(IconName::WandSparkles)
                        .label(tr("Notizen neu erstellen"))
                        .disabled(busy)
                        .tooltip(trf("{} liest das ganze Transkript noch einmal und schreibt die Notizen neu – angeheftete bleiben", &[&companion_name()]))
                        .on_click(cx.listener(|this, _, window, cx| this.open_regenerate(window, cx))),
                )
            })
            .when_some(self.notes_focus.clone().filter(|_| !m.notes.is_empty()), |el, focus| {
                el.child(
                    h_flex()
                        .min_w_0()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::Target).xsmall())
                        .child(div().truncate().child(trf("Fokus: {}", &[&focus]))),
                )
            })
            .child(div().flex_1())
            .when(!m.notes.is_empty(), |el| el.child(toggle));
        v_flex()
            .gap_6()
            .child(toolbar)
            .children(regen)
            .when(!m.notes.is_empty(), |el| el.child(self.render_notes(m, cx)))
            .children(versions)
            .when(!m.chat.is_empty(), |el| el.child(self.render_chat(m, cx)))
            .into_any_element()
    }

    /// The "Regenerate notes" panel, its progress, result (with Undo) or error.
    fn render_regenerate(&mut self, m: &Meeting, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let elsewhere = self.regenerating.is_none().then(|| processing::get(processing::Key::Notes(self.id))).flatten();
        if let Some(job) = elsewhere {
            return Some(
                h_flex()
                    .p_3()
                    .gap_3()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .text_sm()
                    .child(gpui_kit::component::spinner::Spinner::new().small())
                    .child(div().font_medium().child(job.stage))
                    .into_any_element(),
            );
        }
        if let Some((which, (part, parts))) = self.regenerating.clone() {
            return Some(
                h_flex()
                    .p_3()
                    .gap_3()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .text_sm()
                    .child(gpui_kit::component::spinner::Spinner::new().small())
                    .child(
                        v_flex()
                            .child(div().font_medium().child(if parts > 1 {
                                trf("{} liest die Besprechung … (Teil {} von {})", &[&companion_name(), &part, &parts])
                            } else {
                                trf("{} liest die Besprechung …", &[&companion_name()])
                            }))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(which)),
                    )
                    .into_any_element(),
            );
        }
        if let Some(input) = self.regen_panel.clone() {
            let target = match Config::load().notes_request() {
                Ok(req) => Ok(format!("{} · {}", req.connection.name, req.model)),
                Err(e) => Err(format!("{e:#}")),
            };
            let reads = if m.summary.is_some() { tr("das ganze Transkript und die Zusammenfassung") } else { tr("das ganze Transkript") };
            let pinned = m.notes.iter().filter(|n| n.pinned).count();
            return Some(
                v_flex()
                    .p_4()
                    .gap_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .child(h_flex().gap_2().child(Icon::new(IconName::WandSparkles).small().text_color(theme.primary)).child(div().font_semibold().child(tr("Notizen neu erstellen"))))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(trf(
                        "{} liest {} noch einmal und schreibt die Notizen neu, mit Zeitstempeln. Angeheftete Notizen ({}) bleiben, die übrigen werden ersetzt – rückgängig machbar.",
                        &[&companion_name(), &reads, &pinned],
                    )))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().font_medium().child(trf("Worauf soll {} achten? (optional)", &[&companion_name()])))
                            .child(Input::new(&input).small()),
                    )
                    .child({
                        let now = self.regen_style.unwrap_or_else(|| Config::load().notes_style());
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().font_medium().child(tr("Notizstil")))
                            .child(h_flex().flex_wrap().gap_1().children(companion::NoteStyle::ALL.map(|style| {
                                let on = style == now;
                                Button::new(SharedString::from(format!("regen-style-{}", style.id())))
                                    .small()
                                    .label(style.label())
                                    .when(on, |b| b.primary())
                                    .when(!on, |b| b.ghost())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.regen_style = Some(style);
                                        cx.notify();
                                    }))
                            })))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(now.hint()))
                    })
                    .child(match &target {
                        Ok(t) => div().text_xs().text_color(theme.muted_foreground).child(trf("Sendet das Transkript an {}.", &[t])),
                        Err(e) => div().text_xs().text_color(theme.danger).child(e.clone()),
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(Button::new("regen-cancel").small().ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| {
                                this.regen_panel = None;
                                cx.notify();
                            })))
                            .child(
                                Button::new("regen-start")
                                    .small()
                                    .primary()
                                    .icon(IconName::WandSparkles)
                                    .label(tr("Neu erstellen"))
                                    .disabled(target.is_err())
                                    .on_click(cx.listener(|this, _, _, cx| this.start_regenerate(cx))),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if let Some(e) = self.regen_error.clone() {
            return Some(
                h_flex()
                    .p_3()
                    .gap_2()
                    .items_start()
                    .rounded(theme.radius)
                    .bg(theme.danger.opacity(0.1))
                    .text_color(theme.danger)
                    .text_sm()
                    .child(Icon::new(IconName::TriangleAlert).small().mt_0p5())
                    .child(div().flex_1().child(e))
                    .child(Button::new("regen-retry").xsmall().outline().label(tr("Erneut versuchen")).on_click(cx.listener(|this, _, window, cx| this.open_regenerate(window, cx))))
                    .child(Button::new("regen-error-close").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                        this.regen_error = None;
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }
        let (line, version) = self.regen_done.clone()?;
        Some(
            h_flex()
                .p_3()
                .gap_2()
                .rounded(theme.radius)
                .bg(theme.success.opacity(0.12))
                .text_sm()
                .child(Icon::new(IconName::Check).small().text_color(theme.success))
                .child(div().flex_1().child(line))
                .when_some(version, |el, v| {
                    el.child(
                        Button::new("regen-undo")
                            .xsmall()
                            .outline()
                            .icon(IconName::Undo2)
                            .label(tr("Rückgängig"))
                            .tooltip(tr("Stellt die vorherigen Notizen wieder her"))
                            .on_click(cx.listener(move |this, _, _, cx| this.restore_notes(v, cx))),
                    )
                })
                .child(Button::new("regen-done-close").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                    this.regen_done = None;
                    cx.notify();
                })))
                .into_any_element(),
        )
    }

    /// Earlier sets of notes (replaced by a regenerate), restorable.
    fn render_note_versions(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.note_versions.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let open = self.versions_open;
        let header = h_flex()
            .id("note-versions-toggle")
            .gap_1p5()
            .cursor_pointer()
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(theme.muted_foreground))
            .child(ui::section_title(tr("FRÜHERE NOTIZEN"), cx))
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!("({})", self.note_versions.len())))
            .on_click(cx.listener(|this, _, _, cx| {
                this.versions_open = !this.versions_open;
                cx.notify();
            }));
        let rows: Vec<AnyElement> = self
            .note_versions
            .iter()
            .map(|v| {
                let id = v.id;
                let live = v.notes.iter().all(|n| n.origin == "live");
                let what = if live { tr("Live mitgeschrieben") } else { tr("Neu erstellt") };
                let preview = v.notes.iter().take(3).map(|n| n.text.as_str()).collect::<Vec<_>>().join(" · ");
                h_flex()
                    .pl_5()
                    .gap_3()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div().text_sm().font_medium().child(format!(
                                    "{} · {} · {}",
                                    what,
                                    if v.notes.len() == 1 { tr("1 Notiz").to_string() } else { trf("{} Notizen", &[&v.notes.len()]) },
                                    trf("ersetzt {}", &[&i18n::date_time(&v.archived_at)])
                                )),
                            )
                            .when_some(v.focus.clone(), |el, f| el.child(div().text_xs().text_color(theme.muted_foreground).child(trf("Fokus: {}", &[&f]))))
                            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(preview)),
                    )
                    .child(
                        Button::new(("note-version-restore", id as usize))
                            .xsmall()
                            .outline()
                            .icon(IconName::RotateCcw)
                            .label(tr("Wiederherstellen"))
                            .tooltip(tr("Holt diese Notizen zurück; die jetzigen (außer angehefteten) kommen in die Liste"))
                            .on_click(cx.listener(move |this, _, _, cx| this.restore_notes(id, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        Some(v_flex().gap_3().child(header).when(open, |el| el.children(rows)).into_any_element())
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
            .child(ui::section_title(trf("CHAT MIT {}", &[&meeting_companion(m).to_uppercase()]), cx))
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
        let (imported, untimed) = (m.is_text_import(), m.is_untimed());
        let row = |n: &mostalgia_core::store::Note, cx: &mut Context<Self>| {
            let (id, at, pinned) = (n.id, n.at_secs, n.pinned);
            let kind = PointKind::parse(&n.kind);
            let color = match kind {
                Some(PointKind::Decision) => theme.success,
                Some(PointKind::Action) => theme.primary,
                Some(PointKind::Question) => theme.warning,
                Some(PointKind::Point) => theme.magenta,
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
                .when(!untimed, |el| {
                    el.child(
                        div()
                            .w(ui::transcript_rems(3.75))
                            .flex_none()
                            .pt_0p5()
                            .text_size(ui::transcript_rems(0.75))
                            .text_color(theme.primary)
                            .font_family(theme.mono_font_family.clone())
                            .child(ui::hms(at)),
                    )
                })
                .when(!self.notes_grouped, |el| {
                    el.child(
                        h_flex()
                            .flex_none()
                            .w(ui::transcript_rems(8.5))
                            .pt_0p5()
                            .gap_1p5()
                            .text_size(ui::transcript_rems(0.75))
                            .font_semibold()
                            .text_color(color)
                            .child(div().size(px(6.)).rounded_full().bg(color))
                            .child(kind.map_or(n.kind.clone(), |k| k.label().to_string())),
                    )
                })
                .child(ui::bidi_text(n.text.clone(), true).flex_1().min_w_0().text_size(ui::transcript_rems(0.875)))
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
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(if imported { tr("Im Transkript zeigen") } else { tr("Ab hier abspielen") }).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| this.jump_to(at, cx)))
                .into_any_element()
        };
        let mut out = v_flex().gap_1();
        if self.notes_grouped {
            let groups = [
                (PointKind::Decision, tr("ENTSCHEIDUNGEN")),
                (PointKind::Action, tr("AUFGABEN")),
                (PointKind::Question, tr("OFFENE FRAGEN")),
                (PointKind::Point, tr("KERNPUNKTE")),
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
            out = out.child(ui::section_title(trf("NOTIZEN VON {}", &[&meeting_companion(m).to_uppercase()]), cx).pb_1());
            for n in &m.notes {
                out = out.child(row(n, cx));
            }
        }
        out.into_any_element()
    }

    fn render_summary_body(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if self.summarizing || processing::get(processing::Key::Summary(self.id)).is_some() {
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
                .child(div().text_size(ui::transcript_rems(1.)).child(if ui::is_rtl(&s.markdown) {
                    ui::rtl_markdown(&summary_body(&s.markdown), cx).into_any_element()
                } else {
                    TextView::markdown("summary", summary_body(&s.markdown)).selectable(true).into_any_element()
                }))
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
        let (imported, untimed) = (m.is_text_import(), m.is_untimed());
        let engine_line = h_flex()
            .mb_3()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .when(imported, |el| {
                el.child(Icon::new(IconName::FileText).xsmall()).child(if untimed {
                    tr("Aus Text importiert · ohne Audio und ohne Zeiten")
                } else {
                    tr("Aus Text importiert · ohne Audio")
                })
            })
            .when_some(m.transcript_engine.clone().filter(|_| !imported), |el, engine| {
                el.child(Icon::new(if engine.contains("Whisper") { IconName::Cpu } else { IconName::Globe }).xsmall())
                    .child(div().min_w_0().child(if m.transcript_error.is_some() {
                        engine
                    } else {
                        trf("Transkribiert mit {}", &[&engine])
                    }))
            })
            .child(div().flex_1())
            .when(!imported, |el| el.child(
                Button::new("retranscribe-tab")
                    .xsmall()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .label(tr("Erneut transkribieren"))
                    .disabled(!has_audio || self.retranscribing.is_some())
                    .when(!has_audio, |b| b.tooltip(audio_gone(m)))
                    .on_click(cx.listener(|this, _, _, cx| this.open_retranscribe(cx))),
            ));
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
                    .flex_wrap()
                    .bg(theme.muted)
                    .child(div().text_sm().child(trf("„{}“ umbenennen in", &[&ui::speaker_display(current)])))
                    // Never squeezed by the hints next to it; they wrap instead.
                    .child(Input::new(input).w(px(240. * ui::ui_scale())).flex_none().small())
                    .child(Button::new("spk-save").small().primary().label(tr("Speichern")).on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))))
                    .child(Button::new("spk-cancel").small().ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))))
                    .when(current != "Ich" && mostalgia_core::diarize::available(), |el| {
                        el.child(div().text_xs().text_color(theme.muted_foreground).child(tr("Die Stimme wird gemerkt und künftig mit diesem Namen erkannt.")))
                    })
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Nur einzelne Zeilen? Rechtsklick auf eine Zeile.")))
                    .into_any_element(),
            );
        }

        if let Some(Edit::Reassign(ids, input)) = &self.edit {
            let n = ids.len();
            rows.push(
                h_flex()
                    .mb_3()
                    .p_3()
                    .gap_2()
                    .flex_wrap()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .child(div().text_sm().child(if n == 1 { tr("Diese Zeile gehört zu").to_string() } else { trf("Diese {} Zeilen gehören zu", &[&n]) }))
                    .child(Input::new(input).w(px(240. * ui::ui_scale())).flex_none().small())
                    .child(Button::new("reassign-save").small().primary().label(tr("Speichern")).on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))))
                    .child(Button::new("reassign-cancel").small().ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))))
                    .into_any_element(),
            );
        } else if !self.selected.is_empty() {
            let n = self.selected.len();
            let ids = self.selected.clone();
            let names = speaker_choices(self.meeting_speakers());
            rows.push(
                h_flex()
                    .mb_3()
                    .p_3()
                    .gap_2()
                    .flex_wrap()
                    .rounded(theme.radius)
                    .bg(theme.primary.opacity(0.08))
                    .child(div().text_sm().font_medium().child(if n == 1 { tr("1 Zeile ausgewählt").to_string() } else { trf("{} Zeilen ausgewählt", &[&n]) }))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(tr("Zuweisen an:")))
                    .children(names.into_iter().enumerate().map(|(i, name)| {
                        let ids = ids.clone();
                        Button::new(("split-to", i))
                            .xsmall()
                            .outline()
                            .label(ui::speaker_display(&name))
                            .on_click(cx.listener(move |this, _, _, cx| this.reassign(ids.clone(), name.clone(), cx)))
                    }))
                    .child({
                        let ids = ids.clone();
                        Button::new("split-new").xsmall().ghost().icon(IconName::Plus).label(tr("Neuer Name …")).on_click(cx.listener(move |this, _, window, cx| {
                            let ids = ids.clone();
                            this.begin_edit(move |input| Edit::Reassign(ids, input), String::new(), window, cx)
                        }))
                    })
                    .child(div().flex_1())
                    .child(Button::new("split-clear").xsmall().ghost().label(tr("Auswahl aufheben")).on_click(cx.listener(|this, _, _, cx| {
                        this.selected.clear();
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }

        let choices = self.meeting_speakers();
        let view = cx.entity().downgrade();
        let mut marks = m.bookmarks.iter().peekable();
        for (ix, seg) in m.segments.iter().enumerate() {
            while let Some(b) = marks.next_if(|b| b.at_secs <= seg.start_secs) {
                rows.push(ui::mark_row(b, false, cx).into_any_element());
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
                    .text_size(ui::transcript_rems(1.))
                    .line_height(ui::transcript_rems(1.55))
                    .child(ui::bidi_text(seg.text.clone(), true))
                    .into_any_element(),
            };
            let group = SharedString::from(format!("seg-{ix}"));
            let flashing = self.flash.is_some_and(|(id, _)| id == seg_id);
            let is_selected = self.selected.contains(&seg_id);
            // Right-click: the selection if this line is in it, else this line.
            let targets = if is_selected { self.selected.clone() } else { vec![seg_id] };
            let menu = {
                let (view, choices, current) = (view.clone(), choices.clone(), label.clone());
                move |menu: gpui_kit::component::menu::PopupMenu, _: &mut Window, _: &mut Context<gpui_kit::component::menu::PopupMenu>| {
                    let n = targets.len();
                    let choices = speaker_choices(choices.clone());
                    let mut menu = menu.item(PopupMenuItem::label(if n == 1 {
                        tr("Sprecher ändern").to_string()
                    } else {
                        trf("Sprecher von {} Zeilen ändern", &[&n])
                    }));
                    for name in choices.iter().filter(|c| n > 1 || **c != current) {
                        let (view, targets, name) = (view.clone(), targets.clone(), name.clone());
                        menu = menu.item(PopupMenuItem::new(ui::speaker_display(&name)).icon(IconName::User).on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.reassign(targets.clone(), name.clone(), cx)).ok();
                        }));
                    }
                    let (new_view, new_targets) = (view.clone(), targets.clone());
                    menu = menu.item(PopupMenuItem::new(tr("Neuer Name …")).icon(IconName::Plus).on_click(move |_, window, cx| {
                        let targets = new_targets.clone();
                        new_view.update(cx, |this, cx| this.begin_edit(move |input| Edit::Reassign(targets, input), String::new(), window, cx)).ok();
                    }));
                    let view = view.clone();
                    menu.separator().item(
                        PopupMenuItem::new(if is_selected { tr("Aus der Auswahl nehmen") } else { tr("Auswählen (zum Aufteilen)") })
                            .icon(IconName::Check)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.toggle_selected(ix, seg_id);
                                    cx.notify();
                                })
                                .ok();
                            }),
                    )
                }
            };
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
                    .when(is_selected, |el| el.bg(theme.primary.opacity(0.14)))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| this.click_line(ix, seg_id, ev.modifiers(), cx)))
                    .items_start()
                    .gap_3()
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .when(playing, |el| el.bg(theme.primary.opacity(0.08)))
                    .hover(|s| s.bg(theme.muted.opacity(0.5)))
                    .when(!untimed, |el| el.child(
                        div()
                            .id(("ts", ix))
                            .w(ui::transcript_rems(3.75))
                            .flex_none()
                            .pt_0p5()
                            .text_size(ui::transcript_rems(0.75))
                            .font_family(theme.mono_font_family.clone())
                            .text_color(if playing { theme.primary } else { theme.muted_foreground })
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.primary))
                            .child(ui::hms(start))
                            .on_click(cx.listener(move |this, _, _, cx| this.play_from(start, cx)))
                    ))
                    .when(!(imported && seg.speaker.is_none()), |el| el.child(
                        div()
                            .id(("spk", ix))
                            .cursor_pointer()
                            .child(ui::speaker_chip(&label, cx))
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr("Sprecher umbenennen")).build(window, cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let current = label.clone();
                                let value = if current == "Ich" || current == "Andere" { String::new() } else { current.clone() };
                                this.begin_edit(move |input| Edit::Speaker(current, input), value, window, cx);
                            }))
                    ))
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
                    .context_menu(menu)
                    .into_any_element(),
            );
        }
        // Marks after the last line (e.g. the mic switched off near the end).
        for b in marks {
            rows.push(ui::mark_row(b, false, cx).into_any_element());
        }
        v_flex().children(rows).into_any_element()
    }

    fn render_audio(&mut self, m: &Meeting, cx: &mut Context<Self>) -> AnyElement {
        if m.tracks().is_empty() {
            return v_flex()
                .py_16()
                .items_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::Trash).large())
                .child(div().font_medium().child(audio_gone(m)))
                .child(div().text_sm().child(tr("Transkript, Zusammenfassung, Notizen und Sprecher sind geblieben.")))
                .into_any_element();
        }
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
        if m.is_text_import() && self.tab > 2 {
            self.tab = 1;
        }
        let body = match self.tab {
            0 => self.render_summary(&m, cx),
            1 => self.render_transcript(&m, cx),
            2 => self.render_notes_tab(&m, cx),
            _ => self.render_audio(&m, cx),
        };
        let note_count = m.notes.len();
        let lines = m.segments.len();
        // No audio tab for a meeting imported from text.
        let imported = m.is_text_import();
        self.meeting = Some(m);
        let theme = cx.theme();

        v_flex()
            .size_full()
            .relative()
            .key_context("Meeting")
            .on_action(cx.listener(Self::export))
            .on_action(cx.listener(|this, action: &CopyRecap, window, cx| {
                let length = if action.0 >= 2 { recap::Length::Full } else { recap::Length::Short };
                let style = if action.0 % 2 == 1 { recap::Style::Markdown } else { recap::Style::Plain };
                this.copy_recap(length, style, window, cx);
            }))
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
                        .when(!imported, |bar| bar.child(Tab::new().prefix(Icon::new(IconName::AudioLines).small()).label(tr("Audio")))),
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
                        .child(div().w_full().max_w(px(860.)).mx_auto().px_8().py_6().child(body)),
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

/// Why a meeting has no audio: deleted by the cleanup after N days, or gone.
fn audio_gone(m: &Meeting) -> SharedString {
    match mostalgia_core::cleanup::deleted(&m.dir) {
        Some(d) => trf("Audio nach {} Tagen gelöscht ({})", &[&d.days, &i18n::date_time(&mostalgia_core::cleanup::local(d.at))]).into(),
        None => tr("Audiodateien gelöscht").into(),
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

/// The chosen companion's name (who writes and regenerates the notes).
/// The companion who took this meeting's notes and chat.
fn meeting_companion(m: &Meeting) -> &'static str {
    mostalgia_core::companion::Character::parse(&m.companion).unwrap_or(mostalgia_core::companion::Character::Mori).name()
}

fn companion_name() -> &'static str {
    Config::load().companion_character().name()
}

/// Names to offer when changing a line's speaker: the meeting's speakers,
/// then the remembered voices (read when the menu opens).
fn speaker_choices(mut names: Vec<String>) -> Vec<String> {
    for v in voices::Voices::load().voices {
        if !names.contains(&v.name) {
            names.push(v.name);
        }
    }
    names
}
