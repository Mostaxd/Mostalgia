//! AI connections: the providers Mostalgia talks to for summaries, "ask
//! your meetings" and (optionally) cloud transcription. Claude speaks the
//! Anthropic Messages API; everything else speaks the OpenAI-compatible
//! API (OpenAI, DeepSeek, Ollama, LiteLLM, LM Studio, vLLM, OpenRouter,
//! Groq, Together, or any custom endpoint).
//!
//! Connections live in the config; their API keys live in the system
//! keyring. Nothing here ever switches provider on its own: a failing
//! connection is reported with its name and the reason.

use std::net::IpAddr;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::i18n::{tr, trf};

/// Which wire protocol a connection speaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Api {
    Anthropic,
    #[default]
    OpenAi,
}

/// A configured provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    /// Stable id, used in the config and for the keyring entry.
    pub id: String,
    /// Shown on the card ("Ollama", "Firmen-LiteLLM").
    pub name: String,
    /// Preset it was created from ("openai", "ollama", …, "custom").
    pub preset: String,
    pub api: Api,
    /// API root including the version, e.g. `http://localhost:11434/v1`.
    pub base_url: String,
    /// Model ids typed in by hand, for servers without a `/models` list.
    pub models: Vec<String>,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            preset: "custom".into(),
            api: Api::OpenAi,
            base_url: String::new(),
            models: Vec::new(),
        }
    }
}

/// A speech-to-text model a preset offers, with its list price.
#[derive(Clone, Copy, Debug)]
pub struct SttModel {
    pub id: &'static str,
    /// US dollars per hour of audio (list price; check the provider).
    pub usd_per_hour: f64,
    /// Whether it returns segment timestamps (`verbose_json`).
    pub timestamps: bool,
}

/// A template for a new connection.
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub api: Api,
    pub base_url: &'static str,
    /// Whether the provider requires an API key.
    pub needs_key: bool,
    /// Environment variable checked before the keyring.
    pub env_var: Option<&'static str>,
    /// Default model for summaries.
    pub default_model: &'static str,
    /// Cloud transcription models (empty: no speech-to-text).
    pub stt: &'static [SttModel],
    /// Where to get a key.
    pub key_url: &'static str,
}

const OPENAI_STT: &[SttModel] = &[
    SttModel { id: "gpt-4o-transcribe", usd_per_hour: 0.36, timestamps: false },
    SttModel { id: "gpt-4o-mini-transcribe", usd_per_hour: 0.18, timestamps: false },
    SttModel { id: "whisper-1", usd_per_hour: 0.36, timestamps: true },
];
const GROQ_STT: &[SttModel] = &[
    SttModel { id: "whisper-large-v3-turbo", usd_per_hour: 0.04, timestamps: true },
    SttModel { id: "whisper-large-v3", usd_per_hour: 0.111, timestamps: true },
];
/// Self-hosted Whisper servers (faster-whisper-server, LocalAI, vLLM …).
const GENERIC_STT: &[SttModel] = &[SttModel { id: "whisper-1", usd_per_hour: 0.0, timestamps: true }];

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "claude",
        name: "Claude",
        api: Api::Anthropic,
        base_url: "https://api.anthropic.com/v1",
        needs_key: true,
        env_var: Some("ANTHROPIC_API_KEY"),
        default_model: "claude-opus-5-5",
        stt: &[],
        key_url: "https://console.anthropic.com/settings/keys",
    },
    Preset {
        id: "openai",
        name: "OpenAI",
        api: Api::OpenAi,
        base_url: "https://api.openai.com/v1",
        needs_key: true,
        env_var: Some("OPENAI_API_KEY"),
        default_model: "gpt-5",
        stt: OPENAI_STT,
        key_url: "https://platform.openai.com/api-keys",
    },
    Preset {
        id: "deepseek",
        name: "DeepSeek",
        api: Api::OpenAi,
        base_url: "https://api.deepseek.com/v1",
        needs_key: true,
        env_var: Some("DEEPSEEK_API_KEY"),
        default_model: "deepseek-chat",
        stt: &[],
        key_url: "https://platform.deepseek.com/api_keys",
    },
    Preset {
        id: "ollama",
        name: "Ollama",
        api: Api::OpenAi,
        base_url: "http://localhost:11434/v1",
        needs_key: false,
        env_var: None,
        default_model: "",
        stt: &[],
        key_url: "",
    },
    Preset {
        id: "litellm",
        name: "LiteLLM",
        api: Api::OpenAi,
        base_url: "http://localhost:4000/v1",
        needs_key: false,
        env_var: None,
        default_model: "",
        stt: GENERIC_STT,
        key_url: "",
    },
    Preset {
        id: "lmstudio",
        name: "LM Studio",
        api: Api::OpenAi,
        base_url: "http://localhost:1234/v1",
        needs_key: false,
        env_var: None,
        default_model: "",
        stt: &[],
        key_url: "",
    },
    Preset {
        id: "vllm",
        name: "vLLM",
        api: Api::OpenAi,
        base_url: "http://localhost:8000/v1",
        needs_key: false,
        env_var: None,
        default_model: "",
        stt: GENERIC_STT,
        key_url: "",
    },
    Preset {
        id: "openrouter",
        name: "OpenRouter",
        api: Api::OpenAi,
        base_url: "https://openrouter.ai/api/v1",
        needs_key: true,
        env_var: Some("OPENROUTER_API_KEY"),
        default_model: "",
        stt: &[],
        key_url: "https://openrouter.ai/keys",
    },
    Preset {
        id: "groq",
        name: "Groq",
        api: Api::OpenAi,
        base_url: "https://api.groq.com/openai/v1",
        needs_key: true,
        env_var: Some("GROQ_API_KEY"),
        default_model: "",
        stt: GROQ_STT,
        key_url: "https://console.groq.com/keys",
    },
    Preset {
        id: "together",
        name: "Together",
        api: Api::OpenAi,
        base_url: "https://api.together.xyz/v1",
        needs_key: true,
        env_var: Some("TOGETHER_API_KEY"),
        default_model: "",
        stt: &[],
        key_url: "https://api.together.xyz/settings/api-keys",
    },
    Preset {
        id: "custom",
        name: "Eigener Endpunkt",
        api: Api::OpenAi,
        base_url: "",
        needs_key: false,
        env_var: None,
        default_model: "",
        stt: GENERIC_STT,
        key_url: "",
    },
];

pub fn preset(id: &str) -> &'static Preset {
    PRESETS.iter().find(|p| p.id == id).unwrap_or(PRESETS.last().expect("custom preset"))
}

/// Connections a fresh config starts with (shown as "not set up" until a
/// key is saved).
pub fn default_connections() -> Vec<Connection> {
    ["claude", "openai", "deepseek"].iter().map(|id| Connection::from_preset(preset(id), id)).collect()
}

const KEYRING_SERVICE: &str = "mostalgia";

impl Connection {
    pub fn from_preset(p: &Preset, id: &str) -> Self {
        Self {
            id: id.into(),
            name: tr(p.name).into(),
            preset: p.id.into(),
            api: p.api,
            base_url: p.base_url.into(),
            models: Vec::new(),
        }
    }

    pub fn preset(&self) -> &'static Preset {
        preset(&self.preset)
    }

    /// The base URL without a trailing slash.
    pub fn base(&self) -> &str {
        self.base_url.trim().trim_end_matches('/')
    }

    /// Whether the endpoint runs on this machine or the local network, so
    /// nothing sent to it leaves the user's own hardware.
    pub fn is_local(&self) -> bool {
        is_local_url(self.base())
    }

    pub fn needs_key(&self) -> bool {
        self.preset().needs_key
    }

    /// Cloud transcription models this connection offers.
    pub fn stt_models(&self) -> &'static [SttModel] {
        self.preset().stt
    }

    fn keyring_user(&self) -> String {
        match self.id.as_str() {
            "claude" => "anthropic-api-key".into(),
            "deepseek" => "deepseek-api-key".into(),
            id => format!("{id}-api-key"),
        }
    }

    /// The env var that holds this connection's key (only for the built-in
    /// connection of a preset, so two Groq connections don't share one).
    fn env_var(&self) -> Option<&'static str> {
        (self.id == self.preset).then(|| self.preset().env_var).flatten()
    }

    /// The API key: its environment variable, else the keyring entry.
    pub fn api_key(&self) -> Result<Option<String>> {
        if let Some(key) = self.env_var().and_then(|v| std::env::var(v).ok()).filter(|k| !k.trim().is_empty()) {
            return Ok(Some(key.trim().to_string()));
        }
        let entry = keyring::Entry::new(KEYRING_SERVICE, &self.keyring_user()).context(tr("Schlüsselbund nicht erreichbar"))?;
        match entry.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context(tr("API-Schlüssel konnte nicht aus dem Schlüsselbund gelesen werden")),
        }
    }

    /// Whether the key comes from the environment (rather than the keyring).
    pub fn key_from_env(&self) -> bool {
        self.env_var().and_then(|v| std::env::var(v).ok()).is_some_and(|k| !k.trim().is_empty())
    }

    pub fn save_api_key(&self, key: &str) -> Result<()> {
        keyring::Entry::new(KEYRING_SERVICE, &self.keyring_user())
            .and_then(|e| e.set_password(key.trim()))
            .context(tr("API-Schlüssel konnte nicht im Schlüsselbund gespeichert werden"))
    }

    pub fn delete_api_key(&self) -> Result<()> {
        match keyring::Entry::new(KEYRING_SERVICE, &self.keyring_user()).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).context(tr("API-Schlüssel konnte nicht entfernt werden")),
        }
    }

    /// The key when one is needed: an error naming the connection if a
    /// required key is missing; `None` for keyless local servers.
    pub fn require_key(&self) -> Result<Option<String>> {
        let key = self.api_key()?;
        if key.is_none() && self.needs_key() {
            anyhow::bail!("{}", trf("{}: kein API-Schlüssel hinterlegt (Einstellungen → KI)", &[&self.name]));
        }
        Ok(key)
    }

    /// Set up enough to be used: has a key (or doesn't need one) and a URL.
    pub fn is_configured(&self) -> bool {
        !self.base().is_empty() && (!self.needs_key() || matches!(self.api_key(), Ok(Some(_))))
    }
}

/// The privacy line for a local endpoint: this machine, or the local network.
pub fn local_label(url: &str) -> &'static str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or_default();
    let this_machine = rest.starts_with("[::1]") || host == "localhost" || host.ends_with(".localhost") || host.starts_with("127.");
    if this_machine { tr("Lokal · nichts verlässt den Rechner") } else { tr("Lokal · bleibt in deinem Netzwerk") }
}

/// Whether `url`'s host is loopback, private (RFC 1918 / ULA), link-local
/// or a `.local`/`.lan`/`.internal`/`.home.arpa` name.
pub fn is_local_url(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    }
    .to_ascii_lowercase();
    if host == "localhost" || [".localhost", ".local", ".lan", ".internal", ".home.arpa"].iter().any(|s| host.ends_with(s)) {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80,
        Err(_) => false,
    }
}

/// Looks up a connection by id.
pub fn find<'a>(connections: &'a [Connection], id: &str) -> Option<&'a Connection> {
    connections.iter().find(|c| c.id == id)
}

/// A fresh id for a new connection from `preset` (the preset's id if free).
pub fn new_id(connections: &[Connection], preset: &str) -> String {
    if find(connections, preset).is_none() {
        return preset.to_string();
    }
    (2..).map(|n| format!("{preset}-{n}")).find(|id| find(connections, id).is_none()).expect("free id")
}

pub(crate) fn http(timeout_secs: u64) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?)
}

/// Turns a transport error into "Ollama: not reachable at http://…".
pub(crate) fn unreachable(conn: &Connection, e: reqwest::Error) -> anyhow::Error {
    if e.is_timeout() {
        anyhow::anyhow!("{}", trf("{}: Zeitüberschreitung bei {}", &[&conn.name, &conn.base()]))
    } else if e.is_connect() {
        anyhow::anyhow!("{}", trf("{}: nicht erreichbar unter {} — läuft der Server?", &[&conn.name, &conn.base()]))
    } else {
        anyhow::anyhow!("{}", trf("{}: Verbindungsfehler: {}", &[&conn.name, &e]))
    }
}

/// Turns an error response into "OpenAI: API key rejected (401)".
pub(crate) fn api_error(conn: &Connection, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let value: Value = serde_json::from_str(body).unwrap_or_default();
    let message = value["error"]["message"]
        .as_str()
        .or_else(|| value["error"].as_str())
        .or_else(|| value["message"].as_str())
        .or_else(|| value["detail"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(200).collect::<String>());
    let reason = match status.as_u16() {
        401 | 403 => trf("API-Schlüssel abgelehnt ({}): {}", &[&status.as_u16(), &message]),
        404 => trf("nicht gefunden ({}): {}", &[&status.as_u16(), &message]),
        429 => trf("Limit erreicht ({}): {}", &[&status.as_u16(), &message]),
        _ => trf("Fehler {}: {}", &[&status.as_u16(), &message]),
    };
    anyhow::anyhow!("{}: {}", conn.name, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_urls() {
        for url in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:4000",
            "http://192.168.1.20:1234/v1",
            "http://10.0.0.5/v1",
            "http://[::1]:8000/v1",
            "http://gpu-box.local:8000/v1",
            "http://user:pw@172.20.0.2:4000/v1",
        ] {
            assert!(is_local_url(url), "{url}");
        }
        for url in ["https://api.openai.com/v1", "https://openrouter.ai/api/v1", "http://8.8.8.8/v1", "http://172.32.0.1/v1"] {
            assert!(!is_local_url(url), "{url}");
        }
    }

    #[test]
    fn ids_stay_unique() {
        let list = default_connections();
        assert_eq!(new_id(&list, "ollama"), "ollama");
        assert_eq!(new_id(&list, "openai"), "openai-2");
    }
}
