//! Answering questions about meetings: the model searches the library
//! with tools and answers only from what it found, citing passages.

use std::io::{BufRead, BufReader};

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate, TimeZone};
use crossbeam_channel::Sender;
use rusqlite::params;
use serde_json::{Value, json};

use super::search::{self, Filter};
use crate::chunker::format_ts;
use crate::config::Config;
use crate::i18n::{tr, trf};
use crate::store::Store;
use crate::connections::{Api, api_error, unreachable};
use crate::llm::{ANTHROPIC_VERSION, CLAUDE_FALLBACK_MODELS};
use crate::paths;
use crate::summary::SummaryRequest;

/// Tool rounds before the model must answer.
const MAX_ROUNDS: usize = 6;

/// Progress of an answer.
#[derive(Clone, Debug)]
pub enum AskEvent {
    /// What the model is doing ("Suche: Budget", "Lese Transkript …").
    Status(String),
    /// More answer text (may contain [[m:ID@SECS]] markers).
    TextDelta(String),
    /// Throw away the text streamed so far (it was a note before a tool call).
    ClearText,
    Done { answer: String, citations: Vec<Citation> },
    Error(String),
}

/// A reference into a meeting, from a [[m:ID@SECS]] marker.
#[derive(Clone, Debug, PartialEq)]
pub struct Citation {
    pub meeting_id: i64,
    pub secs: f64,
    /// "Titel · dd.mm. · hh:mm:ss"
    pub label: String,
}

/// A piece of an answer: plain text or a citation.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Text(String),
    Cite(Citation),
}

fn system_prompt() -> String {
    format!(
        "Du beantwortest Fragen zu den aufgezeichneten Besprechungen des Nutzers. \
Heute ist {today}.\n\n\
Nutze die Werkzeuge, um die Antwort in den Transkripten und Zusammenfassungen zu finden: \
search_meetings für Inhalte (mehrere Suchbegriffe/Umschreibungen probieren, bei Zeitangaben \
wie \"letzte Woche\" from_date/to_date setzen), list_meetings für Zeiträume, read_transcript für \
mehr Kontext um einen Treffer. Schreibe vor Werkzeugaufrufen keinen Text.\n\n\
Antworte ausschließlich auf Grundlage der Werkzeugergebnisse, knapp und konkret, in der Sprache \
der Frage. Belege jede Aussage direkt dahinter mit einem Marker [[m:<meeting_id>@<sekunden>]] \
(Sekunden = start_secs der passenden Stelle; für Zusammenfassungen 0). Wenn die Transkripte die \
Antwort nicht enthalten, sag das klar und rate nicht. Sprecher heißen \"Ich\" (der Nutzer), \
\"Andere\" oder haben Namen.",
        today = Local::now().format("%A, %d.%m.%Y")
    )
}

fn tools_claude() -> Value {
    json!([
        {
            "name": "search_meetings",
            "description": "Hybrid-Suche (Stichwörter + Bedeutung) über alle Transkript-Abschnitte und Zusammenfassungen. Liefert die besten Treffer mit meeting_id, Titel, Datum, start_secs und Text.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Suchanfrage, gern als Umschreibung des Gesuchten"},
                    "from_date": {"type": "string", "description": "Frühestes Datum YYYY-MM-DD (optional)"},
                    "to_date": {"type": "string", "description": "Spätestes Datum YYYY-MM-DD (optional)"}
                },
                "required": ["query"]
            }
        },
        {
            "name": "list_meetings",
            "description": "Listet Besprechungen (id, Titel, Datum, Dauer, ob Zusammenfassung vorhanden), optional im Zeitraum.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "from_date": {"type": "string", "description": "YYYY-MM-DD"},
                    "to_date": {"type": "string", "description": "YYYY-MM-DD"}
                }
            }
        },
        {
            "name": "read_transcript",
            "description": "Liest das Transkript einer Besprechung (Zeilen mit Sekunden, Zeit und Sprecher), optional nur einen Zeitbereich.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "meeting_id": {"type": "integer"},
                    "from_secs": {"type": "number"},
                    "to_secs": {"type": "number"}
                },
                "required": ["meeting_id"]
            }
        }
    ])
}

fn date(v: &Value) -> Option<NaiveDate> {
    v.as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

/// Runs one tool call against the library; returns (result text, status line).
fn run_tool(name: &str, input: &Value) -> (String, String) {
    let result = (|| -> Result<(String, String)> {
        let store = Store::open(&paths::database())?;
        match name {
            "search_meetings" => {
                let q = input["query"].as_str().unwrap_or_default();
                let filter = Filter {
                    from: date(&input["from_date"]),
                    to: date(&input["to_date"]),
                    meeting: None,
                };
                let t = std::time::Instant::now();
                let hits = search::search(&store, q, &filter, 15)?;
                let ms = t.elapsed().as_millis();
                let out: Vec<Value> = hits
                    .iter()
                    .map(|h| {
                        json!({
                            "meeting_id": h.meeting_id,
                            "title": h.title,
                            "date": h.started_at.format("%Y-%m-%d %H:%M").to_string(),
                            "kind": h.kind,
                            "start_secs": h.start_secs.round(),
                            "text": h.text,
                        })
                    })
                    .collect();
                Ok((serde_json::to_string(&out)?, trf("Suche: {} ({} Treffer, {} ms)", &[&q, &out.len(), &ms])))
            }
            "list_meetings" => {
                let filter = Filter {
                    from: date(&input["from_date"]),
                    to: date(&input["to_date"]),
                    meeting: None,
                };
                let list = store.list(500)?;
                let (lo, hi) = (
                    filter.from.map(|d| d.and_hms_opt(0, 0, 0).unwrap()),
                    filter.to.map(|d| d.and_hms_opt(23, 59, 59).unwrap()),
                );
                let out: Vec<Value> = list
                    .iter()
                    .filter(|m| lo.is_none_or(|lo| m.started_at.naive_local() >= lo) && hi.is_none_or(|hi| m.started_at.naive_local() <= hi))
                    .map(|m| {
                        json!({
                            "meeting_id": m.id,
                            "title": m.title,
                            "date": m.started_at.format("%Y-%m-%d %H:%M").to_string(),
                            "minutes": (m.duration_secs / 60.0).round(),
                            "has_summary": m.has_summary,
                        })
                    })
                    .collect();
                Ok((serde_json::to_string(&out)?, "Besprechungen auflisten".into()))
            }
            "read_transcript" => {
                let id = input["meeting_id"].as_i64().context("meeting_id fehlt")?;
                let from = input["from_secs"].as_f64().unwrap_or(0.0);
                let to = input["to_secs"].as_f64().unwrap_or(f64::MAX);
                let m = store.get(id)?.context(tr("Besprechung nicht gefunden"))?;
                let mut out = format!("{} · {}\n", m.title, m.started_at.format("%Y-%m-%d %H:%M"));
                for s in m.segments.iter().filter(|s| s.end_secs >= from && s.start_secs <= to) {
                    out.push_str(&format!("[{:.0}s {}] {}: {}\n", s.start_secs, format_ts(s.start_secs), s.speaker_label(), s.text));
                    if out.len() > 40_000 {
                        out.push_str("… (gekürzt; kleineren Zeitbereich lesen)\n");
                        break;
                    }
                }
                Ok((out, trf("Lese Transkript: {}", &[&m.title])))
            }
            other => bail!("{}", trf("unbekanntes Werkzeug {}", &[&other])),
        }
    })();
    match result {
        Ok(r) => r,
        Err(e) => (format!("Fehler: {e:#}"), format!("Werkzeugfehler: {e:#}")),
    }
}

/// Answers `question`, with earlier (question, answer) turns as context,
/// using the "ask" connection/model from the config. Streams progress to
/// `events`; always ends with `Done` or `Error`.
pub fn ask(question: &str, history: &[(String, String)], events: &Sender<AskEvent>) {
    let result = Config::load().ask_request().and_then(|req| match req.connection.api {
        Api::Anthropic => ask_claude(question, history, &req, events),
        Api::OpenAi => ask_openai(question, history, &req, events),
    });
    match result {
        Ok(answer) => {
            let citations = parse_answer(&answer)
                .into_iter()
                .filter_map(|p| if let Piece::Cite(c) = p { Some(c) } else { None })
                .collect();
            let _ = events.send(AskEvent::Done { answer, citations });
        }
        Err(e) => {
            let _ = events.send(AskEvent::Error(format!("{e:#}")));
        }
    }
}

fn ask_claude(question: &str, history: &[(String, String)], req: &SummaryRequest, events: &Sender<AskEvent>) -> Result<String> {
    let conn = &req.connection;
    let key = conn.require_key()?.unwrap_or_default();
    let model = req.model.clone();
    let mut messages: Vec<Value> = Vec::new();
    for (q, a) in history {
        messages.push(json!({"role": "user", "content": q}));
        messages.push(json!({"role": "assistant", "content": a}));
    }
    messages.push(json!({"role": "user", "content": question}));
    let mut effort = req.effort.clone();
    let client = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(900)).build()?;

    for round in 0..MAX_ROUNDS {
        let mut body = json!({
            "model": model,
            "max_tokens": 16000,
            "stream": true,
            "system": system_prompt(),
            "tools": tools_claude(),
            "tool_choice": {"type": "auto"},
            "messages": messages,
        });
        if let Some(e) = &effort {
            body["output_config"] = json!({ "effort": e });
        }
        let mut request = client
            .post(format!("{}/messages", conn.base()))
            .header("x-api-key", &key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        if CLAUDE_FALLBACK_MODELS.contains(&model.as_str()) {
            body["fallbacks"] = json!("default");
            request = request.header("anthropic-beta", "server-side-fallback-2026-07-01");
        }
        let resp = request.json(&body).send().map_err(|e| unreachable(conn, e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            // Older models have no effort control; retry the round without it.
            if effort.is_some() && body.contains("effort") {
                effort = None;
                continue;
            }
            return Err(api_error(conn, status, &body));
        }

        // Rebuild the assistant's content blocks from the stream so they can
        // be passed back unchanged (thinking blocks included).
        let mut blocks: Vec<Value> = Vec::new();
        let mut tool_json: Vec<String> = Vec::new();
        let mut stop_reason = String::new();
        for line in BufReader::new(resp).lines() {
            let line = line.context("Claude-Stream lesen")?;
            let Some(data) = line.strip_prefix("data: ") else { continue };
            let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
            match ev["type"].as_str() {
                Some("content_block_start") => {
                    blocks.push(ev["content_block"].clone());
                    tool_json.push(String::new());
                }
                Some("content_block_delta") => {
                    let i = ev["index"].as_u64().unwrap_or(0) as usize;
                    let (Some(block), Some(json_buf)) = (blocks.get_mut(i), tool_json.get_mut(i)) else { continue };
                    let d = &ev["delta"];
                    match d["type"].as_str() {
                        Some("text_delta") => {
                            let t = d["text"].as_str().unwrap_or_default();
                            let joined = format!("{}{t}", block["text"].as_str().unwrap_or_default());
                            block["text"] = json!(joined);
                            let _ = events.send(AskEvent::TextDelta(t.to_string()));
                        }
                        Some("input_json_delta") => json_buf.push_str(d["partial_json"].as_str().unwrap_or_default()),
                        Some("thinking_delta") => {
                            let joined = format!("{}{}", block["thinking"].as_str().unwrap_or_default(), d["thinking"].as_str().unwrap_or_default());
                            block["thinking"] = json!(joined);
                        }
                        Some("signature_delta") => block["signature"] = d["signature"].clone(),
                        _ => {}
                    }
                }
                Some("content_block_stop") => {
                    let i = ev["index"].as_u64().unwrap_or(0) as usize;
                    if let (Some(block), Some(buf)) = (blocks.get_mut(i), tool_json.get(i)) {
                        if block["type"] == "tool_use" {
                            block["input"] = serde_json::from_str(if buf.is_empty() { "{}" } else { buf }).unwrap_or(json!({}));
                        }
                    }
                }
                Some("message_delta") => {
                    if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                        stop_reason = r.to_string();
                    }
                }
                Some("error") => bail!("{}: {}", conn.name, ev["error"]["message"].as_str().unwrap_or(tr("unbekannt"))),
                _ => {}
            }
        }

        match stop_reason.as_str() {
            "tool_use" => {
                let _ = events.send(AskEvent::ClearText);
                let mut results = Vec::new();
                for b in blocks.iter().filter(|b| b["type"] == "tool_use") {
                    let (text, status) = run_tool(b["name"].as_str().unwrap_or_default(), &b["input"]);
                    let _ = events.send(AskEvent::Status(status));
                    results.push(json!({"type": "tool_result", "tool_use_id": b["id"], "content": text}));
                }
                messages.push(json!({"role": "assistant", "content": blocks}));
                messages.push(json!({"role": "user", "content": results}));
                if round + 2 == MAX_ROUNDS {
                    messages.push(json!({"role": "user", "content": "Bitte antworte jetzt mit dem, was du gefunden hast."}));
                }
            }
            "refusal" => bail!("{}", trf("{} hat die Frage abgelehnt", &[&conn.name])),
            _ => {
                let answer: String = blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                return Ok(answer.trim().to_string());
            }
        }
    }
    bail!("{}", trf("keine Antwort nach {} Suchschritten", &[&MAX_ROUNDS]))
}

fn tools_openai() -> Vec<Value> {
    tools_claude()
        .as_array()
        .expect("tool list")
        .iter()
        .map(|t| {
            json!({"type": "function", "function": {
                "name": t["name"], "description": t["description"], "parameters": t["input_schema"]}})
        })
        .collect()
}

/// Whether an error says the model or server can't do tool calling
/// (Ollama: "does not support tools", vLLM: "auto tool choice requires …").
fn no_tool_support(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("tool") && ["support", "not enabled", "requires", "invalid", "unrecognized", "unknown", "not allowed"].iter().any(|w| b.contains(w))
}

/// OpenAI-compatible tool calling (OpenAI, DeepSeek, Ollama, LiteLLM …).
/// A model without tool support gets the search results up front instead.
fn ask_openai(question: &str, history: &[(String, String)], req: &SummaryRequest, events: &Sender<AskEvent>) -> Result<String> {
    let conn = &req.connection;
    let key = conn.require_key()?;
    let mut messages = vec![json!({"role": "system", "content": system_prompt()})];
    for (q, a) in history {
        messages.push(json!({"role": "user", "content": q}));
        messages.push(json!({"role": "assistant", "content": a}));
    }
    messages.push(json!({"role": "user", "content": question}));
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(900))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut effort = req.effort.clone().filter(|_| !crate::llm::openai_efforts(conn, &req.model).is_empty());
    let mut round = 0;
    while round < MAX_ROUNDS {
        let mut body = json!({"model": req.model, "messages": messages, "tools": tools_openai(), "stream": false});
        if let Some(e) = &effort {
            body["reasoning_effort"] = json!(match e.as_str() { "low" => "low", "medium" => "medium", _ => "high" });
        }
        let mut request = client.post(format!("{}/chat/completions", conn.base())).json(&body);
        if let Some(key) = &key {
            request = request.bearer_auth(key);
        }
        let resp = request.send().map_err(|e| unreachable(conn, e))?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if !status.is_success() {
            if effort.is_some() && text.contains("reasoning_effort") {
                effort = None;
                continue;
            }
            if round == 0 && no_tool_support(&text) {
                return retrieve_then_answer(question, history, req, events);
            }
            return Err(api_error(conn, status, &text));
        }
        let v: Value = serde_json::from_str(&text).with_context(|| trf("{}: Antwort konnte nicht gelesen werden", &[&conn.name]))?;
        let msg = v["choices"][0]["message"].clone();
        let calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();
        if calls.is_empty() {
            let answer = msg["content"].as_str().unwrap_or_default().trim().to_string();
            let _ = events.send(AskEvent::TextDelta(answer.clone()));
            return Ok(answer);
        }
        let mut assistant = json!({"role": "assistant", "content": msg["content"].clone(), "tool_calls": calls});
        if msg["content"].is_null() {
            assistant["content"] = json!("");
        }
        messages.push(assistant);
        for call in &calls {
            let name = call["function"]["name"].as_str().unwrap_or_default();
            let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
            let (text, status) = run_tool(name, &args);
            let _ = events.send(AskEvent::Status(status));
            messages.push(json!({"role": "tool", "tool_call_id": call["id"], "content": text}));
        }
        round += 1;
        if round + 1 == MAX_ROUNDS {
            messages.push(json!({"role": "user", "content": "Bitte antworte jetzt mit dem, was du gefunden hast."}));
        }
    }
    bail!("{}", trf("keine Antwort nach {} Suchschritten", &[&MAX_ROUNDS]))
}

/// For models without tool calling: search the library for the question
/// first and hand the hits to the model with the question.
fn retrieve_then_answer(question: &str, history: &[(String, String)], req: &SummaryRequest, events: &Sender<AskEvent>) -> Result<String> {
    let _ = events.send(AskEvent::Status(trf("{} kann keine Werkzeuge nutzen – suche vorab", &[&req.model])));
    let (hits, status) = run_tool("search_meetings", &json!({ "query": question }));
    let _ = events.send(AskEvent::Status(status));
    let mut user = String::new();
    for (q, a) in history {
        user.push_str(&format!("Frühere Frage: {q}\nFrühere Antwort: {a}\n\n"));
    }
    user.push_str(&format!(
        "Suchergebnisse aus den Besprechungen (JSON):\n{hits}\n\nFrage: {question}\n\n\
Antworte nur auf Grundlage dieser Ergebnisse und belege jede Aussage mit [[m:<meeting_id>@<start_secs>]]."
    ));
    let system = system_prompt().replace("Nutze die Werkzeuge", "Die Suchergebnisse stehen in der Nachricht; nutze sie");
    let done = crate::llm::complete(&req.connection, &req.model, req.effort.as_deref(), &system, &user)?;
    let answer = done.text.trim().to_string();
    let _ = events.send(AskEvent::TextDelta(answer.clone()));
    Ok(answer)
}

/// Splits an answer into text and citations ([[m:ID@SECS]] markers),
/// labelling citations "Titel · dd.mm. · hh:mm:ss" from the library.
pub fn parse_answer(answer: &str) -> Vec<Piece> {
    let store = Store::open(&paths::database()).ok();
    let mut titles: std::collections::HashMap<i64, (String, String)> = Default::default();
    let mut label = |id: i64, secs: f64| -> String {
        let (title, date) = titles
            .entry(id)
            .or_insert_with(|| {
                store
                    .as_ref()
                    .and_then(|s| {
                        s.conn()
                            .query_row("SELECT title, started_at FROM meetings WHERE id = ?1", params![id], |r| {
                                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                            })
                            .ok()
                    })
                    .map(|(t, ts)| (t, Local.timestamp_opt(ts, 0).single().map_or(String::new(), |d| crate::i18n::short_date(&d))))
                    .unwrap_or_else(|| (trf("Besprechung #{}", &[&id]), String::new()))
            })
            .clone();
        if date.is_empty() { format!("{title} · {}", format_ts(secs)) } else { format!("{title} · {date} · {}", format_ts(secs)) }
    };
    let mut pieces = Vec::new();
    let mut rest = answer;
    while let Some(start) = rest.find("[[m:") {
        let Some(end) = rest[start..].find("]]").map(|e| start + e) else { break };
        let inner = &rest[start + 4..end];
        let parsed = inner.split_once('@').and_then(|(id, secs)| Some((id.trim().parse::<i64>().ok()?, secs.trim().parse::<f64>().ok()?)));
        if !rest[..start].is_empty() {
            pieces.push(Piece::Text(rest[..start].to_string()));
        }
        match parsed {
            Some((id, secs)) => pieces.push(Piece::Cite(Citation { meeting_id: id, secs, label: label(id, secs) })),
            None => pieces.push(Piece::Text(rest[start..end + 2].to_string())),
        }
        rest = &rest[end + 2..];
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest.to_string()));
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markers() {
        let p = parse_answer("Budget eingefroren [[m:1@27]]. Ende [[m:x]]");
        assert!(matches!(&p[0], Piece::Text(t) if t == "Budget eingefroren "));
        assert!(matches!(&p[1], Piece::Cite(c) if c.meeting_id == 1 && c.secs == 27.0));
        assert!(matches!(&p[3], Piece::Text(t) if t == "[[m:x]]"));
    }
}
