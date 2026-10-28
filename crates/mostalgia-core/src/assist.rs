//! The live answer helper: when someone in a recording asks the user a
//! question, the companion quietly suggests an answer from past meetings
//! and/or the documents drive, with sources. Spotting a question is a
//! cheap local check on finished transcript lines; the model then decides
//! whether it really is one the user could answer from what the search
//! found, and otherwise stays silent ("NONE").

use anyhow::Result;

use crate::ask::cite;
use crate::ask::search::{self, Filter};
use crate::companion::{ContextLine, word_count};
use crate::config::Config;
use crate::i18n::tr;
use crate::llm;
use crate::paths;
use crate::store::Store;

/// Where suggested answers come from (`Config::companion_answer_sources`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sources {
    Meetings,
    Documents,
    #[default]
    Both,
}

impl Sources {
    pub const ALL: [Sources; 3] = [Sources::Both, Sources::Meetings, Sources::Documents];

    pub fn id(self) -> &'static str {
        match self {
            Sources::Meetings => "meetings",
            Sources::Documents => "documents",
            Sources::Both => "both",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == s.trim())
    }

    pub fn label(self) -> &'static str {
        match self {
            Sources::Meetings => tr("Frühere Besprechungen"),
            Sources::Documents => tr("Dokumente"),
            Sources::Both => tr("Besprechungen und Dokumente"),
        }
    }

    fn meetings(self) -> bool {
        self != Sources::Documents
    }

    fn documents(self) -> bool {
        self != Sources::Meetings
    }
}

/// Fewest words for a line to count as a question ("Any news?" is too thin).
const MIN_WORDS: usize = 3;
/// Search hits from each source sent with a suggestion request.
const HITS_PER_SOURCE: usize = 6;
/// Characters of one hit sent to the model.
const HIT_CHARS: usize = 1_400;
/// Characters of the transcript before the question sent along.
const CONTEXT_CHARS: usize = 2_500;
/// Room for the answer.
const MAX_TOKENS: u32 = 700;

/// The question in a finished line from someone else, if it holds one:
/// the text up to its last question mark (also "؟"), at least
/// [`MIN_WORDS`] words. `others_heard`: the recording separates the user
/// ("Me") from the others; then the user's own questions don't count
/// (a microphone-only recording of a room has everyone as "Me").
pub fn question_in(line: &ContextLine, others_heard: bool) -> Option<String> {
    if line.interim || others_heard && line.speaker == "Me" {
        return None;
    }
    let text = line.text.trim();
    let end = text.rfind(['?', '؟'])?;
    let mark = text[end..].chars().next().map_or(1, char::len_utf8);
    let question = text[..end + mark].trim();
    // Only the last sentence or two: "OK, thanks. Anna, when does X ship?"
    let start = question[..question.len() - mark]
        .rfind(['.', '!', '?', '؟'])
        .map(|i| i + 1)
        .filter(|&i| word_count(&question[i..]) >= MIN_WORDS)
        .unwrap_or(0);
    let question = question[start..].trim();
    (word_count(question) >= MIN_WORDS).then(|| question.to_string())
}

/// A suggested answer: text with [[m:…]] / [[d:…]] markers.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub answer: String,
    pub model: String,
    /// Search hits that went with the request (for the log).
    pub hits: usize,
    pub usage: Option<llm::Usage>,
}

const SYSTEM: &str = "\
You help the user during a meeting that is being recorded right now. Someone in the meeting \
just asked a question. You get that question, the transcript right before it (from automatic \
speech recognition, so silently correct obviously misheard words; \"Me\" is the user) and \
search results from the user's past meetings and documents, each starting with its source marker.

First decide: is this a real question that the user could be expected to answer (asked to the \
user or to the room — not rhetorical, not answered by the asker in the same breath, not small \
talk), and do the search results actually contain the answer? If not, reply with exactly NONE.

Otherwise reply with only a suggested answer the user can say out loud: one to three short \
sentences, in the language of the question, using only facts from the search results, in the \
first person where natural. End each sentence with the marker of the result it relies on, exactly \
as given ([[m:…]] or [[d:…]]), before the final punctuation. No preamble, no quotes around it, no \
other citations.";

/// Characters of `s`, cut at a character boundary with "…".
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>().trim_end()) }
}

/// The search results for a question, each line starting with its marker.
pub fn sources_for(question: &str, sources: Sources) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if sources.meetings() {
        let store = Store::open(&paths::database())?;
        for h in search::search(&store, question, &Filter::default(), HITS_PER_SOURCE)? {
            out.push(format!("[[m:{}@{}]] {}", h.meeting_id, h.start_secs.round(), clip(&h.text, HIT_CHARS)));
        }
    }
    if sources.documents() {
        for h in crate::docs::search(question, None, HITS_PER_SOURCE)? {
            let at = if h.kind.paged() { format!("page {}", h.loc) } else { format!("section {}", h.loc) };
            let heading = if h.heading.is_empty() { String::new() } else { format!(" · {}", h.heading) };
            out.push(format!("[[d:{}@{}]] Document \"{}\" · {at}{heading}\n{}", h.document_id, h.loc, h.title, clip(&h.text, HIT_CHARS)));
        }
    }
    Ok(out)
}

/// The user message of a suggestion request.
pub fn message(question: &str, before: &[ContextLine], hits: &[String]) -> String {
    let context = crate::companion::transcript_context(before, CONTEXT_CHARS);
    format!(
        "Transcript right before the question:\n{context}\n\nThe question: {question}\n\nSearch results:\n\n{}\n\nSuggested answer (or NONE):",
        hits.join("\n\n")
    )
}

/// The model's reply as a suggestion: `None` for "NONE", an empty reply
/// or one that cites nothing (an answer without sources isn't from them).
pub fn parse_reply(reply: &str) -> Option<String> {
    let text = reply.trim().trim_matches('"').trim();
    let first = text.split_whitespace().next().unwrap_or_default().trim_matches(|c: char| !c.is_alphanumeric());
    if text.is_empty() || first.eq_ignore_ascii_case("none") {
        return None;
    }
    let text = ["Suggested answer:", "Answer:", "Antwort:", "Vorschlag:"].iter().fold(text, |t, p| t.strip_prefix(p).unwrap_or(t).trim_start());
    (!cite::parse(text).sources.is_empty()).then(|| text.to_string())
}

/// Searches `sources` for `question` and asks the notes connection (fast,
/// little thinking) for a suggested answer. `Ok(None)`: nothing to search,
/// no hits, or the model found no answer. Errors name the provider.
pub fn suggest(question: &str, before: &[ContextLine], sources: Sources) -> Result<Option<Suggestion>> {
    let hits = sources_for(question, sources)?;
    if hits.is_empty() {
        return Ok(None);
    }
    let req = Config::load().notes_request()?;
    let user = message(question, before, &hits);
    let done = llm::chat_quick(&req.connection, &req.model, SYSTEM, &user, 0, MAX_TOKENS, false, &mut |_| {})?;
    Ok(parse_reply(&done.text).map(|answer| Suggestion { answer, model: done.model, hits: hits.len(), usage: done.usage }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(speaker: &str, text: &str) -> ContextLine {
        ContextLine { at_secs: 10., speaker: speaker.into(), text: text.into(), interim: false }
    }

    #[test]
    fn spots_questions_from_others() {
        assert_eq!(question_in(&line("Others", "OK, thanks. Mosta, when does the migration ship?"), true).as_deref(), Some("Mosta, when does the migration ship?"));
        assert_eq!(question_in(&line("Speaker 2", "Wie hoch war das Budget für Q3? Weißt du das noch"), true).as_deref(), Some("Wie hoch war das Budget für Q3?"));
        assert!(question_in(&line("Me", "When does the migration ship?"), true).is_none());
        // Microphone only: everyone is "Me".
        assert!(question_in(&line("Me", "When does the migration ship?"), false).is_some());
        assert!(question_in(&line("Others", "Really?"), true).is_none());
        assert!(question_in(&line("Others", "The migration ships in May."), true).is_none());
        assert!(question_in(&ContextLine { interim: true, ..line("Others", "When does it ship then?") }, true).is_none());
        assert_eq!(question_in(&line("Others", "متى يتم إطلاق النسخة الجديدة؟"), true).as_deref(), Some("متى يتم إطلاق النسخة الجديدة؟"));
    }

    #[test]
    fn replies() {
        assert_eq!(parse_reply("NONE"), None);
        assert_eq!(parse_reply("None."), None);
        assert_eq!(parse_reply("  "), None);
        assert_eq!(parse_reply("It ships in May."), None);
        assert_eq!(parse_reply("Answer: It ships in May [[m:4@120]]."), Some("It ships in May [[m:4@120]].".into()));
        assert_eq!(parse_reply("\"Das VPN-Passwort läuft nach 90 Tagen ab [[d:2@3]].\""), Some("Das VPN-Passwort läuft nach 90 Tagen ab [[d:2@3]].".into()));
    }
}
