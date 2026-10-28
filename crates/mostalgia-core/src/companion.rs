//! Mori, the optional note-taking companion: answers questions about the
//! meeting being recorded right now (from the live transcript) and pops up
//! notable points while people speak. Both use the "ask" connection and
//! model; a missing or failing connection is an error naming it, never a
//! fallback to another provider.

use anyhow::{Context, Result, bail};
use crossbeam_channel::Sender;
use serde_json::Value;

use crate::chunker::format_ts;
use crate::config::{Config, NotesMode};
use crate::summary::SummaryRequest;
use crate::i18n::{tr, trf};
use crate::llm::{self, Message};

/// One line of the live transcript as Mori sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextLine {
    pub at_secs: f64,
    /// As the language model sees it: "Me", "Others", "Speaker 2", a name.
    pub speaker: String,
    pub text: String,
    /// Still being spoken (provisional text).
    pub interim: bool,
}

/// The transcript as text for the model: "[00:02:03] Me: …", newest lines
/// kept when it is longer than `max_chars`.
pub fn transcript_context(lines: &[ContextLine], max_chars: usize) -> String {
    let rendered: Vec<String> = lines
        .iter()
        .filter(|l| !l.text.trim().is_empty())
        .map(|l| {
            let still = if l.interim { " (still speaking)" } else { "" };
            format!("[{}] {}{still}: {}", format_ts(l.at_secs), l.speaker, l.text.trim())
        })
        .collect();
    let mut kept = Vec::new();
    let mut len = 0;
    for line in rendered.iter().rev() {
        if len + line.len() + 1 > max_chars && !kept.is_empty() {
            break;
        }
        len += line.len() + 1;
        kept.push(line.as_str());
    }
    kept.reverse();
    let mut out = String::new();
    if kept.len() < rendered.len() {
        out.push_str("… (earlier lines left out)\n");
    }
    out.push_str(&kept.join("\n"));
    out
}

/// Progress of a live answer.
#[derive(Clone, Debug)]
pub enum AnswerEvent {
    Delta(String),
    Done(String),
    Error(String),
}

/// Who Mori is, for every prompt she answers: her name, her manner, what
/// Mostalgia is, what she can do, and (truthfully) which model runs her.
pub fn persona(req: &SummaryRequest) -> String {
    format!(
        "You are Mori, the note-taking companion in Mostalgia — a local-first meeting recorder \
(\"Nostalgia, but for meetings you can actually recall in full\"). You're a small animated \
character who sits next to the user's meetings, writes along while people talk and remembers \
what was said. Your manner: warm, calm, a little witty — never at the user's expense — and \
concise. Answer in the language the user writes in.\n\n\
What you can do: in the chat the user picks a scope — \"Live meeting\" (the recording in \
progress, from its live transcript), \"This meeting\" (the meeting open in the app, from its \
transcript, summary and notes) or \"All meetings\" (searching the whole library). You cite \
the moments you rely on so the user can jump there and listen. While a recording runs you \
also pin up live notes — decisions, action items, open questions, numbers — which are saved \
with the meeting. You can't join calls, send messages or change settings.\n\n\
If asked who you are, you're Mori. You are not the language model itself: if asked which \
model powers you, say it's the connection chosen in Mostalgia's settings — currently {} with \
the model {} — and don't claim to be anything else.",
        req.connection.name, req.model
    )
}

const LIVE_SYSTEM: &str = "\
The user is in a meeting that is being recorded right now and asks you about it. \
You get the live transcript so far; it comes from automatic speech recognition, \
so silently correct obviously misheard words. Speakers: \"Me\" is the user, \
\"Others\" is everyone else on the call, \"Speaker N\" are separated voices, \
other labels are names. Lines marked (still speaking) are provisional.

Answer only from the transcript, concisely (a few sentences or short bullets), \
in the language of the question. Cite the moment sparingly as [[t:hh:mm:ss]] with the \
time of the transcript line (a span as [[t:hh:mm:ss-hh:mm:ss]]): at most one citation per \
sentence or bullet, placed at its end before the final punctuation. Write no other times in \
brackets. If the transcript doesn't say, say so plainly and don't guess.";

const MEETING_SYSTEM: &str = "\
The user has a recorded meeting open in Mostalgia and asks you about it. You get its \
transcript (from automatic speech recognition, so silently correct obviously misheard words), \
and its summary and notes if it has them. Speakers: \"Me\" is the user, \"Others\" is \
everyone else on the call, \"Speaker N\" are separated voices, other labels are names.

Answer only from this meeting, concisely (a few sentences or short bullets), in the language \
of the question. Cite the moment sparingly as [[t:hh:mm:ss]] with the time of the transcript \
line (a span as [[t:hh:mm:ss-hh:mm:ss]]): at most one citation per sentence or bullet, placed \
at its end before the final punctuation. Write no other times in brackets. If the meeting \
doesn't say, say so plainly and don't guess.";

/// Characters of a saved meeting sent with a "This meeting" question.
const MEETING_CONTEXT_CHARS: usize = 120_000;

/// Answers a question about a saved meeting (the one open in the app) from
/// its transcript, summary and notes. Streams to `events`; always ends
/// with `Done` or `Error`.
pub fn ask_meeting(question: &str, history: &[(String, String)], meeting_id: i64, events: &Sender<AnswerEvent>) {
    let result = (|| -> Result<llm::Completion> {
        let req = Config::load().ask_request()?;
        let store = crate::store::Store::open(&crate::paths::database())?;
        let meeting = store.get(meeting_id)?.context(tr("Besprechung nicht gefunden"))?;
        drop(store);
        let lines: Vec<ContextLine> = meeting
            .segments
            .iter()
            .map(|s| ContextLine { at_secs: s.start_secs, speaker: crate::summary::llm_speaker(s.speaker_label()), text: s.text.clone(), interim: false })
            .collect();
        let mut context = format!(
            "Meeting \"{}\" on {}, {} min.\n\n",
            meeting.title,
            meeting.started_at.format("%Y-%m-%d %H:%M"),
            (meeting.duration_secs / 60.0).round()
        );
        if let Some(summary) = &meeting.summary {
            context.push_str(&format!("Summary:\n{}\n\n", summary.markdown.trim()));
        }
        if !meeting.notes.is_empty() {
            context.push_str("Notes taken during the meeting:\n");
            for n in &meeting.notes {
                context.push_str(&format!("[{}] {}: {}\n", format_ts(n.at_secs), n.kind, n.text));
            }
            context.push('\n');
        }
        let budget = MEETING_CONTEXT_CHARS.saturating_sub(context.len()).max(10_000);
        context.push_str(&format!("Transcript:\n{}", transcript_context(&lines, budget)));
        let mut messages = Vec::new();
        for (q, a) in history {
            messages.push(Message::user(q.clone()));
            messages.push(Message::assistant(a.clone()));
        }
        messages.push(Message::user(format!("{context}\n\nQuestion: {question}")));
        let system = format!("{}\n\n{MEETING_SYSTEM}", persona(&req));
        let mut on_delta = |d: &str| {
            let _ = events.send(AnswerEvent::Delta(d.to_string()));
        };
        llm::chat(&req.connection, &req.model, req.effort.as_deref(), &system, &messages, 16_000, &mut on_delta)
    })();
    let _ = events.send(match result {
        Ok(done) => AnswerEvent::Done(done.text.trim().to_string()),
        Err(e) => AnswerEvent::Error(format!("{e:#}")),
    });
}

/// Characters of transcript sent with a live question (the newest part).
const LIVE_CONTEXT_CHARS: usize = 60_000;

/// Answers a question about the meeting being recorded, from `transcript`
/// (see [`transcript_context`]), with earlier (question, answer) turns.
/// Streams to `events`; always ends with `Done` or `Error`.
pub fn ask_live(question: &str, history: &[(String, String)], transcript: &str, events: &Sender<AnswerEvent>) {
    let result = Config::load().ask_request().and_then(|req| {
        let messages = live_messages(question, history, transcript);
        let mut on_delta = |d: &str| {
            let _ = events.send(AnswerEvent::Delta(d.to_string()));
        };
        let system = format!("{}\n\n{LIVE_SYSTEM}", persona(&req));
        llm::chat(&req.connection, &req.model, req.effort.as_deref(), &system, &messages, 16_000, &mut on_delta)
    });
    let _ = events.send(match result {
        Ok(done) => AnswerEvent::Done(done.text.trim().to_string()),
        Err(e) => AnswerEvent::Error(format!("{e:#}")),
    });
}

fn live_messages(question: &str, history: &[(String, String)], transcript: &str) -> Vec<Message> {
    let mut messages = Vec::new();
    for (q, a) in history {
        messages.push(Message::user(q.clone()));
        messages.push(Message::assistant(a.clone()));
    }
    let transcript = if transcript.trim().is_empty() { "(nothing has been said yet)" } else { transcript };
    let mut context = transcript.to_string();
    if context.len() > LIVE_CONTEXT_CHARS {
        let cut = context.len() - LIVE_CONTEXT_CHARS;
        let cut = (cut..context.len()).find(|i| context.is_char_boundary(*i)).unwrap_or(0);
        context = format!("…{}", &context[cut..]);
    }
    messages.push(Message::user(format!("Live transcript so far:\n\n{context}\n\nQuestion: {question}")));
    messages
}

/// What kind of point Mori noticed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointKind {
    Decision,
    Action,
    Question,
    Fact,
}

impl PointKind {
    pub const ALL: [PointKind; 4] = [PointKind::Decision, PointKind::Action, PointKind::Question, PointKind::Fact];

    pub fn as_str(self) -> &'static str {
        match self {
            PointKind::Decision => "decision",
            PointKind::Action => "action",
            PointKind::Question => "question",
            PointKind::Fact => "fact",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|k| k.as_str() == s || s.trim_end_matches('s') == k.as_str())
    }

    pub fn label(self) -> &'static str {
        match self {
            PointKind::Decision => tr("Entscheidung"),
            PointKind::Action => tr("Aufgabe"),
            PointKind::Question => tr("Offene Frage"),
            PointKind::Fact => tr("Zahl / Fakt"),
        }
    }
}

/// A notable point from the live transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub kind: PointKind,
    pub text: String,
    pub at_secs: f64,
}

const POINTS_SYSTEM: &str = "\
You take notes live while a meeting is being recorded. Every few seconds you get the \
meeting so far (a running memory of its earlier part and/or the transcript), the notes \
already taken, and the newest lines. Decide whether the newest lines hold something \
worth a note card for the person recording:
- a decision the group landed on (\"let's do X\", \"agreed\", \"okay, so we ...\"),
- an action item: a task someone takes on or is given, with the owner (\"I'll ...\", \"could you ...\" and a yes),
- an open question the group left unanswered or parked,
- a key figure: a metric, amount or date that will matter later.

Most of the time the newest lines hold none of these: then reply []. Skip small talk, \
ideas and opinions still being discussed, and anything already in the notes, even in \
other words. Never invent owners or deadlines.

Each note must make sense on its own, read days later without the transcript: say who, \
what and about what, with names and the subject instead of pronouns or \"it\" / \"that\" \
(\"Anna sends the revised Q3 forecast by Friday\", not \"She'll send it Friday\"). Use the \
earlier part of the meeting to resolve who and what is meant.

The transcript comes from speech recognition (silently correct obviously misheard \
words); \"Me\" is the user, \"Others\" everyone else on the call. Only take notes from \
the lines under \"Newest lines\"; everything else is context.

Reply with ONLY a JSON array, no other text:
[{\"kind\": \"decision\" | \"action\" | \"question\" | \"fact\", \"text\": \"...\", \"at_secs\": <seconds of the line it comes from>}]

Each text is one sentence (at most about 20 words), written in the language the people \
in the transcript speak (an English meeting gets English notes), whatever language these \
instructions or earlier notes are in.";

/// How many notes one request may bring: as it happens, usually none or
/// one (two only for clearly separate, important things); with a longer
/// interval a few more.
pub fn notes_limit(mode: NotesMode) -> usize {
    match mode {
        NotesMode::Live => 2,
        _ => 3,
    }
}

fn notes_system(req: &SummaryRequest, mode: NotesMode) -> String {
    let limit = match mode {
        NotesMode::Live => "Usually reply [] or one note; two only if two clearly separate, important things happened.",
        _ => "At most three notes, the most important ones.",
    };
    format!("{}\n\n{POINTS_SYSTEM} {limit}", persona(req))
}

/// What a live notes request looks at: a running memory of the meeting's
/// earlier part (`None` while the whole transcript fits), the finalized
/// lines since that memory ends, those after `new_after` (recording
/// seconds) being the newest, and the notes taken so far.
pub struct NotesInput<'a> {
    pub memory: Option<&'a str>,
    pub lines: &'a [ContextLine],
    pub new_after: f64,
    pub notes: &'a [Point],
}

/// Notes listed with a request (the newest).
const NOTES_LISTED: usize = 30;

/// The user message of a live notes request (see [`NotesInput`]) and the
/// length of its stable start. The parts that change least come first —
/// the memory, then the transcript, which only grows at its end — so
/// providers can cache that prefix; the notes and newest lines come last.
pub fn notes_message(input: &NotesInput, max_chars: usize) -> (String, usize) {
    let mut out = String::new();
    if let Some(memory) = input.memory.map(str::trim).filter(|m| !m.is_empty()) {
        out.push_str(&format!("Meeting memory (the part before the transcript below):\n{memory}\n\n"));
    }
    let (old, new): (Vec<ContextLine>, Vec<ContextLine>) = input.lines.iter().filter(|l| !l.interim).cloned().partition(|l| l.at_secs <= input.new_after);
    let new_text = points_context(&new, max_chars);
    if !old.is_empty() {
        let label = if input.memory.is_some() { "Transcript since then" } else { "Transcript so far" };
        out.push_str(&format!("{label}:\n{}\n\n", points_context(&old, max_chars.saturating_sub(new_text.len()).max(2_000))));
    }
    let stable = out.len();
    if input.notes.is_empty() {
        out.push_str("Notes so far: none\n\n");
    } else {
        out.push_str("Notes so far (don't repeat these):\n");
        for p in &input.notes[input.notes.len().saturating_sub(NOTES_LISTED)..] {
            out.push_str(&format!("- [{}] {}\n", p.kind.as_str(), p.text));
        }
        out.push('\n');
    }
    out.push_str(&format!("Newest lines:\n{}", if new.is_empty() { "(nothing)" } else { &new_text }));
    (out, stable)
}

/// Asks the notes connection (see `Config::notes_request`) for new points
/// in `message` (see [`notes_message`]; `stable` is the length of its
/// cacheable start), with as little thinking as the model allows. Points
/// that repeat `previous` ones are left out, and at most [`notes_limit`]
/// are kept. Errors name the provider.
pub fn live_points(message: &str, stable: usize, previous: &[Point], mode: NotesMode) -> Result<PointsReply> {
    let req = Config::load().notes_request()?;
    let system = notes_system(&req, mode);
    let started = std::time::Instant::now();
    let mut first: Option<f64> = None;
    let done = llm::chat_quick(&req.connection, &req.model, &system, message, stable, 2_000, &mut |d| {
        if first.is_none() && !d.is_empty() {
            first = Some(started.elapsed().as_secs_f64());
        }
    })?;
    let points = parse_points(&done.text).with_context(|| req.connection.name.clone())?;
    let mut reply = filter_points(points, previous, notes_limit(mode));
    reply.usage = done.usage;
    reply.first_token_secs = first;
    reply.model = done.model;
    Ok(reply)
}

const MEMORY_SYSTEM: &str = "\
You keep a compact running memory of a meeting that is being recorded, so that later \
notes can be understood without the full transcript. You get the previous memory (if \
any), the notes taken so far, and the next part of the transcript (from speech \
recognition; \"Me\" is the user, \"Others\" everyone else on the call). Reply with the \
updated memory only, as plain text of at most about 150 words in the language of the \
meeting: the topic and purpose, who takes part (names and roles where known), what was \
discussed and decided so far, and the threads still open. No preamble, no markdown headings.";

/// Transcript (characters, as sent) kept verbatim before older lines fold
/// into the meeting memory: about 12–15 minutes of talk, so shorter
/// meetings always go in whole.
pub const VERBATIM_MAX_CHARS: usize = 12_000;
/// When the memory takes over older lines, this much recent transcript
/// (seconds) stays verbatim.
pub const VERBATIM_KEEP_SECS: f64 = 180.;

/// Whether the meeting memory should take over older lines: when the
/// verbatim transcript (`lines`, from where the memory ends) has grown past
/// [`VERBATIM_MAX_CHARS`]. Returns the recording second up to which the
/// new memory covers (lines before it fold in).
pub fn memory_due(lines: &[ContextLine]) -> Option<f64> {
    let finalized: Vec<ContextLine> = lines.iter().filter(|l| !l.interim).cloned().collect();
    let chars = points_context(&finalized, usize::MAX).len();
    if chars <= VERBATIM_MAX_CHARS {
        return None;
    }
    let newest = finalized.iter().map(|l| l.at_secs).fold(0., f64::max);
    let cut = newest - VERBATIM_KEEP_SECS;
    finalized.iter().any(|l| l.at_secs < cut).then_some(cut)
}

/// The user message of a memory update: the previous memory, the notes,
/// and the lines it now takes over.
pub fn memory_message(previous: Option<&str>, lines: &[ContextLine], notes: &[Point]) -> String {
    let mut out = format!("Previous memory:\n{}\n\n", previous.map(str::trim).filter(|m| !m.is_empty()).unwrap_or("(none yet: this is the start of the meeting)"));
    if !notes.is_empty() {
        out.push_str("Notes so far:\n");
        for p in &notes[notes.len().saturating_sub(NOTES_LISTED)..] {
            out.push_str(&format!("- [{}] {}\n", p.kind.as_str(), p.text));
        }
        out.push('\n');
    }
    let finalized: Vec<ContextLine> = lines.iter().filter(|l| !l.interim).cloned().collect();
    out.push_str(&format!("Next part of the transcript:\n{}", transcript_context(&finalized, 60_000)));
    out
}

/// Updates the meeting memory (see [`memory_message`]) on the notes
/// connection, quickly. Errors name the provider.
pub fn meeting_memory(message: &str) -> Result<(String, Option<llm::Usage>)> {
    let req = Config::load().notes_request()?;
    let system = format!("{}\n\n{MEMORY_SYSTEM}", persona(&req));
    let done = llm::chat_quick(&req.connection, &req.model, &system, message, 0, 2_000, &mut |_| {})?;
    let mut text = done.text.as_str();
    while let Some(end) = text.find("</think>") {
        text = &text[end + "</think>".len()..];
    }
    let text = text.trim();
    anyhow::ensure!(!text.is_empty(), "{}: {}", req.connection.name, tr("leere Antwort"));
    Ok((text.to_string(), done.usage))
}

/// Drops points that repeat `previous` ones (or each other), then keeps at
/// most `limit`.
pub fn filter_points(points: Vec<Point>, previous: &[Point], limit: usize) -> PointsReply {
    let mut reply = PointsReply::default();
    for p in points {
        if previous.iter().chain(&reply.points).any(|q| same_note(&q.text, &p.text)) {
            reply.duplicates += 1;
        } else if reply.points.len() >= limit {
            reply.over_limit += 1;
        } else {
            reply.points.push(p);
        }
    }
    reply
}

/// Whether two note texts say the same thing: equal, or nearly the same
/// words (case, punctuation and order aside).
pub fn same_note(a: &str, b: &str) -> bool {
    let words = |s: &str| -> std::collections::BTreeSet<String> {
        s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
    };
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return a == b;
    }
    let common = a.intersection(&b).count() as f64;
    let union = a.union(&b).count() as f64;
    a == b || a.len().min(b.len()) >= 3 && common / union >= 0.75
}

/// What a [`live_points`] request brought: the new points, how many the
/// model returned that repeated earlier ones or went over the limit (left
/// out), and what it cost.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointsReply {
    pub points: Vec<Point>,
    pub duplicates: usize,
    pub over_limit: usize,
    pub usage: Option<llm::Usage>,
    /// Seconds until the first answer text arrived.
    pub first_token_secs: Option<f64>,
    /// The model that answered.
    pub model: String,
}

/// As it happens: no request sooner than this after the previous one started.
pub const LIVE_NOTES_GAP: f64 = 9.;
/// As it happens: new finalized words worth a request…
pub const LIVE_NOTES_MIN_WORDS: usize = 6;
/// …or, with fewer, how long the first of them may wait.
pub const LIVE_NOTES_MAX_WAIT: f64 = 15.;

/// Where the live notes stand, for [`notes_due`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NotesClock {
    /// A request is running.
    pub in_flight: bool,
    /// Seconds since the last request started (`None`: none yet).
    pub since_last: Option<f64>,
    /// Finalized lines since the last request…
    pub new_lines: usize,
    /// …their words…
    pub new_words: usize,
    /// …and the finalized speech (seconds) since then.
    pub new_speech: f64,
    /// How long the first of those lines has been waiting (seconds).
    pub waiting: f64,
}

/// Whether to ask for live notes now. As it happens: after a finished line
/// (the chunker cuts lines at pauses), one request at a time, at least
/// [`LIVE_NOTES_GAP`] apart, and only with enough new speech (or once a
/// short remark has waited [`LIVE_NOTES_MAX_WAIT`]). Every N s: after N
/// seconds of new speech.
pub fn notes_due(mode: NotesMode, c: &NotesClock) -> bool {
    if c.in_flight {
        return false;
    }
    match mode {
        NotesMode::Off => false,
        NotesMode::Every(secs) => c.new_speech >= secs as f64,
        NotesMode::Live => {
            c.new_lines > 0
                && c.since_last.is_none_or(|s| s >= LIVE_NOTES_GAP)
                && (c.new_words >= LIVE_NOTES_MIN_WORDS || c.waiting >= LIVE_NOTES_MAX_WAIT)
        }
    }
}

/// When `n` cards that arrive at `now` may appear: one after another, at
/// least `gap` seconds after the previous card (shown at `last`).
pub fn stagger(now: f64, last: Option<f64>, n: usize, gap: f64) -> Vec<f64> {
    let mut next = last.map_or(now, |l| (l + gap).max(now));
    (0..n)
        .map(|_| {
            let at = next;
            next += gap;
            at
        })
        .collect()
}

/// Words in a line of transcript.
pub fn word_count(text: &str) -> usize {
    text.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count()
}

/// Lines for [`live_points`]: like [`transcript_context`] but with the
/// seconds, so the model can say where a point was made.
pub fn points_context(lines: &[ContextLine], max_chars: usize) -> String {
    let with_secs: Vec<ContextLine> = lines
        .iter()
        .filter(|l| !l.interim)
        .map(|l| ContextLine { speaker: format!("{} ({:.0}s)", l.speaker, l.at_secs), ..l.clone() })
        .collect();
    transcript_context(&with_secs, max_chars)
}

/// Reads the model's point list: a JSON array, possibly in a code fence or
/// with text around it, or an object holding the array. Unknown kinds
/// count as facts; empty texts are skipped. Anything else is an error.
pub fn parse_points(answer: &str) -> Result<Vec<Point>> {
    // Reasoning models may put their thinking (with brackets of its own)
    // before the answer.
    let mut answer = answer;
    while let Some(end) = answer.find("</think>") {
        answer = &answer[end + "</think>".len()..];
    }
    let text = answer.trim();
    let text = match text.find("```") {
        Some(start) => {
            let inner = &text[start + 3..];
            let inner = inner.split_once('\n').map_or(inner, |(first, rest)| if first.trim().chars().all(char::is_alphanumeric) { rest } else { inner });
            inner.split("```").next().unwrap_or(inner)
        }
        None => text,
    };
    // The first `[` that starts a JSON array of objects (prose around it
    // may hold brackets too, e.g. an echoed "[123s 02:03]").
    let value = text.match_indices('[').find_map(|(a, _)| {
        let v = serde_json::Deserializer::from_str(&text[a..]).into_iter::<Value>().next()?.ok()?;
        v.as_array().is_some_and(|items| items.iter().all(Value::is_object)).then_some(v)
    });
    let value = match value {
        Some(v) => v,
        None => match (text.find('{'), text.rfind('}')) {
            (Some(a), Some(b)) if a < b => serde_json::from_str::<Value>(&text[a..=b]).ok().and_then(|v| {
                v.as_object().and_then(|o| o.values().find(|x| x.is_array()).cloned())
            }),
            _ => None,
        }
        .with_context(|| tr("Die Antwort war keine gültige Notizenliste (JSON)"))?,
    };
    let Some(items) = value.as_array() else {
        bail!("{}", tr("Die Antwort war keine gültige Notizenliste (JSON)"));
    };
    let mut out = Vec::new();
    for item in items {
        let text = item["text"].as_str().unwrap_or_default().trim();
        if text.is_empty() {
            continue;
        }
        let kind = item["kind"].as_str().and_then(PointKind::parse).unwrap_or(PointKind::Fact);
        let at_secs = item["at_secs"]
            .as_f64()
            .or_else(|| item["at_secs"].as_str().and_then(|s| s.trim().trim_end_matches('s').parse().ok()))
            .unwrap_or(0.0)
            .max(0.0);
        out.push(Point { kind, text: text.to_string(), at_secs });
    }
    Ok(out)
}

/// Why Mori can't talk to her connection yet, if she can't: no "ask"
/// connection, or one without its key.
pub fn setup_problem(config: &Config) -> Option<String> {
    match config.ask_request() {
        Ok(req) if req.connection.is_configured() => None,
        Ok(req) => Some(trf("{} ist nicht eingerichtet — in Einstellungen → KI hinterlegen.", &[&req.connection.name])),
        Err(e) => Some(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(at: f64, who: &str, text: &str, interim: bool) -> ContextLine {
        ContextLine { at_secs: at, speaker: who.into(), text: text.into(), interim }
    }

    #[test]
    fn parses_points_plain_fenced_and_wrapped() {
        let plain = r#"[{"kind":"decision","text":"Freeze marketing spend","at_secs":27}]"#;
        assert_eq!(parse_points(plain).unwrap(), vec![Point { kind: PointKind::Decision, text: "Freeze marketing spend".into(), at_secs: 27.0 }]);

        let fenced = "Here you go:\n```json\n[{\"kind\": \"action\", \"text\": \"Anna: forecast by Friday\", \"at_secs\": \"36.8\"},\n {\"kind\": \"questions\", \"text\": \"Who books the room?\", \"at_secs\": 50}]\n```\nHope that helps.";
        let p = parse_points(fenced).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].kind, PointKind::Action);
        assert_eq!(p[0].at_secs, 36.8);
        assert_eq!(p[1].kind, PointKind::Question);

        let wrapped = r#"{"points": [{"kind": "milestone", "text": "Launch on 3 May", "at_secs": 12}, {"kind": "fact", "text": " "}]}"#;
        let p = parse_points(wrapped).unwrap();
        assert_eq!(p, vec![Point { kind: PointKind::Fact, text: "Launch on 3 May".into(), at_secs: 12.0 }]);

        let prose = "At [120s 02:00] they agreed: [{\"kind\": \"decision\", \"text\": \"Ship on Friday\", \"at_secs\": 120}] (see [1]).";
        assert_eq!(parse_points(prose).unwrap(), vec![Point { kind: PointKind::Decision, text: "Ship on Friday".into(), at_secs: 120.0 }]);
        let thinking = "<think>Maybe [x]? The list: [\"a\"]</think>\n[{\"kind\": \"fact\", \"text\": \"Budget 10% under plan\", \"at_secs\": 15}]";
        assert_eq!(parse_points(thinking).unwrap().len(), 1);
        assert!(parse_points("[]").unwrap().is_empty());
        assert!(parse_points("Nothing new so far.").is_err());
        assert!(parse_points("[{\"kind\": \"decision\", \"text\": ").is_err());
        assert!(parse_points("{\"error\": \"x\"}").is_err());
    }

    #[test]
    fn builds_transcript_context() {
        let lines = vec![
            line(3.2, "Others", "Morning, everyone.", false),
            line(8.9, "Me", "Shall we start with the budget?", false),
            line(15.0, "Speaker 2", "", false),
            line(58.3, "Others", "And one more thing", true),
        ];
        let text = transcript_context(&lines, 10_000);
        assert_eq!(
            text,
            "[00:00:03] Others: Morning, everyone.\n[00:00:08] Me: Shall we start with the budget?\n[00:00:58] Others (still speaking): And one more thing"
        );
        // Only the newest lines fit; the cut is marked.
        let short = transcript_context(&lines, 70);
        assert!(short.starts_with("… (earlier lines left out)\n"));
        assert!(short.ends_with("And one more thing"));
        assert!(!short.contains("Morning"));
        // Points see seconds and no provisional text.
        let points = points_context(&lines, 10_000);
        assert!(points.contains("[00:00:08] Me (9s): Shall we"));
        assert!(!points.contains("one more thing"));
    }

    #[test]
    fn live_question_carries_history_and_transcript() {
        let m = live_messages("What was decided?", &[("Hi?".into(), "Hello.".into())], "[00:03] Me: Budget frozen.");
        assert_eq!(m.len(), 3);
        assert_eq!(m[1], Message::assistant("Hello."));
        assert!(m[2].content.contains("[00:03] Me: Budget frozen.") && m[2].content.ends_with("Question: What was decided?"));
    }

    fn clock(since_last: Option<f64>, lines: usize, words: usize, waiting: f64) -> NotesClock {
        NotesClock { in_flight: false, since_last, new_lines: lines, new_words: words, new_speech: 0., waiting }
    }

    #[test]
    fn live_notes_are_due_after_a_finished_line_debounced() {
        let live = NotesMode::Live;
        // The first finished line with enough words: at once.
        assert!(notes_due(live, &clock(None, 1, 9, 0.)));
        // Nothing new: never.
        assert!(!notes_due(live, &clock(Some(60.), 0, 0, 0.)));
        // Too soon after the last request.
        assert!(!notes_due(live, &clock(Some(LIVE_NOTES_GAP - 0.5), 2, 30, 1.)));
        assert!(notes_due(live, &clock(Some(LIVE_NOTES_GAP), 2, 30, 1.)));
        // One at a time.
        assert!(!notes_due(live, &NotesClock { in_flight: true, ..clock(Some(30.), 3, 30, 5.) }));
        // A short "Okay." waits for more, but not forever.
        assert!(!notes_due(live, &clock(Some(20.), 1, 1, 3.)));
        assert!(notes_due(live, &clock(Some(20.), 1, 1, LIVE_NOTES_MAX_WAIT)));
        // Every N s counts speech; off is off.
        let every = |speech| notes_due(NotesMode::Every(60), &NotesClock { new_speech: speech, ..clock(Some(5.), 1, 50, 0.) });
        assert!(!every(59.));
        assert!(every(60.));
        assert!(!notes_due(NotesMode::Off, &clock(None, 5, 50, 100.)));
    }

    #[test]
    fn cards_are_staggered() {
        assert_eq!(stagger(10., None, 1, 3.5), vec![10.]);
        assert_eq!(stagger(10., None, 3, 3.5), vec![10., 13.5, 17.]);
        // The previous card was shown a second ago: the next waits its turn.
        assert_eq!(stagger(10., Some(9.), 2, 3.5), vec![12.5, 16.]);
        // Long ago: at once.
        assert_eq!(stagger(10., Some(1.), 1, 3.5), vec![10.]);
        assert!(stagger(10., None, 0, 3.5).is_empty());
    }

    #[test]
    fn repeated_and_surplus_points_are_dropped() {
        let p = |text: &str| Point { kind: PointKind::Action, text: text.into(), at_secs: 1. };
        assert!(same_note("Anna sends the Q3 forecast by Friday.", "anna sends the q3 forecast by friday"));
        assert!(same_note("Anna sends the revised Q3 forecast by Friday", "By Friday Anna sends the revised Q3 forecast to us"));
        assert!(!same_note("Anna sends the Q3 forecast by Friday", "Ben books the room for Tuesday"));
        assert!(!same_note("Ship Friday", "Ship Monday"));
        let previous = vec![p("Anna sends the Q3 forecast by Friday")];
        let reply = filter_points(
            vec![p("Anna sends the Q3 forecast by Friday."), p("Ben books the room for Tuesday"), p("Ben books the room for Tuesday!"), p("Chris updates the onboarding emails"), p("Dana writes the report")],
            &previous,
            2,
        );
        assert_eq!(reply.points.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["Ben books the room for Tuesday", "Chris updates the onboarding emails"]);
        assert_eq!((reply.duplicates, reply.over_limit), (2, 1));
    }

    #[test]
    fn notes_message_keeps_a_stable_prefix_and_marks_the_newest_lines() {
        let lines = vec![
            line(3., "Others", "Morning, this is the Q3 planning with Anna and Ben.", false),
            line(20., "Me", "Anna, can you send the revised forecast by Friday?", false),
            line(31., "Others", "Sure, I'll send it Friday.", false),
            line(40., "Others", "half a sent", true),
        ];
        let notes = vec![Point { kind: PointKind::Fact, text: "Q3 budget is 10% under plan".into(), at_secs: 3. }];
        let input = NotesInput { memory: None, lines: &lines, new_after: 20., notes: &notes };
        let (msg, stable) = notes_message(&input, 10_000);
        assert!(msg.starts_with("Transcript so far:\n[00:00:03] Others (3s): Morning"));
        let (head, tail) = msg.split_at(stable);
        assert!(head.contains("(20s): Anna, can you") && !head.contains("I'll send it"));
        assert!(tail.starts_with("Notes so far (don't repeat these):\n- [fact] Q3 budget"));
        assert!(tail.ends_with("Newest lines:\n[00:00:31] Others (31s): Sure, I'll send it Friday."));
        assert!(!msg.contains("half a sent"));
        // The next run only grows the transcript: the old prefix stays as is
        // (what providers' prompt caches need).
        let more = [lines.clone(), vec![line(45., "Me", "Great, thanks.", false)]].concat();
        let (next, next_stable) = notes_message(&NotesInput { memory: None, lines: &more, new_after: 31., notes: &notes }, 10_000);
        assert!(next_stable > stable && next.starts_with(&msg[..stable - 1]));
        // With a memory it comes first.
        let (with_memory, _) = notes_message(&NotesInput { memory: Some("Q3 planning; Anna owns the forecast."), lines: &lines[2..], new_after: 20., notes: &[] }, 10_000);
        assert!(with_memory.starts_with("Meeting memory (the part before the transcript below):\nQ3 planning"));
        assert!(with_memory.contains("Notes so far: none") && !with_memory.contains("Transcript since then"));
    }

    #[test]
    fn memory_takes_over_long_transcripts_only() {
        let short: Vec<ContextLine> = (0..20).map(|i| line(i as f64 * 10., "Others", "We talked about the roadmap for a while.", false)).collect();
        assert_eq!(memory_due(&short), None);
        let long: Vec<ContextLine> = (0..400).map(|i| line(i as f64 * 5., "Others", "We talked about the roadmap and the budget for quite a while now.", false)).collect();
        let cut = memory_due(&long).unwrap();
        assert_eq!(cut, 1995. - VERBATIM_KEEP_SECS);
        let msg = memory_message(Some("Roadmap review."), &long[..10], &[]);
        assert!(msg.starts_with("Previous memory:\nRoadmap review.\n\nNext part of the transcript:\n[00:00:00] Others: We talked"));
        assert!(memory_message(None, &[], &[]).contains("(none yet"));
    }
}
