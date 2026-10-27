//! Meeting summaries from an LLM provider: Claude (Anthropic Messages API)
//! or DeepSeek (OpenAI-compatible Chat Completions). Raw HTTP, blocking.

use std::io::{BufRead, BufReader};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::chunker::format_ts;
use crate::i18n::{tr, trf};
use crate::store::Meeting;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Claude,
    DeepSeek,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Claude, Provider::DeepSeek];

    pub fn id(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::DeepSeek => "deepseek",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::DeepSeek => "DeepSeek",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id().eq_ignore_ascii_case(s))
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Claude => DEFAULT_MODEL,
            Provider::DeepSeek => "deepseek-chat",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Provider::Claude => "ANTHROPIC_API_KEY",
            Provider::DeepSeek => "DEEPSEEK_API_KEY",
        }
    }

    fn keyring_user(self) -> &'static str {
        match self {
            Provider::Claude => "anthropic-api-key",
            Provider::DeepSeek => "deepseek-api-key",
        }
    }
}

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Thinking level for summaries unless configured otherwise.
pub const DEFAULT_EFFORT: &str = "high";
/// Effort levels from least to most thinking.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION: &str = "2026-10-21";
const DEEPSEEK_URL: &str = "https://api.deepseek.com";
const KEYRING_SERVICE: &str = "mostalgia";

/// Claude models that accept the server-side refusal fallback.
const FALLBACK_MODELS: [&str; 4] = ["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

const SYSTEM_PROMPT: &str = "\
You summarize meeting transcripts for the person who recorded them.

The transcript comes from automatic speech recognition, so expect some \
misheard words; silently correct obvious ones from context. Lines are \
labelled by speaker: \"Ich\" is the person who recorded the meeting \
(their microphone), \"Andere\" is everyone else on the call (possibly \
several people). Other labels are named speakers. When people are named \
in the conversation, use their names.

Write the summary in the language of the transcript (German unless the \
transcript is clearly in another language), as Markdown in exactly this \
shape, translating the headings if the transcript isn't German:

# <a short, specific title for the meeting, at most 8 words>

## Kurzfassung
<2-4 sentences: what the meeting was about and its outcome>

## Kernpunkte
- <the important points discussed, one per bullet>

## Entscheidungen
- <decisions that were made; write \"Keine\" if none>

## Aufgaben
- [ ] <task> — **Verantwortlich:** <owner, or \"offen\" if nobody took it> — **Frist:** <deadline, only if one was mentioned>

## Offene Fragen
- <open questions; omit this section if there are none>

Only include what the transcript supports. Never invent owners, deadlines \
or decisions. Output only the Markdown.";

/// A model a provider offers.
#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    /// Supported thinking levels (subset of [`EFFORTS`]); empty when the
    /// model has no effort control.
    pub efforts: Vec<String>,
}

/// What to summarize with.
#[derive(Clone, Debug)]
pub struct SummaryRequest {
    pub provider: Provider,
    pub model: String,
    /// Thinking level; ignored where the model has no effort control.
    pub effort: Option<String>,
}

/// A generated summary: the Markdown and the title from its first heading.
pub struct GeneratedSummary {
    pub title: Option<String>,
    pub markdown: String,
    pub provider: Provider,
    pub model: String,
    pub effort: Option<String>,
}

/// The API key for a provider: its environment variable, else the one
/// saved in the system keyring with [`save_api_key`].
pub fn api_key(provider: Provider) -> Result<Option<String>> {
    if let Ok(key) = std::env::var(provider.env_var()) {
        if !key.trim().is_empty() {
            return Ok(Some(key.trim().to_string()));
        }
    }
    let entry = keyring::Entry::new(KEYRING_SERVICE, provider.keyring_user()).context(tr("Schlüsselbund nicht erreichbar"))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).context(tr("API-Schlüssel konnte nicht aus dem Schlüsselbund gelesen werden")),
    }
}

/// Whether the key comes from the environment (rather than the keyring).
pub fn key_from_env(provider: Provider) -> bool {
    std::env::var(provider.env_var()).is_ok_and(|k| !k.trim().is_empty())
}

/// Stores a provider's API key in the system keyring (GNOME Keyring).
pub fn save_api_key(provider: Provider, key: &str) -> Result<()> {
    keyring::Entry::new(KEYRING_SERVICE, provider.keyring_user())
        .and_then(|e| e.set_password(key.trim()))
        .context(tr("API-Schlüssel konnte nicht im Schlüsselbund gespeichert werden"))
}

/// Built-in model list, used until (or if) the live list can't be fetched.
pub fn fallback_models(provider: Provider) -> Vec<ModelInfo> {
    let all: Vec<String> = EFFORTS.iter().map(|s| s.to_string()).collect();
    let m = |id: &str, name: &str, efforts: &Vec<String>| ModelInfo {
        id: id.into(),
        display_name: name.into(),
        efforts: efforts.clone(),
    };
    match provider {
        Provider::Claude => vec![
            m("claude-opus-5-5", "Claude Opus 5.5", &all),
            m("claude-fable-5-1", "Claude Fable 5.1", &all),
            m("claude-sonnet-5-5", "Claude Sonnet 5.5", &all),
            m("claude-haiku-5-5", "Claude Haiku 5.5", &all),
            m("claude-opus-5", "Claude Opus 5", &all),
        ],
        Provider::DeepSeek => vec![
            m("deepseek-chat", "DeepSeek Chat", &Vec::new()),
            m("deepseek-reasoner", "DeepSeek Reasoner", &Vec::new()),
        ],
    }
}

fn client(timeout_secs: u64) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()?)
}

/// The provider's current model list (live).
pub fn list_models(provider: Provider, key: &str) -> Result<Vec<ModelInfo>> {
    let value: Value = match provider {
        Provider::Claude => client(20)?
            .get(format!("{ANTHROPIC_URL}/models?limit=100"))
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()?
            .error_for_status()?
            .json()?,
        Provider::DeepSeek => client(20)?
            .get(format!("{DEEPSEEK_URL}/models"))
            .bearer_auth(key)
            .send()?
            .error_for_status()?
            .json()?,
    };
    let models = value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let efforts = EFFORTS
                .iter()
                .filter(|e| m["capabilities"]["effort"][**e]["supported"] == true)
                .map(|e| e.to_string())
                .collect();
            Some(ModelInfo {
                display_name: m["display_name"].as_str().unwrap_or(&id).to_string(),
                id,
                efforts,
            })
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(!models.is_empty(), "{}", tr("Der Anbieter hat keine Modelle geliefert"));
    Ok(models)
}

/// Renders a meeting's transcript the way the summary prompt expects.
pub fn transcript_text(meeting: &Meeting) -> String {
    let mut out = String::new();
    for s in &meeting.segments {
        out.push_str(&format!("[{}] {}: {}\n", format_ts(s.start_secs), s.speaker_label(), s.text));
    }
    out
}

fn user_message(meeting: &Meeting) -> Result<String> {
    let transcript = transcript_text(meeting);
    anyhow::ensure!(!transcript.is_empty(), "{}", tr("Die Besprechung hat kein Transkript zum Zusammenfassen"));
    Ok(format!(
        "Meeting on {}, {} min.\n\n<transcript>\n{}</transcript>",
        meeting.started_at.format("%d.%m.%Y %H:%M"),
        (meeting.duration_secs / 60.0).round(),
        transcript
    ))
}

pub fn summarize(req: &SummaryRequest, key: &str, meeting: &Meeting) -> Result<GeneratedSummary> {
    let user = user_message(meeting)?;
    let (markdown, model, effort) = match req.provider {
        Provider::Claude => claude(req, key, &user)?,
        Provider::DeepSeek => deepseek(req, key, &user)?,
    };
    let markdown = markdown.trim().to_string();
    anyhow::ensure!(!markdown.is_empty(), "{}", tr("Das Modell hat eine leere Zusammenfassung geliefert"));
    let title = markdown
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    Ok(GeneratedSummary {
        title,
        markdown,
        provider: req.provider,
        model,
        effort,
    })
}

/// Claude Messages API, streamed: higher thinking levels can take minutes,
/// and streaming keeps the connection alive and avoids request timeouts.
fn claude(req: &SummaryRequest, key: &str, user: &str) -> Result<(String, String, Option<String>)> {
    match claude_once(req, key, user) {
        // Older models have no effort control and reject the parameter.
        Err(e) if req.effort.is_some() && format!("{e:#}").contains("effort") => {
            let without = SummaryRequest { effort: None, ..req.clone() };
            claude_once(&without, key, user)
        }
        other => other,
    }
}

fn claude_once(req: &SummaryRequest, key: &str, user: &str) -> Result<(String, String, Option<String>)> {
    let mut body = json!({
        "model": req.model,
        "max_tokens": 64000,
        "stream": true,
        "system": SYSTEM_PROMPT,
        "messages": [{ "role": "user", "content": user }],
    });
    // Thinking stays at the model's adaptive default; only the effort is set.
    if let Some(effort) = &req.effort {
        body["output_config"] = json!({ "effort": effort });
    }
    let mut request = client(1800)?
        .post(format!("{ANTHROPIC_URL}/messages"))
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION);
    if FALLBACK_MODELS.contains(&req.model.as_str()) {
        // If a safety classifier declines, retry server-side on the
        // recommended fallback model instead of failing.
        body["fallbacks"] = json!("default");
        request = request.header("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let resp = request.json(&body).send().context(tr("Claude-API nicht erreichbar"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let value: Value = resp.json().unwrap_or_default();
        let msg = value["error"]["message"].as_str().unwrap_or(tr("unbekannt"));
        bail!("{}", trf("Claude-API-Fehler ({}): {}", &[&status, &msg]));
    }

    let mut text = String::new();
    let mut model = req.model.clone();
    let mut stop_reason = String::new();
    for line in BufReader::new(resp).lines() {
        let line = line.context(tr("Antwort der Claude-API konnte nicht gelesen werden"))?;
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
        match event["type"].as_str() {
            Some("message_start") => {
                if let Some(m) = event["message"]["model"].as_str() {
                    model = m.to_string();
                }
            }
            Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                text.push_str(event["delta"]["text"].as_str().unwrap_or_default());
            }
            Some("message_delta") => {
                if let Some(r) = event["delta"]["stop_reason"].as_str() {
                    stop_reason = r.to_string();
                }
            }
            Some("error") => bail!(
                "{}",
                trf("Claude-API-Fehler: {}", &[&event["error"]["message"].as_str().unwrap_or(tr("unbekannt"))])
            ),
            _ => {}
        }
    }
    match stop_reason.as_str() {
        "refusal" => bail!("{}", tr("Claude hat die Zusammenfassung abgelehnt (refusal); nichts gespeichert")),
        "max_tokens" => bail!("{}", tr("Die Zusammenfassung wurde abgeschnitten (max_tokens)")),
        _ => {}
    }
    Ok((text, model, req.effort.clone()))
}

/// DeepSeek's OpenAI-compatible Chat Completions API.
fn deepseek(req: &SummaryRequest, key: &str, user: &str) -> Result<(String, String, Option<String>)> {
    let body = json!({
        "model": req.model,
        "stream": false,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": user },
        ],
    });
    let resp = client(1800)?
        .post(format!("{DEEPSEEK_URL}/chat/completions"))
        .bearer_auth(key)
        .json(&body)
        .send()
        .context(tr("DeepSeek-API nicht erreichbar"))?;
    let status = resp.status();
    let value: Value = resp.json().context(tr("Antwort der DeepSeek-API konnte nicht gelesen werden"))?;
    if !status.is_success() {
        let msg = value["error"]["message"].as_str().unwrap_or(tr("unbekannt"));
        bail!("{}", trf("DeepSeek-API-Fehler ({}): {}", &[&status, &msg]));
    }
    let choice = &value["choices"][0];
    match choice["finish_reason"].as_str() {
        Some("length") => bail!("{}", tr("Die Zusammenfassung wurde abgeschnitten (length)")),
        Some("content_filter") => bail!("{}", tr("DeepSeek hat die Zusammenfassung abgelehnt; nichts gespeichert")),
        _ => {}
    }
    let text = choice["message"]["content"].as_str().unwrap_or_default().to_string();
    let model = value["model"].as_str().unwrap_or(&req.model).to_string();
    Ok((text, model, None))
}
