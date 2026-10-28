//! "Add meeting from text": a meeting nobody recorded, from a transcript
//! the user has as text — pasted, or a .txt / .md / .vtt / .srt file.
//!
//! Understood: plain text (paragraphs or lines), "Name: text" lines,
//! "[hh:mm:ss] Name: text" and "hh:mm:ss Name: text", WebVTT / SRT
//! subtitles (with `<v Name>` or "Name: " speakers), and the Teams / Zoom /
//! Meet export styles ("Name  0:01:23" or "Name" + "0:03" over the text,
//! "Name (0:01:23): text", "[Name] 14:03:12", "00:05:00" marker lines).
//!
//! The meeting gets no audio. Its transcript engine is [`ENGINE`] (or
//! [`ENGINE_UNTIMED`] when the text had no times; its lines then get times
//! spread over the duration by length, so citations and jumps still land
//! on the right line, and the UI hides them).

use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};

use crate::audio::Source;
use crate::finalize::FinalSegment;
use crate::i18n::{tr, trf};
use crate::store::{NewMeeting, Store};

/// `transcript_engine` of a meeting imported from text with times.
pub const ENGINE: &str = "Text import"; // i18n: not UI
/// `transcript_engine` of a meeting imported from text without times.
pub const ENGINE_UNTIMED: &str = "Text import (no times)"; // i18n: not UI

/// File types the "Open file…" picker and the CLI read.
pub const EXTENSIONS: [&str; 5] = ["txt", "md", "vtt", "srt", "text"];

/// Whether a meeting with this transcript engine was imported from text.
pub fn is_text_import(engine: Option<&str>) -> bool {
    engine.is_some_and(|e| e.starts_with(ENGINE))
}

/// Whether its times were made up (spread over the duration).
pub fn is_untimed(engine: Option<&str>) -> bool {
    engine == Some(ENGINE_UNTIMED)
}

/// What the text looked like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Paragraphs or lines without speakers or times.
    Plain,
    /// "Name: text" lines (or name headers) without times.
    Speakers,
    /// Lines with times (with or without speakers): chat-style exports,
    /// Teams / Zoom / Meet transcripts.
    Timestamps,
    WebVtt,
    Srt,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Plain => tr("Fließtext"),
            Format::Speakers => tr("Zeilen mit Sprechern"),
            Format::Timestamps => tr("Transkript mit Zeitstempeln"),
            Format::WebVtt => tr("WebVTT-Untertitel"),
            Format::Srt => tr("SRT-Untertitel"),
        }
    }
}

/// One transcript line as read from the text.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub speaker: Option<String>,
    /// Seconds from the start, if the text said.
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Parsed {
    pub format: Format,
    pub lines: Vec<Line>,
    /// The time of day the first timestamp stood for (seconds after
    /// midnight), when the times were clock times (Zoom's "[Name] 14:03:12").
    pub clock_start: Option<f64>,
}

/// Seconds a line of `words` words takes to say (150 words a minute).
fn speaking_secs(words: usize) -> f64 {
    words as f64 / 2.5
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count().max(1)
}

impl Parsed {
    /// Whether the text had times.
    pub fn timed(&self) -> bool {
        self.lines.iter().any(|l| l.start.is_some())
    }

    /// The speakers in order of appearance.
    pub fn speakers(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in &self.lines {
            if let Some(s) = &l.speaker {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
        }
        out
    }

    /// How long the meeting was: up to the last time (plus its line), else
    /// from the length of the text (150 words a minute, at least a minute).
    pub fn estimated_duration(&self) -> f64 {
        let words: usize = self.lines.iter().map(|l| word_count(&l.text)).sum();
        if !self.timed() {
            return speaking_secs(words).max(60.).round();
        }
        // Words after the last known time still take their time.
        let mut end: f64 = 0.;
        let mut after = 0;
        for l in &self.lines {
            if let Some(s) = l.start {
                end = end.max(s);
                after = 0;
            }
            if let Some(e) = l.end {
                end = end.max(e);
            }
            after += word_count(&l.text);
        }
        let last_has_end = self.lines.last().is_some_and(|l| l.end.is_some());
        if !last_has_end {
            end += speaking_secs(after);
        }
        end.max(1.).ceil()
    }

    /// The lines as transcript segments for a meeting of `duration` seconds.
    /// Lines without a time get one between their neighbours' (by length);
    /// without any times, the lines are spread over the whole duration.
    pub fn segments(&self, duration: f64) -> Vec<FinalSegment> {
        let n = self.lines.len();
        let words: Vec<f64> = self.lines.iter().map(|l| word_count(&l.text) as f64).collect();
        let mut starts: Vec<f64> = vec![0.; n];
        // Anchors: the known times, with 0 before the first line and the
        // duration after the last.
        let mut anchors: Vec<(isize, f64)> = vec![(-1, 0.)];
        let mut last = 0.;
        for (i, l) in self.lines.iter().enumerate() {
            if let Some(s) = l.start {
                let s = s.max(last);
                anchors.push((i as isize, s));
                last = s;
            }
        }
        let tail_end = if self.timed() { duration.max(last + speaking_secs(self.lines.iter().rev().take_while(|l| l.start.is_none()).map(|l| word_count(&l.text)).sum::<usize>() + 1)) } else { duration };
        anchors.push((n as isize, tail_end.max(last)));
        for pair in anchors.windows(2) {
            let ((a, ta), (b, tb)) = (pair[0], pair[1]);
            let from = a.max(0) as usize;
            let to = b as usize;
            let total: f64 = words[from..to].iter().sum::<f64>().max(1.);
            let mut acc = 0.;
            for i in from..to {
                starts[i] = if i as isize == a { ta } else { ta + (tb - ta) * acc / total };
                acc += words[i];
            }
        }
        let timed = self.timed();
        (0..n)
            .map(|i| {
                let l = &self.lines[i];
                let start = starts[i];
                let next = starts.get(i + 1).copied().unwrap_or(if timed { start + speaking_secs(words[i] as usize) } else { duration });
                let end = match l.end {
                    Some(e) if e > start => e,
                    _ if next > start => next,
                    _ => start + speaking_secs(words[i] as usize).min(5.),
                };
                FinalSegment { source: Source::System, start_secs: start, end_secs: end, text: l.text.clone(), speaker: l.speaker.clone() }
            })
            .collect()
    }
}

/// Reads a text file's bytes: UTF-8 (with or without BOM) or UTF-16 with
/// a BOM. Word files are refused with a hint to paste the text instead.
pub fn decode(bytes: &[u8]) -> Result<String> {
    anyhow::ensure!(!bytes.starts_with(b"PK\x03\x04"), "{}", tr("Word- und andere Office-Dateien werden nicht unterstützt — öffne die Datei und füge den Text ein."));
    anyhow::ensure!(!bytes.starts_with(b"%PDF"), "{}", tr("PDF-Dateien werden nicht unterstützt — kopiere den Text heraus und füge ihn ein."));
    let utf16 = |be: bool| {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    Ok(match bytes {
        [0xFF, 0xFE, ..] => utf16(false),
        [0xFE, 0xFF, ..] => utf16(true),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    })
}

/// Reads a transcript from text in any of the understood formats.
pub fn parse(text: &str) -> Parsed {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    let mut parsed = if first.starts_with("WEBVTT") {
        Parsed { format: Format::WebVtt, lines: parse_cues(&text), clock_start: None }
    } else {
        let cues = if text.contains("-->") { parse_cues(&text) } else { Vec::new() };
        if cues.is_empty() {
            let lines = parse_lines(&text);
            let format = if lines.iter().any(|l| l.start.is_some()) {
                Format::Timestamps
            } else if lines.iter().any(|l| l.speaker.is_some()) {
                Format::Speakers
            } else {
                Format::Plain
            };
            Parsed { format, lines, clock_start: None }
        } else {
            Parsed { format: Format::Srt, lines: cues, clock_start: None }
        }
    };
    parsed.lines = split_long(parsed.lines);
    parsed.clock_start = normalize_times(&mut parsed.lines);
    parsed
}

/// Times that start an hour or more in are clock times (Zoom, chat logs):
/// they become times from the first one. Returns that first clock time.
fn normalize_times(lines: &mut [Line]) -> Option<f64> {
    let first = lines.iter().find_map(|l| l.start)?;
    if first < 3600. {
        return None;
    }
    for l in lines.iter_mut() {
        for t in [&mut l.start, &mut l.end].into_iter().flatten() {
            *t -= first;
            // Past midnight.
            if *t < -1. {
                *t += 86_400.;
            }
            *t = t.max(0.);
        }
    }
    Some(first)
}

/// "1:02:03", "01:02", "00:01:02.500", "00:01:02,500" → seconds.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let (clock, frac) = match s.find(['.', ',']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    if parts[0].len() > 3 || parts[1..].iter().any(|p| p.len() != 2) {
        return None;
    }
    let nums: Vec<f64> = parts.iter().map(|p| p.parse::<f64>().unwrap_or(0.)).collect();
    if nums[1..].iter().any(|n| *n >= 60.) {
        return None;
    }
    let mut secs = nums.iter().fold(0., |acc, n| acc * 60. + n);
    if let Some(f) = frac {
        if f.is_empty() || f.len() > 3 || !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        secs += f.parse::<f64>().unwrap_or(0.) / 10f64.powi(f.len() as i32);
    }
    Some(secs)
}

/// Words that end with ":" at the start of a line without being a speaker.
const NOT_NAMES: &[&str] = &[ // i18n: not UI
    "agenda", "note", "notes", "summary", "todo", "to do", "action items", "action item", "decision", "decisions",
    "attendees", "participants", "date", "time", "location", "subject", "topic", "re", "fyi", "update", "next steps",
    "teilnehmer", "datum", "zeit", "ort", "betreff", "thema", "notiz", "notizen", "zusammenfassung", "aufgaben",
    "entscheidung", "entscheidungen", "nächste schritte", "http", "https",
];

/// Whether `s` can be a speaker's name: short, a few words, no sentence
/// punctuation. `strict`: every word capitalised (for names standing
/// alone, e.g. above a Teams time).
fn looks_like_name(s: &str, strict: bool) -> bool {
    let s = s.trim();
    let chars = s.chars().count();
    if chars == 0 || chars > 40 {
        return false;
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.is_empty() || words.len() > 5 {
        return false;
    }
    if !s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '@') {
        return false;
    }
    if s.chars().any(|c| "?!;:,\"“”„<>{}[]()=/\\|*#".contains(c)) || s.ends_with('.') {
        return false;
    }
    if NOT_NAMES.contains(&s.to_lowercase().as_str()) {
        return false;
    }
    !strict || words.iter().all(|w| w.chars().next().is_some_and(|c| !c.is_lowercase()))
}

/// What one line of a line-based transcript is.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Blank,
    /// Only a time ("00:05:00"): the time of what follows.
    Time(f64),
    /// A speaker (and time) above their text: "Name  0:01:23", "Name (0:01:23)",
    /// "[Name] 14:03:12", "Name:".
    Header(String, Option<f64>),
    /// A line with a time and/or speaker in front of its text.
    Turn(Option<f64>, Option<String>, String),
    /// "Name: text" — a speaker if the name comes up often enough.
    Said(String, String),
    Text(String),
}

/// Splits "[00:01:02] rest", "(00:01:02) rest" or "00:01:02 rest".
fn leading_time(s: &str) -> Option<(f64, &str)> {
    let (t, rest) = if let Some(inner) = s.strip_prefix('[').or_else(|| s.strip_prefix('(')) {
        let close = inner.find([']', ')'])?;
        (parse_time(&inner[..close])?, &inner[close + 1..])
    } else {
        let end = s.find(char::is_whitespace).unwrap_or(s.len());
        let token = s[..end].trim_end_matches([':', '-', '|']);
        (parse_time(token)?, &s[end..])
    };
    let rest = rest.trim_start().trim_start_matches(['-', '–', '|', ':']).trim_start();
    Some((t, rest))
}

/// "Name: text" → ("Name", "text").
fn split_said(s: &str) -> Option<(&str, &str)> {
    let colon = s.find(':')?;
    let (name, text) = (s[..colon].trim(), s[colon + 1..].trim());
    // "https://…", "10:30" are no speakers.
    if text.starts_with("//") || name.ends_with(|c: char| c.is_ascii_digit()) && text.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    looks_like_name(name, false).then_some((name, text))
}

fn classify(raw: &str) -> Kind {
    // Markdown emphasis around names ("**Anna:** …") and quote markers.
    let cleaned = raw.replace("**", "").replace("__", "");
    let s = cleaned.trim().trim_start_matches(">> ").trim();
    if s.is_empty() {
        return Kind::Blank;
    }
    if let Some((t, rest)) = leading_time(s) {
        if rest.is_empty() {
            return Kind::Time(t);
        }
        return match split_said(rest) {
            Some((name, text)) if !text.is_empty() => Kind::Turn(Some(t), Some(name.to_string()), text.to_string()),
            Some((name, _)) => Kind::Header(name.to_string(), Some(t)),
            None => Kind::Turn(Some(t), None, rest.to_string()),
        };
    }
    // "[Name] 14:03:12" (Zoom).
    if let Some(inner) = s.strip_prefix('[') {
        if let Some(close) = inner.find(']') {
            let (name, rest) = (inner[..close].trim(), inner[close + 1..].trim());
            if looks_like_name(name, false) {
                if let Some(t) = parse_time(rest.trim_end_matches(':')) {
                    return Kind::Header(name.to_string(), Some(t));
                }
                if let Some(text) = rest.strip_prefix(':').map(str::trim).or(Some(rest)).filter(|r| !r.is_empty()) {
                    return Kind::Turn(None, Some(name.to_string()), text.to_string());
                }
            }
        }
    }
    // "Name (0:01:23): text" / "Name (0:01:23)".
    if let Some(open) = s.find('(') {
        if let Some(close) = s[open..].find(')').map(|c| open + c) {
            let name = s[..open].trim();
            if let Some(t) = parse_time(&s[open + 1..close]).filter(|_| looks_like_name(name, false)) {
                let rest = s[close + 1..].trim_start().trim_start_matches(':').trim();
                return if rest.is_empty() {
                    Kind::Header(name.to_string(), Some(t))
                } else {
                    Kind::Turn(Some(t), Some(name.to_string()), rest.to_string())
                };
            }
        }
    }
    // "Name  0:01:23" (Teams, Otter).
    if let Some(space) = s.rfind(char::is_whitespace) {
        let (name, time) = (s[..space].trim().trim_end_matches(['-', '–', '|', '·']).trim(), &s[space + 1..]);
        if let Some(t) = parse_time(time).filter(|_| looks_like_name(name, true)) {
            return Kind::Header(name.to_string(), Some(t));
        }
    }
    if let Some((name, text)) = split_said(s) {
        return if text.is_empty() { Kind::Header(name.to_string(), None) } else { Kind::Said(name.to_string(), text.to_string()) };
    }
    Kind::Text(s.to_string())
}

/// Most characters in one transcript line before it is split.
const MAX_LINE: usize = 600;

/// Line-based transcripts (everything but subtitles).
fn parse_lines(text: &str) -> Vec<Line> {
    let mut kinds: Vec<Kind> = text.lines().map(classify).collect();
    // "Name" over "0:03" (Teams' newer copy format) is a header.
    for i in 0..kinds.len().saturating_sub(1) {
        if let (Kind::Text(name), Kind::Time(t)) = (&kinds[i], &kinds[i + 1]) {
            if looks_like_name(name, true) {
                kinds[i] = Kind::Header(name.clone(), Some(*t));
                kinds[i + 1] = Kind::Blank;
            }
        }
    }
    // "Name: text" counts as a speaker when that name comes up more than
    // once, when most lines look like that, or when other lines name
    // speakers in front of their text.
    let nonblank = kinds.iter().filter(|k| **k != Kind::Blank).count();
    let said = kinds.iter().filter(|k| matches!(k, Kind::Said(..))).count();
    let known: Vec<String> = kinds
        .iter()
        .filter_map(|k| match k {
            Kind::Header(n, _) | Kind::Turn(_, Some(n), _) => Some(n.clone()),
            _ => None,
        })
        .collect();
    let mut counts: std::collections::HashMap<String, usize> = Default::default();
    for k in &kinds {
        if let Kind::Said(name, _) = k {
            *counts.entry(name.clone()).or_default() += 1;
        }
    }
    let mostly = said * 2 >= nonblank;
    // Speakers above their text (Teams): "Question: …" in the text is text.
    let headers = kinds.iter().any(|k| matches!(k, Kind::Header(_, Some(_))));
    let inline = kinds.iter().any(|k| matches!(k, Kind::Turn(_, Some(_), _)));
    for k in kinds.iter_mut() {
        if let Kind::Said(name, text) = k {
            *k = if known.contains(name) || (!headers && (mostly || inline || counts[name.as_str()] >= 2)) {
                Kind::Turn(None, Some(name.clone()), text.clone())
            } else {
                Kind::Text(format!("{name}: {text}"))
            };
        }
    }
    let structured = kinds.iter().any(|k| matches!(k, Kind::Time(_) | Kind::Header(..) | Kind::Turn(..)));
    // Plain text without blank lines: every line is a line of its own.
    let line_per_line = !structured && !kinds.contains(&Kind::Blank);

    let mut out: Vec<Line> = Vec::new();
    let mut current: Option<Line> = None;
    let mut speaker: Option<String> = None;
    let mut pending_time: Option<f64> = None;
    let flush = |current: &mut Option<Line>, out: &mut Vec<Line>| {
        if let Some(l) = current.take().filter(|l| !l.text.trim().is_empty()) {
            out.push(l);
        }
    };
    for k in kinds {
        match k {
            Kind::Blank => flush(&mut current, &mut out),
            Kind::Time(t) => {
                flush(&mut current, &mut out);
                pending_time = Some(t);
            }
            Kind::Header(name, t) => {
                flush(&mut current, &mut out);
                speaker = Some(name);
                pending_time = t.or(pending_time);
            }
            Kind::Turn(t, name, text) => {
                flush(&mut current, &mut out);
                if name.is_some() {
                    speaker = name;
                }
                current = Some(Line { speaker: speaker.clone(), start: t.or(pending_time.take()), end: None, text });
            }
            Kind::Said(..) => unreachable!(),
            Kind::Text(text) => match &mut current {
                Some(l) if !line_per_line && l.text.len() + text.len() < MAX_LINE => {
                    l.text.push(' ');
                    l.text.push_str(&text);
                }
                _ => {
                    flush(&mut current, &mut out);
                    current = Some(Line { speaker: speaker.clone(), start: pending_time.take(), end: None, text });
                }
            },
        }
    }
    flush(&mut current, &mut out);
    out
}

/// Splits lines longer than [`MAX_LINE`] at sentence ends (the parts after
/// the first have no time of their own), so citations stay precise.
fn split_long(lines: Vec<Line>) -> Vec<Line> {
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        if l.text.len() <= MAX_LINE + 200 {
            out.push(l);
            continue;
        }
        let mut parts: Vec<String> = Vec::new();
        let mut buf = String::new();
        for sentence in sentences(&l.text) {
            if !buf.is_empty() && buf.len() + sentence.len() > MAX_LINE {
                parts.push(std::mem::take(&mut buf));
            }
            if !buf.is_empty() {
                buf.push(' ');
            }
            buf.push_str(sentence);
        }
        if !buf.is_empty() {
            parts.push(buf);
        }
        let last = parts.len() - 1;
        for (i, text) in parts.into_iter().enumerate() {
            out.push(Line {
                speaker: l.speaker.clone(),
                start: if i == 0 { l.start } else { None },
                end: if i == last { l.end } else { None },
                text,
            });
        }
    }
    out
}

/// The sentences of a text (split after . ! ? … followed by a space).
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for w in chars.windows(2) {
        let ((i, c), (_, next)) = (w[0], w[1]);
        if ".!?…。".contains(c) && next.is_whitespace() {
            let end = i + c.len_utf8();
            out.push(text[start..end].trim());
            start = end;
        }
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        out.push(rest);
    }
    out.retain(|s| !s.is_empty());
    out
}

/// Subtitles (WebVTT, SRT): cues with times; `<v Name>` or "Name: " for speakers.
fn parse_cues(text: &str) -> Vec<Line> {
    let mut cues: Vec<Line> = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !block.is_empty() {
                blocks.push(std::mem::take(&mut block));
            }
        } else {
            block.push(line.trim());
        }
    }
    if !block.is_empty() {
        blocks.push(block);
    }
    for block in blocks {
        let Some(at) = block.iter().position(|l| l.contains("-->")) else { continue };
        if block[0].starts_with("NOTE") || block[0].starts_with("STYLE") || block[0].starts_with("REGION") {
            continue;
        }
        let (a, b) = block[at].split_once("-->").unwrap_or_default();
        let Some(start) = parse_time(a.trim()) else { continue };
        let end = b.split_whitespace().next().and_then(parse_time);
        let raw = block[at + 1..].join(" ");
        let (speaker, text) = cue_text(&raw);
        if text.is_empty() {
            continue;
        }
        cues.push(Line { speaker, start: Some(start), end, text });
    }
    // "Name: text" inside cues, when that name comes up more than once or
    // most cues look like that.
    let said: Vec<Option<(String, String)>> = cues
        .iter()
        .map(|c| if c.speaker.is_some() { None } else { split_said(&c.text).map(|(n, t)| (n.to_string(), t.to_string())) })
        .collect();
    let mostly = said.iter().flatten().count() * 2 >= cues.len();
    for (i, s) in said.iter().enumerate() {
        if let Some((name, text)) = s {
            let n = said.iter().flatten().filter(|(other, _)| other == name).count();
            if (n >= 2 || mostly) && !text.is_empty() {
                cues[i].speaker = Some(name.clone());
                cues[i].text = text.clone();
            }
        }
    }
    // A speaker without "Name:" in the next cue keeps talking (SRT lines
    // only name who speaks when it changes).
    let named = cues.iter().any(|c| c.speaker.is_some());
    if named {
        let mut last: Option<String> = None;
        for c in cues.iter_mut() {
            match &c.speaker {
                Some(s) => last = Some(s.clone()),
                None => c.speaker = last.clone(),
            }
        }
    }
    merge_cues(cues)
}

/// Joins consecutive cues of the same speaker into lines of readable length.
fn merge_cues(cues: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for c in cues {
        if let Some(prev) = out.last_mut() {
            let gap = c.start.unwrap_or(0.) - prev.end.or(prev.start).unwrap_or(0.);
            let sentence_done = prev.text.ends_with(['.', '?', '!', '…']);
            if prev.speaker == c.speaker && gap <= 2. && prev.text.len() + c.text.len() < 300 && !(sentence_done && prev.text.len() > 150) {
                prev.text.push(' ');
                prev.text.push_str(&c.text);
                prev.end = c.end.or(c.start);
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// A cue's text without markup, and its `<v Name>` speaker.
fn cue_text(raw: &str) -> (Option<String>, String) {
    let mut speaker = None;
    let mut text = String::new();
    let mut rest = raw;
    while let Some(open) = rest.find(['<', '{']) {
        text.push_str(&rest[..open]);
        let closer = if rest[open..].starts_with('<') { '>' } else { '}' };
        let Some(close) = rest[open..].find(closer).map(|c| open + c) else {
            rest = &rest[open..];
            break;
        };
        let tag = &rest[open + 1..close];
        if let Some(v) = tag.strip_prefix('v').filter(|v| v.starts_with([' ', '.'])) {
            // "<v.loud Anna Schmidt>": classes, then the name.
            let name = v.split_once(' ').map_or("", |(_, n)| n).trim();
            if !name.is_empty() && speaker.is_none() {
                speaker = Some(name.to_string());
            }
        }
        rest = &rest[close + 1..];
    }
    text.push_str(rest);
    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.trim_start_matches(">> ").trim_start_matches("- ").trim().to_string();
    (speaker, text)
}

/// Frequent short words per language, for telling Latin-script languages apart.
const STOP_WORDS: &[(&str, &[&str])] = &[ // i18n: not UI
    ("en", &["the", "and", "is", "are", "we", "you", "that", "this", "to", "of", "it", "for", "have", "will", "with", "not", "what", "be", "was", "can"]),
    ("de", &["und", "der", "die", "das", "ist", "nicht", "wir", "ich", "sie", "es", "mit", "auf", "für", "ein", "eine", "auch", "dass", "haben", "wird", "noch"]),
    ("fr", &["le", "la", "les", "et", "est", "nous", "vous", "que", "pas", "une", "des", "pour", "dans", "ce", "je", "on", "avec", "sur", "c'est", "il"]),
    ("es", &["el", "la", "los", "las", "y", "es", "que", "no", "una", "por", "para", "con", "se", "lo", "pero", "como", "muy", "está", "del", "yo"]),
    ("it", &["il", "la", "che", "e", "è", "non", "per", "una", "sono", "di", "con", "questo", "anche", "ma", "noi", "ci", "gli", "del", "abbiamo", "io"]),
    ("nl", &["de", "het", "een", "en", "is", "niet", "wij", "we", "dat", "van", "ik", "op", "met", "voor", "ook", "maar", "zijn", "er", "hebben", "wat"]),
    ("pt", &["o", "a", "os", "que", "não", "é", "uma", "para", "com", "em", "mas", "você", "nós", "isso", "está", "por", "se", "do", "da", "eu"]),
    ("pl", &["i", "w", "nie", "to", "że", "na", "się", "jest", "z", "do", "jak", "ale", "tak", "co", "mamy", "czy", "już", "jestem", "ja"]),
    ("tr", &["ve", "bir", "bu", "da", "de", "için", "ne", "çok", "ama", "ben", "biz", "var", "değil", "mi", "olarak", "ile", "şimdi"]),
    ("sv", &["och", "att", "det", "är", "som", "en", "på", "jag", "vi", "inte", "med", "för", "har", "den", "till", "kan", "ska"]),
];

/// The language a text is in, as a Whisper code ("en", "de", "ar", …), or
/// `None` when it can't tell.
pub fn detect_language(text: &str) -> Option<&'static str> {
    let sample: String = text.chars().take(20_000).collect();
    let mut letters = 0usize;
    let mut scripts: [(&str, usize); 10] = [("ar", 0), ("he", 0), ("ru", 0), ("el", 0), ("ko", 0), ("ja", 0), ("zh", 0), ("th", 0), ("hi", 0), ("fa", 0)];
    for c in sample.chars().filter(|c| c.is_alphabetic()) {
        letters += 1;
        let i = match c as u32 {
            // Persian-only letters (پ چ ژ گ) count for Farsi as well as Arabic.
            0x067E | 0x0686 | 0x0698 | 0x06AF => Some(9),
            0x0600..=0x06FF | 0x0750..=0x077F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Some(0),
            0x0590..=0x05FF => Some(1),
            0x0400..=0x04FF => Some(2),
            0x0370..=0x03FF => Some(3),
            0xAC00..=0xD7AF | 0x1100..=0x11FF => Some(4),
            0x3040..=0x30FF => Some(5),
            0x4E00..=0x9FFF => Some(6),
            0x0E00..=0x0E7F => Some(7),
            0x0900..=0x097F => Some(8),
            _ => None,
        };
        if let Some(i) = i {
            scripts[i].1 += 1;
            if i == 9 {
                scripts[0].1 += 1;
            }
        }
    }
    if letters < 20 {
        return None;
    }
    let share = |n: usize| n * 10 >= letters * 3;
    // Japanese text mixes kana and kanji; any kana decides it.
    if share(scripts[5].1) || (scripts[5].1 > 0 && share(scripts[5].1 + scripts[6].1)) {
        return Some("ja");
    }
    if share(scripts[0].1) {
        // A few Persian-only letters make it Farsi.
        return Some(if scripts[9].1 * 50 >= scripts[0].1 { "fa" } else { "ar" });
    }
    if let Some((code, _)) = scripts.iter().filter(|(c, _)| *c != "fa" && *c != "ar").find(|(_, n)| share(*n)) {
        return Some(code);
    }
    let words: Vec<String> = sample.split(|c: char| !(c.is_alphabetic() || c == '\'')).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    let mut best: Option<(&'static str, usize)> = None;
    let mut second = 0;
    for (code, list) in STOP_WORDS {
        let hits = words.iter().filter(|w| list.contains(&w.as_str())).count();
        match best {
            Some((_, b)) if hits <= b => second = second.max(hits),
            _ => {
                second = best.map_or(0, |(_, b)| b);
                best = Some((code, hits));
            }
        }
    }
    let (code, hits) = best?;
    (hits >= 3 && hits * 4 >= second * 5).then_some(code)
}

/// Languages offered for an imported meeting (code, name in that language).
pub const LANGUAGES: &[(&str, &str)] = &[ // i18n: not UI
    ("en", "English"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("nl", "Nederlands"),
    ("pt", "Português"),
    ("pl", "Polski"),
    ("tr", "Türkçe"),
    ("sv", "Svenska"),
    ("ru", "Русский"),
    ("ar", "العربية"),
    ("fa", "فارسی"),
    ("he", "עברית"),
    ("el", "Ελληνικά"),
    ("hi", "हिन्दी"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("zh", "中文"),
    ("th", "ไทย"),
];

/// A language's name from [`LANGUAGES`], else Whisper's.
pub fn language_display(code: &str) -> String {
    LANGUAGES.iter().find(|(c, _)| *c == code).map_or_else(|| crate::summary::language_name(code), |(_, n)| n.to_string())
}

/// "2026-10-27 14:00", "2026-10-27T14:00", "27.10.2026 14:00", "2026-10-27"
/// (at 09:00), "14:00" (today) → a local time.
pub fn parse_date_time(s: &str, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let s = s.trim();
    let naive = ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%d.%m.%Y %H:%M", "%d.%m.%Y, %H:%M", "%d.%m.%y %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .or_else(|| {
            ["%Y-%m-%d", "%d.%m.%Y", "%d.%m.%y"]
                .iter()
                .find_map(|f| NaiveDate::parse_from_str(s, f).ok())
                .and_then(|d| d.and_hms_opt(9, 0, 0))
        })
        .or_else(|| NaiveTime::parse_from_str(s, "%H:%M").ok().map(|t| now.date_naive().and_time(t)))?;
    Local.from_local_datetime(&naive).earliest()
}

/// "45" (minutes), "45m", "45 min", "1h", "1h30", "1:30:00", "90s" → seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim().to_lowercase().replace(' ', "");
    if s.is_empty() {
        return None;
    }
    if s.contains(':') {
        return parse_time(&s).filter(|d| *d > 0.);
    }
    let num = |t: &str| t.replace(',', ".").parse::<f64>().ok().filter(|n| *n >= 0.);
    let secs = if let Some((h, m)) = s.split_once('h') {
        let m = m.trim_end_matches("min").trim_end_matches('m');
        num(h)? * 3600. + if m.is_empty() { 0. } else { num(m)? * 60. }
    } else if let Some(v) = s.strip_suffix("min").or_else(|| s.strip_suffix('m')) {
        num(v)? * 60.
    } else if let Some(v) = s.strip_suffix('s') {
        num(v)?
    } else {
        num(&s)? * 60.
    };
    (secs > 0.).then_some(secs)
}

/// What to import.
pub struct Request<'a> {
    pub text: &'a str,
    /// `None`: "Besprechung <date>" until the summary names it.
    pub title: Option<&'a str>,
    pub started_at: DateTime<Local>,
    /// `None`: from the times, else estimated from the length.
    pub duration_secs: Option<f64>,
    /// A language code; `None` or "auto": detected from the text.
    pub language: Option<&'a str>,
}

pub struct Imported {
    pub meeting_id: i64,
    pub format: Format,
    pub lines: usize,
    pub speakers: usize,
    pub timed: bool,
    pub duration_secs: f64,
    /// The meeting's language code ("auto" if it couldn't be told).
    pub language: String,
}

/// Saves the text as a meeting without audio (the search index, summary and
/// notes are up to the caller).
pub fn import(store: &mut Store, req: &Request) -> Result<Imported> {
    let parsed = parse(req.text);
    anyhow::ensure!(!parsed.lines.is_empty(), "{}", tr("Im Text steht nichts, woraus sich eine Besprechung machen ließe."));
    let duration = req.duration_secs.filter(|d| *d > 0.).unwrap_or_else(|| parsed.estimated_duration());
    let segments = parsed.segments(duration);
    let language = req
        .language
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "auto")
        .map(str::to_string)
        .or_else(|| detect_language(req.text).map(str::to_string))
        .unwrap_or_else(|| "auto".into());
    let title = req
        .title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| trf("Besprechung {}", &[&crate::i18n::date_time(&req.started_at)]));
    // Meetings are keyed by folder; an imported one gets a unique one that
    // never exists on disk.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = crate::paths::data_dir().join("imported").join(format!(
        "text-{}-{}-{}",
        Local::now().timestamp_millis(),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let id = store.insert_meeting(&NewMeeting { title: &title, started_at: req.started_at, duration_secs: duration, language: &language, dir: &dir }, &segments)?;
    let timed = parsed.timed();
    store.set_transcript_info(id, if timed { ENGINE } else { ENGINE_UNTIMED }, None)?;
    Ok(Imported {
        meeting_id: id,
        format: parsed.format,
        lines: segments.len(),
        speakers: parsed.speakers().len(),
        timed,
        duration_secs: duration,
        language,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speakers_and_texts(p: &Parsed) -> Vec<(Option<&str>, &str)> {
        p.lines.iter().map(|l| (l.speaker.as_deref(), l.text.as_str())).collect()
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1:02:03"), Some(3723.));
        assert_eq!(parse_time("01:02"), Some(62.));
        assert_eq!(parse_time("00:00:01.500"), Some(1.5));
        assert_eq!(parse_time("00:00:01,250"), Some(1.25));
        assert_eq!(parse_time("0:61"), None);
        assert_eq!(parse_time("12"), None);
        assert_eq!(parse_time("1:2"), None);
        assert_eq!(parse_time("ab:cd"), None);
        assert_eq!(parse_time("10:30am"), None);
    }

    #[test]
    fn plain_paragraphs_become_lines() {
        let p = parse("We talked about the budget.\nIt is ten percent under plan.\n\nAnna will send a forecast by Friday.\n");
        assert_eq!(p.format, Format::Plain);
        assert_eq!(
            speakers_and_texts(&p),
            vec![(None, "We talked about the budget. It is ten percent under plan."), (None, "Anna will send a forecast by Friday.")]
        );
        assert!(!p.timed());
    }

    #[test]
    fn plain_lines_without_blank_lines_stay_lines() {
        let p = parse("First point\nSecond point\nThird point");
        assert_eq!(p.lines.len(), 3);
        assert!(p.lines.iter().all(|l| l.speaker.is_none()));
    }

    #[test]
    fn name_colon_lines() {
        let p = parse("Anna: Morning everyone.\nBen: Morning! Shall we start?\nAnna: Yes, budget first.\nit continues here\n");
        assert_eq!(p.format, Format::Speakers);
        assert_eq!(
            speakers_and_texts(&p),
            vec![(Some("Anna"), "Morning everyone."), (Some("Ben"), "Morning! Shall we start?"), (Some("Anna"), "Yes, budget first. it continues here")]
        );
    }

    #[test]
    fn a_single_label_in_prose_is_no_speaker() {
        let p = parse("We met on Monday.\n\nNote: the budget is tight.\n\nEveryone agreed to wait.\n\nThe next meeting is in May.");
        assert!(p.lines.iter().all(|l| l.speaker.is_none()), "{:?}", p.lines);
        assert_eq!(p.lines[1].text, "Note: the budget is tight.");
        // Links and clock times aren't speakers either.
        let p = parse("See https://example.com/x for it\nAt 10:30 we start\nAt 11:00 we stop");
        assert!(p.lines.iter().all(|l| l.speaker.is_none()), "{:?}", p.lines);
    }

    #[test]
    fn bracketed_and_bare_timestamps() {
        let p = parse("[00:00:05] Anna: Hello.\n[00:01:10] Ben: Hi Anna.\n00:02:00 Anna: Let's begin.\n0:02:30 - Ben: Sure\n[03:00] just text");
        assert_eq!(p.format, Format::Timestamps);
        let starts: Vec<Option<f64>> = p.lines.iter().map(|l| l.start).collect();
        assert_eq!(starts, vec![Some(5.), Some(70.), Some(120.), Some(150.), Some(180.)]);
        assert_eq!(
            speakers_and_texts(&p),
            vec![(Some("Anna"), "Hello."), (Some("Ben"), "Hi Anna."), (Some("Anna"), "Let's begin."), (Some("Ben"), "Sure"), (Some("Ben"), "just text")]
        );
    }

    #[test]
    fn teams_header_lines() {
        let text = "Anna Schmidt   0:03\nOkay, let's start.\nThe budget first.\n\nBen Müller   0:15\nSure.\nOne question: when?\nOne question: who?\n\nAnna Schmidt   1:02:07\nThanks all.";
        let p = parse(text);
        assert_eq!(p.format, Format::Timestamps);
        assert_eq!(
            speakers_and_texts(&p),
            vec![(Some("Anna Schmidt"), "Okay, let's start. The budget first."), (Some("Ben Müller"), "Sure. One question: when? One question: who?"), (Some("Anna Schmidt"), "Thanks all.")]
        );
        assert_eq!(p.lines.iter().map(|l| l.start).collect::<Vec<_>>(), vec![Some(3.), Some(15.), Some(3727.)]);
        assert_eq!(p.clock_start, None);
    }

    #[test]
    fn teams_name_then_time_lines() {
        let p = parse("Anna Schmidt\n0:03\nOkay, let's start.\nBen Müller\n0:15\nSure, go ahead.");
        assert_eq!(speakers_and_texts(&p), vec![(Some("Anna Schmidt"), "Okay, let's start."), (Some("Ben Müller"), "Sure, go ahead.")]);
        assert_eq!(p.lines[1].start, Some(15.));
    }

    #[test]
    fn zoom_styles() {
        let p = parse("Anna (0:01:23): Hello there.\nBen (0:01:40): Hi.\n");
        assert_eq!(speakers_and_texts(&p), vec![(Some("Anna"), "Hello there."), (Some("Ben"), "Hi.")]);
        assert_eq!(p.lines[0].start, Some(83.));
        // Zoom's saved chat transcript: clock times become times from the first.
        let p = parse("[Anna Schmidt] 14:03:12\nHello everyone\n\n[Ben] 14:04:00\nHi Anna\n");
        assert_eq!(speakers_and_texts(&p), vec![(Some("Anna Schmidt"), "Hello everyone"), (Some("Ben"), "Hi Anna")]);
        assert_eq!(p.clock_start, Some(14. * 3600. + 3. * 60. + 12.));
        assert_eq!(p.lines.iter().map(|l| l.start).collect::<Vec<_>>(), vec![Some(0.), Some(48.)]);
    }

    #[test]
    fn meet_marker_lines() {
        let p = parse("Weekly sync - Transcript\n00:00:00\n\nAnna: Hi all.\nBen: Hello.\n00:05:00\nAnna: Moving on.\n");
        assert_eq!(p.lines[0].text, "Weekly sync - Transcript");
        assert_eq!(p.lines[1].start, Some(0.));
        assert_eq!(p.lines[2].start, None);
        assert_eq!(p.lines[3].start, Some(300.));
        assert_eq!(p.lines[3].speaker.as_deref(), Some("Anna"));
    }

    #[test]
    fn webvtt_with_voice_tags() {
        let text = "WEBVTT\nKind: captions\n\nNOTE made by Teams\n\n1a2b-0\n00:00:01.000 --> 00:00:03.500\n<v Anna Schmidt>Okay, let's start.</v>\n\n00:00:03.600 --> 00:00:05.000\n<v Anna Schmidt>Budget first.</v>\n\n00:00:06.000 --> 00:00:08.000\n<v.loud Ben Müller>Sure &amp; fine.</v>\n\n00:00:09.000 --> 00:00:10.000\n<c.yellow>Unnamed</c> <00:00:09.500>words\n";
        let p = parse(text);
        assert_eq!(p.format, Format::WebVtt);
        assert_eq!(
            speakers_and_texts(&p),
            vec![(Some("Anna Schmidt"), "Okay, let's start. Budget first."), (Some("Ben Müller"), "Sure & fine. Unnamed words")]
        );
        assert_eq!(p.lines[0].start, Some(1.));
        assert_eq!(p.lines[0].end, Some(5.));
    }

    #[test]
    fn srt_with_name_prefixes() {
        let text = "1\n00:00:01,000 --> 00:00:02,000\nAnna: Hello\n\n2\n00:00:02,500 --> 00:00:04,000\nhow are you?\n\n3\n00:00:10,000 --> 00:00:12,000\nBen: <i>Fine</i>, thanks.\n\n4\n00:00:20,000 --> 00:00:21,000\nAnna: Good.\n";
        let p = parse(text);
        assert_eq!(p.format, Format::Srt);
        assert_eq!(
            speakers_and_texts(&p),
            vec![(Some("Anna"), "Hello how are you?"), (Some("Ben"), "Fine, thanks."), (Some("Anna"), "Good.")]
        );
        // SRT without speakers: cues merge into readable lines.
        let p = parse("1\n00:00:01,000 --> 00:00:02,000\nWe should\n\n2\n00:00:02,100 --> 00:00:03,000\nstart now.\n");
        assert_eq!(speakers_and_texts(&p), vec![(None, "We should start now.")]);
    }

    #[test]
    fn markdown_bold_names_and_crlf() {
        let p = parse("**Anna:** Hi.\r\n**Ben:** Hello.\r\n**Anna:** Bye.\r\n");
        assert_eq!(speakers_and_texts(&p), vec![(Some("Anna"), "Hi."), (Some("Ben"), "Hello."), (Some("Anna"), "Bye.")]);
    }

    #[test]
    fn long_paragraphs_are_split_at_sentences() {
        let sentence = "This is a sentence about the budget and the plan for next year. ";
        let p = parse(&sentence.repeat(30));
        assert!(p.lines.len() >= 3, "{}", p.lines.len());
        assert!(p.lines.iter().all(|l| l.text.len() <= MAX_LINE + 70));
    }

    #[test]
    fn untimed_lines_are_spread_by_length() {
        let p = parse("one two three four\n\nfive six\n\nseven eight");
        let segs = p.segments(80.);
        let starts: Vec<f64> = segs.iter().map(|s| s.start_secs).collect();
        assert_eq!(starts, vec![0., 40., 60.]);
        assert_eq!(segs.last().unwrap().end_secs, 80.);
        assert!(segs.iter().all(|s| s.source == Source::System && s.end_secs > s.start_secs));
        // Estimated: 150 words a minute, at least a minute.
        assert_eq!(p.estimated_duration(), 60.);
        let long = parse(&"word ".repeat(1500));
        assert_eq!(long.estimated_duration(), 600.);
    }

    #[test]
    fn timed_lines_fill_gaps_between_times() {
        let p = parse("[00:00:10] Anna: one two\nBen: three four\n[00:00:30] Anna: five\n");
        let segs = p.segments(p.estimated_duration());
        assert_eq!(segs[0].start_secs, 10.);
        assert_eq!(segs[1].start_secs, 20.);
        assert_eq!(segs[2].start_secs, 30.);
        assert_eq!(segs[0].end_secs, 20.);
        assert!(segs[2].end_secs > 30.);
        assert!(p.estimated_duration() > 30.);
    }

    #[test]
    fn languages() {
        assert_eq!(detect_language("We talked about the budget and it is not what we planned for this year, so we will have to cut."), Some("en"));
        assert_eq!(detect_language("Wir haben über das Budget gesprochen und es ist nicht das, was wir für dieses Jahr geplant haben."), Some("de"));
        assert_eq!(detect_language("Nous avons parlé du budget et ce n'est pas ce que nous avions prévu pour cette année."), Some("fr"));
        assert_eq!(detect_language("Hablamos del presupuesto y no es lo que planeamos para este año, pero está bien."), Some("es"));
        assert_eq!(detect_language("تحدثنا عن الميزانية وهي ليست كما خططنا لهذا العام"), Some("ar"));
        assert_eq!(detect_language("Мы обсудили бюджет, и он не такой, как мы планировали"), Some("ru"));
        assert_eq!(detect_language("今日は予算について話しました。来年の計画も確認しました。"), Some("ja"));
        assert_eq!(detect_language("ok"), None);
    }

    #[test]
    fn dates_and_durations() {
        let now = Local.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap();
        let at = |s| parse_date_time(s, now).map(|d| d.format("%Y-%m-%d %H:%M").to_string());
        assert_eq!(at("2026-10-27 14:00").as_deref(), Some("2026-10-27 14:00"));
        assert_eq!(at("2026-10-27T14:05").as_deref(), Some("2026-10-27 14:05"));
        assert_eq!(at("27.10.2026 9:30").as_deref(), Some("2026-10-27 09:30"));
        assert_eq!(at("2026-10-27").as_deref(), Some("2026-10-27 09:00"));
        assert_eq!(at("08:15").as_deref(), Some("2026-10-28 08:15"));
        assert_eq!(at("yesterday"), None);
        assert_eq!(parse_duration("45"), Some(2700.));
        assert_eq!(parse_duration("45 min"), Some(2700.));
        assert_eq!(parse_duration("45m"), Some(2700.));
        assert_eq!(parse_duration("1h30"), Some(5400.));
        assert_eq!(parse_duration("1h"), Some(3600.));
        assert_eq!(parse_duration("1:30:00"), Some(5400.));
        assert_eq!(parse_duration("90s"), Some(90.));
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("soon"), None);
    }

    #[test]
    fn decodes_files() {
        assert_eq!(decode(b"\xEF\xBB\xBFhi").unwrap(), "hi");
        assert_eq!(decode(&[0xFF, 0xFE, b'h', 0, b'i', 0]).unwrap(), "hi");
        assert!(decode(b"PK\x03\x04rest").is_err());
    }

    #[test]
    fn import_stores_an_untimed_meeting_without_audio() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("mostalgia-text-import-{}", std::process::id()));
        let mut store = Store::open(&dir.join("t.db"))?;
        let started_at = Local.with_ymd_and_hms(2026, 10, 27, 14, 0, 0).unwrap();
        let req = Request { text: "Anna: We agreed on the budget.\nBen: I will send the forecast.\nAnna: Thanks.", title: None, started_at, duration_secs: Some(600.), language: None };
        let done = import(&mut store, &req)?;
        assert_eq!((done.lines, done.speakers, done.timed, done.language.as_str()), (3, 2, false, "en"));
        let m = store.get(done.meeting_id)?.unwrap();
        assert!(m.is_text_import() && m.is_untimed());
        assert!(m.tracks().is_empty());
        assert_eq!(m.duration_secs, 600.);
        assert_eq!(m.segments[1].speaker_label(), "Ben");
        assert_eq!(m.segments[2].end_secs, 600.);
        assert!(store.list(10)?[0].text_import);
        // A second import doesn't replace the first.
        let again = import(&mut store, &Request { title: Some("Planning"), ..req })?;
        assert_ne!(again.meeting_id, done.meeting_id);
        assert_eq!(store.get(again.meeting_id)?.unwrap().title, "Planning");
        assert!(import(&mut store, &Request { text: " \n\n ", title: None, started_at, duration_secs: None, language: None }).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
