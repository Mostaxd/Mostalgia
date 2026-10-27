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
    /// Show Mori, the note-taking companion, in the window.
    pub companion: bool,
    /// Mori's size (1.0 ≈ 220 px tall).
    pub companion_scale: f32,
    /// Ask for new live points after this much new speech (0 = off).
    pub companion_points_interval_secs: u32,
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
}

/// Which speech-to-text engine to use: local Whisper (`connection` empty)
/// or a cloud model on one of the configured connections.
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
    /// Transcription local, summaries/answers from a cloud model.
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
            companion: false,
            companion_scale: 1.0,
            companion_points_interval_secs: 90,
            companion_pos: None,
            companion_clear_button: true,
            companion_desktop: false,
            companion_desktop_pos: None,
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
        let ai_local = [self.summary_provider.as_str(), self.ask_provider.as_str()]
            .iter()
            .filter(|id| !id.is_empty())
            .all(|id| self.connection(id).is_some_and(|c| c.is_local()));
        if ai_local { Mode::Local } else { Mode::Hybrid }
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
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
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
