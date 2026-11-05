//! Meeting summaries from the configured AI connection (Claude, OpenAI,
//! DeepSeek, Ollama or any OpenAI-compatible endpoint).

use anyhow::Result;

use crate::chunker::format_ts;
use crate::connections::{self, Connection};
use crate::i18n::tr;
use crate::store::Meeting;

pub use crate::llm::{EFFORTS, ModelInfo};

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Thinking level for summaries unless configured otherwise.
pub const DEFAULT_EFFORT: &str = "high";

const SYSTEM_PROMPT: &str = "\
You summarize meeting transcripts for the person who recorded them.

The transcript comes from automatic speech recognition, so expect some \
misheard words; silently correct obvious ones from context. Lines are \
labelled by speaker: \"Me\" is the person who recorded the meeting \
(their microphone), \"Others\" is everyone else on the call (possibly \
several people). Other labels are named speakers. When people are named \
in the conversation, use their names.

Write the summary in the language named at the top of the user's message \
(translating the headings below into it), as Markdown in exactly this shape:

# <a short, specific title for the meeting, at most 8 words>

## Summary
<2-4 sentences: what the meeting was about and its outcome>

## Key points
- <the important points discussed, one per bullet>

## Decisions
- <decisions that were made; write \"None\" if there were none>

## Action items
- [ ] <task> — **Owner:** <owner, or \"open\" if nobody took it> — **Due:** <deadline, only if one was mentioned>

## Open questions
- <open questions; omit this section if there are none>

Only include what the transcript supports. Never invent owners, deadlines \
or decisions. Output only the Markdown.";

/// Like [`SYSTEM_PROMPT`], with key points topic by topic.
const DETAILED_PROMPT: &str = "\
You summarize meeting transcripts for the person who recorded them, in \
detail: a reader who missed the meeting should learn everything that \
mattered in it.

The transcript comes from automatic speech recognition, so expect some \
misheard words; silently correct obvious ones from context. Lines are \
labelled by speaker: \"Me\" is the person who recorded the meeting \
(their microphone), \"Others\" is everyone else on the call (possibly \
several people). Other labels are named speakers. When people are named \
in the conversation, use their names.

Write the summary in the language named at the top of the user's message \
(translating the headings below into it), as Markdown in exactly this shape:

# <a short, specific title for the meeting, at most 8 words>

## Summary
<one or two paragraphs: what the meeting was about, its main threads and its outcome>

## Key points
### <topic, in the order discussed> (<the time it starts, as written in the transcript>)
- <specific points: what was said and why, figures, names, examples, disagreements>

## Decisions
- <decisions that were made; write \"None\" if there were none>

## Action items
- [ ] <task> — **Owner:** <owner, or \"open\" if nobody took it> — **Due:** <deadline, only if one was mentioned>

## Open questions
- <open questions; omit this section if there are none>

Make every bullet specific, never a vague label like \"they discussed \
marketing\". Only include what the transcript supports. Never invent \
owners, deadlines or decisions. Output only the Markdown.";

/// For interviews, talks, podcasts and livestreams: the ideas, not tasks.
const TALK_PROMPT: &str = "\
You summarize a recorded talk, interview, podcast or livestream for \
someone who wants its substance without listening to all of it.

The transcript comes from automatic speech recognition, so expect some \
misheard words; silently correct obvious ones from context. Lines are \
labelled by speaker (\"Me\" is the person who recorded it, \"Others\" \
everyone else); when people are named in the conversation, use their names.

Write in the language named at the top of the user's message (translating \
the headings below into it), as Markdown in exactly this shape:

# <a short, specific title, at most 8 words>

## Summary
<one or two paragraphs: who is speaking, what it is about and its core message>

## Main ideas
### <idea or topic, in the order it comes up> (<the time it starts, as written in the transcript>)
- <the claim, the reasoning behind it, figures and examples given>

## Advice & takeaways
- <concrete advice or lessons a listener can act on>

## Stories & examples
- <a story or example that was told, and the point it makes>

## Memorable quotes
- \"<a striking line, close to verbatim>\" — <speaker>

## Mentioned
- <books, people, companies, tools or resources mentioned, with a few words of context; omit this section if there are none>

Be specific and keep the speaker's own frameworks, names and numbers. Only \
include what the transcript supports. Output only the Markdown.";

/// For lectures, courses and tutorials: what to learn from it.
const LECTURE_PROMPT: &str = "\
You turn the transcript of a lecture, course session, tutorial or training \
into study notes for someone learning the subject.

The transcript comes from automatic speech recognition, so expect some \
misheard words; silently correct obvious ones from context, especially \
technical terms.

Write in the language named at the top of the user's message (translating \
the headings below into it), as Markdown in exactly this shape:

# <the subject, at most 8 words>

## Summary
<one paragraph: what was taught and why it matters>

## Concepts
### <concept, in the order taught> (<the time it starts, as written in the transcript>)
- <what it is, how it works, examples, formulas or steps as explained>

## Key terms
- **<term>**: <its meaning as used here>

## Takeaways
- <the most important things to remember>

## Review questions
- <a question that checks understanding> — <a short answer>

Explain clearly and keep the lecturer's examples. Only include what the \
transcript supports. Output only the Markdown.";

/// Notes on one part of a long transcript, combined into the summary later.
const NOTES_PROMPT: &str = "\
You take detailed notes on one part of a long recording's transcript (a \
meeting, call, interview, talk or podcast). The notes of all parts are \
combined into one summary later, and the transcript itself is not read \
again, so whatever your notes leave out is lost.

The transcript comes from automatic speech recognition, so expect some \
misheard words; silently correct obvious ones from context. \"Me\" is the \
person who recorded it, \"Others\" everyone else on the call; other labels \
are named speakers. When people are named in the conversation, use their \
names.

Write the notes in the language named at the top of the user's message, as \
Markdown: one heading per topic, in the order they come up, then bullets.

### <topic> (<the time it starts, as written in the transcript>)
- <a specific point: a claim and its reasoning, a figure, a name, an \
example or story, a piece of advice, a disagreement>

Mark a decision as \"**Decision:** …\", a task as \"**Task:** … — owner, \
due date if one was said\", an open question as \"**Question:** …\", a \
striking line as \"**Quote:** “…” — speaker\" and a book, person, tool \
or resource that is recommended or mentioned as \"**Mentioned:** …\". \
Cover the whole part, keep concrete details and memorable wording, and \
leave out small talk and filler. Only note what the transcript says. No \
introduction or conclusion; output only the notes.";

/// A speaker label as the language model sees it: stored labels ("Ich",
/// "Andere", "Sprecher 2") in English, names unchanged.
pub fn llm_speaker(label: &str) -> String {
    match label {
        "Ich" => "Me".into(),
        "Andere" => "Others".into(),
        other => match other.strip_prefix("Sprecher ") { // i18n: not UI
            Some(n) if n.parse::<usize>().is_ok() => format!("Speaker {n}"),
            _ => other.to_string(),
        },
    }
}

/// The kind of summary to write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Template {
    /// Meetings in brief: summary, key points, decisions, action items.
    #[default]
    Standard,
    /// The same sections, with key points topic by topic.
    Detailed,
    /// Talks, interviews, podcasts: ideas, advice, stories, quotes.
    Talk,
    /// Lectures and courses: concepts, terms, takeaways, review questions.
    Lecture,
}

impl Template {
    pub const ALL: [Template; 4] = [Template::Standard, Template::Detailed, Template::Talk, Template::Lecture];

    /// Stored in the config and with the summary.
    pub fn id(self) -> &'static str {
        match self {
            Template::Standard => "standard",
            Template::Detailed => "detailed",
            Template::Talk => "talk",
            Template::Lecture => "lecture",
        }
    }

    /// Unknown or missing ids are the standard summary.
    pub fn parse(id: &str) -> Self {
        Self::ALL.into_iter().find(|t| t.id() == id).unwrap_or_default()
    }

    /// The name shown in the app (German source text, through `tr`).
    pub fn label(self) -> &'static str {
        match self {
            Template::Standard => tr("Standard"),
            Template::Detailed => tr("Ausführlich"),
            Template::Talk => tr("Vortrag & Podcast"),
            Template::Lecture => tr("Vorlesung & Kurs"),
        }
    }

    /// One line on what it writes.
    pub fn hint(self) -> &'static str {
        match self {
            Template::Standard => tr("Kurz: Kernpunkte, Entscheidungen, Aufgaben"),
            Template::Detailed => tr("Wie Standard, Kernpunkte Thema für Thema mit Zeitmarken"),
            Template::Talk => tr("Ideen, Ratschläge, Geschichten, Zitate, Erwähntes"),
            Template::Lecture => tr("Konzepte, Begriffe, Merksätze, Kontrollfragen"),
        }
    }

    /// Whether it has the meeting sections (summary, key points, decisions,
    /// action items, open questions) that recaps and the decision log read.
    pub fn has_meeting_sections(self) -> bool {
        matches!(self, Template::Standard | Template::Detailed)
    }

    fn system_prompt(self) -> &'static str {
        match self {
            Template::Standard => SYSTEM_PROMPT,
            Template::Detailed => DETAILED_PROMPT,
            Template::Talk => TALK_PROMPT,
            Template::Lecture => LECTURE_PROMPT,
        }
    }
}

/// What to summarize with.
#[derive(Clone, Debug)]
pub struct SummaryRequest {
    pub connection: Connection,
    pub model: String,
    /// Language code to write in (e.g. "en"); `None` = the meeting's own.
    pub language: Option<String>,
    /// Thinking level; ignored where the model has no effort control.
    pub effort: Option<String>,
    pub template: Template,
}

/// A generated summary: the Markdown and the title from its first heading.
pub struct GeneratedSummary {
    pub title: Option<String>,
    pub markdown: String,
    /// Display name of the connection that wrote it.
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub template: Template,
}

/// The name to show for a summary's stored provider (older summaries
/// stored a preset id such as "claude").
pub fn provider_display(stored: &str) -> String {
    connections::PRESETS
        .iter()
        .find(|p| p.id == stored)
        .map_or_else(|| stored.to_string(), |p| tr(p.name).to_string())
}

/// Renders a meeting's transcript the way the summary prompt expects.
/// (A meeting imported from text: no made-up times, and lines without a
/// speaker have no label.)
pub fn transcript_text(meeting: &Meeting) -> String {
    transcript_lines(meeting).into_iter().map(|(_, line)| line).collect()
}

/// The transcript as (start time, line ending in a newline) per segment.
fn transcript_lines(meeting: &Meeting) -> Vec<(f64, String)> {
    let (imported, untimed) = (meeting.is_text_import(), meeting.is_untimed());
    meeting
        .segments
        .iter()
        .map(|s| {
            let mut out = String::new();
            if !untimed {
                out.push_str(&format!("[{}] ", format_ts(s.start_secs)));
            }
            if !(imported && s.speaker.is_none()) {
                out.push_str(&format!("{}: ", llm_speaker(s.speaker_label())));
            }
            out.push_str(&s.text);
            out.push('\n');
            (s.start_secs, out)
        })
        .collect()
}

/// The language a summary is written in: the request's, else the
/// meeting's (as transcribed or detected), as a Whisper code like "en".
pub fn summary_language(req: &SummaryRequest, meeting: &Meeting) -> Option<String> {
    req.language
        .clone()
        .or_else(|| Some(meeting.language.clone()))
        .filter(|l| !l.is_empty() && l != "auto")
}

/// "en" → "English" (Whisper's language names), for prompts and labels.
pub fn language_name(code: &str) -> String {
    (0..100)
        .find(|&id| whisper_rs::get_lang_str(id) == Some(code))
        .and_then(whisper_rs::get_lang_str_full)
        .map(|name| {
            let mut c = name.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .unwrap_or_else(|| code.to_string())
}

/// From this many minutes (or [`LONG_CHARS`] of transcript) a template's
/// topic list is asked to grow with the recording.
const LONG_MINUTES: f64 = 20.0;
const LONG_CHARS: usize = 20_000;

/// The longest transcript (characters) the templates other than Standard
/// read in one go. Longer ones are read in parts of about half this size
/// (20-40 minutes of talk) first, each into detailed notes, and the summary
/// is written from the notes, so the middle of a long recording isn't
/// skimmed. Local models get smaller parts for their small context windows.
fn single_pass_limit(conn: &Connection) -> usize {
    if conn.is_local() { 40_000 } else { 60_000 }
}

/// Parts read at the same time on cloud connections.
const PARALLEL_PARTS: usize = 4;

fn language_line(req: &SummaryRequest, meeting: &Meeting, what: &str) -> String {
    match summary_language(req, meeting) {
        Some(code) => format!("Write the {what} in {}.", language_name(&code)),
        None => format!("Write the {what} in the language most of the transcript is in."),
    }
}

/// How many topic headings to aim for, by the recording's length.
fn detail_line(meeting: &Meeting, chars: usize) -> String {
    let minutes = meeting.duration_secs / 60.0;
    let untimed = if meeting.is_untimed() { " The transcript has no times, so leave the times out of the headings." } else { "" };
    if minutes < LONG_MINUTES && chars < LONG_CHARS {
        return format!("It is short: a few topic headings are enough.{untimed}");
    }
    // About one topic per 5-15 minutes; imported text goes by its length.
    let minutes = if minutes > 0.0 { minutes } else { chars as f64 / 900.0 };
    let topics = format!("{}-{}", (minutes / 15.0).ceil().max(3.0), (minutes / 5.0).ceil().max(5.0));
    format!(
        "It is long, so the summary must be long and detailed too: about {topics} topic headings with 2-6 specific bullets each, \
covering the whole recording from start to end; never squeeze it into a few generic bullets.{untimed}"
    )
}

fn header(meeting: &Meeting) -> String {
    format!("Meeting on {}, {} min.", meeting.started_at.format("%Y-%m-%d %H:%M"), (meeting.duration_secs / 60.0).round())
}

/// The standard summary's request, exactly as it has always been.
fn standard_message(req: &SummaryRequest, meeting: &Meeting, transcript: &str) -> String {
    format!("{}\n\n{}\n\n<transcript>\n{}</transcript>", language_line(req, meeting, "summary"), header(meeting), transcript)
}

/// Splits the transcript at segment boundaries into parts of at most about
/// `max_chars`, as (first start, last start, text).
fn split_parts(lines: &[(f64, String)], max_chars: usize) -> Vec<(f64, f64, String)> {
    let mut parts: Vec<(f64, f64, String)> = Vec::new();
    for (start, line) in lines {
        match parts.last_mut() {
            Some((_, end, text)) if text.len() + line.len() <= max_chars || text.is_empty() => {
                *end = *start;
                text.push_str(line);
            }
            _ => parts.push((*start, *start, line.clone())),
        }
    }
    // A short tail goes with the part before it.
    if parts.len() > 1 && parts.last().is_some_and(|p| p.2.len() < max_chars / 4) {
        let (_, end, text) = parts.pop().unwrap();
        let last = parts.last_mut().unwrap();
        last.1 = end;
        last.2.push_str(&text);
    }
    parts
}

pub fn summarize(req: &SummaryRequest, meeting: &Meeting) -> Result<GeneratedSummary> {
    summarize_with_progress(req, meeting, &mut |_, _| {})
}

/// Like [`summarize`]. With a template other than Standard a long
/// transcript is read in parts first, and `progress(done, total)` hears
/// about each step (parts, then the summary).
pub fn summarize_with_progress(req: &SummaryRequest, meeting: &Meeting, progress: &mut dyn FnMut(usize, usize)) -> Result<GeneratedSummary> {
    let lines = transcript_lines(meeting);
    let transcript: String = lines.iter().map(|(_, l)| l.as_str()).collect();
    anyhow::ensure!(!transcript.is_empty(), "{}", tr("Die Besprechung hat kein Transkript zum Zusammenfassen"));
    let limit = single_pass_limit(&req.connection);
    let user = if req.template == Template::Standard {
        standard_message(req, meeting, &transcript)
    } else if transcript.len() <= limit {
        let detail = detail_line(meeting, transcript.len());
        format!("{}\n\n{} {detail}\n\n<transcript>\n{transcript}</transcript>", language_line(req, meeting, "summary"), header(meeting))
    } else {
        let detail = detail_line(meeting, transcript.len());
        let parts = split_parts(&lines, limit / 2);
        let notes = part_notes(req, meeting, &parts, progress)?;
        let untimed = meeting.is_untimed();
        let mut body = String::new();
        for (i, ((from, to, _), notes)) in parts.iter().zip(&notes).enumerate() {
            let time = if untimed { String::new() } else { format!(" time=\"{}–{}\"", format_ts(*from), format_ts(*to)) };
            body.push_str(&format!("<notes part=\"{}\"{time}>\n{}\n</notes>\n", i + 1, notes.trim()));
        }
        format!(
            "{}\n\n{} {detail}\n\nThe transcript was too long to read at once, so it was read in {} parts and detailed notes were taken on each, in order. \
Write the summary from these notes, covering the whole recording; they are all you get.\n\n{body}",
            language_line(req, meeting, "summary"),
            header(meeting),
            parts.len()
        )
    };
    let done = crate::llm::complete(&req.connection, &req.model, req.effort.as_deref(), req.template.system_prompt(), &user)?;
    let markdown = done.text.trim().to_string();
    anyhow::ensure!(!markdown.is_empty(), "{}: {}", req.connection.name, tr("Das Modell hat eine leere Zusammenfassung geliefert"));
    let title = markdown
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    Ok(GeneratedSummary {
        title,
        markdown,
        provider: req.connection.name.clone(),
        model: done.model,
        effort: done.effort,
        template: req.template,
    })
}

/// Detailed notes on each part, in order; on cloud connections a few parts
/// at a time.
fn part_notes(req: &SummaryRequest, meeting: &Meeting, parts: &[(f64, f64, String)], progress: &mut dyn FnMut(usize, usize)) -> Result<Vec<String>> {
    let total = parts.len() + 1;
    let language = language_line(req, meeting, "notes");
    let message = |i: usize, text: &str| {
        format!("{language}\n\n{} This is part {} of {} of its transcript.\n\n<transcript>\n{text}</transcript>", header(meeting), i + 1, parts.len())
    };
    let notes_for = |i: usize, text: &str| -> Result<String> {
        let done = crate::llm::complete(&req.connection, &req.model, req.effort.as_deref(), NOTES_PROMPT, &message(i, text))?;
        let notes = done.text.trim().to_string();
        anyhow::ensure!(!notes.is_empty(), "{}: {}", req.connection.name, tr("Das Modell hat eine leere Zusammenfassung geliefert"));
        Ok(notes)
    };
    let notes_for = &notes_for;
    progress(0, total);
    let at_once = if req.connection.is_local() { 1 } else { PARALLEL_PARTS };
    let mut out = Vec::with_capacity(parts.len());
    for batch in parts.chunks(at_once).enumerate().map(|(b, c)| (b * at_once, c)) {
        let (first, chunk) = batch;
        let results: Vec<Result<String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk.iter().enumerate().map(|(j, (_, _, text))| scope.spawn(move || notes_for(first + j, text))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("notes thread panicked")))).collect()
        });
        for r in results {
            out.push(r?);
        }
        progress(out.len(), total);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize, len: usize) -> Vec<(f64, String)> {
        (0..n).map(|i| (i as f64 * 10.0, format!("{}\n", "x".repeat(len - 1)))).collect()
    }

    #[test]
    fn long_transcripts_split_at_segments() {
        let parts = split_parts(&lines(100, 100), 2_000);
        assert_eq!(parts.len(), 5);
        assert!(parts.iter().all(|p| p.2.len() == 2_000));
        assert_eq!((parts[1].0, parts[1].1), (200.0, 390.0));
        // Everything is kept, in order.
        assert_eq!(parts.iter().map(|p| p.2.len()).sum::<usize>(), 10_000);
        // A short tail joins the last part instead of standing alone.
        let parts = split_parts(&lines(42, 100), 2_000);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].2.len(), 2_200);
        // A single overlong line still makes a part.
        assert_eq!(split_parts(&lines(1, 5_000), 2_000).len(), 1);
    }
}
