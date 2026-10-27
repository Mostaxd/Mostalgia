//! Talking to language models over a [`Connection`]: model lists, a
//! connection test, and streamed single-turn completions (summaries).
//! The tool-calling loop for "ask your meetings" lives in `ask::answer`.

use std::io::{BufRead, BufReader};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::connections::{Api, Connection, api_error, http, unreachable};
use crate::i18n::{tr, trf};

pub const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Effort levels from least to most thinking.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// A model a connection offers.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    /// Supported thinking levels (subset of [`EFFORTS`]); empty when the
    /// model has no effort control.
    pub efforts: Vec<String>,
}

impl ModelInfo {
    fn new(id: &str, name: &str, efforts: Vec<String>) -> Self {
        Self { id: id.into(), display_name: name.into(), efforts }
    }
}

fn all_efforts() -> Vec<String> {
    EFFORTS.iter().map(|s| s.to_string()).collect()
}

/// OpenAI reasoning models take `reasoning_effort` (low/medium/high);
/// other OpenAI-compatible models have no effort control.
pub fn openai_efforts(conn: &Connection, model: &str) -> Vec<String> {
    let m = model.rsplit('/').next().unwrap_or(model);
    let reasoning = ["o1", "o3", "o4", "gpt-5"].iter().any(|p| m.starts_with(p)) && !m.contains("chat");
    if reasoning && matches!(conn.preset.as_str(), "openai" | "openrouter" | "litellm" | "custom") {
        ["low", "medium", "high"].iter().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    }
}

/// Our effort level in OpenAI's vocabulary.
fn openai_effort(effort: &str) -> &'static str {
    match effort {
        "low" => "low",
        "medium" => "medium",
        _ => "high",
    }
}

/// Built-in model list, shown until (or if) the live list can't be fetched.
pub fn fallback_models(conn: &Connection) -> Vec<ModelInfo> {
    let mut out = match conn.preset.as_str() {
        "claude" => vec![
            ModelInfo::new("claude-opus-5-5", "Claude Opus 5.5", all_efforts()),
            ModelInfo::new("claude-fable-5-1", "Claude Fable 5.1", all_efforts()),
            ModelInfo::new("claude-sonnet-5-5", "Claude Sonnet 5.5", all_efforts()),
            ModelInfo::new("claude-haiku-5-5", "Claude Haiku 5.5", all_efforts()),
            ModelInfo::new("claude-opus-5", "Claude Opus 5", all_efforts()),
        ],
        "deepseek" => vec![
            ModelInfo::new("deepseek-chat", "DeepSeek Chat", Vec::new()),
            ModelInfo::new("deepseek-reasoner", "DeepSeek Reasoner", Vec::new()),
        ],
        "openai" => ["gpt-5", "gpt-5-mini", "gpt-5-nano", "gpt-4.1", "gpt-4.1-mini", "o4-mini"]
            .iter()
            .map(|id| ModelInfo::new(id, id, openai_efforts(conn, id)))
            .collect(),
        _ => Vec::new(),
    };
    for id in &conn.models {
        if !out.iter().any(|m| &m.id == id) {
            out.push(ModelInfo::new(id, id, openai_efforts(conn, id)));
        }
    }
    out
}

/// Whether a model id is for chat (not embeddings, speech, images …).
fn is_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !["embed", "whisper", "transcribe", "tts", "dall-e", "image", "moderation", "audio", "realtime", "rerank", "search"]
        .iter()
        .any(|w| id.contains(w))
}

/// The connection's current model list (live), with hand-typed models added.
pub fn list_models(conn: &Connection, key: Option<&str>) -> Result<Vec<ModelInfo>> {
    let mut request = match conn.api {
        Api::Anthropic => http(20)?
            .get(format!("{}/models?limit=100", conn.base()))
            .header("x-api-key", key.unwrap_or_default())
            .header("anthropic-version", ANTHROPIC_VERSION),
        Api::OpenAi => http(20)?.get(format!("{}/models", conn.base())),
    };
    if let (Api::OpenAi, Some(key)) = (conn.api, key) {
        request = request.bearer_auth(key);
    }
    let resp = request.send().map_err(|e| unreachable(conn, e))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(conn, status, &body));
    }
    let value: Value = serde_json::from_str(&body).with_context(|| trf("{}: unerwartete Antwort auf /models", &[&conn.name]))?;
    let mut models: Vec<ModelInfo> = value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let efforts = match conn.api {
                Api::Anthropic => EFFORTS
                    .iter()
                    .filter(|e| m["capabilities"]["effort"][**e]["supported"] == true)
                    .map(|e| e.to_string())
                    .collect(),
                Api::OpenAi => openai_efforts(conn, &id),
            };
            let name = m["display_name"].as_str().or_else(|| m["name"].as_str()).unwrap_or(&id).to_string();
            Some(ModelInfo { display_name: name, id, efforts })
        })
        .filter(|m| conn.api == Api::Anthropic || is_chat_model(&m.id))
        .collect();
    for id in &conn.models {
        if !models.iter().any(|m| &m.id == id) {
            models.push(ModelInfo::new(id, id, openai_efforts(conn, id)));
        }
    }
    anyhow::ensure!(!models.is_empty(), "{}: {}", conn.name, tr("Der Anbieter hat keine Modelle geliefert"));
    Ok(models)
}

/// The ids of all models a server lists, unfiltered (for speech-to-text
/// pickers on generic servers).
pub fn list_model_ids(conn: &Connection, key: Option<&str>) -> Result<Vec<String>> {
    let mut request = http(20)?.get(format!("{}/models", conn.base()));
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let resp = request.send().map_err(|e| unreachable(conn, e))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(conn, status, &body));
    }
    let value: Value = serde_json::from_str(&body).unwrap_or_default();
    Ok(value["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
}

/// Checks a connection end to end (key, URL, model list). Returns a short
/// success note ("12 Modelle").
pub fn test(conn: &Connection) -> Result<String> {
    anyhow::ensure!(!conn.base().is_empty(), "{}", trf("{}: keine URL angegeben", &[&conn.name]));
    let key = conn.require_key()?;
    match list_models(conn, key.as_deref()) {
        Ok(models) => Ok(trf("{} Modelle verfügbar", &[&models.len()])),
        // Some proxies don't list models; hand-typed ones still work.
        Err(e) if !conn.models.is_empty() && format!("{e:#}").contains("404") => {
            Ok(trf("Erreichbar · {} eigene Modelle", &[&conn.models.len()]))
        }
        Err(e) => Err(e),
    }
}

/// A finished completion.
pub struct Completion {
    pub text: String,
    /// The model that actually answered (as reported by the server).
    pub model: String,
    /// The thinking level that was applied, if any.
    pub effort: Option<String>,
}

/// One system + user turn, streamed so long thinking can't time out.
/// `effort` is sent only where the model is known to support it; if the
/// server still rejects it, that is an error (never a silent retry without).
pub fn complete(conn: &Connection, model: &str, effort: Option<&str>, system: &str, user: &str) -> Result<Completion> {
    let key = conn.require_key()?;
    let key = key.as_deref();
    let effort = match conn.api {
        Api::Anthropic => effort,
        Api::OpenAi => effort.filter(|_| !openai_efforts(conn, model).is_empty()),
    };
    let result = match conn.api {
        Api::Anthropic => claude(conn, key.unwrap_or_default(), model, effort, system, user),
        Api::OpenAi => openai(conn, key, model, effort, system, user),
    };
    result.map_err(|e| effort_hint(e, effort.is_some()))
}

/// Adds a pointer to the setting when a server rejects the thinking level.
pub(crate) fn effort_hint(e: anyhow::Error, effort_sent: bool) -> anyhow::Error {
    if effort_sent && format!("{e:#}").contains("effort") {
        e.context(tr("Das Modell unterstützt die gewählte Denktiefe nicht — in den Einstellungen ändern"))
    } else {
        e
    }
}

fn claude(conn: &Connection, key: &str, model: &str, effort: Option<&str>, system: &str, user: &str) -> Result<Completion> {
    let mut body = json!({
        "model": model,
        "max_tokens": 64000,
        "stream": true,
        "system": system,
        "messages": [{ "role": "user", "content": user }],
    });
    // Thinking stays at the model's adaptive default; only the effort is set.
    if let Some(effort) = effort {
        body["output_config"] = json!({ "effort": effort });
    }
    let request = http(1800)?
        .post(format!("{}/messages", conn.base()))
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION);
    let resp = request.json(&body).send().map_err(|e| unreachable(conn, e))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(api_error(conn, status, &resp.text().unwrap_or_default()));
    }

    let mut text = String::new();
    let mut served = model.to_string();
    let mut stop_reason = String::new();
    for line in BufReader::new(resp).lines() {
        let line = line.with_context(|| trf("{}: Antwort konnte nicht gelesen werden", &[&conn.name]))?;
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
        match event["type"].as_str() {
            Some("message_start") => {
                if let Some(m) = event["message"]["model"].as_str() {
                    served = m.to_string();
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
            Some("error") => bail!("{}: {}", conn.name, event["error"]["message"].as_str().unwrap_or(tr("unbekannt"))),
            _ => {}
        }
    }
    match stop_reason.as_str() {
        "refusal" => bail!("{}", trf("{} hat die Anfrage abgelehnt (refusal); nichts gespeichert", &[&conn.name])),
        "max_tokens" => bail!("{}", trf("{}: Antwort abgeschnitten (max_tokens)", &[&conn.name])),
        _ => {}
    }
    Ok(Completion { text, model: served, effort: effort.map(str::to_string) })
}

fn openai(conn: &Connection, key: Option<&str>, model: &str, effort: Option<&str>, system: &str, user: &str) -> Result<Completion> {
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(openai_effort(effort));
    }
    let mut request = http(1800)?.post(format!("{}/chat/completions", conn.base())).json(&body);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let resp = request.send().map_err(|e| unreachable(conn, e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(api_error(conn, status, &resp.text().unwrap_or_default()));
    }
    let streamed = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.contains("event-stream"));

    let mut text = String::new();
    let mut served = model.to_string();
    let mut finish = String::new();
    if streamed {
        for line in BufReader::new(resp).lines() {
            let line = line.with_context(|| trf("{}: Antwort konnte nicht gelesen werden", &[&conn.name]))?;
            let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
            if data == "[DONE]" {
                break;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(msg) = chunk["error"]["message"].as_str() {
                bail!("{}: {}", conn.name, msg);
            }
            if let Some(m) = chunk["model"].as_str() {
                served = m.to_string();
            }
            let choice = &chunk["choices"][0];
            text.push_str(choice["delta"]["content"].as_str().unwrap_or_default());
            if let Some(r) = choice["finish_reason"].as_str() {
                finish = r.to_string();
            }
        }
    } else {
        // A server that ignores "stream": true answers in one piece.
        let value: Value = resp.json().with_context(|| trf("{}: Antwort konnte nicht gelesen werden", &[&conn.name]))?;
        let choice = &value["choices"][0];
        text = choice["message"]["content"].as_str().unwrap_or_default().to_string();
        finish = choice["finish_reason"].as_str().unwrap_or_default().to_string();
        served = value["model"].as_str().unwrap_or(model).to_string();
    }
    match finish.as_str() {
        "length" => bail!("{}", trf("{}: Antwort abgeschnitten (length)", &[&conn.name])),
        "content_filter" => bail!("{}", trf("{} hat die Anfrage abgelehnt (content_filter); nichts gespeichert", &[&conn.name])),
        _ => {}
    }
    Ok(Completion { text, model: served, effort: effort.map(str::to_string) })
}
