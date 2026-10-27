use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::vad;

/// User settings shared by the app and the CLI, stored as JSON in
/// `$XDG_CONFIG_HOME/mostalgia/config.json`. Missing fields take defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
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
    /// Summary provider.
    pub summary_provider: crate::summary::Provider,
    /// Model for summaries (an id of the chosen provider).
    pub summary_model: String,
    /// Thinking level for summaries ("low" … "max"), where the model supports it.
    pub summary_effort: String,
    /// Summarize automatically after each recording (needs an API key).
    pub auto_summary: bool,
    /// App theme id ("system" follows GNOME's light/dark setting).
    pub theme: String,
    /// UI font family ("" = the theme's own font).
    pub font: String,
    /// Fewer and shorter animations.
    pub reduce_motion: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mic_device: None,
            system_device: None,
            record_mic: true,
            record_system: true,
            language: "de".into(),
            whisper_model: None,
            vad_threshold: vad::DEFAULT_THRESHOLD,
            speaker_labels: true,
            diarize: false,
            denoise: false,
            interim_text: true,
            diarize_threshold: 0.5,
            settings_category: String::new(),
            summary_provider: crate::summary::Provider::Claude,
            summary_model: crate::summary::DEFAULT_MODEL.into(),
            summary_effort: crate::summary::DEFAULT_EFFORT.into(),
            auto_summary: true,
            theme: "system".into(),
            font: String::new(),
            reduce_motion: false,
        }
    }
}

impl Config {
    /// The configured summary request (provider, model, thinking level).
    pub fn summary_request(&self) -> crate::summary::SummaryRequest {
        let models = crate::summary::fallback_models(self.summary_provider);
        let supports_effort = models
            .iter()
            .find(|m| m.id == self.summary_model)
            .map_or(self.summary_provider == crate::summary::Provider::Claude, |m| !m.efforts.is_empty());
        crate::summary::SummaryRequest {
            provider: self.summary_provider,
            model: self.summary_model.clone(),
            effort: supports_effort.then(|| self.summary_effort.clone()),
        }
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
