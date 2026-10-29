use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::vad;

/// User settings shared by the app and the CLI, stored as JSON in
/// `$XDG_CONFIG_HOME/mostalgia/config.json`. Missing fields take defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// App language: "en" (default), "de", or "system" (from the locale).
    pub ui_language: String,
    /// Pulse source for the mic (`None` = system default).
    pub mic_device: Option<String>,
    /// Pulse source for system audio (`None` = monitor of the default output).
    pub system_device: Option<String>,
    pub record_mic: bool,
    pub record_system: bool,
    /// "de", "en" or "auto".
    pub language: String,
    /// Languages spoken in the user's meetings (Whisper codes like "de");
    /// with "auto", detection only chooses among these. Empty = any.
    pub meeting_languages: Vec<String>,
    /// Whisper model file (`None` = large-v3-turbo in the usual places).
    pub whisper_model: Option<PathBuf>,
    /// Silero speech threshold, 0–1 (lower = more sensitive).
    pub vad_threshold: f32,
    /// Show "Ich" / "Andere" on live lines.
    pub speaker_labels: bool,
    /// Tell several remote speakers apart ("Sprecher 1", "Sprecher 2", …).
    pub diarize: bool,
    /// Noise suppression (RNNoise) on the mic for transcription.
    pub denoise: bool,
    /// Show provisional text while someone is still speaking.
    pub interim_text: bool,
    /// Speaker clustering threshold (higher = fewer speakers).
    pub diarize_threshold: f32,
    /// Last category open in the app's settings.
    pub settings_category: String,
    /// When a call app starts using the mic: "aus", "fragen" (notify and
    /// offer to record) or "automatisch" (start recording).
    pub call_autostart: String,
    /// Stop a recording once the call that started it ends.
    pub call_stop_auto: bool,
    /// Call apps seen so far (labels like "Zoom"), for the settings list.
    pub call_apps_seen: Vec<String>,
    /// Call apps that never trigger auto-start.
    pub call_apps_ignored: Vec<String>,
    /// AI connections (Claude, OpenAI, Ollama, custom endpoints …).
    pub connections: Vec<crate::connections::Connection>,
    /// Connection id used for summaries.
    pub summary_provider: String,
    /// Model for summaries (an id of the chosen provider).
    pub summary_model: String,
    /// Thinking level for summaries ("low" … "max"), where the model supports it.
    pub summary_effort: String,
    /// Language summaries are written in: "meeting" (the language spoken,
    /// as transcribed or detected), "app" (the UI language) or a code like "en".
    pub summary_language: String,
    /// Summarize automatically after each recording (needs an API key).
    pub auto_summary: bool,
    /// App theme id ("system" follows GNOME's light/dark setting).
    pub theme: String,
    /// UI font family ("" = the theme's own font).
    pub font: String,
    /// Fewer and shorter animations.
    pub reduce_motion: bool,
    /// Thinking level for "ask your meetings" answers (where supported).
    pub ask_effort: String,
    /// Connection for "ask your meetings" ("" = same as summaries).
    pub ask_provider: String,
    /// Model for "ask your meetings" ("" = the summary model).
    pub ask_model: String,
    /// Speech-to-text during a recording.
    pub live_engine: SttChoice,
    /// Speech-to-text for the final pass after a recording.
    pub final_engine: SttChoice,
    /// Embeddings for meaning search over meetings and documents: the
    /// local e5 model (`connection` empty) or a cloud model. Changing it
    /// re-embeds the library (see `ask::reembed`).
    pub embedding: SttChoice,
    /// Show the note-taking companion (Mori or one of the others).
    pub companion: bool,
    /// Which companion: "mori", "tsumugi" or "hoshi" (unknown → Mori).
    /// "off" in a hand-edited config turns her off (see [`Config::load`]).
    pub companion_character: String,
    /// Mori's size (1.0 ≈ 220 px tall).
    pub companion_scale: f32,
    /// Ask for new live points after this much new speech (0 = off). Only
    /// read while `companion_notes_mode` is empty (older configs).
    pub companion_points_interval_secs: u32,
    /// Mori's live notes: "live" (as it happens), "interval" (after
    /// `companion_notes_interval_secs` of new speech) or "off"; older
    /// configs: a number of seconds, or "" = from
    /// `companion_points_interval_secs`. See [`Config::notes_mode`].
    pub companion_notes_mode: String,
    /// As it happens: the least time between two checks (seconds, 5–60).
    pub companion_notes_gap_secs: f32,
    /// Interval mode: seconds of new speech per check (30–600; 0 = from
    /// the older settings, see [`Config::notes_interval`]).
    pub companion_notes_interval_secs: u32,
    /// What Mori should look out for in her notes ("" = nothing special);
    /// a recording can override it from her bubble.
    pub companion_notes_focus: String,
    /// How much her notes cover: "default" (decisions, action items, open
    /// questions, key figures) or "modern" (also the key points of talks,
    /// lectures and videos; more notes). See [`Config::notes_style`]; a
    /// recording or a regenerate can choose another.
    pub companion_notes_style: String,
    /// A note card shrinks to one quiet line this long after it appeared
    /// (seconds; 0 = never).
    pub companion_card_collapse_secs: f32,
    /// Cards that arrive together appear this far apart (seconds).
    pub companion_card_stagger_secs: f32,
    /// Connection for live notes ("" = the "ask" connection).
    pub companion_notes_provider: String,
    /// Model for live notes ("" = the "ask" model when it's the same
    /// connection, else that connection's default).
    pub companion_notes_model: String,
    /// Where Mori stands: her top-left as a fraction (0–1) of the room the
    /// window leaves her horizontally and vertically (`None` = top right).
    pub companion_pos: Option<(f32, f32)>,
    /// Show the "New chat" button (clear the chat, with undo) in Mori's bubble.
    pub companion_clear_button: bool,
    /// Mori stands on the desktop (a small always-on-top window of her own)
    /// instead of inside the Mostalgia window.
    pub companion_desktop: bool,
    /// On the desktop: her top-left in X11 root-window pixels (`None` =
    /// top right of the primary monitor).
    pub companion_desktop_pos: Option<(i32, i32)>,
    /// Mori's figure is hidden: only a small bar (record controls, notes,
    /// the chat) stays where she stood.
    pub companion_hidden: bool,

    // ── Companion bar indicator ──
    /// How her bar (figure hidden) shows that a recording runs:
    /// "sparkles" (default), "spinner" or "dot" (the red recording dot).
    pub companion_bar_indicator: String,

    // ── Text size ──
    /// App-wide text size (1.0 = 100 %), see [`Config::ui_scale`].
    pub ui_scale: f32,
    /// Extra size for the transcript and notes (1.0 = 100 %, on top of
    /// `ui_scale`), see [`Config::transcript_scale`].
    pub transcript_scale: f32,
    /// Live answer helper: when someone asks the user a question during a
    /// recording, the companion suggests an answer from past meetings
    /// and/or documents, with sources.
    pub companion_answers: bool,
    /// Where suggested answers come from: "meetings", "documents" or
    /// "both" (see [`Config::answer_sources`]).
    pub companion_answer_sources: String,

    // ── Audio cleanup ──
    /// Delete a meeting's audio this many days after it (0 = keep it);
    /// transcript, summary, notes and speakers stay. See `cleanup`.
    pub audio_keep_days: u32,
    /// The user agreed to deleting audio (asked once, the first time it
    /// would delete any).
    pub audio_cleanup_confirmed: bool,

    // ── Decision log ──
    /// The decision log (its page in the sidebar, decisions collected from
    /// each new summary). Off by default.
    pub decision_log: bool,
    /// Earlier meetings were taken into the log (offered once).
    pub decision_log_built: bool,

    // ── Live captions ──
    /// A small always-on-top window with the live transcript during a
    /// recording (to follow and flag lines over a call). Off by default.
    pub live_captions: bool,
    /// Its top-left (X11 root pixels) and size (logical px), once moved.
    pub live_captions_pos: Option<(i32, i32)>,
    pub live_captions_size: Option<(f32, f32)>,
    /// Its background opacity, 0.4–1.0.
    pub live_captions_opacity: f32,
}

/// When Mori looks for live notes during a recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotesMode {
    /// After each finished line (debounced): one small request at a time.
    Live,
    /// After this many seconds of new speech.
    Every(u32),
    Off,
}

impl NotesMode {
    /// The value stored in `companion_notes_mode` (the interval itself
    /// goes to `companion_notes_interval_secs`, see [`Config::set_notes_mode`]).
    pub fn as_config(self) -> &'static str {
        match self {
            NotesMode::Live => "live",
            NotesMode::Every(_) => "interval",
            NotesMode::Off => "off",
        }
    }
}

/// As it happens: the least seconds between two checks, settable…
pub const NOTES_GAP_RANGE: (f32, f32) = (5., 60.);
pub const NOTES_GAP_DEFAULT: f32 = 9.;
/// …and the interval mode's seconds of speech per check.
pub const NOTES_INTERVAL_RANGE: (u32, u32) = (30, 600);
pub const NOTES_INTERVAL_DEFAULT: u32 = 60;
/// Note cards: seconds until one shrinks to a line (0 = never), up to…
pub const CARD_COLLAPSE_DEFAULT: f32 = 7.;
pub const CARD_COLLAPSE_MAX: f32 = 120.;
/// …and seconds between cards that arrive together.
pub const CARD_STAGGER_DEFAULT: f32 = 3.5;
pub const CARD_STAGGER_MAX: f32 = 15.;

/// App-wide text size: allowed range and the steps Ctrl+= / Ctrl+-
/// go through.
pub const UI_SCALE_RANGE: (f32, f32) = (0.8, 1.5);
pub const UI_SCALE_STEPS: &[f32] = &[0.8, 0.9, 1.0, 1.1, 1.15, 1.2, 1.3, 1.4, 1.5];
/// Transcript text size (on top of the app's).
pub const TRANSCRIPT_SCALE_RANGE: (f32, f32) = (0.9, 1.5);
pub const TRANSCRIPT_SCALE_STEPS: &[f32] = &[0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5];

/// The next step up (`up`) or down from `value` in `steps`, or `None` at
/// the end.
pub fn scale_step(steps: &[f32], value: f32, up: bool) -> Option<f32> {
    if up {
        steps.iter().find(|s| **s > value + 0.001).copied()
    } else {
        steps.iter().rev().find(|s| **s < value - 0.001).copied()
    }
}

fn clamp_or(v: f32, (lo, hi): (f32, f32), default: f32) -> f32 {
    if v.is_finite() && v > 0. { v.clamp(lo, hi) } else { default }
}

/// Which engine to use for speech-to-text (or embeddings): the local model
/// (`connection` empty) or a cloud model on one of the configured connections.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttChoice {
    pub connection: String,
    pub model: String,
}

impl SttChoice {
    pub fn local() -> Self {
        Self::default()
    }

    pub fn is_local(&self) -> bool {
        self.connection.is_empty()
    }
}

/// Where the work happens, derived from the chosen engines and connections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Transcription and AI run on this machine (or the local network).
    Local,
    /// Transcription local, summaries/answers (or search embeddings) from
    /// a cloud model.
    Hybrid,
    /// Transcription in the cloud too.
    Cloud,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Local => crate::i18n::tr("Lokal"),
            Mode::Hybrid => crate::i18n::tr("Hybrid"),
            Mode::Cloud => crate::i18n::tr("Cloud"),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ui_language: "en".into(),
            mic_device: None,
            system_device: None,
            record_mic: true,
            record_system: true,
            language: "auto".into(),
            meeting_languages: Vec::new(),
            whisper_model: None,
            vad_threshold: vad::DEFAULT_THRESHOLD,
            speaker_labels: true,
            diarize: false,
            denoise: false,
            interim_text: true,
            diarize_threshold: 0.5,
            settings_category: String::new(),
            call_autostart: "fragen".into(),
            call_stop_auto: true,
            call_apps_seen: Vec::new(),
            call_apps_ignored: Vec::new(),
            connections: crate::connections::default_connections(),
            summary_provider: "claude".into(),
            summary_model: crate::summary::DEFAULT_MODEL.into(),
            summary_effort: crate::summary::DEFAULT_EFFORT.into(),
            summary_language: "meeting".into(),
            auto_summary: true,
            theme: "system".into(),
            font: String::new(),
            reduce_motion: false,
            ask_effort: "medium".into(),
            ask_provider: String::new(),
            ask_model: String::new(),
            live_engine: SttChoice::local(),
            final_engine: SttChoice::local(),
            embedding: SttChoice::local(),
            companion: false,
            companion_character: "mori".into(),
            companion_scale: 1.0,
            companion_points_interval_secs: 90,
            companion_notes_mode: String::new(),
            companion_notes_gap_secs: NOTES_GAP_DEFAULT,
            companion_notes_interval_secs: 0,
            companion_notes_focus: String::new(),
            companion_notes_style: crate::companion::NoteStyle::Default.id().into(),
            companion_card_collapse_secs: CARD_COLLAPSE_DEFAULT,
            companion_card_stagger_secs: CARD_STAGGER_DEFAULT,
            companion_notes_provider: String::new(),
            companion_notes_model: String::new(),
            companion_pos: None,
            companion_clear_button: true,
            companion_desktop: false,
            companion_desktop_pos: None,
            companion_hidden: false,
            companion_bar_indicator: "sparkles".into(),
            ui_scale: 1.0,
            transcript_scale: 1.0,
            companion_answers: false,
            companion_answer_sources: crate::assist::Sources::Both.id().into(),
            audio_keep_days: 7,
            audio_cleanup_confirmed: false,
            decision_log: false,
            decision_log_built: false,
            live_captions: false,
            live_captions_pos: None,
            live_captions_size: None,
            live_captions_opacity: 0.92,
        }
    }
}

impl Config {
    /// A connection by id.
    pub fn connection(&self, id: &str) -> Option<&crate::connections::Connection> {
        crate::connections::find(&self.connections, id)
    }

    /// The connection for summaries; an error naming the id if it's gone.
    pub fn summary_connection(&self) -> Result<crate::connections::Connection> {
        self.connection(&self.summary_provider)
            .cloned()
            .with_context(|| crate::i18n::trf("KI-Verbindung \"{}\" gibt es nicht mehr — in den Einstellungen wählen", &[&self.summary_provider]))
    }

    /// The configured summary request (connection, model, thinking level).
    pub fn summary_request(&self) -> Result<crate::summary::SummaryRequest> {
        let connection = self.summary_connection()?;
        let effort = effort_for(&connection, &self.summary_model, &self.summary_effort);
        Ok(crate::summary::SummaryRequest { connection, model: self.summary_model.clone(), language: self.summary_language_code(), effort })
    }

    /// Connection, model and thinking level for "ask your meetings".
    pub fn ask_request(&self) -> Result<crate::summary::SummaryRequest> {
        if self.ask_provider.is_empty() || self.ask_provider == self.summary_provider && self.ask_model.is_empty() {
            let mut req = self.summary_request()?;
            req.effort = effort_for(&req.connection, &req.model, &self.ask_effort);
            return Ok(req);
        }
        let connection = self
            .connection(&self.ask_provider)
            .cloned()
            .with_context(|| crate::i18n::trf("KI-Verbindung \"{}\" gibt es nicht mehr — in den Einstellungen wählen", &[&self.ask_provider]))?;
        let model = if self.ask_model.is_empty() { self.summary_model.clone() } else { self.ask_model.clone() };
        let effort = effort_for(&connection, &model, &self.ask_effort);
        Ok(crate::summary::SummaryRequest { connection, model, language: self.summary_language_code(), effort })
    }

    /// When Mori looks for live notes. Older configs: a number of seconds in
    /// `companion_notes_mode` (→ that interval), or only
    /// `companion_points_interval_secs`: 0 → off, the old default 90 s (61–119)
    /// → as it happens, anything else → that interval.
    pub fn notes_mode(&self) -> NotesMode {
        match self.companion_notes_mode.trim() {
            "live" => NotesMode::Live,
            "off" | "0" => NotesMode::Off,
            "interval" => NotesMode::Every(self.notes_interval()),
            s if s.parse::<u32>().is_ok() => NotesMode::Every(self.notes_interval()),
            _ => match self.companion_points_interval_secs {
                0 => NotesMode::Off,
                61..=119 => NotesMode::Live,
                _ => NotesMode::Every(self.notes_interval()),
            },
        }
    }

    /// The interval mode's seconds of speech per check (also remembered
    /// while another mode is chosen), from the newest setting that has one.
    pub fn notes_interval(&self) -> u32 {
        let (lo, hi) = NOTES_INTERVAL_RANGE;
        let legacy = self.companion_notes_mode.trim().parse::<u32>().ok().filter(|s| *s > 0);
        let old = Some(self.companion_points_interval_secs).filter(|s| *s > 0 && !(61..=119).contains(s));
        let secs = Some(self.companion_notes_interval_secs).filter(|s| *s > 0).or(legacy).or(old).unwrap_or(NOTES_INTERVAL_DEFAULT);
        secs.clamp(lo, hi)
    }

    /// Chooses the live-notes mode (an interval is remembered with it).
    pub fn set_notes_mode(&mut self, mode: NotesMode) {
        if let NotesMode::Every(secs) = mode {
            self.companion_notes_interval_secs = secs.clamp(NOTES_INTERVAL_RANGE.0, NOTES_INTERVAL_RANGE.1);
        } else if self.companion_notes_interval_secs == 0 {
            // Keep an older config's interval for when it's chosen again.
            self.companion_notes_interval_secs = self.notes_interval();
        }
        self.companion_notes_mode = mode.as_config().into();
    }

    /// As it happens: the least seconds between two checks.
    pub fn notes_gap(&self) -> f32 {
        clamp_or(self.companion_notes_gap_secs, NOTES_GAP_RANGE, NOTES_GAP_DEFAULT)
    }

    /// Seconds until a note card shrinks to one line (`None` = never).
    pub fn card_collapse(&self) -> Option<f32> {
        let s = self.companion_card_collapse_secs;
        if !s.is_finite() || s < 0. {
            return Some(CARD_COLLAPSE_DEFAULT);
        }
        (s > 0.).then(|| s.clamp(1., CARD_COLLAPSE_MAX))
    }

    /// App-wide text size, clamped to [`UI_SCALE_RANGE`] (broken values: 1).
    pub fn ui_scale(&self) -> f32 {
        clamp_or(self.ui_scale, UI_SCALE_RANGE, 1.)
    }

    /// Transcript text size, clamped to [`TRANSCRIPT_SCALE_RANGE`].
    pub fn transcript_scale(&self) -> f32 {
        clamp_or(self.transcript_scale, TRANSCRIPT_SCALE_RANGE, 1.)
    }

    /// Ctrl+= / Ctrl+- : one step bigger or smaller; Ctrl+0 (`None`): back
    /// to 100 %. Returns whether it changed.
    pub fn zoom(&mut self, up: Option<bool>) -> bool {
        let now = self.ui_scale();
        let next = match up {
            Some(up) => scale_step(UI_SCALE_STEPS, now, up).unwrap_or(now),
            None => 1.,
        };
        self.ui_scale = next;
        (next - now).abs() > 0.001
    }

    /// Seconds between cards that arrive together.
    pub fn card_stagger(&self) -> f32 {
        let s = self.companion_card_stagger_secs;
        if s.is_finite() && s >= 0. { s.min(CARD_STAGGER_MAX) } else { CARD_STAGGER_DEFAULT }
    }

    /// Connection and model for live notes (the thinking level is left to
    /// `llm::chat_quick`: as little as possible).
    pub fn notes_request(&self) -> Result<crate::summary::SummaryRequest> {
        let ask = self.ask_request();
        let mut req = if self.companion_notes_provider.is_empty() {
            ask?
        } else {
            let connection = self
                .connection(&self.companion_notes_provider)
                .cloned()
                .with_context(|| crate::i18n::trf("KI-Verbindung \"{}\" gibt es nicht mehr — in den Einstellungen wählen", &[&self.companion_notes_provider]))?;
            let model = if !self.companion_notes_model.is_empty() {
                self.companion_notes_model.clone()
            } else {
                match ask {
                    Ok(a) if a.connection.id == connection.id => a.model,
                    _ => {
                        let default = connection.preset().default_model;
                        if default.is_empty() { connection.models.first().cloned().unwrap_or_default() } else { default.to_string() }
                    }
                }
            };
            anyhow::ensure!(!model.is_empty(), "{}", crate::i18n::trf("{}: kein Modell für Live-Notizen gewählt", &[&connection.name]));
            crate::summary::SummaryRequest { connection, model, language: self.summary_language_code(), effort: None }
        };
        req.effort = None;
        Ok(req)
    }

    /// Where the live answer helper looks (unknown values: both).
    pub fn answer_sources(&self) -> crate::assist::Sources {
        crate::assist::Sources::parse(&self.companion_answer_sources).unwrap_or_default()
    }

    /// The fixed summary language, if one is set ("app" resolves to the UI language).
    pub fn summary_language_code(&self) -> Option<String> {
        match self.summary_language.as_str() {
            "" | "meeting" => None,
            "app" => Some(match crate::i18n::resolve(&self.ui_language) {
                crate::i18n::Lang::De => "de".into(),
                crate::i18n::Lang::En => "en".into(),
            }),
            code => Some(code.to_string()),
        }
    }

    /// Where the work happens with the current choices.
    pub fn mode(&self) -> Mode {
        let stt_cloud = |e: &SttChoice| !e.is_local() && self.connection(&e.connection).is_none_or(|c| !c.is_local());
        if stt_cloud(&self.live_engine) || stt_cloud(&self.final_engine) {
            return Mode::Cloud;
        }
        let ai_local = [self.summary_provider.as_str(), self.ask_provider.as_str(), self.embedding.connection.as_str()]
            .iter()
            .filter(|id| !id.is_empty())
            .all(|id| self.connection(id).is_some_and(|c| c.is_local()));
        if ai_local { Mode::Local } else { Mode::Hybrid }
    }

    /// Display name of the embedding choice: "multilingual-e5-small (lokal)"
    /// or "OpenAI · text-embedding-3-small".
    pub fn embedding_label(&self) -> String {
        if self.embedding.is_local() {
            return crate::i18n::tr("multilingual-e5-small (lokal)").to_string();
        }
        let name = self.connection(&self.embedding.connection).map_or(self.embedding.connection.as_str(), |c| c.name.as_str());
        format!("{name} · {}", self.embedding.model)
    }

    /// Display name of an engine choice: "Whisper (lokal)" or "OpenAI · gpt-4o-transcribe".
    pub fn engine_label(&self, choice: &SttChoice) -> String {
        if choice.is_local() {
            return crate::i18n::tr("Whisper large-v3-turbo (lokal)").to_string();
        }
        let name = self.connection(&choice.connection).map_or(choice.connection.as_str(), |c| c.name.as_str());
        format!("{name} · {}", choice.model)
    }

    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("mostalgia")
            .join("config.json")
    }

    /// Loads the config, falling back to defaults if it's missing or broken.
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str::<Self>(&s).ok())
            .map(Self::normalized)
            .unwrap_or_default()
    }

    /// `companion_character: "off"` is the same as `companion: false`.
    fn normalized(mut self) -> Self {
        if self.companion_character.trim() == "off" {
            self.companion = false;
            self.companion_character = crate::companion::Character::Mori.id().into();
        }
        self
    }

    /// The companion shown, `None` when she's turned off.
    pub fn character(&self) -> Option<crate::companion::Character> {
        (self.companion && self.companion_character.trim() != "off").then(|| self.companion_character())
    }

    /// The chosen companion (also while turned off: names in the settings).
    /// Her note style (unknown values: the default one).
    pub fn notes_style(&self) -> crate::companion::NoteStyle {
        crate::companion::NoteStyle::parse(&self.companion_notes_style).unwrap_or_default()
    }

    pub fn companion_character(&self) -> crate::companion::Character {
        crate::companion::Character::parse(&self.companion_character).unwrap_or(crate::companion::Character::Mori)
    }

    /// Picks a companion, or turns her off (`None`; the choice is kept for
    /// when she's turned on again).
    pub fn set_character(&mut self, character: Option<crate::companion::Character>) {
        self.companion = character.is_some();
        if let Some(c) = character {
            self.companion_character = c.id().into();
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("creating config directory")?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?).context("writing config")?;
        std::fs::rename(&tmp, &path).context("saving config")?;
        Ok(())
    }

    pub fn whisper_model_path(&self) -> PathBuf {
        self.whisper_model
            .clone()
            .unwrap_or_else(|| crate::paths::model(crate::paths::WHISPER_MODEL))
    }

    /// The VAD model next to the Whisper model, else in the usual places.
    pub fn vad_model_path(&self) -> PathBuf {
        let beside = self.whisper_model_path().with_file_name(crate::paths::VAD_MODEL);
        if beside.exists() {
            beside
        } else {
            crate::paths::model(crate::paths::VAD_MODEL)
        }
    }
}

/// The thinking level to send for `model` on `conn`, if it has effort control.
pub fn effort_for(conn: &crate::connections::Connection, model: &str, effort: &str) -> Option<String> {
    if effort.is_empty() {
        return None;
    }
    let supported = match conn.api {
        crate::connections::Api::Anthropic => crate::llm::fallback_models(conn)
            .iter()
            .find(|m| m.id == model)
            .is_none_or(|m| !m.efforts.is_empty()),
        crate::connections::Api::OpenAi => !crate::llm::openai_efforts(conn, model).is_empty(),
    };
    supported.then(|| effort.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::Character;

    #[test]
    fn text_scales_default_and_clamp() {
        let mut c = Config::default();
        assert_eq!((c.ui_scale(), c.transcript_scale()), (1., 1.));
        // Older configs without the fields get 100 %.
        let old: Config = serde_json::from_str(r#"{"theme":"system"}"#).unwrap();
        assert_eq!((old.ui_scale, old.transcript_scale), (1., 1.));
        c.ui_scale = 9.;
        c.transcript_scale = 0.1;
        assert_eq!((c.ui_scale(), c.transcript_scale()), (1.5, 0.9));
        for broken in [0., -1., f32::NAN, f32::INFINITY] {
            c.ui_scale = broken;
            c.transcript_scale = broken;
            assert_eq!((c.ui_scale(), c.transcript_scale()), (1., 1.));
        }
    }

    #[test]
    fn zoom_steps_and_resets() {
        let mut c = Config::default();
        assert!(c.zoom(Some(true)));
        assert_eq!(c.ui_scale, 1.1);
        c.ui_scale = 1.5;
        assert!(!c.zoom(Some(true)));
        assert_eq!(c.ui_scale, 1.5);
        assert!(c.zoom(Some(false)));
        assert_eq!(c.ui_scale, 1.4);
        // An odd value in between goes to the next step.
        c.ui_scale = 1.17;
        c.zoom(Some(false));
        assert_eq!(c.ui_scale, 1.15);
        c.ui_scale = 0.8;
        assert!(!c.zoom(Some(false)));
        assert!(c.zoom(None));
        assert_eq!(c.ui_scale, 1.);
        assert!(!c.zoom(None));
    }

    #[test]
    fn character_maps_off_and_unknown() {
        let mut c = Config::default();
        assert_eq!(c.character(), None);
        assert_eq!(c.companion_character(), Character::Mori);
        c.set_character(Some(Character::Hoshi));
        assert!(c.companion);
        assert_eq!((c.character(), c.companion_character.as_str()), (Some(Character::Hoshi), "hoshi"));
        c.set_character(None);
        assert!(!c.companion);
        // The choice stays for the next time she's turned on.
        assert_eq!((c.character(), c.companion_character()), (None, Character::Hoshi));
        let saved: Config = serde_json::from_str(r#"{"companion": true, "companion_character": "off"}"#).unwrap();
        let loaded = saved.normalized();
        assert_eq!((loaded.companion, loaded.character(), loaded.companion_character.as_str()), (false, None, "mori"));
        // Older configs (no field) keep Mori; unknown names fall back to her.
        let old: Config = serde_json::from_str(r#"{"companion": true}"#).unwrap();
        assert_eq!(old.character(), Some(Character::Mori));
        let odd = Config { companion: true, companion_character: "nobody".into(), ..Config::default() };
        assert_eq!(odd.character(), Some(Character::Mori));
    }

    #[test]
    fn cloud_embeddings_make_local_ai_hybrid() {
        let mut c = Config::default();
        c.connections.push(crate::connections::Connection::from_preset(crate::connections::preset("ollama"), "ollama"));
        c.summary_provider = "ollama".into();
        assert_eq!(c.mode(), Mode::Local);
        c.embedding = SttChoice { connection: "ollama".into(), model: "bge-m3".into() };
        assert_eq!(c.mode(), Mode::Local);
        c.embedding = SttChoice { connection: "openai".into(), model: "text-embedding-3-small".into() };
        assert_eq!(c.mode(), Mode::Hybrid);
        assert_eq!(c.embedding_label(), "OpenAI · text-embedding-3-small");
    }

    #[test]
    fn notes_mode_migrates_the_old_interval() {
        let with = |interval: u32, mode: &str| Config { companion_points_interval_secs: interval, companion_notes_mode: mode.into(), ..Config::default() };
        assert_eq!(Config::default().notes_mode(), NotesMode::Live);
        assert_eq!(with(0, "").notes_mode(), NotesMode::Off);
        assert_eq!(with(60, "").notes_mode(), NotesMode::Every(60));
        assert_eq!(with(45, "").notes_mode(), NotesMode::Every(45));
        assert_eq!(with(90, "").notes_mode(), NotesMode::Live);
        assert_eq!(with(120, "").notes_mode(), NotesMode::Every(120));
        assert_eq!(with(180, "").notes_mode(), NotesMode::Every(180));
        assert_eq!(with(5000, "").notes_mode(), NotesMode::Every(600));
        // The newer field wins once it's set; old numeric values still read.
        assert_eq!(with(60, "live").notes_mode(), NotesMode::Live);
        assert_eq!(with(90, "off").notes_mode(), NotesMode::Off);
        assert_eq!(with(90, "120").notes_mode(), NotesMode::Every(120));
        assert_eq!(with(90, "interval").notes_mode(), NotesMode::Every(60));
        // An old config file without the new fields (the user's: interval 60).
        let old: Config = serde_json::from_str(r#"{"companion_points_interval_secs": 60, "companion_notes_mode": ""}"#).unwrap();
        assert_eq!(old.notes_mode(), NotesMode::Every(60));
        assert_eq!((old.notes_gap(), old.card_collapse(), old.card_stagger()), (9., Some(7.), 3.5));
        assert_eq!(old.companion_notes_focus, "");
        assert_eq!((old.companion_notes_style.as_str(), old.notes_style()), ("default", crate::companion::NoteStyle::Default));
        assert_eq!(Config::default().notes_style(), crate::companion::NoteStyle::Default);
        // Choosing a mode remembers the interval across the others.
        let mut c = old.clone();
        c.set_notes_mode(NotesMode::Live);
        assert_eq!((c.notes_mode(), c.notes_interval()), (NotesMode::Live, 60));
        c.set_notes_mode(NotesMode::Every(c.notes_interval()));
        assert_eq!(c.notes_mode(), NotesMode::Every(60));
        c.set_notes_mode(NotesMode::Every(10));
        assert_eq!(c.notes_mode(), NotesMode::Every(30));
        c.set_notes_mode(NotesMode::Every(275));
        c.set_notes_mode(NotesMode::Off);
        assert_eq!(c.notes_mode(), NotesMode::Off);
        c.set_notes_mode(NotesMode::Every(c.notes_interval()));
        assert_eq!(c.notes_mode(), NotesMode::Every(275));
        let saved: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(saved.notes_mode(), NotesMode::Every(275));
        // Timing values are kept in range.
        let t = Config { companion_notes_gap_secs: 1., companion_card_collapse_secs: 0., companion_card_stagger_secs: 99., ..Config::default() };
        assert_eq!((t.notes_gap(), t.card_collapse(), t.card_stagger()), (5., None, CARD_STAGGER_MAX));
        let t = Config { companion_notes_gap_secs: 0., companion_card_collapse_secs: -1., companion_card_stagger_secs: 0., ..Config::default() };
        assert_eq!((t.notes_gap(), t.card_collapse(), t.card_stagger()), (9., Some(7.), 0.));
    }

    #[test]
    fn notes_request_defaults_to_the_ask_connection() {
        let mut c = Config { summary_provider: "deepseek".into(), summary_model: "deepseek-chat".into(), ..Config::default() };
        let r = c.notes_request().unwrap();
        assert_eq!((r.connection.id.as_str(), r.model.as_str(), r.effort), ("deepseek", "deepseek-chat", None));
        c.companion_notes_provider = "claude".into();
        let r = c.notes_request().unwrap();
        assert_eq!(r.connection.id, "claude");
        assert!(!r.model.is_empty());
        c.companion_notes_model = "claude-haiku-5-5".into();
        assert_eq!(c.notes_request().unwrap().model, "claude-haiku-5-5");
        c.companion_notes_provider = "gone".into();
        assert!(c.notes_request().is_err());
    }
}
