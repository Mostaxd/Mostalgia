//! Cloud speech-to-text over the OpenAI-compatible `/audio/transcriptions`
//! endpoint (OpenAI, Groq, LiteLLM, self-hosted Whisper servers).
//! Each call uploads one utterance or speech span as a 16-bit WAV.

use std::io::Cursor;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::SAMPLE_RATE;
use crate::connections::{Api, Connection, api_error, http, unreachable};
use crate::i18n::trf;
use crate::transcribe::{MAX_NO_SPEECH_PROB, Segment, is_annotation};

pub struct CloudEngine {
    conn: Connection,
    key: Option<String>,
    model: String,
    language: Option<String>,
    /// Ask for `verbose_json` (segment timestamps); falls back to plain
    /// `json` once if the server doesn't support it.
    timestamps: bool,
    client: reqwest::blocking::Client,
    /// Seconds of audio per language the service reported (auto mode).
    detected: Vec<(String, f64)>,
    /// Meeting languages (empty = any); see [`CloudEngine::reported_probs`].
    allowed: Vec<String>,
}

impl CloudEngine {
    pub fn new(conn: &Connection, model: &str, language: &str) -> Result<Self> {
        anyhow::ensure!(
            conn.api == Api::OpenAi,
            "{}",
            trf("{}: bietet keine Transkription an", &[&conn.name])
        );
        anyhow::ensure!(!model.is_empty(), "{}", trf("{}: kein Transkriptionsmodell gewählt", &[&conn.name]));
        let key = conn.require_key()?;
        let timestamps = conn
            .stt_models()
            .iter()
            .find(|m| m.id == model)
            .map_or(model.contains("whisper"), |m| m.timestamps);
        let mut engine = Self {
            conn: conn.clone(),
            key,
            model: model.to_string(),
            language: None,
            timestamps,
            client: http(300)?,
            detected: Vec::new(),
            allowed: Vec::new(),
        };
        engine.set_language(language);
        Ok(engine)
    }

    pub fn label(&self) -> String {
        format!("{} · {}", self.conn.name, self.model)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn set_language(&mut self, language: &str) {
        self.language = (language != "auto" && !language.is_empty()).then(|| language.to_string());
    }

    pub fn set_meeting_languages(&mut self, languages: &[String]) {
        self.allowed = languages.to_vec();
    }

    pub fn is_auto(&self) -> bool {
        self.language.is_none()
    }

    /// The services report no probabilities, only the language they heard;
    /// the share of audio per reported meeting language stands in for them
    /// (`None` until something was reported).
    pub fn reported_probs(&self) -> Option<crate::language::Probs> {
        if !self.is_auto() {
            return None;
        }
        let counts = |only: bool| -> crate::language::Probs {
            self.detected
                .iter()
                .filter(|(c, _)| !only || self.allowed.contains(c))
                .map(|(c, secs)| (c.clone(), *secs as f32))
                .collect()
        };
        let mut probs = counts(!self.allowed.is_empty());
        if probs.is_empty() {
            return None;
        }
        let total: f32 = probs.iter().map(|(_, p)| p).sum();
        for (_, p) in &mut probs {
            *p /= total;
        }
        probs.sort_by(|a, b| b.1.total_cmp(&a.1));
        Some(probs)
    }

    pub fn transcribe(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        self.transcribe_as(samples, None, &crate::glossary::current())
    }

    /// Transcribes in `language` when set to "auto" (a fixed one wins).
    /// Always `/audio/transcriptions`, never `/audio/translations`.
    /// The glossary's terms go along as the `prompt`; fix rules are
    /// the caller's ([`crate::transcribe::Engine`]).
    pub fn transcribe_as(&mut self, samples: &[f32], language: Option<&str>, glossary: &crate::glossary::Glossary) -> Result<Vec<Segment>> {
        let hint = glossary.hint();
        let language = self.language.clone().or_else(|| language.map(str::to_string));
        if samples.len() < SAMPLE_RATE as usize / 5 {
            return Ok(Vec::new());
        }
        let wav = encode_wav(samples)?;
        let duration = samples.len() as f64 / SAMPLE_RATE as f64;
        match self.request(&wav, self.timestamps, language.as_deref(), hint.as_deref()) {
            Err(e) if self.timestamps && format!("{e:#}").contains("response_format") => {
                self.timestamps = false;
                self.request(&wav, false, language.as_deref(), hint.as_deref())
            }
            other => other,
        }
        .map(|value| {
            let reported = value["language"].as_str().and_then(language_code);
            if let Some(code) = language.or(reported).filter(|_| self.language.is_none()) {
                match self.detected.iter_mut().find(|(c, _)| *c == code) {
                    Some((_, total)) => *total += duration,
                    None => self.detected.push((code, duration)),
                }
            }
            parse(&value, duration)
        })
    }

    pub fn reset_detected_language(&mut self) {
        self.detected.clear();
    }

    pub fn detected(&self) -> &[(String, f64)] {
        &self.detected
    }

    fn request(&self, wav: &[u8], verbose: bool, language: Option<&str>, hint: Option<&str>) -> Result<Value> {
        let file = reqwest::blocking::multipart::Part::bytes(wav.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        let mut form = reqwest::blocking::multipart::Form::new()
            .part("file", file)
            .text("model", self.model.clone())
            .text("response_format", if verbose { "verbose_json" } else { "json" })
            .text("temperature", "0");
        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }
        if let Some(hint) = hint {
            form = form.text("prompt", hint.to_string());
        }
        let mut request = self.client.post(format!("{}/audio/transcriptions", self.conn.base())).multipart(form);
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let resp = request.send().map_err(|e| unreachable(&self.conn, e))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            return Err(api_error(&self.conn, status, &body));
        }
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => Ok(v),
            // `json` answered as plain text by some servers.
            Err(_) if !body.trim().is_empty() && !body.trim_start().starts_with('{') => Ok(serde_json::json!({ "text": body })),
            Err(e) => Err(e).with_context(|| trf("{}: unerwartete Antwort der Transkription", &[&self.conn.name])),
        }
    }
}

/// A reported language ("english" or "en") as a Whisper code.
fn language_code(reported: &str) -> Option<String> {
    let r = reported.trim().to_ascii_lowercase();
    if r.len() == 2 {
        return Some(r);
    }
    (0..100).filter_map(|id| Some((whisper_rs::get_lang_str(id)?, whisper_rs::get_lang_str_full(id)?))).find(|(_, full)| *full == r).map(|(code, _)| code.to_string())
}

/// Segments from a `verbose_json` or `json` response.
fn parse(value: &Value, duration: f64) -> Vec<Segment> {
    let keep = |text: &str| !text.trim().is_empty() && !is_annotation(text);
    if let Some(segments) = value["segments"].as_array().filter(|s| !s.is_empty()) {
        return segments
            .iter()
            .filter(|s| s["no_speech_prob"].as_f64().is_none_or(|p| p <= MAX_NO_SPEECH_PROB as f64))
            .filter_map(|s| {
                let text = s["text"].as_str()?.to_string();
                keep(&text).then(|| Segment {
                    start_secs: s["start"].as_f64().unwrap_or(0.0),
                    end_secs: s["end"].as_f64().unwrap_or(duration).min(duration),
                    text,
                })
            })
            .collect();
    }
    let text = value["text"].as_str().unwrap_or_default().trim().to_string();
    if keep(&text) { vec![Segment { start_secs: 0.0, end_secs: duration, text }] } else { Vec::new() }
}

fn encode_wav(samples: &[f32]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::with_capacity(samples.len() * 2 + 44));
    {
        let mut writer = hound::WavWriter::new(&mut buf, spec)?;
        for &s in samples {
            writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        writer.finalize()?;
    }
    Ok(buf.into_inner())
}

/// Estimated cost of `hours` of audio with `model` on `conn` (list price,
/// US dollars), if known.
pub fn estimated_cost(conn: &Connection, model: &str, hours: f64) -> Option<f64> {
    conn.stt_models().iter().find(|m| m.id == model).map(|m| m.usd_per_hour * hours)
}

/// Fails with a clear message when a cloud engine can't be set up, naming
/// the connection; used before a recording starts so nothing is lost.
pub fn check(conn: Option<&Connection>, id: &str, model: &str) -> Result<()> {
    let Some(conn) = conn else {
        bail!("{}", trf("Transkriptions-Verbindung \"{}\" gibt es nicht mehr — in den Einstellungen wählen", &[&id]));
    };
    CloudEngine::new(conn, model, "auto").map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_formats() {
        let verbose = serde_json::json!({"text": "a b", "segments": [
            {"start": 0.0, "end": 1.2, "text": " Hallo", "no_speech_prob": 0.1},
            {"start": 1.2, "end": 2.0, "text": " [Musik]", "no_speech_prob": 0.1},
            {"start": 2.0, "end": 3.0, "text": " Stille", "no_speech_prob": 0.9}
        ]});
        let segs = parse(&verbose, 3.0);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text.trim(), "Hallo");
        let plain = parse(&serde_json::json!({"text": " Guten Morgen "}), 2.5);
        assert_eq!(plain.len(), 1);
        assert_eq!((plain[0].start_secs, plain[0].end_secs), (0.0, 2.5));
        assert!(parse(&serde_json::json!({"text": ""}), 1.0).is_empty());
    }

    #[test]
    fn wav_roundtrip() {
        let wav = encode_wav(&vec![0.25f32; 1600]).unwrap();
        let reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
        assert_eq!(reader.spec().sample_rate, SAMPLE_RATE);
        assert_eq!(reader.len(), 1600);
    }
}
