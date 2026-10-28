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

/// What to summarize with.
#[derive(Clone, Debug)]
pub struct SummaryRequest {
    pub connection: Connection,
    pub model: String,
    /// Language code to write in (e.g. "en"); `None` = the meeting's own.
    pub language: Option<String>,
    /// Thinking level; ignored where the model has no effort control.
    pub effort: Option<String>,
}

/// A generated summary: the Markdown and the title from its first heading.
pub struct GeneratedSummary {
    pub title: Option<String>,
    pub markdown: String,
    /// Display name of the connection that wrote it.
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
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
    let mut out = String::new();
    let (imported, untimed) = (meeting.is_text_import(), meeting.is_untimed());
    for s in &meeting.segments {
        if !untimed {
            out.push_str(&format!("[{}] ", format_ts(s.start_secs)));
        }
        if !(imported && s.speaker.is_none()) {
            out.push_str(&format!("{}: ", llm_speaker(s.speaker_label())));
        }
        out.push_str(&s.text);
        out.push('\n');
    }
    out
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

fn user_message(req: &SummaryRequest, meeting: &Meeting) -> Result<String> {
    let transcript = transcript_text(meeting);
    anyhow::ensure!(!transcript.is_empty(), "{}", tr("Die Besprechung hat kein Transkript zum Zusammenfassen"));
    let language = match summary_language(req, meeting) {
        Some(code) => format!("Write the summary in {}.", language_name(&code)),
        None => "Write the summary in the language most of the transcript is in.".to_string(),
    };
    Ok(format!(
        "{language}\n\nMeeting on {}, {} min.\n\n<transcript>\n{}</transcript>",
        meeting.started_at.format("%Y-%m-%d %H:%M"),
        (meeting.duration_secs / 60.0).round(),
        transcript
    ))
}

pub fn summarize(req: &SummaryRequest, meeting: &Meeting) -> Result<GeneratedSummary> {
    let user = user_message(req, meeting)?;
    let done = crate::llm::complete(&req.connection, &req.model, req.effort.as_deref(), SYSTEM_PROMPT, &user)?;
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
    })
}
