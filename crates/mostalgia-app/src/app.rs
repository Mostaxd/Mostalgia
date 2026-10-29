//! The window: title bar, library sidebar, and the current screen.

use std::sync::{Arc, Mutex};

use chrono::{Datelike, Local};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, StyledExt, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::i18n::{self, tr, trf};
use mostalgia_core::paths;
use mostalgia_core::store::{MeetingSummary, SearchHit, Store};

use crate::companion::{Companion, MoriEvent};
use crate::meeting::{MeetingEvent, MeetingView};
use crate::recording::{RecordingEvent, RecordingView, SharedEngine};
use crate::settings::{SettingsEvent, SettingsView};
use crate::{control, ui};

actions!(mostalgia, [OpenAsk, ZoomIn, ZoomOut, ZoomReset]);

enum Screen {
    Recording,
    Ask(Entity<crate::ask_view::AskView>),
    Documents(Entity<crate::documents::DocumentsView>),
    /// The decision log (when it's switched on).
    Decisions(Entity<crate::decisions_view::DecisionsView>),
    Meeting(Entity<MeetingView>),
    Settings(Entity<SettingsView>),
    /// "Besprechung aus Text" (a meeting that wasn't recorded).
    TextImport(Entity<crate::text_import::TextImportView>),
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
    tray: Option<control::TrayHandle>,
    /// "Fragen" screen, kept so the conversation survives switching away.
    ask_view: Option<Entity<crate::ask_view::AskView>>,
    /// "Dokumente" screen, kept like the ask view.
    documents_view: Option<Entity<crate::documents::DocumentsView>>,
    /// Notices calls and offers / starts recordings for them.
    autostart: crate::autostart::AutoStart,
    /// Language the input placeholders were last set in.
    lang_seen: Option<i18n::Lang>,
    /// Selected meetings in the sidebar (Ctrl/Shift-click for several).
    selected: Vec<i64>,
    /// Where a Shift-click range starts.
    anchor: Option<i64>,
    /// Meetings waiting for the delete confirmation, and whether their
    /// audio files go too.
    confirm_delete: Option<Vec<i64>>,
    /// Audio the cleanup would delete, waiting for the user's first yes.
    confirm_cleanup: Option<Vec<mostalgia_core::cleanup::Candidate>>,
    /// The live-caption window's process, and the stall it was last told.
    caption_host: crate::companion::host::DesktopHost,
    captions_stalled: Option<Option<u64>>,
    /// "Import recording…": the file, its title and date being edited.
    import: Option<ImportDialog>,
    /// The `audio_keep_days` the last cleanup ran with.
    cleanup_days: Option<u32>,
    delete_audio: bool,
    /// Meeting being renamed inline in the sidebar.
    rename: Option<(i64, Entity<InputState>)>,
    library_focus: FocusHandle,
    /// Mori, drawn over the content when turned on.
    companion: Entity<Companion>,
    /// Mori on the desktop (her own process), when she's put there.
    mori_host: crate::companion::host::DesktopHost,
    /// The open meeting as last told to Mori (for her "This meeting" scope).
    mori_open_meeting: Option<Option<(i64, String)>>,
    _subscriptions: Vec<Subscription>,
}

actions!(library, [DeleteSelected]);

/// The import dialog for an audio or video file.
struct ImportDialog {
    path: std::path::PathBuf,
    title: Entity<InputState>,
    date: Entity<InputState>,
    error: Option<SharedString>,
}

impl MostalgiaApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine: SharedEngine = Arc::new(Mutex::new(None));
        let recording = cx.new(|cx| RecordingView::new(engine.clone(), cx));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Transkripte durchsuchen")));
        let companion = cx.new(|cx| Companion::new(recording.clone(), window, cx));
        let subs = vec![
            cx.subscribe_in(&recording, window, |this, _, event: &RecordingEvent, window, cx| match event {
                RecordingEvent::Saved(id) => {
                    // Pinned points go into the meeting before it opens
                    // (on the desktop she stores them and says so).
                    if this.mori_host.is_running() {
                        this.mori_host.send(&crate::companion::ipc::ToMori::Saved { meeting_id: *id });
                    } else {
                        this.companion.update(cx, |c, cx| c.recording_saved(*id, cx));
                    }
                    this.reload(cx);
                    this.open_meeting(*id, window, cx);
                    if let Screen::Meeting(view) = &this.screen {
                        view.update(cx, |v, cx| v.auto_summarize(cx));
                    }
                }
                // Mori says so if she started it (her record button).
                RecordingEvent::StartFailed(message) => {
                    if this.mori_host.is_running() {
                        this.mori_host.send(&crate::companion::ipc::ToMori::StartFailed { message: message.clone() });
                    } else {
                        let message = message.clone();
                        this.companion.update(cx, |c, cx| c.recording_start_failed(message, cx));
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
            cx.subscribe(&companion, |this, _, event: &crate::companion::ShowLiveMoment, cx| {
                this.screen = Screen::Recording;
                this.recording.update(cx, |r, cx| r.scroll_to_time(event.secs, cx));
                cx.notify();
            }),
            cx.subscribe_in(&companion, window, |this, _, event: &MoriEvent, window, cx| match event {
                MoriEvent::OpenCitation { meeting_id, secs } => {
                    this.open_meeting(*meeting_id, window, cx);
                    if let Some(m) = this.meeting_view() {
                        let secs = *secs;
                        m.update(cx, |m, cx| m.jump_to(secs, cx));
                    }
                }
                MoriEvent::OpenDocument { doc_id, loc } => this.open_document(*doc_id, Some(*loc), window, cx),
                MoriEvent::OpenSettings => {
                    this.open_settings(window, cx);
                    if let Some(s) = this.settings_view() {
                        s.update(cx, |s, cx| s.show_category("ki", cx));
                    }
                }
                MoriEvent::PopOut => this.set_mori_desktop(true, cx),
                MoriEvent::BackToApp => this.set_mori_desktop(false, cx),
                MoriEvent::StartRecording { mic, system, people } => this.start_recording_with(*mic, *system, *people, cx),
                MoriEvent::SetPaused { paused } => {
                    let paused = *paused;
                    this.recording.update(cx, |r, cx| r.set_paused(paused, cx));
                }
                MoriEvent::StopRecording => this.recording.update(cx, |r, cx| r.stop(cx)),
                MoriEvent::SetSource { source, on } => {
                    let (source, on) = (*source, *on);
                    this.recording.update(cx, |r, cx| r.set_source(source, on, cx));
                }
                MoriEvent::OpenMoriSettings => this.open_mori_settings(window, cx),
                MoriEvent::NotesSaved { meeting_id } => {
                    if let Some(m) = this.meeting_view().filter(|m| m.read(cx).id == *meeting_id) {
                        m.update(cx, |m, cx| m.reload(cx));
                    }
                }
            }),
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
            ask_view: None,
            documents_view: None,
            autostart: crate::autostart::AutoStart::new(),
            lang_seen: None,
            selected: Vec::new(),
            anchor: None,
            confirm_delete: None,
            confirm_cleanup: None,
            caption_host: crate::companion::host::DesktopHost::captions(),
            captions_stalled: None,
            import: None,
            cleanup_days: None,
            delete_audio: true,
            rename: None,
            library_focus: cx.focus_handle(),
            companion,
            mori_host: Default::default(),
            mori_open_meeting: None,
            _subscriptions: subs,
        };
        this.reload(cx);
        this.sync_mori(false);
        // Recordings a crash left unfinished become meetings now.
        this.recording.update(cx, |r, cx| r.recover_orphans(cx));
        // Old audio: now, then once a day.
        this.run_cleanup(cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(24 * 3600)).await;
                if this.update(cx, |this, cx| this.run_cleanup(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Work on meetings (see `processing`): redraw the library and the
        // open screens whenever it moves on.
        cx.spawn(async move |this, cx| {
            let mut seen = crate::processing::generation();
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let now = crate::processing::generation();
                if now == seen {
                    continue;
                }
                seen = now;
                let alive = this
                    .update(cx, |this, cx| {
                        this.recording.update(cx, |_, cx| cx.notify());
                        if let Some(m) = this.meeting_view() {
                            m.update(cx, |m, cx| m.processing_changed(cx));
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        cx.on_app_quit(|this, _| {
            this.mori_host.stop();
            this.caption_host.stop();
            async {}
        })
        .detach();
        this.start_control(window, cx);
        // Bring the "ask your meetings" index up to date in the background.
        // It's cancelled and joined on quit: exiting while the embedding
        // model is mid-run crashes in native teardown.
        if std::env::var_os("MOSTALGIA_NO_BACKFILL").is_none() {
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker = {
                let cancel = cancel.clone();
                std::thread::spawn(move || {
                    if let Err(e) = mostalgia_core::ask::index_pending_until(&cancel) {
                        eprintln!("[mostalgia] search index: {e:#}");
                    }
                    // Documents added before the embedding model was there.
                    if let Err(e) = mostalgia_core::docs::index_pending(&cancel) {
                        eprintln!("[mostalgia] documents index: {e:#}");
                    }
                    // Finish a switch to another embedding model.
                    if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        mostalgia_core::ask::reembed::start();
                    }
                })
            };
            let worker = Mutex::new(Some(worker));
            cx.on_app_quit(move |_, _| {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Some(w) = worker.lock().unwrap().take() {
                    let _ = w.join();
                }
                mostalgia_core::ask::reembed::stop();
                async {}
            })
            .detach();
        }
        this
    }

    #[doc(hidden)]
    pub fn ask_view(&self) -> Option<Entity<crate::ask_view::AskView>> {
        self.ask_view.clone()
    }

    /// Opens the "Fragen" screen (Ctrl+K), keeping an earlier conversation.
    pub fn open_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = match &self.ask_view {
            Some(v) => v.clone(),
            None => {
                let v = cx.new(|cx| crate::ask_view::AskView::new(window, cx));
                let sub = cx.subscribe_in(&v, window, |this, _, event: &crate::ask_view::AskViewEvent, window, cx| match event {
                    crate::ask_view::AskViewEvent::OpenCitation { meeting_id, secs } => {
                        this.open_meeting(*meeting_id, window, cx);
                        if let Some(m) = this.meeting_view() {
                            let secs = *secs;
                            m.update(cx, |m, cx| m.jump_to(secs, cx));
                        }
                    }
                    crate::ask_view::AskViewEvent::OpenDocument { doc_id, loc } => this.open_document(*doc_id, Some(*loc), window, cx),
                });
                self._subscriptions.push(sub);
                self.ask_view = Some(v.clone());
                v
            }
        };
        // Coming from "Dokumente", its selection is offered first.
        let offers = match &self.screen {
            Screen::Documents(d) => d.read(cx).ask_offers(),
            _ => Vec::new(),
        };
        view.update(cx, |v, cx| v.offer(offers, cx));
        let focus = view.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        self.screen = Screen::Ask(view);
        cx.notify();
    }

    /// Opens the "Dokumente" screen, keeping its state.
    /// The decision log; `build` starts filling it from earlier meetings.
    pub fn open_decisions(&mut self, build: bool, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| crate::decisions_view::DecisionsView::new(window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, event: &crate::decisions_view::DecisionsEvent, window, cx| match event {
            crate::decisions_view::DecisionsEvent::Open { meeting_id, secs } => {
                this.open_meeting(*meeting_id, window, cx);
                if let Some(m) = this.meeting_view() {
                    let secs = *secs;
                    m.update(cx, |m, cx| match secs {
                        Some(secs) => m.jump_to(secs, cx),
                        None => m.set_tab(1, cx),
                    });
                }
            }
        });
        self._subscriptions.push(sub);
        if build {
            view.update(cx, |v, cx| v.build(cx));
        }
        self.screen = Screen::Decisions(view);
        cx.notify();
    }

    #[doc(hidden)]
    pub fn decisions_view(&self) -> Option<Entity<crate::decisions_view::DecisionsView>> {
        match &self.screen {
            Screen::Decisions(v) => Some(v.clone()),
            _ => None,
        }
    }

    pub fn open_documents(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<crate::documents::DocumentsView> {
        let view = match &self.documents_view {
            Some(v) => v.clone(),
            None => {
                let v = cx.new(|cx| crate::documents::DocumentsView::new(window, cx));
                // "Ask about this": the Ask screen, about that document or collection.
                let sub = cx.subscribe_in(&v, window, |this, _, event: &crate::documents::DocumentsEvent, window, cx| match event {
                    crate::documents::DocumentsEvent::Ask(within) => {
                        let within = *within;
                        this.open_ask(window, cx);
                        if let Some(a) = &this.ask_view {
                            a.update(cx, |a, cx| a.set_within(Some(within), cx));
                        }
                    }
                });
                self._subscriptions.push(sub);
                self.documents_view = Some(v.clone());
                v
            }
        };
        self.screen = Screen::Documents(view.clone());
        cx.notify();
        view
    }

    /// Opens a document in the reader, at a page or section (a citation).
    pub fn open_document(&mut self, doc_id: i64, loc: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.open_documents(window, cx);
        view.update(cx, |v, cx| v.open(doc_id, loc, cx));
    }

    #[doc(hidden)]
    pub fn documents_view(&self) -> Option<Entity<crate::documents::DocumentsView>> {
        self.documents_view.clone()
    }

    /// The control socket (`mostalgia toggle`) and the tray icon.
    fn start_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, rx) = std::sync::mpsc::channel();
        if let Err(e) = control::listen(tx.clone()) {
            eprintln!("[mostalgia] control socket: {e}");
        }
        match control::TrayHandle::spawn(control::Tray { tx, recording: false, mic_on: false }) {
            Ok(handle) => self.tray = Some(handle),
            Err(e) => eprintln!("[mostalgia] tray icon unavailable: {e}"),
        }
        cx.on_app_quit(|_, _| async { control::cleanup() }).detach();
        cx.spawn_in(window, async move |this, cx| {
            let mut was_recording = (false, false);
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
                let commands: Vec<_> = rx.try_iter().collect();
                let alive = this
                    .update_in(cx, |this, window, cx| {
                        for cmd in commands {
                            this.handle_command(cmd, window, cx);
                        }
                        this.autostart_tick(cx);
                        this.mori_tick(window, cx);
                        let mic = this.recording.read(cx).source_on(mostalgia_core::Source::Mic);
                        let state = (mic.is_some(), mic.unwrap_or(false));
                        if state != was_recording {
                            was_recording = state;
                            if let Some(tray) = &this.tray {
                                tray.set_state(state.0, state.1);
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

    /// Moves Mori between the app window and the desktop (dragged out, or
    /// "back into the app"), the same as the setting.
    fn set_mori_desktop(&mut self, desktop: bool, cx: &mut Context<Self>) {
        let mut config = mostalgia_core::config::Config::load();
        if config.companion_desktop == desktop {
            return;
        }
        config.companion_desktop = desktop;
        if let Err(e) = config.save() {
            eprintln!("[mostalgia] saving settings: {e:#}");
        }
        if let Some(s) = self.settings_view() {
            s.update(cx, |s, cx| s.set_companion_desktop(desktop, cx));
        }
        self.companion.update(cx, |c, cx| c.reload_config(cx));
        self.sync_mori(true);
        cx.notify();
    }

    /// Tells Mori (in the window and on the desktop) which meeting is open.
    fn tell_mori_open_meeting(&mut self, cx: &mut Context<Self>) {
        let open = self.meeting_view().map(|v| {
            let id = v.read(cx).id;
            (id, self.meetings.iter().find(|m| m.id == id).map(|m| m.title.clone()).unwrap_or_default())
        });
        if self.mori_open_meeting.as_ref() == Some(&open) {
            return;
        }
        self.mori_open_meeting = Some(open.clone());
        let for_window = open.clone();
        self.companion.update(cx, |c, cx| c.set_open_meeting(for_window, cx));
        self.mori_host.send(&crate::companion::ipc::ToMori::OpenMeeting { meeting: open });
    }

    /// Starts or ends Mori's desktop process to match the settings.
    fn sync_mori(&mut self, settings_changed: bool) {
        let config = mostalgia_core::config::Config::load();
        if config.companion && config.companion_desktop {
            self.mori_host.start(settings_changed);
        } else {
            self.mori_host.stop();
            self.mori_host.problem = None;
        }
    }

    /// Mori's messages from the desktop, and the recording state for her.
    fn mori_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::companion::ipc::FromMori;
        let had_problem = self.mori_host.problem.is_some();
        for event in self.mori_host.poll() {
            match event {
                // A (re)started Mori learns the open meeting again.
                FromMori::Ready => self.mori_open_meeting = None,
                FromMori::OpenMeeting { meeting_id, secs } => {
                    self.open_meeting(meeting_id, window, cx);
                    if let Some(m) = self.meeting_view() {
                        m.update(cx, |m, cx| m.jump_to(secs, cx));
                    }
                    window.activate_window();
                }
                FromMori::OpenDocument { doc_id, loc } => {
                    self.open_document(doc_id, Some(loc), window, cx);
                    window.activate_window();
                }
                FromMori::ShowLiveMoment { secs } => {
                    self.screen = Screen::Recording;
                    self.recording.update(cx, |r, cx| r.scroll_to_time(secs, cx));
                    window.activate_window();
                    cx.notify();
                }
                FromMori::OpenSettings => {
                    self.open_settings(window, cx);
                    if let Some(s) = self.settings_view() {
                        s.update(cx, |s, cx| s.show_category("ki", cx));
                    }
                    window.activate_window();
                }
                FromMori::BackToApp => {
                    self.set_mori_desktop(false, cx);
                    window.activate_window();
                }
                FromMori::StartRecording { mic, system, people } => self.start_recording_with(mic, system, people, cx),
                FromMori::PauseRecording { paused } => self.recording.update(cx, |r, cx| r.set_paused(paused, cx)),
                FromMori::StopRecording => self.recording.update(cx, |r, cx| r.stop(cx)),
                FromMori::SetSource { source, on } => {
                    if let Some(source) = mostalgia_core::Source::parse(&source) {
                        self.recording.update(cx, |r, cx| r.set_source(source, on, cx));
                    }
                }
                FromMori::OpenMoriSettings => {
                    self.open_mori_settings(window, cx);
                    window.activate_window();
                }
                FromMori::Flag { secs } => self.recording.update(cx, |r, cx| r.flag_at(secs, cx)),
                FromMori::ShowApp => window.activate_window(),
                FromMori::NotesSaved { meeting_id } => {
                    if let Some(m) = self.meeting_view().filter(|m| m.read(cx).id == meeting_id) {
                        m.update(cx, |m, cx| m.reload(cx));
                    }
                }
            }
        }
        if self.mori_host.problem.is_some() != had_problem {
            cx.notify();
        }
        let recording = self.recording.read(cx);
        self.mori_host.feed(recording);
        self.captions_tick(window, cx);
    }

    /// The live-caption window: up while a recording runs (if switched
    /// on), fed like Mori, and what it asks for.
    fn captions_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::companion::ipc::{FromMori, ToMori};
        let recording = self.recording.read(cx);
        let wanted = recording.live_state().is_some() && mostalgia_core::config::Config::load().live_captions;
        if !wanted {
            if self.caption_host.is_running() {
                self.caption_host.stop();
            }
            self.caption_host.problem = None;
            self.captions_stalled = None;
            return;
        }
        if !self.caption_host.is_running() {
            self.caption_host.start(false);
            self.captions_stalled = None;
        }
        let stalled = recording.stalled_secs();
        self.caption_host.feed(recording);
        if self.captions_stalled != Some(stalled) {
            self.captions_stalled = Some(stalled);
            self.caption_host.send(&ToMori::Stalled { secs: stalled });
        }
        for event in self.caption_host.poll() {
            match event {
                FromMori::Ready => self.captions_stalled = None,
                FromMori::Flag { secs } => self.recording.update(cx, |r, cx| r.flag_at(secs, cx)),
                FromMori::ShowApp => {
                    self.screen = Screen::Recording;
                    window.activate_window();
                    cx.notify();
                }
                FromMori::PauseRecording { paused } => self.recording.update(cx, |r, cx| r.set_paused(paused, cx)),
                FromMori::SetSource { source, on } => {
                    if let Some(source) = mostalgia_core::Source::parse(&source) {
                        self.recording.update(cx, |r, cx| r.set_source(source, on, cx));
                    }
                }
                _ => {}
            }
        }
    }

    /// Why Mori isn't on the desktop although she should be.
    pub fn mori_problem(&self) -> Option<&str> {
        self.mori_host.problem.as_deref()
    }

    fn handle_command(&mut self, cmd: control::Command, window: &mut Window, cx: &mut Context<Self>) {
        use control::Command;
        let recording = self.recording.read(cx).is_recording();
        let active = self.recording.read(cx).is_active();
        match cmd {
            Command::Toggle if recording => self.recording.update(cx, |r, cx| r.stop(cx)),
            Command::Toggle | Command::Start if !active => self.start_recording(cx),
            Command::Stop if recording => self.recording.update(cx, |r, cx| r.stop(cx)),
            Command::ToggleMic if recording => {
                self.recording.update(cx, |r, cx| {
                    if let Some(on) = r.source_on(mostalgia_core::Source::Mic) {
                        r.set_source(mostalgia_core::Source::Mic, !on, cx);
                    }
                })
            }
            Command::Show => window.activate_window(),
            Command::Quit => cx.quit(),
            _ => {}
        }
    }

    fn autostart_tick(&mut self, cx: &mut Context<Self>) {
        let active = self.recording.read(cx).is_active();
        for action in self.autostart.tick(active) {
            self.apply_autostart(action, cx);
        }
        if self.autostart.prompt.is_some() {
            cx.notify();
        }
    }

    fn apply_autostart(&mut self, action: crate::autostart::Action, cx: &mut Context<Self>) {
        match action {
            crate::autostart::Action::Start(_) if !self.recording.read(cx).is_active() => {
                self.screen = Screen::Recording;
                self.recording.update(cx, |r, cx| r.start_call(cx));
                cx.notify();
            }
            crate::autostart::Action::Stop if self.recording.read(cx).is_recording() => {
                self.recording.update(cx, |r, cx| r.stop(cx))
            }
            _ => {}
        }
    }

    /// "Anruf erkannt" bar above the content while asking whether to record.
    fn render_call_prompt(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let app = self.autostart.prompt.clone()?;
        let theme = cx.theme();
        Some(
            h_flex()
                .px_4()
                .py_2()
                .gap_3()
                .bg(theme.primary.opacity(0.12))
                .border_b_1()
                .border_color(theme.border)
                .child(Icon::new(IconName::Mic).small().text_color(theme.primary))
                .child(div().flex_1().text_sm().child(trf("Anruf in {} erkannt – aufnehmen?", &[&app])))
                .child(
                    Button::new("call-record")
                        .small()
                        .primary()
                        .label(tr("Aufnahme starten"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(action) = this.autostart.accept() {
                                this.apply_autostart(action, cx);
                            }
                            cx.notify();
                        })),
                )
                .child(Button::new("call-ignore").small().ghost().label(tr("Ignorieren")).on_click(cx.listener(|this, _, _, cx| {
                    this.autostart.dismiss();
                    cx.notify();
                })))
                .into_any_element(),
        )
    }

    /// Switches to the recording screen and starts recording.
    pub fn start_recording(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Recording;
        self.recording.update(cx, |r, cx| r.start(cx));
        cx.notify();
    }

    /// Mori's record button: a recording from exactly these sources (the
    /// setup screen's saved choice stays as it is), with the people in the
    /// call if picked there.
    fn start_recording_with(&mut self, mic: bool, system: bool, people: u8, cx: &mut Context<Self>) {
        self.screen = Screen::Recording;
        self.recording.update(cx, |r, cx| {
            if people != 0 {
                r.set_people(people, cx);
            }
            r.start_with(mic, system, cx)
        });
        cx.notify();
    }

    /// Settings → Companion, where her options are.
    fn open_mori_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
        if let Some(s) = self.settings_view() {
            s.update(cx, |s, cx| s.show_category("begleiterin", cx));
        }
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
        let engine = self.engine.clone();
        let view = cx.new(|cx| MeetingView::new(id, cx).with_engine(engine));
        let sub = cx.subscribe_in(&view, window, |this, _, event: &MeetingEvent, _, cx| match event {
            MeetingEvent::Changed => this.reload(cx),
            MeetingEvent::Deleted => {
                this.screen = Screen::Recording;
                this.reload(cx);
            }
            MeetingEvent::RequestDelete(id) => {
                this.confirm_delete = Some(vec![*id]);
                this.delete_audio = true;
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        self.screen = Screen::Meeting(view);
        cx.notify();
    }

    /// The "meeting from text" screen.
    pub fn open_text_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::text_import::{TextImportEvent, TextImportView};
        let view = cx.new(|cx| TextImportView::new(window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, event: &TextImportEvent, window, cx| match event {
            TextImportEvent::Imported { meeting_id, keep_title, focus, style } => {
                this.reload(cx);
                this.open_meeting(*meeting_id, window, cx);
                if let Some(m) = this.meeting_view() {
                    let (keep_title, focus, style) = (*keep_title, focus.clone(), *style);
                    m.update(cx, |m, cx| m.after_text_import(keep_title, focus, style, cx));
                }
            }
            TextImportEvent::Cancel => this.show_recording(cx),
        });
        self._subscriptions.push(sub);
        self.screen = Screen::TextImport(view);
        cx.notify();
    }

    #[doc(hidden)]
    pub fn text_import_view(&self) -> Option<Entity<crate::text_import::TextImportView>> {
        match &self.screen {
            Screen::TextImport(v) => Some(v.clone()),
            _ => None,
        }
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gpu = self
            .engine
            .lock()
            .ok()
            .and_then(|e| e.as_ref().and_then(|e| e.gpu().map(str::to_string)));
        let view = cx.new(|cx| SettingsView::new(gpu, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, event: &SettingsEvent, window, cx| {
            if let SettingsEvent::BuildDecisions = event {
                this.open_decisions(true, window, cx);
                return;
            }
            if this.cleanup_days != Some(mostalgia_core::config::Config::load().audio_keep_days) {
                this.run_cleanup(cx);
            }
            this.recording.update(cx, |r, cx| r.reload_config(cx));
            this.companion.update(cx, |c, cx| c.reload_config(cx));
            this.sync_mori(true);
            this.mori_host.send(&crate::companion::ipc::ToMori::ReloadConfig);
            cx.notify();
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
    pub fn companion_view(&self) -> Entity<Companion> {
        self.companion.clone()
    }

    #[doc(hidden)]
    pub fn meeting_view(&self) -> Option<Entity<MeetingView>> {
        match &self.screen {
            Screen::Meeting(v) => Some(v.clone()),
            _ => None,
        }
    }

    #[doc(hidden)]
    /// Ctrl+= / Ctrl+- / Ctrl+0: the app-wide text size one step bigger,
    /// smaller, or back to 100 % (saved; Mori on the desktop follows).
    fn zoom(&mut self, up: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        let mut config = mostalgia_core::config::Config::load();
        if !config.zoom(up) {
            return;
        }
        if let Err(e) = config.save() {
            eprintln!("[mostalgia] saving the text size: {e:#}");
        }
        ui::apply_theme(window, cx);
        if let Some(s) = self.settings_view() {
            s.update(cx, |s, cx| s.reload_text_scale(cx));
        }
        self.mori_host.send(&crate::companion::ipc::ToMori::ReloadConfig);
        cx.notify();
    }

    /// The zoom keys while nothing has focus (they reach no view then).
    pub fn zoom_unfocused(up: Option<bool>, cx: &mut App) {
        let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) else { return };
        let _ = handle.update(cx, |root, window, cx| {
            let Ok(root) = root.downcast::<gpui_kit::component::Root>() else { return };
            let Ok(app) = root.read(cx).view().clone().downcast::<MostalgiaApp>() else { return };
            app.update(cx, |a, cx| a.zoom(up, window, cx));
        });
    }

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

    /// Click on a sidebar meeting: plain = open it, Ctrl = add/remove from
    /// the selection, Shift = select the range from the last click.
    fn click_meeting(&mut self, id: i64, modifiers: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        self.library_focus.focus(window, cx);
        if modifiers.control || modifiers.platform {
            match self.selected.iter().position(|s| *s == id) {
                Some(i) => {
                    self.selected.remove(i);
                }
                None => self.selected.push(id),
            }
            self.anchor = Some(id);
        } else if modifiers.shift {
            let ids: Vec<i64> = self.meetings.iter().map(|m| m.id).collect();
            let from = self.anchor.and_then(|a| ids.iter().position(|x| *x == a)).unwrap_or(0);
            let to = ids.iter().position(|x| *x == id).unwrap_or(from);
            let (a, b) = if from <= to { (from, to) } else { (to, from) };
            self.selected = ids[a..=b].to_vec();
        } else {
            self.selected = vec![id];
            self.anchor = Some(id);
            self.open_meeting(id, window, cx);
        }
        cx.notify();
    }

    /// The meetings an action on `id` applies to: the whole selection if
    /// `id` is part of it, else just `id`.
    fn targets(&self, id: i64) -> Vec<i64> {
        if self.selected.contains(&id) { self.selected.clone() } else { vec![id] }
    }

    /// Opens the delete confirmation for `ids` (an empty list closes it).
    pub fn request_delete(&mut self, ids: Vec<i64>, cx: &mut Context<Self>) {
        self.confirm_delete = (!ids.is_empty()).then_some(ids);
        self.delete_audio = true;
        cx.notify();
    }

    fn delete_selected(&mut self, _: &DeleteSelected, _: &mut Window, cx: &mut Context<Self>) {
        let ids = self.selected.clone();
        self.request_delete(ids, cx);
    }

    fn confirm_delete_now(&mut self, cx: &mut Context<Self>) {
        let Some(ids) = self.confirm_delete.take() else { return };
        let delete_audio = self.delete_audio;
        let result = Store::open(&paths::database()).and_then(|store| {
            for id in &ids {
                store.delete_meeting(*id, delete_audio)?;
            }
            Ok(())
        });
        if let Err(e) = result {
            self.error = Some(format!("{e:#}").into());
        }
        if let Screen::Meeting(v) = &self.screen {
            if ids.contains(&v.read(cx).id) {
                self.screen = Screen::Recording;
            }
        }
        self.selected.retain(|s| !ids.contains(s));
        self.reload(cx);
    }

    fn begin_rename(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.meetings.iter().find(|m| m.id == id).map(|m| m.title.clone()).unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |s, cx| s.set_value(title, window, cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        let sub = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => {
                let title = input.read(cx).value().trim().to_string();
                if !title.is_empty() {
                    if let Err(e) = Store::open(&paths::database()).and_then(|s| s.set_title(id, &title)) {
                        this.error = Some(format!("{e:#}").into());
                    }
                }
                this.rename = None;
                this.reload(cx);
                if let Screen::Meeting(v) = &this.screen {
                    v.update(cx, |v, cx| v.reload(cx));
                }
            }
            InputEvent::Blur => {
                this.rename = None;
                cx.notify();
            }
            _ => {}
        });
        self._subscriptions.push(sub);
        self.rename = Some((id, input));
        cx.notify();
    }

    fn export_meeting(&mut self, id: i64, format: mostalgia_core::export::Format, cx: &mut Context<Self>) {
        use mostalgia_core::export;
        let Ok(Some(m)) = Store::open(&paths::database()).and_then(|s| s.get(id)) else { return };
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| ".".into());
        let dir = if home.join("Documents").is_dir() { home.join("Documents") } else { home };
        let picked = cx.prompt_for_new_path(&dir, Some(&export::suggested_name(&m, format)));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let result = export::write(&m, format, &path);
            this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.error = Some(format!("{e:#}").into());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Deletes audio older than `audio_keep_days` (see `cleanup`), never
    /// of a meeting being worked on or open. The first time it would delete
    /// anything it asks instead (`confirm_cleanup`). Errors show.
    fn run_cleanup(&mut self, cx: &mut Context<Self>) {
        use mostalgia_core::cleanup;
        let config = mostalgia_core::config::Config::load();
        self.cleanup_days = Some(config.audio_keep_days);
        let mut busy: Vec<i64> = crate::processing::all().iter().filter_map(|(k, _)| k.meeting()).collect();
        if let Some(m) = self.meeting_view() {
            busy.push(m.read(cx).id);
        }
        let recordings = paths::recordings_dir();
        let plan = Store::open(&paths::database()).and_then(|s| cleanup::plan(&s, &recordings, config.audio_keep_days, chrono::Local::now(), &busy));
        match plan {
            Ok(plan) if plan.is_empty() => {}
            Ok(plan) if !config.audio_cleanup_confirmed => {
                self.confirm_cleanup = Some(plan);
                cx.notify();
            }
            Ok(plan) => self.delete_audio_now(plan, config.audio_keep_days, cx),
            Err(e) => {
                self.error = Some(trf("Aufräumen des Audios fehlgeschlagen: {}", &[&format!("{e:#}")]).into());
                cx.notify();
            }
        }
    }

    fn delete_audio_now(&mut self, plan: Vec<mostalgia_core::cleanup::Candidate>, days: u32, cx: &mut Context<Self>) {
        let recordings = paths::recordings_dir();
        let task = cx.background_spawn(async move {
            let mut errors = Vec::new();
            for c in &plan {
                if let Err(e) = mostalgia_core::cleanup::delete(c, days, &recordings) {
                    errors.push(format!("{}: {e:#}", c.title));
                }
            }
            errors
        });
        cx.spawn(async move |this, cx| {
            let errors = task.await;
            this.update(cx, |this, cx| {
                if !errors.is_empty() {
                    this.error = Some(trf("Aufräumen des Audios fehlgeschlagen: {}", &[&errors.join(" · ")]).into());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Import recording…": picks an audio or video file.
    fn pick_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(tr("Importieren").into()) });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(e)) => {
                    this.update(cx, |this, cx| {
                        this.error = Some(trf("Dateiauswahl: {}", &[&format!("{e:#}")]).into());
                        cx.notify();
                    })
                    .ok();
                    None
                }
                _ => None,
            };
            if let Some(path) = path {
                this.update_in(cx, |this, window, cx| this.open_import(path, window, cx)).ok();
            }
        })
        .detach();
    }

    /// Files dropped on the library: the first audio or video file opens
    /// the import dialog; anything else says what's accepted.
    fn drop_files(&mut self, paths: &[std::path::PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        match paths.iter().find(|p| mostalgia_core::media_import::is_media(p)) {
            Some(p) => self.open_import(p.clone(), window, cx),
            None => {
                self.error = Some(trf("Nur Audio- oder Videodateien lassen sich als Aufnahme importieren ({}).", &[&mostalgia_core::media_import::EXTENSIONS.join(", ")]).into());
                cx.notify();
            }
        }
    }

    /// Screenshots: the import dialog for `path` (`None` closes it).
    #[doc(hidden)]
    pub fn demo_import(&mut self, path: Option<std::path::PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        match path {
            Some(path) => self.open_import(path, window, cx),
            None => self.import = None,
        }
        cx.notify();
    }

    fn open_import(&mut self, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let info = mostalgia_core::media_import::info(&path);
        let title = cx.new(|cx| InputState::new(window, cx));
        title.update(cx, |s, cx| s.set_value(info.title.clone(), window, cx));
        let date = cx.new(|cx| InputState::new(window, cx).placeholder("2026-10-28 13:01"));
        date.update(cx, |s, cx| s.set_value(info.started_at.format("%Y-%m-%d %H:%M").to_string(), window, cx));
        self.import = Some(ImportDialog { path, title, date, error: None });
        cx.notify();
    }

    /// Imports the dialog's file in the background: read, transcribed and
    /// saved with progress in the library, then opened with summary and notes.
    fn start_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::processing::{self, Job, Key};
        use mostalgia_core::media_import::{self, Stage};
        let Some(dialog) = &mut self.import else { return };
        let title = dialog.title.read(cx).value().trim().to_string();
        let date = dialog.date.read(cx).value().to_string();
        let Some(started_at) = media_import::parse_date(&date) else {
            dialog.error = Some(trf("Unbekanntes Datum {} (z. B. \"2026-10-27 14:00\")", &[&format!("{date:?}")]).into());
            cx.notify();
            return;
        };
        let path = dialog.path.clone();
        self.import = None;
        let key = Key::Import(chrono::Local::now().timestamp_millis());
        let shown = if title.is_empty() { path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { title.clone() };
        processing::set(key, Job { title: shown, stage: tr("Audio wird gelesen …").to_string(), fraction: None });
        cx.notify();
        let config = mostalgia_core::config::Config::load();
        let task = cx.background_spawn(async move {
            let vad = config.vad_model_path();
            let opts = mostalgia_core::finalize::FinalPassOptions {
                vad_model: vad.exists().then_some(vad),
                diarize: config.diarize,
                denoise_mic: false,
                people: 0,
                progress: mostalgia_core::finalize::Progress::new(move |stage| processing::stage(key, stage.label(), stage.fraction())),
            };
            let report = move |stage: Stage| {
                if let Stage::Reading(f) = stage {
                    let label = match f {
                        Some(f) => trf("Audio wird gelesen … {} %", &[&((f * 100.0) as u32)]),
                        None => tr("Audio wird gelesen …").to_string(),
                    };
                    processing::stage(key, label, f);
                }
            };
            let result = mostalgia_core::finalize::engine_for(&config, &config.final_engine)
                .and_then(|mut engine| media_import::import(&path, &title, started_at, &mut engine, &opts, &config.language, &report));
            processing::clear(key);
            result.map_err(|e| trf("Import von {} fehlgeschlagen: {}", &[&path.display(), &format!("{e:#}")]))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(saved) => {
                        if let Some(e) = &saved.error {
                            eprintln!("[mostalgia] {e}");
                        }
                        this.reload(cx);
                        this.open_meeting(saved.meeting_id, window, cx);
                        if let Some(m) = this.meeting_view() {
                            let config = mostalgia_core::config::Config::load();
                            let (focus, style) = (config.companion_notes_focus.clone(), config.notes_style());
                            m.update(cx, |m, cx| m.after_text_import(false, focus, style, cx));
                        }
                    }
                    Err(e) => {
                        eprintln!("[mostalgia] {e}");
                        this.error = Some(e.into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_import_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.import.as_ref()?;
        let theme = cx.theme().clone();
        let name = dialog.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Some(
            div()
                .id("import-overlay")
                .absolute()
                .inset_0()
                .bg(gpui_kit::black().opacity(0.45))
                .flex()
                .items_center()
                .justify_center()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.import = None;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("import-dialog")
                        .w(px(480. * ui::ui_scale()))
                        .max_w(relative(0.9))
                        .p_5()
                        .gap_3()
                        .rounded(theme.radius_lg)
                        .border(ui::border_width(cx))
                        .border_color(theme.border)
                        .bg(theme.background)
                        .shadow_lg()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(h_flex().gap_2().items_start().child(Icon::new(IconName::Upload).flex_none().mt_1().text_color(theme.primary)).child(div().flex_1().min_w_0().text_lg().font_semibold().child(tr("Aufnahme importieren"))))
                        .child(div().text_sm().text_color(theme.muted_foreground).truncate().child(name))
                        .child(v_flex().gap_1().child(div().text_sm().font_medium().child(tr("Titel"))).child(Input::new(&dialog.title).small()))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Die Zusammenfassung gibt ihr danach einen passenden Titel.")))
                        .child(v_flex().gap_1().child(div().text_sm().font_medium().child(tr("Datum und Uhrzeit"))).child(Input::new(&dialog.date).small()))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Aus den Metadaten der Datei, sonst ihrem Dateidatum.")))
                        .when_some(dialog.error.clone(), |el, e| el.child(div().p_2().rounded(theme.radius).bg(theme.danger.opacity(0.1)).text_color(theme.danger).text_sm().child(e)))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Die Datei wird kopiert (das Original kann danach weg), transkribiert, nach Sprechern getrennt und zusammengefasst – wie eine Aufnahme.")))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("import-cancel").ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| {
                                    this.import = None;
                                    cx.notify();
                                })))
                                .child(Button::new("import-start").primary().icon(IconName::Upload).label(tr("Importieren")).on_click(cx.listener(|this, _, window, cx| this.start_import(window, cx)))),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The first cleanup: what it would delete, and a yes or no.
    fn render_cleanup_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let plan = self.confirm_cleanup.clone()?;
        let theme = cx.theme().clone();
        let days = mostalgia_core::config::Config::load().audio_keep_days;
        let bytes: u64 = plan.iter().map(|c| c.bytes).sum();
        let names: Vec<String> = plan.iter().take(3).map(|c| c.title.clone()).collect();
        Some(
            div()
                .id("cleanup-overlay")
                .absolute()
                .inset_0()
                .bg(gpui_kit::black().opacity(0.45))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    v_flex()
                        .id("cleanup-dialog")
                        .w(px(460. * ui::ui_scale()))
                        .max_w(relative(0.9))
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
                                .items_start()
                                .child(Icon::new(IconName::Trash).flex_none().mt_1().text_color(theme.warning))
                                .child(div().flex_1().min_w_0().text_lg().font_semibold().child(if plan.len() == 1 {
                                    trf("Audio von 1 Besprechung löschen ({})?", &[&mostalgia_core::cleanup::human(bytes)])
                                } else {
                                    trf("Audio von {} Besprechungen löschen ({})?", &[&plan.len(), &mostalgia_core::cleanup::human(bytes)])
                                })),
                        )
                        .children(names.into_iter().map(|n| ui::bidi_text(n, false).text_sm().font_medium()))
                        .when(plan.len() > 3, |el| el.child(div().text_sm().text_color(theme.muted_foreground).child(trf("… und {} weitere", &[&(plan.len() - 3)]))))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(trf(
                            "Die Aufnahmen sind älter als {}. Nur die Audiodateien werden gelöscht – Transkript, Zusammenfassung, Notizen und Sprecher bleiben. Danach geschieht das automatisch; „Audio behalten“ in einer Besprechung nimmt sie aus.",
                            &[&if days == 1 { tr("1 Tag").to_string() } else { trf("{} Tage", &[&days]) }],
                        )))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("cleanup-no").ghost().label(tr("Nein, Audio behalten")).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_cleanup = None;
                                    let mut config = mostalgia_core::config::Config::load();
                                    config.audio_keep_days = 0;
                                    if let Err(e) = config.save() {
                                        this.error = Some(format!("{e:#}").into());
                                    }
                                    this.cleanup_days = Some(0);
                                    this.recording.update(cx, |r, cx| r.reload_config(cx));
                                    cx.notify();
                                })))
                                .child(
                                    Button::new("cleanup-yes")
                                        .danger()
                                        .icon(IconName::Trash)
                                        .label(tr("Audio löschen"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let Some(plan) = this.confirm_cleanup.take() else { return };
                                            let mut config = mostalgia_core::config::Config::load();
                                            config.audio_cleanup_confirmed = true;
                                            if let Err(e) = config.save() {
                                                this.error = Some(format!("{e:#}").into());
                                                cx.notify();
                                                return;
                                            }
                                            this.recording.update(cx, |r, cx| r.reload_config(cx));
                                            this.delete_audio_now(plan, config.audio_keep_days, cx);
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_delete_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ids = self.confirm_delete.clone()?;
        let theme = cx.theme().clone();
        let title = if ids.len() == 1 {
            tr("Diese Besprechung endgültig vergessen?").to_string()
        } else {
            trf("{} Besprechungen löschen?", &[&ids.len()])
        };
        let names: Vec<String> = self.meetings.iter().filter(|m| ids.contains(&m.id)).take(3).map(|m| m.title.clone()).collect();
        Some(
            div()
                .id("delete-overlay")
                .absolute()
                .inset_0()
                .bg(gpui_kit::black().opacity(0.45))
                .flex()
                .items_center()
                .justify_center()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.confirm_delete = None;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("delete-dialog")
                        .w(px(440. * ui::ui_scale()))
                        .max_w(relative(0.9))
                        .p_5()
                        .gap_3()
                        .rounded(theme.radius_lg)
                        .border(ui::border_width(cx))
                        .border_color(theme.border)
                        .bg(theme.background)
                        .shadow_lg()
                        // Clicks inside the dialog don't close it.
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(h_flex().gap_2().items_start().child(Icon::new(IconName::Trash).flex_none().mt_1().text_color(theme.danger)).child(div().flex_1().min_w_0().text_lg().font_semibold().child(title)))
                        .children(names.into_iter().map(|n| ui::bidi_text(n, false).text_sm().font_medium()))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(tr(
                            "Transkript, Zusammenfassung, Lesezeichen, Sprechernamen und Suchindex werden entfernt. Das lässt sich nicht rückgängig machen.",
                        )))
                        .child(
                            gpui_kit::component::checkbox::Checkbox::new("delete-audio")
                                .checked(self.delete_audio)
                                .label(tr("Audiodateien ebenfalls löschen"))
                                .on_click(cx.listener(|this, v: &bool, _, cx| {
                                    this.delete_audio = *v;
                                    cx.notify();
                                })),
                        )
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("delete-cancel").ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_delete = None;
                                    cx.notify();
                                })))
                                .child(
                                    Button::new("delete-confirm")
                                        .danger()
                                        .icon(IconName::Trash)
                                        .label(tr("Löschen"))
                                        .on_click(cx.listener(|this, _, _, cx| this.confirm_delete_now(cx))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let recording_active = self.recording.read(cx).is_active();
        let is_recording = self.recording.read(cx).is_recording();
        let on_record_screen = matches!(self.screen, Screen::Recording);
        let selected = self.selected_meeting(cx);

        let record_button = Button::new("new-recording")
            .w_full()
            .when(is_recording, |b| b.danger().icon(IconName::Radio).label(tr("Aufnahme läuft")))
            .when(!is_recording, |b| {
                b.primary()
                    .icon(IconName::Plus)
                    .label(if recording_active { tr("Aufnahme") } else { tr("Neue Aufnahme") })
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.screen = Screen::Recording;
                cx.notify();
            }));

        let mut items: Vec<AnyElement> = Vec::new();
        if self.query.trim().is_empty() {
            // Recordings not in the library yet: the one that just stopped,
            // and any recovered after a crash.
            let pending: Vec<crate::processing::Job> =
                crate::processing::all().into_iter().filter(|(k, _)| k.meeting().is_none()).map(|(_, j)| j).collect();
            if !pending.is_empty() {
                items.push(ui::section_title(tr("WIRD VERARBEITET"), cx).px_2().pt_4().pb_1().into_any_element());
            }
            for (ix, job) in pending.into_iter().enumerate() {
                items.push(
                    v_flex()
                        .id(("processing-recording", ix))
                        .px_2()
                        .py_1p5()
                        .gap_1()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .when(on_record_screen, |el| el.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
                        .when(!on_record_screen, |el| el.hover(|s| s.bg(theme.sidebar_accent.opacity(0.6))))
                        .child(div().w_full().text_sm().font_medium().truncate().child(job.title.clone()))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(gpui_kit::component::spinner::Spinner::new().xsmall().color(theme.primary))
                                .child(div().truncate().child(job.stage.clone())),
                        )
                        .child(ui::progress_bar(job.fraction, cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = Screen::Recording;
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            let today = Local::now().date_naive();
            let mut group: Option<&'static str> = None;
            for (ix, m) in self.meetings.iter().enumerate() {
                let date = m.started_at.date_naive();
                let days = (today - date).num_days();
                let g = if days == 0 {
                    tr("HEUTE")
                } else if days == 1 {
                    tr("GESTERN")
                } else if days < 7 && date.weekday().num_days_from_monday() < today.weekday().num_days_from_monday() {
                    tr("DIESE WOCHE")
                } else if date.year() == today.year() && date.month() == today.month() {
                    tr("DIESER MONAT")
                } else {
                    tr("FRÜHER")
                };
                if group != Some(g) {
                    group = Some(g);
                    items.push(ui::section_title(g, cx).px_2().pt_4().pb_1().into_any_element());
                }
                let id = m.id;
                let active = selected == Some(id) || self.selected.contains(&id);
                let renaming = self.rename.as_ref().filter(|(rid, _)| *rid == id).map(|(_, input)| input.clone());
                let time = if days == 0 || days == 1 {
                    m.started_at.format("%H:%M").to_string()
                } else {
                    format!("{} {}", i18n::short_date(&m.started_at), m.started_at.format("%H:%M"))
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
                        .child(match renaming {
                            Some(input) => Input::new(&input).small().into_any_element(),
                            None => div()
                                .w_full()
                                .text_sm()
                                .font_medium()
                                .when(!ui::is_rtl(&m.title), |el| el.truncate().child(m.title.clone()))
                                .when(ui::is_rtl(&m.title), |el| el.child(ui::bidi_text(m.title.clone(), false)))
                                .into_any_element(),
                        })
                        .when_some(crate::processing::for_meeting(id), |el, (_, job)| {
                            el.child(
                                h_flex()
                                    .gap_1p5()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(gpui_kit::component::spinner::Spinner::new().xsmall().color(theme.primary))
                                    .child(div().truncate().child(match job.fraction {
                                        Some(f) if !job.stage.contains('%') => format!("{} {}", job.stage, crate::processing::percent(f)),
                                        _ => job.stage,
                                    })),
                            )
                        })
                        .child(
                            h_flex()
                                .gap_1p5()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .when(crate::processing::has_failure(id), |el| el.child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.danger)))
                                .child(format!("{time} · {}", ui::minutes(m.duration_secs)))
                                .when(m.has_summary, |el| el.child(Icon::new(IconName::Sparkles).xsmall().text_color(theme.primary)))
                                .when(m.text_import, |el| el.child(Icon::new(IconName::FileText).xsmall())),
                        )
                        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| this.click_meeting(id, ev.modifiers(), window, cx)))
                        .context_menu({
                            let app = cx.entity().downgrade();
                            let targets = self.targets(id);
                            move |menu, _, _| {
                                let n = targets.len();
                                let mut menu = menu;
                                if n == 1 {
                                    let rename_app = app.clone();
                                    menu = menu.item(PopupMenuItem::new(tr("Umbenennen")).icon(IconName::Pencil).on_click(move |_, window, cx| {
                                        rename_app.update(cx, |this, cx| this.begin_rename(id, window, cx)).ok();
                                    }));
                                    for f in mostalgia_core::export::Format::ALL {
                                        let app = app.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(trf("Exportieren: {}", &[&tr(f.label())]))
                                                .icon(IconName::Download)
                                                .on_click(move |_, _, cx| {
                                                    app.update(cx, |this, cx| this.export_meeting(id, f, cx)).ok();
                                                }),
                                        );
                                    }
                                    menu = menu.separator();
                                }
                                let app = app.clone();
                                let targets = targets.clone();
                                let label = if n == 1 { tr("Löschen…").to_string() } else { trf("{} Besprechungen löschen…", &[&n]) };
                                menu.item(PopupMenuItem::new(label).icon(IconName::Trash).on_click(move |_, _, cx| {
                                    let targets = targets.clone();
                                    app.update(cx, |this, cx| this.request_delete(targets, cx)).ok();
                                }))
                            }
                        })
                        .into_any_element(),
                );
            }
            if self.meetings.is_empty() && !crate::processing::all().iter().any(|(k, _)| k.meeting().is_none()) {
                items.push(
                    div()
                        .px_2()
                        .py_6()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(tr("Noch keine Erinnerungen. Zeit, welche zu machen."))
                        .into_any_element(),
                );
            }
        } else {
            items.push(ui::section_title(trf("{} TREFFER", &[&self.hits.len()]), cx).px_2().pt_3().pb_1().into_any_element());
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
                                .child(format!("{} · {} · {}", h.title, i18n::short_date(&h.started_at), ui::hms(h.start_secs))),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.open_meeting(id, window, cx)))
                        .into_any_element(),
                );
            }
        }

        let settings_active = matches!(self.screen, Screen::Settings(_));
        v_flex()
            .id("library")
            // Audio or video files dropped here are imported as recordings.
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.08)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.drop_files(paths.paths(), window, cx)))
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
                    .child(
                        h_flex()
                            .gap_2()
                            .child(record_button.flex_1().when(on_record_screen && !is_recording, |b| b.outline()))
                            .child(
                                Button::new("import-recording")
                                    .outline()
                                    .icon(IconName::Upload)
                                    .tooltip(tr("Aufnahme importieren … (Audio- oder Videodatei, auch per Drag & Drop)"))
                                    .on_click(cx.listener(|this, _, window, cx| this.pick_import(window, cx))),
                            )
                            .child(
                                Button::new("from-text")
                                    .outline()
                                    .icon(IconName::FileText)
                                    .when(matches!(self.screen, Screen::TextImport(_)), |b| b.selected(true))
                                    .tooltip(tr("Besprechung aus Text hinzufügen"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if !matches!(this.screen, Screen::TextImport(_)) {
                                            this.open_text_import(window, cx);
                                        }
                                    })),
                            ),
                    )
                    .child(
                        Button::new("ask")
                            .w_full()
                            .ghost()
                            .when(matches!(self.screen, Screen::Ask(_)), |b| b.selected(true))
                            .icon(IconName::Sparkles)
                            .label(tr("Fragen"))
                            .tooltip(tr("Frag deine Besprechungen (Strg+K)"))
                            .on_click(cx.listener(|this, _, window, cx| this.open_ask(window, cx))),
                    )
                    .child(
                        Button::new("documents")
                            .w_full()
                            .ghost()
                            .when(matches!(self.screen, Screen::Documents(_)), |b| b.selected(true))
                            .icon(IconName::Library)
                            .label(tr("Dokumente"))
                            .tooltip(tr("Deine Dokumente – Antworten können sie zitieren"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_documents(window, cx);
                            })),
                    )
                    .when(mostalgia_core::config::Config::load().decision_log, |el| {
                        el.child(
                            Button::new("decisions")
                                .w_full()
                                .ghost()
                                .when(matches!(self.screen, Screen::Decisions(_)), |b| b.selected(true))
                                .icon(IconName::ListChecks)
                                .label(tr("Entscheidungen"))
                                .tooltip(tr("Alle Entscheidungen aus deinen Besprechungen"))
                                .on_click(cx.listener(|this, _, window, cx| this.open_decisions(false, window, cx))),
                        )
                    })
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
                        .key_context("Library")
                        .track_focus(&self.library_focus)
                        .on_action(cx.listener(Self::delete_selected))
                        .size_full()
                        .px_2()
                        .pb_2()
                        .overflow_y_scroll()
                        .children(items),
                ),
            )
            .when_some(self.error.clone(), |el, e| el.child(div().p_3().text_xs().text_color(theme.danger).child(e)))
            .when_some(self.mori_host.problem.clone(), |el, e| el.child(div().p_3().text_xs().text_color(theme.warning).child(e)))
            .child(
                div().p_2().border_t_1().border_color(theme.sidebar_border).child(
                    Button::new("settings")
                        .w_full()
                        .ghost()
                        .when(settings_active, |b| b.selected(true))
                        .icon(IconName::Settings)
                        .label(tr("Einstellungen"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
                ),
            )
    }
}

impl Render for MostalgiaApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The app language can change at runtime; inputs keep their own
        // placeholder text, so refresh it when it does.
        if self.lang_seen != Some(i18n::lang()) {
            self.lang_seen = Some(i18n::lang());
            self.search.update(cx, |s, cx| s.set_placeholder(tr("Transkripte durchsuchen"), window, cx));
        }
        self.tell_mori_open_meeting(cx);
        let sidebar = self.render_sidebar(cx);
        let call_prompt = self.render_call_prompt(cx);
        let content: AnyView = match &self.screen {
            Screen::Recording => self.recording.clone().into(),
            Screen::Meeting(v) => v.clone().into(),
            Screen::Settings(v) => v.clone().into(),
            Screen::Ask(v) => v.clone().into(),
            Screen::TextImport(v) => v.clone().into(),
            Screen::Documents(v) => v.clone().into(),
            Screen::Decisions(v) => v.clone().into(),
        };
        let delete_dialog = self.render_delete_dialog(cx);
        let cleanup_dialog = self.render_cleanup_dialog(cx);
        let import_dialog = self.render_import_dialog(cx);
        let theme = cx.theme();
        v_flex()
            .size_full()
            .relative()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(ui::ui_font(cx))
            .on_action(cx.listener(|this, _: &OpenAsk, window, cx| this.open_ask(window, cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| this.zoom(Some(true), window, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| this.zoom(Some(false), window, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, window, cx| this.zoom(None, window, cx)))
            .child(
                TitleBar::new().child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .font_semibold()
                        .child(img(crate::ICON_PATH).size(px(18.)).flex_none())
                        .child("Mostalgia"),
                ),
            )
            .children(call_prompt)
            .child(h_flex().flex_1().min_h_0().child(sidebar).child(div().flex_1().min_w_0().h_full().child(content)))
            .child(self.companion.clone())
            .children(delete_dialog)
            .children(cleanup_dialog)
            .children(import_dialog)
    }
}
