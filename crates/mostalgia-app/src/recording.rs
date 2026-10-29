//! The recording screen: set up, record with a live transcript, finalize.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::Receiver;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, switch::Switch, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::config::{Config, Mode, SttChoice};
use mostalgia_core::recorder::{Event, RecordConfig, Recorder, Recording};
use mostalgia_core::store::{Bookmark, MarkKind, NewMeeting, Store};
use mostalgia_core::transcribe::Engine;
use mostalgia_core::i18n::{self, tr, trf};
use mostalgia_core::{cloud_stt, finalize};
use mostalgia_core::{Source, paths};

use crate::ui;

/// The local Whisper engine, kept loaded between recordings (never a
/// cloud engine).
pub type SharedEngine = Arc<Mutex<Option<Engine>>>;

pub enum RecordingEvent {
    /// A meeting was recorded and saved; open it.
    Saved(i64),
    /// A recording couldn't start (no source, engine or recorder error, or
    /// one still being finished); the message says why.
    StartFailed(String),
}

impl EventEmitter<RecordingEvent> for RecordingView {}

#[derive(Clone)]
struct LiveLine {
    source: Source,
    speaker: Option<String>,
    start_secs: f64,
    end_secs: f64,
    text: SharedString,
}

/// What Mori needs to know about a recording in progress (cheap to read).
#[derive(Clone, Copy, Debug)]
pub struct LiveState {
    pub paused: bool,
    /// When someone was last heard (speech level, provisional text or a line).
    pub speech_at: Option<Instant>,
    /// When the current stretch of speech began.
    pub speech_since: Option<Instant>,
    /// Seconds of finalized speech so far.
    pub speech_secs: f64,
    /// End of the newest finalized line, in recording seconds (the latest
    /// end over both tracks: their lines don't arrive in time order).
    pub last_line_end: f64,
    /// Finalized lines so far.
    pub lines: usize,
    /// Recording time so far (without pauses), in seconds.
    pub elapsed_secs: f64,
    /// Which sources (mic, system) are being recorded right now; either
    /// can be switched off and on mid-recording.
    pub sources: [bool; 2],
}

enum Phase {
    Setup,
    Loading,
    Recording(Box<Live>),
    /// The final pass after Stop: its progress is in
    /// [`crate::processing`]; the live transcript stays readable meanwhile.
    Finalizing { what: &'static str, detail: SharedString, title: String, lines: Vec<LiveLine> },
}

struct Live {
    /// `None` only for the screenshot demo.
    recorder: Option<Recorder>,
    rx: Receiver<Event>,
    started: Instant,
    paused_total: Duration,
    paused_since: Option<Instant>,
    lines: Vec<LiveLine>,
    /// Provisional text per source (mic, system) for the utterance still
    /// being spoken: (start seconds, text).
    interim: [Option<(f64, SharedString)>; 2],
    /// Smoothed meter levels (mic, system), 0..1.
    levels: [f32; 2],
    bookmarks: Vec<Bookmark>,
    last_latency_ms: Option<u128>,
    /// Since when the mic has been near-silent (for the "mic muted?" hint).
    mic_quiet_since: Option<Instant>,
    /// The engine this recording transcribes with.
    choice: SttChoice,
    /// Latest live engine failure (engine, message) and how many chunks failed.
    engine_error: Option<(SharedString, SharedString)>,
    failed_chunks: usize,
    /// Seconds the live transcriber has been stuck (watchdog), while it is.
    stalled_secs: Option<u64>,
    /// Recorder warnings (deduplicated) still shown.
    warnings: Vec<SharedString>,
    /// Noise floor per source on the meter scale, for the speech gate.
    floor: [f32; 2],
    speech_at: Option<Instant>,
    speech_since: Option<Instant>,
    speech_secs: f64,
    /// Identifies this recording (for Mori on the desktop).
    session: u64,
    /// Sources (mic, system) recorded right now (switchable mid-recording;
    /// the setup screen's saved choice stays as it is).
    sources_on: [bool; 2],
    /// The language each source (mic, system) is transcribed in, with
    /// "auto": detected, locked or picked (`None` while detecting).
    languages: [Option<mostalgia_core::language::Status>; 2],
}

static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Live {
    fn new(recorder: Option<Recorder>, rx: Receiver<Event>, choice: SttChoice, sources_on: [bool; 2]) -> Self {
        Self {
            recorder,
            rx,
            started: Instant::now(),
            paused_total: Duration::ZERO,
            paused_since: None,
            lines: Vec::new(),
            interim: [None, None],
            levels: [0.; 2],
            bookmarks: Vec::new(),
            last_latency_ms: None,
            mic_quiet_since: None,
            choice,
            engine_error: None,
            failed_chunks: 0,
            stalled_secs: None,
            warnings: Vec::new(),
            floor: [1.; 2],
            speech_at: None,
            speech_since: None,
            speech_secs: 0.,
            session: NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            sources_on,
            languages: [None, None],
        }
    }

    fn elapsed(&self) -> Duration {
        let paused_now = self.paused_since.map(|t| t.elapsed()).unwrap_or_default();
        self.started.elapsed().saturating_sub(self.paused_total + paused_now)
    }

    /// Bookmarks stay in time order (rows are merged into the transcript).
    fn add_mark(&mut self, mark: Bookmark) {
        let at = self.bookmarks.partition_point(|b| b.at_secs <= mark.at_secs);
        self.bookmarks.insert(at, mark);
    }

    /// A source was switched off / on at `at` seconds.
    fn switched(&mut self, source: Source, on: bool, at: f64) {
        let i = source as usize;
        self.sources_on[i] = on;
        if !on {
            self.interim[i] = None;
            self.levels[i] = 0.;
        }
        if source == Source::Mic {
            self.mic_quiet_since = None;
        }
        self.add_mark(Bookmark::source_switch(source, on, at));
    }

    /// Someone is speaking now.
    fn heard(&mut self) {
        let now = Instant::now();
        if self.speech_at.is_none_or(|t| now - t > SPEECH_HOLD) {
            self.speech_since = Some(now);
        }
        self.speech_at = Some(now);
    }
}

/// How long speech counts as ongoing after it was last heard.
const SPEECH_HOLD: Duration = Duration::from_millis(1500);

pub struct RecordingView {
    config: Config,
    engine: SharedEngine,
    /// Label of the live engine in use ("Whisper large-v3-turbo · GPU" /
    /// "OpenAI · gpt-4o-transcribe") and whether it is a cloud engine.
    engine_label: Option<(SharedString, bool)>,
    phase: Phase,
    error: Option<SharedString>,
    /// Recordings recovered after a crash, being or just processed.
    recovered: Vec<SharedString>,
    scroll: ScrollHandle,
    /// Keep the newest line in view; turned off when the user scrolls up.
    follow: bool,
    focus: FocusHandle,
    /// This recording was started for a call: always both sides.
    call_mode: bool,
    /// Sources (mic, system) picked for this one recording (Mori's record
    /// button), instead of the setup screen's toggles; not saved.
    sources_override: Option<[bool; 2]>,
    /// People in the call, counting you, for the next recording (a
    /// `diarize::PEOPLE_CHOICES` value; 0 = automatic). Reset after each.
    people: u8,
}

impl RecordingView {
    pub fn new(engine: SharedEngine, cx: &mut Context<Self>) -> Self {
        Self {
            config: Config::load(),
            engine,
            engine_label: None,
            phase: Phase::Setup,
            error: None,
            recovered: Vec::new(),
            scroll: ScrollHandle::new(),
            follow: true,
            focus: cx.focus_handle(),
            call_mode: false,
            sources_override: None,
            people: 0,
        }
    }

    /// People in the call for the next recording (0 = automatic).
    pub fn set_people(&mut self, people: u8, cx: &mut Context<Self>) {
        self.people = people;
        cx.notify();
    }

    /// Mic and system for the next recording: what the setup screen says,
    /// except that a call is always recorded from both sides.
    fn sources(&self) -> [bool; 2] {
        let [mic, system] = self.sources_override.unwrap_or([self.config.record_mic, self.config.record_system]);
        [mic || self.call_mode, system || self.call_mode]
    }

    /// Starts a recording from exactly these sources (Mori's record
    /// button), leaving the setup screen's saved choice alone. Says why
    /// when it can't (`RecordingEvent::StartFailed`).
    pub fn start_with(&mut self, mic: bool, system: bool, cx: &mut Context<Self>) {
        if self.is_recording() {
            return;
        }
        if self.is_active() {
            cx.emit(RecordingEvent::StartFailed(tr("Eine Aufnahme wird gerade gestartet oder fertiggestellt.").to_string()));
            return;
        }
        self.sources_override = Some([mic, system]);
        self.start(cx);
    }

    /// Pauses or resumes the recording in progress.
    pub fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        if self.live_state().is_some_and(|l| l.paused != paused) {
            self.toggle_pause(cx);
        }
    }

    /// Starts a recording for a detected call (both sides, whatever the
    /// setup toggles say).
    pub fn start_call(&mut self, cx: &mut Context<Self>) {
        if self.is_active() {
            return;
        }
        self.call_mode = true;
        self.start(cx);
    }

    /// Shows a recording in progress with the given lines, without any
    /// audio — for screenshots of the UI.
    #[doc(hidden)]
    /// Screenshots: after Stop, the final pass `fraction` of the way
    /// through, over the demo's live lines.
    #[doc(hidden)]
    pub fn demo_finalizing(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let lines = match &self.phase {
            Phase::Recording(live) => live.lines.clone(),
            _ => Vec::new(),
        };
        let title = trf("Besprechung {}", &[&i18n::date_time(&chrono::Local::now())]);
        let stage = finalize::Stage::Transcribing { track: 1, tracks: 2, done: (fraction as f64 * 2.0 - 1.0).max(0.0) * 100.0, total: 100.0 };
        crate::processing::set(crate::processing::Key::Recording, crate::processing::Job { title: title.clone(), stage: stage.label(), fraction: stage.fraction() });
        self.phase = Phase::Finalizing {
            what: tr("Transkript wird fertiggestellt …"),
            detail: trf("Feinschliff mit {}", &[&"Whisper large-v3-turbo"]).into(),
            title,
            lines,
        };
        cx.notify();
    }

    pub fn demo(&mut self, secs: u64, lines: &[(Source, f64, &str)], cx: &mut Context<Self>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        drop(tx);
        self.engine_label = Some(("Whisper large-v3-turbo · NVIDIA RTX PRO 3000 Blackwell Generation Laptop GPU".into(), false));
        let mut live = Live::new(None, rx, SttChoice::local(), [true, true]);
        live.started = Instant::now() - Duration::from_secs(secs);
        live.lines = lines
            .iter()
            .map(|(source, start_secs, text)| LiveLine {
                source: *source,
                speaker: None,
                start_secs: *start_secs,
                end_secs: start_secs + 4.,
                text: SharedString::from(text.to_string()),
            })
            .collect();
        live.speech_secs = live.lines.len() as f64 * 4.;
        live.interim = [None, Some((58.3, "And one more thing about the client meeting".into()))];
        live.levels = [0.62, 0.48];
        live.bookmarks = vec![Bookmark {
            at_secs: 41.0,
            label: trf("Markierung {}", &[&1]),
            kind: MarkKind::Moment,
        }];
        live.last_latency_ms = Some(212);
        let german = mostalgia_core::language::Status {
            language: "de".into(),
            locked: true,
            manual: false,
            probs: vec![("de".into(), 0.98), ("en".into(), 0.02)],
        };
        live.languages = [Some(german.clone()), Some(german)];
        live.heard();
        self.phase = Phase::Recording(Box::new(live));
        cx.notify();
    }

    /// Switches a source off / on in the demo recording at these moments
    /// (screenshots): the markers, and the state after the last switch.
    #[doc(hidden)]
    pub fn demo_switches(&mut self, switches: &[(Source, bool, f64)], cx: &mut Context<Self>) {
        if let Phase::Recording(live) = &mut self.phase {
            for &(source, on, at) in switches {
                live.switched(source, on, at);
            }
            cx.notify();
        }
    }

    /// Shows a live engine failure in the demo recording (screenshots).
    #[doc(hidden)]
    pub fn demo_engine_error(&mut self, engine: &str, message: &str, failed: usize, cx: &mut Context<Self>) {
        self.engine_label = Some((engine.to_string().into(), true));
        if let Phase::Recording(live) = &mut self.phase {
            live.engine_error = Some((engine.to_string().into(), message.to_string().into()));
            live.failed_chunks = failed;
            live.warnings.push(trf("Sprechertrennung aus: {}", &[&tr("Modelle für die Sprechertrennung fehlen — unter Einstellungen → System herunterladen")]).into());
            cx.notify();
        }
    }

    /// Replaces the settings (screenshots of other engine setups).
    #[doc(hidden)]
    pub fn demo_config(&mut self, config: Config, cx: &mut Context<Self>) {
        self.config = config;
        cx.notify();
    }

    /// Back to the setup screen without saving (screenshots).
    #[doc(hidden)]
    pub fn demo_end(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Setup;
        cx.notify();
    }

    /// The recording in progress, for Mori.
    pub fn live_state(&self) -> Option<LiveState> {
        let Phase::Recording(live) = &self.phase else { return None };
        Some(LiveState {
            paused: live.paused_since.is_some(),
            speech_at: live.speech_at,
            speech_since: live.speech_since,
            speech_secs: live.speech_secs,
            last_line_end: live.lines.iter().map(|l| l.end_secs).fold(0., f64::max),
            lines: live.lines.len(),
            elapsed_secs: live.elapsed().as_secs_f64(),
            sources: live.sources_on,
        })
    }

    /// The live transcript from `from_secs` on (finalized lines, then the
    /// provisional text still being spoken), as Mori's model sees it.
    pub fn transcript_lines(&self, from_secs: f64) -> Vec<mostalgia_core::companion::ContextLine> {
        use mostalgia_core::companion::ContextLine;
        use mostalgia_core::summary::llm_speaker;
        let Phase::Recording(live) = &self.phase else { return Vec::new() };
        let mut out: Vec<ContextLine> = live
            .lines
            .iter()
            .filter(|l| l.end_secs >= from_secs)
            .map(|l| ContextLine {
                at_secs: l.start_secs,
                speaker: llm_speaker(l.speaker.as_deref().unwrap_or(l.source.speaker_label())),
                text: l.text.to_string(),
                interim: false,
            })
            .collect();
        for (i, slot) in live.interim.iter().enumerate() {
            if let Some((start, text)) = slot {
                let source = if i == 0 { Source::Mic } else { Source::System };
                out.push(ContextLine { at_secs: *start, speaker: llm_speaker(source.speaker_label()), text: text.to_string(), interim: true });
            }
        }
        out
    }

    /// For Mori on the desktop: the finalized lines from index `from` on,
    /// the provisional text, and how many finalized lines there are.
    pub fn transcript_delta(&self, from: usize) -> (Vec<mostalgia_core::companion::ContextLine>, Vec<mostalgia_core::companion::ContextLine>, usize) {
        let Phase::Recording(live) = &self.phase else { return (Vec::new(), Vec::new(), 0) };
        let total = live.lines.len();
        let (interim, finalized): (Vec<_>, Vec<_>) = self.transcript_lines(f64::MIN).into_iter().partition(|l| l.interim);
        let finalized = finalized.into_iter().skip(from).collect();
        (finalized, interim, total)
    }

    /// Scrolls the live transcript to the line spoken at `secs` (a time
    /// Mori cited) and stops following new lines.
    pub fn scroll_to_time(&mut self, secs: f64, cx: &mut Context<Self>) {
        let Phase::Recording(live) = &self.phase else { return };
        let Some(ix) = live.lines.iter().rposition(|l| l.start_secs <= secs + 0.5).or((!live.lines.is_empty()).then_some(0)) else { return };
        let start = live.lines[ix].start_secs;
        // Bookmark rows come before the line they precede.
        let marks = live.bookmarks.iter().filter(|b| b.at_secs <= start).count();
        self.follow = false;
        self.scroll.scroll_to_top_of_item(ix + marks);
        cx.notify();
    }

    /// A bookmark at `secs` (a line flagged in the live captions); it
    /// goes into the meeting with the others.
    pub fn flag_at(&mut self, secs: f64, cx: &mut Context<Self>) {
        let Phase::Recording(live) = &mut self.phase else { return };
        if live.bookmarks.iter().any(|b| b.kind == MarkKind::Moment && (b.at_secs - secs).abs() < 0.5) {
            return;
        }
        let n = live.bookmarks.iter().filter(|b| b.kind == MarkKind::Moment).count() + 1;
        live.add_mark(Bookmark { at_secs: secs, label: trf("Markierung {}", &[&n]), kind: MarkKind::Moment });
        cx.notify();
    }

    /// Seconds the live transcriber has been stuck, while it is.
    pub fn stalled_secs(&self) -> Option<u64> {
        let Phase::Recording(live) = &self.phase else { return None };
        live.stalled_secs
    }

    /// Identifies the recording in progress (changes with every new one).
    pub fn session_id(&self) -> Option<u64> {
        let Phase::Recording(live) = &self.phase else { return None };
        Some(live.session)
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Setup)
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording(_))
    }

    /// Re-read settings (after the settings screen changed them).
    pub fn reload_config(&mut self, cx: &mut Context<Self>) {
        if !self.is_active() {
            self.config = Config::load();
            cx.notify();
        }
    }

    fn save_config(&mut self) {
        // Mori's settings and position change elsewhere; keep them.
        let disk = Config::load();
        self.config.companion = disk.companion;
        self.config.companion_scale = disk.companion_scale;
        self.config.companion_points_interval_secs = disk.companion_points_interval_secs;
        self.config.companion_notes_mode = disk.companion_notes_mode.clone();
        self.config.companion_notes_gap_secs = disk.companion_notes_gap_secs;
        self.config.companion_notes_interval_secs = disk.companion_notes_interval_secs;
        self.config.companion_notes_focus = disk.companion_notes_focus.clone();
        self.config.companion_card_collapse_secs = disk.companion_card_collapse_secs;
        self.config.companion_card_stagger_secs = disk.companion_card_stagger_secs;
        self.config.companion_notes_provider = disk.companion_notes_provider.clone();
        self.config.companion_notes_model = disk.companion_notes_model.clone();
        self.config.companion_pos = disk.companion_pos;
        self.config.companion_clear_button = disk.companion_clear_button;
        self.config.companion_desktop = disk.companion_desktop;
        self.config.companion_desktop_pos = disk.companion_desktop_pos;
        self.config.companion_hidden = disk.companion_hidden;
        self.config.companion_bar_indicator = disk.companion_bar_indicator;
        // So is the audio cleanup (its dialog).
        self.config.audio_keep_days = disk.audio_keep_days;
        self.config.audio_cleanup_confirmed = disk.audio_cleanup_confirmed;
        if let Err(e) = self.config.save() {
            eprintln!("[mostalgia] saving settings: {e:#}");
        }
    }

    pub fn start(&mut self, cx: &mut Context<Self>) {
        if self.is_active() {
            return;
        }
        if self.sources() == [false, false] {
            self.sources_override = None;
            self.error = Some(tr("Mikrofon oder Desktop-Audio einschalten.").into());
            cx.emit(RecordingEvent::StartFailed(tr("Mikrofon oder Desktop-Audio einschalten.").to_string()));
            cx.notify();
            return;
        }
        self.error = None;
        self.phase = Phase::Loading;
        cx.notify();

        // The configured engine or a visible error; never another engine.
        let config = self.config.clone();
        let slot = self.engine.clone();
        let load = cx.background_spawn(async move {
            check_final_engine(&config)?;
            let choice = &config.live_engine;
            let cached = if choice.is_local() { slot.lock().unwrap().take() } else { None };
            match cached {
                Some(engine) => Ok(engine),
                None => finalize::engine_for(&config, choice).map_err(|e| {
                    anyhow::anyhow!(trf("Transkription mit {} nicht möglich: {}", &[&config.engine_label(choice), &format!("{e:#}")]))
                }),
            }
        });
        cx.spawn(async move |this, cx| {
            let engine = load.await;
            this.update(cx, |this, cx| match engine {
                Ok(engine) => this.begin(engine, cx),
                Err(e) => this.fail_start(e, cx),
            })
            .ok();
        })
        .detach();
    }

    fn begin(&mut self, mut engine: Engine, cx: &mut Context<Self>) {
        if let Err(e) = engine.set_language(&self.config.language) {
            return self.fail_start(e, cx);
        }
        engine.set_meeting_languages(&self.config.meeting_languages);
        self.engine_label = Some((engine.label().into(), engine.is_cloud()));
        let cfg = RecordConfig {
            vad_model: self.config.vad_model_path(),
            vad_threshold: self.config.vad_threshold,
            out_dir: paths::recordings_dir(),
            mic: self.sources()[0],
            system: self.sources()[1],
            mic_device: self.config.mic_device.clone(),
            system_device: self.config.system_device.clone(),
            diarize: self.config.diarize,
            denoise: self.config.denoise,
            interim: self.config.interim_text,
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        match Recorder::start(cfg, engine, tx) {
            Ok(recorder) => {
                self.follow = true;
                let sources = self.sources();
                self.phase = Phase::Recording(Box::new(Live::new(Some(recorder), rx, self.config.live_engine.clone(), sources)));
                self.poll_loop(cx);
            }
            // Recorder::start consumed the engine; it reloads next time.
            Err(e) => self.fail_start(e, cx),
        }
        cx.notify();
    }

    /// Transcribes `sources` in `code` from their next utterance on
    /// (`None`: detect again).
    fn pick_language(&mut self, sources: &[Source], code: Option<String>, cx: &mut Context<Self>) {
        let Phase::Recording(live) = &mut self.phase else { return };
        for &source in sources {
            if let Some(recorder) = &live.recorder {
                recorder.set_language(source, code.clone());
            }
            live.languages[source as usize] = code.clone().map(|language| mostalgia_core::language::Status {
                language,
                locked: true,
                manual: true,
                probs: Vec::new(),
            });
        }
        cx.notify();
    }

    /// "Erkannt: Deutsch ▾" in the live footer (auto mode): what each
    /// source is transcribed in, and a menu to pick another language that
    /// applies from the next utterance on. One button while both sources
    /// agree.
    fn language_picker(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        use mostalgia_core::language::{COMMON, native_name};
        let Phase::Recording(live) = &self.phase else { return Vec::new() };
        if self.config.language != "auto" {
            return Vec::new();
        }
        let on: Vec<Source> = [Source::Mic, Source::System].into_iter().filter(|s| live.sources_on[*s as usize]).collect();
        let lang = |s: Source| live.languages[s as usize].as_ref().map(|st| st.language.clone());
        let groups: Vec<Vec<Source>> = match on.as_slice() {
            [a, b] if lang(*a) != lang(*b) && lang(*a).is_some() && lang(*b).is_some() => vec![vec![*a], vec![*b]],
            [] => Vec::new(),
            all => vec![all.to_vec()],
        };
        let allowed = self.config.meeting_languages.clone();
        let weak = cx.entity().downgrade();
        groups
            .into_iter()
            .map(|sources| {
                let status = sources.iter().find_map(|s| live.languages[*s as usize].clone());
                let label = match &status {
                    None => tr("Sprache wird erkannt …").to_string(),
                    Some(st) if st.manual => trf("Sprache: {}", &[&native_name(&st.language)]),
                    Some(st) if st.locked => trf("Erkannt: {}", &[&native_name(&st.language)]),
                    Some(st) => trf("Erkenne: {} …", &[&native_name(&st.language)]),
                };
                let prefix = match sources.as_slice() {
                    [Source::Mic] => format!("{} · ", tr("Mikrofon")),
                    [Source::System] => format!("{} · ", tr("Desktop-Audio")),
                    _ => String::new(),
                };
                let tooltip = match &status {
                    Some(st) if !st.probs.is_empty() => {
                        let probs: Vec<String> = st.probs.iter().take(3).map(|(c, p)| format!("{} {:.0} %", native_name(c), p * 100.)).collect();
                        trf("Gilt ab dem nächsten Satz. Wahrscheinlichkeit: {}", &[&probs.join(", ")])
                    }
                    _ => tr("Gilt ab dem nächsten Satz.").to_string(),
                };
                let current = status.as_ref().map(|s| s.language.clone());
                let manual = status.as_ref().is_some_and(|s| s.manual);
                let id = SharedString::from(format!("live-lang-{}", sources.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("-")));
                let (weak, allowed) = (weak.clone(), allowed.clone());
                Button::new(id)
                    .xsmall()
                    .ghost()
                    .icon(IconName::Globe)
                    .label(format!("{prefix}{label}"))
                    .dropdown_caret(true)
                    .tooltip(tooltip)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu.scrollable(true).max_h(px(360.));
                        let item = |menu: gpui_kit::component::menu::PopupMenu, code: Option<String>, label: String, checked: bool| {
                            let (weak, sources) = (weak.clone(), sources.clone());
                            menu.item(PopupMenuItem::new(label).checked(checked).on_click(move |_, _, cx| {
                                weak.update(cx, |this, cx| this.pick_language(&sources, code.clone(), cx)).ok();
                            }))
                        };
                        menu = item(menu, None, tr("Automatisch erkennen").to_string(), !manual);
                        menu = menu.separator();
                        let mut shown: Vec<String> = Vec::new();
                        for code in &allowed {
                            menu = item(menu, Some(code.clone()), native_name(code), manual && current.as_deref() == Some(code.as_str()));
                            shown.push(code.clone());
                        }
                        if !allowed.is_empty() {
                            menu = menu.separator().label(tr("Weitere Sprachen"));
                        }
                        for (code, name) in COMMON.iter().filter(|(c, _)| !shown.iter().any(|s| s == c)) {
                            menu = item(menu, Some(code.to_string()), name.to_string(), manual && current.as_deref() == Some(*code));
                        }
                        menu
                    })
                    .into_any_element()
            })
            .collect()
    }

    /// A recording couldn't start: shown here and told to Mori.
    fn fail_start(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        let message = format!("{e:#}");
        self.fail(e, cx);
        cx.emit(RecordingEvent::StartFailed(message));
    }

    fn fail(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        self.call_mode = false;
        self.sources_override = None;
        self.error = Some(format!("{e:#}").into());
        self.phase = Phase::Setup;
        cx.notify();
    }

    /// Drains recorder events ~30×/s while recording.
    fn poll_loop(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let keep = this.update(cx, |this, cx| this.poll(cx)).unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let Phase::Recording(live) = &mut self.phase else {
            return false;
        };
        let at_bottom = {
            let max = self.scroll.max_offset().y;
            max <= px(1.) || -self.scroll.offset().y >= max - px(24.)
        };
        let mut new_lines = false;
        let mut fresh = [0f32; 2];
        let events: Vec<Event> = live.rx.try_iter().collect();
        for event in events {
            match event {
                Event::Interim { source, start_secs, text } => {
                    if !text.is_empty() {
                        live.heard();
                    }
                    live.interim[source as usize] = (!text.is_empty()).then(|| (start_secs, text.into()));
                    new_lines = true;
                }
                Event::Line { source, speaker, start_secs, end_secs, text } => {
                    live.interim[source as usize] = None;
                    live.speech_secs += (end_secs - start_secs).max(0.);
                    live.heard();
                    live.lines.push(LiveLine {
                        source,
                        speaker,
                        start_secs,
                        end_secs,
                        text: text.into(),
                    });
                    new_lines = true;
                }
                Event::Level { source, rms } => {
                    let i = source as usize;
                    if source == Source::Mic && live.sources_on[0] {
                        // ≈ −54 dBFS: below any real room, i.e. muted or gain at zero.
                        if rms < 0.002 {
                            live.mic_quiet_since.get_or_insert_with(Instant::now);
                        } else {
                            live.mic_quiet_since = None;
                        }
                    }
                    let level = ui::meter_level(rms);
                    fresh[i] = fresh[i].max(level);
                    // Adaptive speech gate: well above this source's noise
                    // floor, which follows quiet stretches quickly and loud
                    // ones only slowly.
                    let floor = &mut live.floor[i];
                    *floor = if level < *floor { level } else { *floor + (level - *floor) * 0.002 };
                    if live.paused_since.is_none() && level > *floor + 0.2 && level > 0.3 {
                        live.heard();
                    }
                }
                Event::Chunk { millis, .. } => live.last_latency_ms = Some(millis),
                Event::Language { source, status } => live.languages[source as usize] = Some(status),
                Event::Warning(w) => {
                    eprintln!("[mostalgia] {w}");
                    let w = SharedString::from(w);
                    if !live.warnings.contains(&w) {
                        live.warnings.push(w);
                    }
                }
                Event::EngineError { engine, message } => {
                    eprintln!("[mostalgia] {engine}: {message}");
                    live.engine_error = Some((engine.into(), message.into()));
                    live.failed_chunks += 1;
                }
                Event::Stalled { secs } => {
                    if secs == 0 || live.stalled_secs.is_none() {
                        eprintln!("[mostalgia] live transcription {}", if secs == 0 { "working again".to_string() } else { format!("stalled for {secs} s") });
                    }
                    live.stalled_secs = (secs > 0).then_some(secs);
                }
            }
        }
        // Meters: rise quickly but not instantly, fall slowly — smooth at 30 Hz.
        for i in 0..2 {
            let prev = live.levels[i];
            live.levels[i] = if fresh[i] > prev { prev + (fresh[i] - prev) * 0.6 } else { (prev * 0.85).max(fresh[i]) };
        }
        if new_lines {
            self.follow = at_bottom || self.follow && at_bottom;
            if self.follow {
                self.scroll.scroll_to_bottom();
            }
        } else if !at_bottom {
            self.follow = false;
        } else {
            self.follow = true;
        }
        cx.notify();
        true
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        if let Phase::Recording(live) = &mut self.phase {
            match live.paused_since.take() {
                Some(since) => {
                    live.paused_total += since.elapsed();
                    if let Some(r) = &live.recorder {
                        r.set_paused(false);
                    }
                }
                None => {
                    live.paused_since = Some(Instant::now());
                    if let Some(r) = &live.recorder {
                        r.set_paused(true);
                    }
                }
            }
            cx.notify();
        }
    }

    fn mark_moment(&mut self, cx: &mut Context<Self>) {
        if let Phase::Recording(live) = &mut self.phase {
            let at = live.elapsed().as_secs_f64();
            let n = live.bookmarks.iter().filter(|b| b.kind == MarkKind::Moment).count() + 1;
            live.add_mark(Bookmark {
                at_secs: at,
                label: trf("Markierung {}", &[&n]),
                kind: MarkKind::Moment,
            });
            cx.notify();
        }
    }

    /// Switches a source off or on for the rest of this recording (mic +
    /// desktop ↔ desktop only, and the other way round), marked in the
    /// transcript. A source that wasn't recording starts now, its track
    /// filled with silence up to here. The saved defaults stay as they are.
    pub fn set_source(&mut self, source: Source, on: bool, cx: &mut Context<Self>) {
        let Phase::Recording(live) = &mut self.phase else { return };
        let i = source as usize;
        if live.sources_on[i] == on {
            return;
        }
        let at = match &live.recorder {
            Some(r) => match r.set_source(source, on) {
                Ok(at) => at,
                Err(e) => {
                    let name = if source == Source::Mic { tr("Mikrofon") } else { tr("Desktop-Audio") };
                    let w = SharedString::from(trf("{} lässt sich nicht einschalten: {}", &[&name, &format!("{e:#}")]));
                    eprintln!("[mostalgia] {w}");
                    if !live.warnings.contains(&w) {
                        live.warnings.push(w);
                    }
                    cx.notify();
                    return;
                }
            },
            None => live.elapsed().as_secs_f64(),
        };
        live.switched(source, on, at);
        cx.notify();
    }

    /// Whether `source` is being recorded right now (`None`: no recording).
    pub fn source_on(&self, source: Source) -> Option<bool> {
        let Phase::Recording(live) = &self.phase else { return None };
        Some(live.sources_on[source as usize])
    }

    /// Recordings a crash, kill or power loss left unfinished (see
    /// `recovery`): each is repaired and goes through the normal final pass
    /// in the background, one after another, shown in the library's
    /// processing list and here; then it opens like any saved recording.
    /// One that can't be repaired says so, with its folder.
    pub fn recover_orphans(&mut self, cx: &mut Context<Self>) {
        use crate::processing::{self, Job, Key};
        use mostalgia_core::recovery;
        let mut found = Vec::new();
        for dir in recovery::orphans(&paths::recordings_dir()) {
            match recovery::recording(&dir) {
                Ok(rec) => {
                    let minutes = (rec.duration_secs / 60.0).round() as u64;
                    let line = trf(
                        "Unterbrochene Aufnahme von {} ({} min) wiederhergestellt – wird verarbeitet …",
                        &[&rec.started_at.format("%H:%M"), &minutes],
                    );
                    eprintln!("[mostalgia] {line} ({})", dir.display());
                    self.recovered.push(line.into());
                    let key = Key::Recovered(rec.started_at.timestamp());
                    let title = trf("Besprechung {}", &[&i18n::date_time(&rec.started_at)]);
                    processing::set(key, Job { title, stage: tr("Wartet …").to_string(), fraction: None });
                    found.push(rec);
                }
                Err(e) => {
                    let message = trf("Unterbrochene Aufnahme in {} lässt sich nicht wiederherstellen: {}", &[&dir.display(), &format!("{e:#}")]);
                    eprintln!("[mostalgia] {message}");
                    self.error = Some(message.into());
                }
            }
        }
        if found.is_empty() {
            cx.notify();
            return;
        }
        let config = self.config.clone();
        let people = self.people;
        let (tx, rx) = crossbeam_channel::unbounded::<(std::path::PathBuf, Result<i64, String>)>();
        std::thread::spawn(move || {
            for rec in found {
                let key = Key::Recovered(rec.started_at.timestamp());
                let vad = config.vad_model_path();
                let opts = finalize::FinalPassOptions {
                    vad_model: vad.exists().then_some(vad),
                    diarize: config.diarize,
                    denoise_mic: config.denoise,
                    people,
                    progress: finalize::Progress::new(move |stage| processing::stage(key, stage.label(), stage.fraction())),
                };
                processing::stage(key, tr("Transkript wird fertiggestellt …").to_string(), None);
                let saved = finalize::engine_for(&config, &config.final_engine)
                    .and_then(|mut engine| finalize::save_recording(&rec, &mut engine, &opts, &config.language, &[]));
                processing::clear(key);
                let result = match saved {
                    Ok(saved) => {
                        recovery::finish(&rec.dir);
                        Ok(saved.meeting_id)
                    }
                    Err(e) => Err(trf("Wiederhergestellte Aufnahme ließ sich nicht verarbeiten: {} — sie liegt in {}", &[&format!("{e:#}"), &rec.dir.display()])),
                };
                let _ = tx.send((rec.dir.clone(), result));
            }
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let mut done = false;
                loop {
                    match rx.try_recv() {
                        Ok((_, result)) => {
                            let alive = this
                                .update(cx, |this, cx| {
                                    match result {
                                        Ok(id) => cx.emit(RecordingEvent::Saved(id)),
                                        Err(e) => {
                                            eprintln!("[mostalgia] {e}");
                                            this.error = Some(e.into());
                                        }
                                    }
                                    cx.notify();
                                })
                                .is_ok();
                            if !alive {
                                return;
                            }
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            done = true;
                            break;
                        }
                    }
                }
                if done {
                    this.update(cx, |this, cx| {
                        this.recovered.clear();
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let final_label = self.config.engine_label(&self.config.final_engine);
        let Phase::Recording(live) = &self.phase else { return };
        let started = chrono::Local::now() - chrono::Duration::milliseconds(live.started.elapsed().as_millis() as i64);
        let title = trf("Besprechung {}", &[&i18n::date_time(&started)]);
        let busy = Phase::Finalizing {
            what: tr("Transkript wird fertiggestellt …"),
            detail: trf("Feinschliff mit {}", &[&final_label]).into(),
            title: title.clone(),
            lines: live.lines.clone(),
        };
        let Phase::Recording(live) = std::mem::replace(&mut self.phase, busy) else {
            return;
        };
        let live = *live;
        use crate::processing::{self, Job, Key};
        processing::set(Key::Recording, Job { title, stage: tr("Transkript wird fertiggestellt …").to_string(), fraction: None });
        let progress = finalize::Progress::new(|stage| processing::stage(Key::Recording, stage.label(), stage.fraction()));
        let Some(recorder) = live.recorder else {
            self.phase = Phase::Setup;
            return;
        };
        recorder.stop();
        let dir = recorder.dir().to_path_buf();
        let bookmarks = live.bookmarks;
        let live_choice = live.choice;
        let live_label = self.engine_label.as_ref().map(|(l, _)| l.to_string()).unwrap_or_default();
        let config = self.config.clone();
        let slot = self.engine.clone();
        let people = std::mem::take(&mut self.people);
        cx.notify();

        let task = cx.background_spawn(async move {
            let (rec, live_engine) = recorder.join()?;
            let vad = config.vad_model_path();
            let opts = finalize::FinalPassOptions {
                vad_model: vad.exists().then_some(vad),
                diarize: config.diarize,
                denoise_mic: config.denoise,
                people,
                progress,
            };
            let language = config.language.as_str();
            let final_choice = &config.final_engine;
            if same_engine(&live_choice, final_choice) {
                let mut engine = live_engine;
                let saved = finalize::save_recording(&rec, &mut engine, &opts, language, &bookmarks);
                keep_local(&slot, engine);
                return saved;
            }
            let built = match final_choice.is_local().then(|| slot.lock().unwrap().take()).flatten() {
                Some(mut engine) => engine.set_language(language).map(|_| {
                    engine.set_meeting_languages(&config.meeting_languages);
                    engine
                }),
                None => finalize::engine_for(&config, final_choice),
            };
            keep_local(&slot, live_engine);
            match built {
                Ok(mut engine) => {
                    let saved = finalize::save_recording(&rec, &mut engine, &opts, language, &bookmarks);
                    keep_local(&slot, engine);
                    saved
                }
                // Keep the live transcript and say why the final pass didn't run.
                Err(e) => {
                    let error = trf("Feinschliff mit {} nicht möglich: {}", &[&final_label, &format!("{e:#}")]);
                    save_live_only(&rec, &live_label, error, language, &bookmarks)
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            crate::processing::clear(crate::processing::Key::Recording);
            if result.is_ok() {
                mostalgia_core::recovery::finish(&dir);
            }
            this.update(cx, |this, cx| match result {
                Ok(saved) => {
                    if let Some(e) = &saved.error {
                        eprintln!("[mostalgia] {e}");
                    }
                    this.phase = Phase::Setup;
                    this.call_mode = false;
                    this.sources_override = None;
                    cx.emit(RecordingEvent::Saved(saved.meeting_id));
                    cx.notify();
                }
                Err(e) => this.fail(anyhow::anyhow!(trf("Speichern fehlgeschlagen: {} — die Aufnahme liegt in {}", &[&format!("{e:#}"), &dir.display()])), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Which engines will transcribe, where the audio goes, and the mode.
    fn render_engines(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let config = &self.config;
        let (live, fine) = (&config.live_engine, &config.final_engine);
        let mut clouds: Vec<String> = Vec::new();
        for choice in [live, fine].into_iter().filter(|c| !c.is_local()) {
            let name = config.connection(&choice.connection).map_or(choice.connection.clone(), |c| c.name.clone());
            if !clouds.contains(&name) {
                clouds.push(name);
            }
        }
        let mode = config.mode();
        let mode_color = match mode {
            Mode::Local => theme.success,
            Mode::Hybrid => theme.primary,
            Mode::Cloud => theme.warning,
        };
        v_flex()
            .gap_1p5()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        Icon::new(if clouds.is_empty() { IconName::Cpu } else { IconName::Globe })
                            .xsmall()
                            .mt(px(2.)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .when(same_engine(live, fine), |el| {
                                el.child(trf("Transkription und Feinschliff: {}", &[&config.engine_label(live)]))
                            })
                            .when(!same_engine(live, fine), |el| {
                                el.child(trf("Transkription: {}", &[&config.engine_label(live)]))
                                    .child(trf("Feinschliff: {}", &[&config.engine_label(fine)]))
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_2()
                            .rounded_full()
                            .bg(mode_color.opacity(0.15))
                            .text_color(mode_color)
                            .font_medium()
                            .child(mode.label()),
                    ),
            )
            .when(!clouds.is_empty(), |el| {
                el.child(
                    h_flex()
                        .gap_2()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).xsmall())
                        .child(trf("Audio geht an {}", &[&clouds.join(", ")])),
                )
            })
    }

    fn render_setup(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let source_row = |id: &'static str, icon: IconName, title: &'static str, detail: String, on: bool, cx: &mut Context<Self>| {
            let theme = cx.theme();
            h_flex()
                .gap_3()
                .p_3()
                .rounded(theme.radius)
                .border(ui::border_width(cx))
                .border_color(if on { theme.primary.opacity(0.5) } else { theme.border })
                .child(Icon::new(icon).text_color(if on { theme.primary } else { theme.muted_foreground }))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_medium().child(title))
                        .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(detail)),
                )
                .child(Switch::new(id).checked(on).on_change(cx.listener(move |this, checked: &bool, _, cx| {
                    match id {
                        "mic" => this.config.record_mic = *checked,
                        _ => this.config.record_system = *checked,
                    }
                    this.save_config();
                    cx.notify();
                })))
        };
        let mic_detail = if self.config.record_mic {
            self.config.mic_device.clone().unwrap_or_else(|| tr("Standard-Mikrofon").into())
        } else {
            tr("Aus – deine Stimme wird nicht aufgenommen (bei erkannten Anrufen immer an)").into()
        };
        let sys_detail = self.config.system_device.clone().unwrap_or_else(|| tr("Alles, was gerade abgespielt wird").into());
        let lang = self.config.language.clone();
        let lang_button = |code: &'static str, label: &'static str, cx: &mut Context<Self>| {
            Button::new(SharedString::from(format!("lang-{code}")))
                .label(label)
                .small()
                .when(lang == code, |b| b.primary())
                .when(lang != code, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.config.language = code.into();
                    this.save_config();
                    cx.notify();
                }))
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                v_flex()
                    .w(px(460. * ui::ui_scale()))
                    .max_w_full()
                    .gap_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_2xl().font_semibold().child(tr("Neue Aufnahme")))
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(tr("Nimmt dein Mikrofon und den Ton deines Rechners auf und schreibt live mit.")),
                            ),
                    )
                    .child(source_row("mic", IconName::Mic, tr("Mikrofon"), mic_detail, self.config.record_mic, cx))
                    .child(source_row("system", IconName::Monitor, tr("Desktop-Audio"), sys_detail, self.config.record_system, cx))
                    .child(
                        // Wraps the switch below the language buttons when narrow.
                        h_flex()
                            .flex_wrap()
                            .justify_between()
                            .gap_x_4()
                            .gap_y_2()
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_1()
                                    .child(div().text_sm().text_color(cx.theme().muted_foreground).mr_2().child(tr("Sprache")))
                                    .child(lang_button("de", "Deutsch", cx))
                                    .child(lang_button("en", "English", cx))
                                    .child(lang_button("auto", tr("Automatisch erkennen"), cx).tooltip(tr("Empfohlen: erkennt die Sprache der Besprechung selbst"))),
                            )
                            .child(
                                Switch::new("labels")
                                    .label(tr("Sprecher"))
                                    .checked(self.config.speaker_labels)
                                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                        this.config.speaker_labels = *checked;
                                        this.save_config();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(self.config.diarize && self.sources()[1], |el| el.child(self.render_people(cx)))
                    .child(self.render_engines(cx))
                    .children(self.recovered.iter().map(|line| {
                        h_flex()
                            .p_3()
                            .gap_2()
                            .items_start()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().warning.opacity(0.12))
                            .text_sm()
                            .child(Icon::new(IconName::TriangleAlert).small().text_color(cx.theme().warning).mt(px(2.)))
                            .child(div().flex_1().min_w_0().child(line.clone()))
                    }))
                    .when_some(self.error.clone(), |el, err| {
                        el.child(
                            div()
                                .p_3()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().danger.opacity(0.1))
                                .text_color(cx.theme().danger)
                                .text_sm()
                                .child(err),
                        )
                    })
                    .child(
                        Button::new("start")
                            .primary()
                            .large()
                            .w_full()
                            .icon(IconName::Mic)
                            .label(tr("Aufnahme starten"))
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
            )
    }

    /// "People in the call": tells the speaker separation how many voices
    /// to expect (you on the mic, everyone else on the desktop audio).
    fn render_people(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let picked = self.people;
        h_flex()
            .flex_wrap()
            .gap_1()
            .child(div().text_sm().text_color(cx.theme().muted_foreground).mr_2().child(tr("Personen im Gespräch (mit dir)")))
            .children(mostalgia_core::diarize::PEOPLE_CHOICES.into_iter().map(|n| {
                Button::new(SharedString::from(format!("people-{n}")))
                    .label(ui::people_label(n))
                    .small()
                    .when(picked == n, |b| b.primary())
                    .when(picked != n, |b| b.ghost())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_people(n, cx)))
            }))
    }

    fn render_busy(&self, text: &'static str, detail: Option<SharedString>, stopping: bool, cx: &App) -> impl IntoElement {
        let spec = crate::themes::active(cx);
        let burst = stopping && spec.animations && !self.config.reduce_motion && spec.scene.as_deref() == Some("blocks");
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .when(burst, |el| el.child(anim::block_break(palette(&spec, cx))))
            .child(gpui_kit::component::spinner::Spinner::new().large())
            .child(div().text_color(cx.theme().muted_foreground).child(text))
            .when_some(detail, |el, d| el.child(div().text_xs().text_color(cx.theme().muted_foreground).child(d)))
    }

    /// After Stop: the meeting's title, what the final pass is doing (with
    /// how far, when known) and the live transcript, which the final one
    /// replaces once it's saved.
    fn render_finalizing(&self, what: &'static str, detail: SharedString, title: String, lines: &[LiveLine], cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let job = crate::processing::get(crate::processing::Key::Recording);
        let stage = job.as_ref().map_or_else(|| what.to_string(), |j| j.stage.clone());
        let fraction = job.and_then(|j| j.fraction);
        let speaker_labels = self.config.speaker_labels;
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .px_6()
                    .py_4()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_lg().font_semibold().child(title))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(gpui_kit::component::spinner::Spinner::new().small().color(theme.primary))
                            .child(div().text_sm().font_medium().child(stage))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(detail)),
                    )
                    .child(ui::progress_bar(fraction, cx)),
            )
            .child(
                div()
                    .px_6()
                    .pt_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(tr("Live-Transkript – wird durch das fertige ersetzt, sobald es gespeichert ist.")),
            )
            .child(
                v_flex()
                    .id("finalizing-lines")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .py_2()
                    .gap_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .children(lines.iter().map(|l| {
                        let who = l.speaker.as_deref().unwrap_or(l.source.speaker_label());
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(div().flex_none().w(px(52.)).text_xs().font_family(theme.mono_font_family.clone()).child(ui::hms(l.start_secs)))
                            .when(speaker_labels, |el| el.child(div().flex_none().text_xs().font_medium().child(ui::speaker_display(who))))
                            .child(div().flex_1().min_w_0().child(l.text.clone()))
                    })),
            )
    }

    fn render_live(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let language_picker = self.language_picker(cx);
        let speaker_labels = self.config.speaker_labels;
        let follow = self.follow;
        let engine_label = self.engine_label.clone();
        let Phase::Recording(live) = &self.phase else {
            unreachable!()
        };
        let spec = crate::themes::active(cx);
        let motion = spec.animations && !self.config.reduce_motion;
        let playful = motion && spec.playful;
        let colors = palette(&spec, cx);
        let theme = cx.theme();
        let paused = live.paused_since.is_some();
        let elapsed = live.elapsed().as_secs_f64();
        let enabled = live.sources_on;
        // Not while the mic is off on purpose.
        let mic_silent = enabled[0]
            && live.paused_since.is_none()
            && live.mic_quiet_since.is_some_and(|t| t.elapsed() > Duration::from_secs(20));

        // A switch per source (crossed out while off) and its level.
        let meter = |source: Source, cx: &Context<Self>| {
            let i = source as usize;
            let on = enabled[i];
            let (icon, tip) = match (source, on) {
                (Source::Mic, true) => (IconName::Mic, tr("Mikrofon ausschalten")),
                (Source::Mic, false) => (IconName::MicOff, tr("Mikrofon einschalten")),
                (Source::System, true) => (IconName::Monitor, tr("Desktop-Audio ausschalten")),
                (Source::System, false) => (IconName::MonitorOff, tr("Desktop-Audio einschalten")),
            };
            let color = if on { cx.theme().muted_foreground } else { cx.theme().danger };
            h_flex()
                .gap_1()
                .w(px(178.))
                .child(
                    Button::new(if source == Source::Mic { "toggle-mic" } else { "toggle-system" })
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(icon).text_color(color))
                        .tooltip(tip)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_source(source, !on, cx))),
                )
                .child(if on {
                    ui::level_meter(live.levels[i], !paused, cx).into_any_element()
                } else {
                    div().flex_1().text_xs().text_color(cx.theme().danger).child(tr("Aus")).into_any_element()
                })
        };

        // Wraps the buttons onto a second row on narrow windows.
        let header = h_flex()
            .flex_wrap()
            .px_6()
            .py_4()
            .gap_x_6()
            .gap_y_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .child({
                        let dot = div().size(px(12.)).rounded_full().bg(if paused { theme.warning } else { theme.danger });
                        if motion && !paused { anim::pulse(dot) } else { dot.into_any_element() }
                    })
                    .child(
                        div()
                            .text_3xl()
                            .font_semibold()
                            .font_family(theme.mono_font_family.clone())
                            .child(ui::hms(elapsed)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(if paused { tr("Pausiert") } else { tr("Aufnahme läuft") }),
                    ),
            )
            .child(v_flex().gap_2().child(meter(Source::Mic, cx)).child(meter(Source::System, cx)))
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("mark")
                            .icon(IconName::Bookmark)
                            .label(tr("Markieren"))
                            .ghost()
                            .tooltip(tr("Moment markieren"))
                            .on_click(cx.listener(|this, _, _, cx| this.mark_moment(cx))),
                    )
                    .child(
                        Button::new("pause")
                            .icon(if paused { IconName::Play } else { IconName::Pause })
                            .label(if paused { tr("Fortsetzen") } else { tr("Pause") })
                            .outline()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx))),
                    )
                    .child(
                        Button::new("stop")
                            .icon(IconName::Square)
                            .label(tr("Beenden"))
                            .danger()
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                    ),
            );

        let mut rows: Vec<AnyElement> = Vec::new();
        let mut marks = live.bookmarks.iter().peekable();
        for (ix, line) in live.lines.iter().enumerate() {
            while let Some(b) = marks.next_if(|b| b.at_secs <= line.start_secs) {
                rows.push(bookmark_row(b, cx).into_any_element());
            }
            let label = line.speaker.as_deref().unwrap_or(line.source.speaker_label());
            let row = h_flex()
                    .id(("line", ix))
                    .items_start()
                    .gap_3()
                    .py_1p5()
                    .child(
                        div()
                            .w(ui::transcript_rems(3.75))
                            .flex_none()
                            .pt_0p5()
                            .text_size(ui::transcript_rems(0.75))
                            .text_color(theme.muted_foreground)
                            .font_family(theme.mono_font_family.clone())
                            .child(ui::hms(line.start_secs)),
                    )
                    .when(speaker_labels, |el| el.child(ui::speaker_chip(label, cx)))
                    .child(ui::bidi_text(line.text.clone(), true).flex_1().min_w_0().text_size(ui::transcript_rems(1.)).line_height(ui::transcript_rems(1.5)));
            rows.push(if motion { anim::arrive(row, ("line-in", ix), playful) } else { row.into_any_element() });
        }
        for b in marks {
            rows.push(bookmark_row(b, cx).into_any_element());
        }
        // Provisional text for what's still being said, greyed and italic.
        for (i, slot) in live.interim.iter().enumerate() {
            let Some((start, text)) = slot else { continue };
            let source = if i == 0 { Source::Mic } else { Source::System };
            let row = h_flex()
                    .id(("interim", i))
                    .items_start()
                    .gap_3()
                    .py_1p5()
                    .child(
                        div()
                            .w(ui::transcript_rems(3.75))
                            .flex_none()
                            .pt_0p5()
                            .text_size(ui::transcript_rems(0.75))
                            .text_color(theme.muted_foreground)
                            .font_family(theme.mono_font_family.clone())
                            .child(ui::hms(*start)),
                    )
                    .when(speaker_labels, |el| el.child(ui::speaker_chip(source.speaker_label(), cx)))
                    .child(div().flex_1().min_w_0().italic().text_size(ui::transcript_rems(1.)).line_height(ui::transcript_rems(1.5)).text_color(theme.muted_foreground).child(format!("{text} …")));
            rows.push(if motion { anim::breathe(row, ("interim-breathe", i)) } else { row.opacity(0.6).into_any_element() });
        }
        let empty = live.lines.is_empty();

        let footer = h_flex()
            .px_6()
            .py_2()
            .gap_4()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(trf("{} Zeilen", &[&live.lines.len()]))
            .when_some(live.last_latency_ms, |el, ms| el.child(trf("Latenz {} ms", &[&ms])))
            .children(language_picker)
            .when_some(engine_label, |el, (label, cloud)| {
                el.child(
                    h_flex()
                        .gap_1()
                        .child(Icon::new(if cloud { IconName::Globe } else { IconName::Cpu }).xsmall())
                        .child(label),
                )
            })
            .child(div().flex_1())
            .when(!follow, |el| {
                el.child(
                    Button::new("follow")
                        .xsmall()
                        .ghost()
                        .icon(IconName::ArrowDown)
                        .label(tr("Zum Ende"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.follow = true;
                            this.scroll.scroll_to_bottom();
                            cx.notify();
                        })),
                )
            });

        v_flex()
            .size_full()
            .child(header)
            .when_some(live.stalled_secs, |el, secs| {
                el.child(
                    h_flex()
                        .px_6()
                        .py_2()
                        .gap_2()
                        .items_start()
                        .bg(cx.theme().warning.opacity(0.12))
                        .text_sm()
                        .child(Icon::new(IconName::TriangleAlert).small().text_color(cx.theme().warning).mt(px(2.)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_medium().child(trf("Die Live-Transkription hängt seit {} s", &[&secs])))
                                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(tr(
                                    "Die Aufnahme läuft weiter und wird vollständig gespeichert; der fehlende Live-Text kommt mit der Transkription nach dem Ende.",
                                ))),
                        ),
                )
            })
            .when_some(live.engine_error.clone(), |el, (engine, message)| {
                el.child(
                    h_flex()
                        .px_6()
                        .py_2()
                        .gap_2()
                        .items_start()
                        .bg(cx.theme().danger.opacity(0.1))
                        .text_sm()
                        .child(Icon::new(IconName::TriangleAlert).small().text_color(cx.theme().danger).mt(px(2.)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .font_medium()
                                        .text_color(cx.theme().danger)
                                        .child(trf("Transkription mit {} fehlgeschlagen: {}", &[&engine, &message])),
                                )
                                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(tr(
                                    "Die Aufnahme läuft weiter und wird gespeichert. Danach kannst du sie erneut transkribieren.",
                                ))),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(trf("{} Abschnitte ohne Text", &[&live.failed_chunks])),
                        ),
                )
            })
            .children(live.warnings.iter().enumerate().map(|(i, w)| {
                h_flex()
                    .px_6()
                    .py_2()
                    .gap_2()
                    .bg(cx.theme().warning.opacity(0.15))
                    .text_sm()
                    .child(Icon::new(IconName::TriangleAlert).small().text_color(cx.theme().warning))
                    .child(div().flex_1().min_w_0().child(w.clone()))
                    .child(Button::new(("dismiss-warning", i)).xsmall().ghost().icon(IconName::X).on_click(cx.listener(move |this, _, _, cx| {
                        if let Phase::Recording(live) = &mut this.phase {
                            if i < live.warnings.len() {
                                live.warnings.remove(i);
                            }
                        }
                        cx.notify();
                    })))
            }))
            .when(mic_silent, |el| {
                el.child(
                    h_flex()
                        .px_6()
                        .py_2()
                        .gap_2()
                        .bg(cx.theme().warning.opacity(0.15))
                        .text_sm()
                        .child(Icon::new(IconName::MicOff).small().text_color(cx.theme().warning))
                        .child(tr("Dein Mikrofon ist ganz still. Stummgeschaltet – oder nur nachdenklich? Prüfe das Gerät oder ob die Anruf-App es leiser gestellt hat.")),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .when_some(
                        spec.scene.as_deref().filter(|_| empty && motion).and_then(|kind| anim::scene(kind, colors)),
                        |el, scene| el.child(scene),
                    )
                    .child(
                    v_flex()
                        .id("live-transcript")
                        .size_full()
                        .px_6()
                        .py_4()
                        .track_scroll(&self.scroll)
                        .overflow_y_scroll()
                        .children(rows)
                        .when(empty, |el| {
                            el.child(
                                v_flex()
                                    .flex_1()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .pt_24()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if playful {
                                        anim::bob(Icon::new(IconName::AudioLines).large().text_color(cx.theme().primary))
                                    } else {
                                        Icon::new(IconName::AudioLines).large().into_any_element()
                                    })
                                    .child(tr("Bereit, wenn ihr es seid. Der erste Satz landet hier.")),
                            )
                        }),
                ),
            )
            .child(footer)
    }
}

/// Two choices that mean the same engine (local Whisper has no model id).
fn same_engine(a: &SttChoice, b: &SttChoice) -> bool {
    a.is_local() && b.is_local() || a == b
}

/// Puts a local Whisper engine back into the cache; cloud engines are dropped.
fn keep_local(slot: &SharedEngine, engine: Engine) {
    if !engine.is_cloud() {
        *slot.lock().unwrap() = Some(engine);
    }
}

/// Fails before a recording starts when the final-pass engine can't be
/// used, so the user fixes it instead of finding out afterwards.
fn check_final_engine(config: &Config) -> Result<()> {
    let choice = &config.final_engine;
    if same_engine(choice, &config.live_engine) {
        return Ok(());
    }
    let result = if choice.is_local() {
        let model = config.whisper_model_path();
        if model.exists() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(trf("Whisper-Modell nicht gefunden: {} — unter Einstellungen → System herunterladen", &[&model.display()])))
        }
    } else {
        cloud_stt::check(config.connection(&choice.connection), &choice.connection, &choice.model)
    };
    result.map_err(|e| anyhow::anyhow!(trf("Feinschliff mit {} nicht möglich: {}", &[&config.engine_label(choice), &format!("{e:#}")])))
}

/// Saves a recording with its live transcript when the final-pass engine
/// couldn't even be set up, marked with why.
fn save_live_only(rec: &Recording, live_label: &str, error: String, language: &str, bookmarks: &[Bookmark]) -> Result<finalize::Saved> {
    let mut store = Store::open(&paths::database())?;
    let title = trf("Besprechung {}", &[&i18n::date_time(&rec.started_at)]);
    let id = store.insert_meeting(
        &NewMeeting {
            title: &title,
            started_at: rec.started_at,
            duration_secs: rec.duration_secs,
            language,
            dir: &rec.dir,
        },
        &rec.live_lines,
    )?;
    store.set_transcript_info(id, &trf("Live-Transkript ({})", &[&live_label]), Some(&error))?;
    store.add_bookmarks(id, bookmarks)?;
    if let Err(e) = mostalgia_core::ask::index_meeting(id) {
        eprintln!("[mostalgia] search index: {e:#}");
    }
    Ok(finalize::Saved { meeting_id: id, error: Some(error) })
}

fn bookmark_row(b: &Bookmark, cx: &App) -> Div {
    ui::mark_row(b, true, cx)
}

impl Focusable for RecordingView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RecordingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.phase {
            Phase::Setup => self.render_setup(cx).into_any_element(),
            Phase::Loading => {
                let detail = self.config.engine_label(&self.config.live_engine).into();
                self.render_busy(tr("Erinnerungen werden geladen …"), Some(detail), false, cx).into_any_element()
            }
            Phase::Finalizing { what, detail, title, lines } => self.render_finalizing(what, detail.clone(), title.clone(), lines, cx).into_any_element(),
            Phase::Recording(_) => self.render_live(cx).into_any_element(),
        };
        div().size_full().track_focus(&self.focus).child(content)
    }
}

/// Motion for the recording screen. Everything here is decoration: cheap
/// (capped at 30 fps, a few dozen plain divs), behind or around the
/// transcript, never on the text itself beyond a short fade-in, and off
/// when the theme or the "reduce motion" setting says so.
mod anim {
    use std::f32::consts::TAU;
    use std::time::Duration;

    use gpui_kit::component::{h_flex, v_flex};
    use gpui_kit::prelude::*;
    use gpui_kit::*;

    /// Overshooting ease-out: past the target, then back — a cartoon "pop".
    fn back_out(t: f32) -> f32 {
        let (c1, c3) = (1.70158, 2.70158);
        let x = t - 1.0;
        1.0 + c3 * x * x * x + c1 * x * x
    }

    /// A transcript row arriving: fades in while sliding up a little;
    /// playful themes pop with a small overshoot.
    pub fn arrive(row: Stateful<Div>, id: impl Into<ElementId>, playful: bool) -> AnyElement {
        let row = row.relative();
        if playful {
            row.with_animation(id, Animation::new(Duration::from_millis(420)).with_easing(back_out), |el, d| {
                el.opacity(d.clamp(0., 1.)).top(px((1. - d) * 14.))
            })
            .into_any_element()
        } else {
            row.with_animation(id, Animation::new(Duration::from_millis(300)).with_easing(ease_out_quint()), |el, d| {
                el.opacity(d).top(px((1. - d) * 6.))
            })
            .into_any_element()
        }
    }

    /// The breathing recording dot.
    pub fn pulse(dot: Div) -> AnyElement {
        dot.with_animation(
            "rec-dot",
            Animation::new(Duration::from_millis(1400)).repeat().with_easing(pulsating_between(0.35, 1.0)),
            |el, d| el.opacity(d),
        )
        .into_any_element()
    }

    /// Provisional text gently breathing so it reads as "still changing".
    pub fn breathe(row: Stateful<Div>, id: impl Into<ElementId>) -> AnyElement {
        row.with_animation(
            id,
            Animation::new(Duration::from_millis(2000)).repeat().with_easing(pulsating_between(0.45, 0.75)),
            |el, d| el.opacity(d),
        )
        .into_any_element()
    }

    /// An icon bobbing up and down (playful empty state).
    pub fn bob(el: impl IntoElement + 'static) -> AnyElement {
        div()
            .relative()
            .child(el)
            .with_animation(
                "bob",
                Animation::new(Duration::from_millis(1100)).repeat().with_easing(bounce(ease_in_out)),
                |el, d| el.top(px(-10. * d)),
            )
            .into_any_element()
    }

    /// Repeating ambient layer: `draw` adds children for the phase `t` (0..1).
    fn ambient(id: &'static str, secs: u64, draw: impl Fn(Div, f32) -> Div + 'static) -> AnyElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .overflow_hidden()
            .with_animation(id, Animation::new(Duration::from_secs(secs)).repeat().with_max_fps(30.), move |el, t| draw(el, t))
            .into_any_element()
    }

    /// Deterministic pseudo-random 0..1 for decoration layouts.
    fn rnd(i: usize, salt: u32) -> f32 {
        let mut x = (i as u32).wrapping_mul(2_654_435_761) ^ salt.wrapping_mul(40_503);
        x ^= x >> 13;
        x = x.wrapping_mul(1_274_126_177);
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    fn dot(size: f32, color: Hsla) -> Div {
        div().absolute().size(px(size)).rounded_full().bg(color)
    }

    #[derive(Clone, Copy)]
    pub struct Palette {
        pub bg: Hsla,
        pub fg: Hsla,
        pub primary: Hsla,
        pub muted: Hsla,
        pub surface: Hsla,
        pub accents: [Hsla; 4],
    }

    /// The illustration behind the empty live transcript, by theme scene.
    pub fn scene(kind: &str, p: Palette) -> Option<AnyElement> {
        Some(match kind {
            // Synthwave: a striped sun on the horizon and grid lines
            // sliding toward the viewer.
            "grid" => ambient("scene-grid", 4, move |mut el, t| {
                let horizon = 0.52;
                el = el.child(
                    div()
                        .absolute()
                        .top(relative(horizon - 0.28))
                        .left(relative(0.5))
                        .ml(px(-80.))
                        .size(px(160.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(p.primary.opacity(0.35))
                        .children((0..5).map(|k| {
                            div().absolute().left_0().w_full().top(px(92. + k as f32 * 14.)).h(px(4. + k as f32)).bg(p.bg)
                        })),
                );
                el = el.child(div().absolute().left_0().w_full().top(relative(horizon)).h(px(2.)).bg(p.primary.opacity(0.6)));
                for k in 0..12 {
                    let f = (k as f32 + t) / 12.;
                    el = el.child(
                        div()
                            .absolute()
                            .left_0()
                            .w_full()
                            .h(px(1.))
                            .top(relative(horizon + f * f * (1. - horizon)))
                            .bg(p.accents[1].opacity(0.08 + 0.3 * f)),
                    );
                }
                el
            }),
            // Blockwelt: a row of grass blocks hopping in turn.
            "blocks" => ambient("scene-blocks", 2, move |mut el, t| {
                for i in 0..5 {
                    let hop = ((t + i as f32 / 5.) * TAU).sin().max(0.) * 16.;
                    el = el.child(
                        v_flex()
                            .absolute()
                            .top(relative(0.26))
                            .mt(px(-hop))
                            .left(relative(0.5))
                            .ml(px(-110. + i as f32 * 46.))
                            .size(px(34.))
                            .border_2()
                            .border_color(hsla(0., 0., 0.08, 1.))
                            .child(div().h(px(9.)).w_full().bg(rgb(0x5ea836)))
                            .child(div().flex_1().w_full().bg(rgb(0x7b5534))),
                    );
                }
                el
            }),
            // Comic: a thought bubble with three dots bouncing in turn.
            "comic" => ambient("scene-comic", 1, move |el, t| {
                let bubble = h_flex()
                    .absolute()
                    .top(relative(0.14))
                    .left(relative(0.5))
                    .ml(px(-80.))
                    .w(px(160.))
                    .h(px(80.))
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .rounded(px(40.))
                    .border_3()
                    .border_color(p.fg)
                    .bg(p.surface)
                    .children((0..3).map(|i| {
                        let lift = ((t - i as f32 / 6.) * TAU).sin().max(0.) * 10.;
                        div().size(px(14.)).mb(px(lift)).rounded_full().bg(p.accents[i % 4])
                    }));
                el.child(bubble)
                    .child(dot(18., p.fg).top(relative(0.14)).mt(px(86.)).left(relative(0.5)).ml(px(-58.)))
                    .child(dot(10., p.fg).top(relative(0.14)).mt(px(108.)).left(relative(0.5)).ml(px(-72.)))
            }),
            // Weltraum: twinkling stars and a ringed planet.
            "stars" => ambient("scene-stars", 6, move |mut el, t| {
                for i in 0..48 {
                    let tw = 0.25 + 0.75 * ((t * (1. + rnd(i, 3) * 2.) + rnd(i, 4)) * TAU).sin().abs();
                    el = el.child(dot(1.5 + rnd(i, 5) * 2.5, p.fg.opacity(tw * 0.8)).left(relative(rnd(i, 1))).top(relative(rnd(i, 2))));
                }
                let drift = (t * TAU).sin() * 6.;
                el.child(dot(76., p.primary.opacity(0.35)).right(relative(0.18)).top(relative(0.18)).mt(px(drift)))
                    .child(
                        div()
                            .absolute()
                            .right(relative(0.18))
                            .mr(px(-28.))
                            .top(relative(0.18))
                            .mt(px(drift + 26.))
                            .w(px(132.))
                            .h(px(24.))
                            .rounded_full()
                            .border_2()
                            .border_color(p.accents[0].opacity(0.5)),
                    )
            }),
            // Aurora: soft bands swaying across the sky.
            "aurora" => ambient("scene-aurora", 10, move |mut el, t| {
                for i in 0..3 {
                    let sway = ((t + i as f32 * 0.3) * TAU).sin() * 0.06;
                    el = el.child(
                        div()
                            .absolute()
                            .left(relative(0.04 + i as f32 * 0.08 + sway))
                            .top(relative(0.18 + i as f32 * 0.09))
                            .w(relative(0.7))
                            .h(px(54.))
                            .rounded_full()
                            .bg(p.accents[i].opacity(0.08)),
                    );
                }
                el
            }),
            // Ozean: layered waves rolling along the bottom.
            "waves" => ambient("scene-waves", 7, move |mut el, t| {
                for i in 0..4 {
                    let roll = ((t + i as f32 * 0.25) * TAU).sin() * 0.05;
                    el = el.child(
                        div()
                            .absolute()
                            .left(relative(-0.1 + (i % 2) as f32 * 0.25 + roll))
                            .top(relative(0.74 + i as f32 * 0.06))
                            .w(relative(0.85))
                            .h(px(46.))
                            .rounded_full()
                            .bg(p.primary.opacity(0.05 + i as f32 * 0.025)),
                    );
                }
                el
            }),
            // Wald / Sakura: leaves or petals drifting down.
            "leaves" | "petals" => {
                let petals = kind == "petals";
                ambient(if petals { "scene-petals" } else { "scene-leaves" }, 9, move |mut el, t| {
                    for i in 0..14 {
                        let y = (t + rnd(i, 7)) % 1.;
                        let x = rnd(i, 8) + (y * TAU * 2. + rnd(i, 9) * TAU).sin() * 0.03;
                        let color = if petals { p.primary.opacity(0.35) } else { p.accents[i % 2].opacity(0.4) };
                        el = el.child(
                            div()
                                .absolute()
                                .left(relative(x))
                                .top(relative(y * 1.1 - 0.05))
                                .size(px(8. + rnd(i, 10) * 6.))
                                .rounded_tl(px(10.))
                                .rounded_br(px(10.))
                                .bg(color),
                        );
                    }
                    el
                })
            }
            // Kaffee: a cup with steam curling up.
            "steam" => ambient("scene-steam", 3, move |mut el, t| {
                for i in 0..3 {
                    for k in 0..5 {
                        let f = (t + k as f32 / 5.) % 1.;
                        let x = (f * TAU + i as f32).sin() * 6.;
                        el = el.child(
                            dot(10. - f * 6., p.muted.opacity((1. - f) * 0.9))
                                .left(relative(0.5))
                                .ml(px(-28. + i as f32 * 22. + x))
                                .top(relative(0.3))
                                .mt(px(-20. - f * 70.)),
                        );
                    }
                }
                el.child(
                    div()
                        .absolute()
                        .left(relative(0.5))
                        .ml(px(-45.))
                        .top(relative(0.3))
                        .w(px(90.))
                        .h(px(56.))
                        .rounded_b(px(22.))
                        .bg(p.primary.opacity(0.55)),
                )
                .child(
                    div()
                        .absolute()
                        .left(relative(0.5))
                        .ml(px(40.))
                        .top(relative(0.3))
                        .mt(px(10.))
                        .size(px(26.))
                        .rounded_full()
                        .border_4()
                        .border_color(p.primary.opacity(0.55)),
                )
            }),
            // Retro 90er: Memphis-style shapes bobbing in place.
            "memphis" => ambient("scene-memphis", 5, move |mut el, t| {
                for i in 0..10 {
                    let bobv = ((t + rnd(i, 11)) * TAU).sin() * 8.;
                    let size = 14. + rnd(i, 12) * 22.;
                    let shape = div()
                        .absolute()
                        .left(relative(0.08 + rnd(i, 13) * 0.84))
                        .top(relative(0.1 + rnd(i, 14) * 0.8))
                        .mt(px(bobv))
                        .size(px(size));
                    let color = p.accents[i % 4].opacity(0.45);
                    el = el.child(match i % 3 {
                        0 => shape.rounded_full().bg(color),
                        1 => shape.bg(color),
                        _ => shape.rounded_full().border_4().border_color(color),
                    });
                }
                el
            }),
            _ => return None,
        })
    }

    /// Blockwelt: a little burst of pixel debris when recording stops.
    pub fn block_break(p: Palette) -> AnyElement {
        div()
            .relative()
            .size(px(120.))
            .with_animation("block-break", Animation::new(Duration::from_millis(700)).with_easing(ease_out_quint()), move |mut el, t| {
                for i in 0..12 {
                    let a = i as f32 / 12. * TAU;
                    let r = t * 50.;
                    let fall = t * t * 30.;
                    el = el.child(
                        div()
                            .absolute()
                            .left(px(56. + a.cos() * r))
                            .top(px(56. + a.sin() * r + fall))
                            .size(px(8.))
                            .opacity(1. - t)
                            .bg(if i % 3 == 0 { rgb(0x5ea836).into() } else { p.accents[(i % 2) + 1] }),
                    );
                }
                el
            })
            .into_any_element()
    }
}

/// Colours the decorations draw with, from the active theme.
fn palette(spec: &crate::themes::ThemeSpec, cx: &App) -> anim::Palette {
    let theme = cx.theme();
    let pick = |i: usize| crate::themes::hex(&spec.speakers[i % spec.speakers.len()]);
    anim::Palette {
        bg: theme.background,
        fg: theme.foreground,
        primary: theme.primary,
        muted: theme.muted_foreground,
        surface: theme.popover,
        accents: [pick(0), pick(1), pick(2), pick(3)],
    }
}
