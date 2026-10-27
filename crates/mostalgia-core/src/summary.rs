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

/// What to summarize with.
#[derive(Clone, Debug)]
pub struct SummaryRequest {
    pub connection: Connection,
    pub model: String,
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

pub fn summarize(req: &SummaryRequest, meeting: &Meeting) -> Result<GeneratedSummary> {
    let user = user_message(meeting)?;
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
