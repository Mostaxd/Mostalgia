//! Talking to language models over a [`Connection`]: model lists, a
//! connection test, and streamed completions (summaries, Mori's answers).
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
    /// Tokens used, where the server reports them.
    pub usage: Option<Usage>,
}

/// Token counts of one completion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    /// Of `output`: spent thinking (where reported).
    pub reasoning: u64,
    /// Of `input`: read from the provider's prompt cache (where reported).
    pub cached: u64,
}

/// One system + user turn, streamed so long thinking can't time out.
/// `effort` is sent only where the model is known to support it; if the
/// server still rejects it, that is an error (never a silent retry without).
pub fn complete(conn: &Connection, model: &str, effort: Option<&str>, system: &str, user: &str) -> Result<Completion> {
    chat(conn, model, effort, system, &[Message::user(user)], 64000, &mut |_| {})
}

/// One turn of a conversation for [`chat`].
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// "user" or "assistant".
    pub role: &'static str,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user", content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant", content: content.into() }
    }
}

/// A conversation (ending with a user turn), streamed: `on_delta` gets the
/// answer text as it arrives. Errors name the connection, like [`complete`].
pub fn chat(
    conn: &Connection,
    model: &str,
    effort: Option<&str>,
    system: &str,
    messages: &[Message],
    max_tokens: u32,
    on_delta: &mut dyn FnMut(&str),
) -> Result<Completion> {
    let key = conn.require_key()?;
    let key = key.as_deref();
    let effort = match conn.api {
        Api::Anthropic => effort,
        Api::OpenAi => effort.filter(|_| !openai_efforts(conn, model).is_empty()),
    };
    let result = match conn.api {
        Api::Anthropic => claude(conn, key.unwrap_or_default(), model, effort, system, messages, max_tokens, &json!({}), on_delta),
        Api::OpenAi => openai(conn, key, model, effort, system, messages, &json!({}), on_delta),
    };
    result.map_err(|e| effort_hint(e, effort.is_some()))
}

/// Like [`chat`], for short answers that should come fast (Mori's live
/// notes): one user turn, the least thinking the model allows — the lowest
/// effort where it has effort control, thinking switched off on DeepSeek —
/// and token usage reported where the server can. The system prompt and
/// the first `stable` bytes of `user` are marked for prompt caching on
/// Anthropic (OpenAI-compatible servers cache equal prefixes by themselves).
/// With `json`, OpenAI-compatible servers are asked for a JSON object
/// (`response_format`; the prompts must then mention JSON); a server that
/// rejects that gets the request again without it, and is remembered for
/// the session (see [`json_mode_rejected`]).
#[allow(clippy::too_many_arguments)]
pub fn chat_quick(
    conn: &Connection,
    model: &str,
    system: &str,
    user: &str,
    stable: usize,
    max_tokens: u32,
    json: bool,
    on_delta: &mut dyn FnMut(&str),
) -> Result<Completion> {
    let key = conn.require_key()?;
    let key = key.as_deref();
    match conn.api {
        Api::Anthropic => {
            let effort = crate::config::effort_for(conn, model, "low");
            let ephemeral = json!({ "type": "ephemeral" });
            let mut blocks = Vec::new();
            let stable = (0..=stable.min(user.len())).rev().find(|i| user.is_char_boundary(*i)).unwrap_or(0);
            if stable > 0 {
                blocks.push(json!({ "type": "text", "text": &user[..stable], "cache_control": ephemeral }));
            }
            if stable < user.len() {
                blocks.push(json!({ "type": "text", "text": &user[stable..] }));
            }
            let extra = json!({
                "system": [{ "type": "text", "text": system, "cache_control": ephemeral }],
                "messages": [{ "role": "user", "content": blocks }],
            });
            claude(conn, key.unwrap_or_default(), model, effort.as_deref(), system, &[Message::user(user)], max_tokens, &extra, on_delta)
                .map_err(|e| effort_hint(e, effort.is_some()))
        }
        Api::OpenAi => {
            let effort = (!openai_efforts(conn, model).is_empty()).then_some("low");
            let mut extra = quick_extras(conn, model, max_tokens);
            let json = json && !json_mode_rejected(conn);
            if json {
                extra["response_format"] = json!({ "type": "json_object" });
            }
            let messages = [Message::user(user)];
            match openai(conn, key, model, effort, system, &messages, &extra, on_delta) {
                // Nothing streamed yet, so a plain retry is safe.
                Err(e) if json && rejects_json_mode(&e) => {
                    eprintln!("[llm] {} rejected response_format json_object ({e:#}); asking without it for the rest of the session", conn.name);
                    JSON_MODE_REJECTED.lock().unwrap_or_else(|p| p.into_inner()).insert(json_mode_key(conn));
                    extra.as_object_mut().map(|o| o.remove("response_format"));
                    openai(conn, key, model, effort, system, &messages, &extra, on_delta)
                }
                r => r,
            }
            .map_err(|e| effort_hint(e, effort.is_some()))
        }
    }
}

/// Connections (by id and URL) that rejected `response_format` this session.
static JSON_MODE_REJECTED: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

fn json_mode_key(conn: &Connection) -> String {
    format!("{}|{}", conn.id, conn.base())
}

/// Whether this connection rejected JSON mode earlier in the session.
pub fn json_mode_rejected(conn: &Connection) -> bool {
    JSON_MODE_REJECTED.lock().unwrap_or_else(|p| p.into_inner()).contains(&json_mode_key(conn))
}

/// Whether a request failed because the server doesn't take
/// `response_format` (a 400/422 naming it or JSON mode).
fn rejects_json_mode(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}").to_ascii_lowercase();
    (text.contains(" 400") || text.contains(" 422") || text.contains("(400)") || text.contains("(422)"))
        && ["response_format", "response format", "json_object", "json mode", "json_schema"].iter().any(|w| text.contains(w))
}

/// Extra request fields for [`chat_quick`] on OpenAI-compatible servers
/// known to take them.
fn quick_extras(conn: &Connection, model: &str, max_tokens: u32) -> Value {
    let mut extra = json!({});
    let deepseek = conn.preset == "deepseek" || conn.base().contains("api.deepseek.com");
    // A cap only where the field is known (OpenAI's reasoning models want
    // `max_completion_tokens`, so it's left to the server's default there).
    if deepseek {
        extra["max_tokens"] = json!(max_tokens);
    }
    if deepseek && !model.contains("reasoner") {
        extra["thinking"] = json!({ "type": "disabled" });
    }
    if deepseek || conn.preset == "openai" {
        extra["stream_options"] = json!({ "include_usage": true });
    }
    extra
}

/// Adds a pointer to the setting when a server rejects the thinking level.
pub(crate) fn effort_hint(e: anyhow::Error, effort_sent: bool) -> anyhow::Error {
    if effort_sent && format!("{e:#}").contains("effort") {
        e.context(tr("Das Modell unterstützt die gewählte Denktiefe nicht — in den Einstellungen ändern"))
    } else {
        e
    }
}

#[allow(clippy::too_many_arguments)]
fn claude(
    conn: &Connection,
    key: &str,
    model: &str,
    effort: Option<&str>,
    system: &str,
    messages: &[Message],
    max_tokens: u32,
    extra: &Value,
    on_delta: &mut dyn FnMut(&str),
) -> Result<Completion> {
    let messages: Vec<Value> = messages.iter().map(|m| json!({ "role": m.role, "content": m.content })).collect();
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
        "system": system,
        "messages": messages,
    });
    // Thinking stays at the model's adaptive default; only the effort is set.
    if let Some(effort) = effort {
        body["output_config"] = json!({ "effort": effort });
    }
    merge(&mut body, extra);
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
    let mut usage: Option<Usage> = None;
    for line in BufReader::new(resp).lines() {
        let line = line.with_context(|| trf("{}: Antwort konnte nicht gelesen werden", &[&conn.name]))?;
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
        match event["type"].as_str() {
            Some("message_start") => {
                if let Some(m) = event["message"]["model"].as_str() {
                    served = m.to_string();
                }
                let u = &event["message"]["usage"];
                if let Some(n) = u["input_tokens"].as_u64() {
                    let cached = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    let usage = usage.get_or_insert_default();
                    // Anthropic counts cached and newly cached tokens apart.
                    usage.input = n + cached + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                    usage.cached = cached;
                }
            }
            Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                let delta = event["delta"]["text"].as_str().unwrap_or_default();
                on_delta(delta);
                text.push_str(delta);
            }
            Some("message_delta") => {
                if let Some(r) = event["delta"]["stop_reason"].as_str() {
                    stop_reason = r.to_string();
                }
                if let Some(n) = event["usage"]["output_tokens"].as_u64() {
                    usage.get_or_insert_default().output = n;
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
    Ok(Completion { text, model: served, effort: effort.map(str::to_string), usage })
}

/// Adds `extra`'s fields to the request body.
fn merge(body: &mut Value, extra: &Value) {
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
}

/// Token counts from an OpenAI-style `usage` object.
fn openai_usage(u: &Value) -> Option<Usage> {
    Some(Usage {
        input: u["prompt_tokens"].as_u64()?,
        output: u["completion_tokens"].as_u64().unwrap_or(0),
        reasoning: u["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
        cached: u["prompt_cache_hit_tokens"].as_u64().or_else(|| u["prompt_tokens_details"]["cached_tokens"].as_u64()).unwrap_or(0),
    })
}

fn openai(
    conn: &Connection,
    key: Option<&str>,
    model: &str,
    effort: Option<&str>,
    system: &str,
    messages: &[Message],
    extra: &Value,
    on_delta: &mut dyn FnMut(&str),
) -> Result<Completion> {
    let mut all = vec![json!({ "role": "system", "content": system })];
    all.extend(messages.iter().map(|m| json!({ "role": m.role, "content": m.content })));
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": all,
    });
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(openai_effort(effort));
    }
    merge(&mut body, extra);
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
    let mut usage = None;
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
            if let Some(u) = openai_usage(&chunk["usage"]) {
                usage = Some(u);
            }
            let choice = &chunk["choices"][0];
            let delta = choice["delta"]["content"].as_str().unwrap_or_default();
            if !delta.is_empty() {
                on_delta(delta);
            }
            text.push_str(delta);
            if let Some(r) = choice["finish_reason"].as_str() {
                finish = r.to_string();
            }
        }
    } else {
        // A server that ignores "stream": true answers in one piece.
        let value: Value = resp.json().with_context(|| trf("{}: Antwort konnte nicht gelesen werden", &[&conn.name]))?;
        let choice = &value["choices"][0];
        text = choice["message"]["content"].as_str().unwrap_or_default().to_string();
        on_delta(&text);
        finish = choice["finish_reason"].as_str().unwrap_or_default().to_string();
        served = value["model"].as_str().unwrap_or(model).to_string();
        usage = openai_usage(&value["usage"]);
    }
    match finish.as_str() {
        "length" => bail!("{}", trf("{}: Antwort abgeschnitten (length)", &[&conn.name])),
        "content_filter" => bail!("{}", trf("{} hat die Anfrage abgelehnt (content_filter); nichts gespeichert", &[&conn.name])),
        _ => {}
    }
    Ok(Completion { text, model: served, effort: effort.map(str::to_string), usage })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_servers_that_reject_json_mode() {
        let rejected = anyhow::anyhow!("Local: error 400: 'response_format.type' must be 'json_schema' or 'text'");
        assert!(rejects_json_mode(&rejected));
        assert!(rejects_json_mode(&anyhow::anyhow!("Proxy: Fehler 422: unknown field response_format")));
        // Other failures are not retried without it.
        assert!(!rejects_json_mode(&anyhow::anyhow!("DeepSeek: error 400: max_tokens too large")));
        assert!(!rejects_json_mode(&anyhow::anyhow!("DeepSeek: Limit erreicht (429): response_format quota")));
    }
}
