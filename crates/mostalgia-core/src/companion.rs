//! Mori, the optional note-taking companion: answers questions about the
//! meeting being recorded right now (from the live transcript) and pops up
//! notable points while people speak. Both use the "ask" connection and
//! model; a missing or failing connection is an error naming it, never a
//! fallback to another provider.

use anyhow::{Context, Result, bail};
use crossbeam_channel::Sender;
use serde_json::Value;

use crate::chunker::format_ts;
use crate::config::Config;
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

const LIVE_SYSTEM: &str = "\
You are Mori, a note-taking assistant inside the meeting recorder Mostalgia. \
The user is in a meeting that is being recorded right now and asks you about it. \
You get the live transcript so far; it comes from automatic speech recognition, \
so silently correct obviously misheard words. Speakers: \"Me\" is the user, \
\"Others\" is everyone else on the call, \"Speaker N\" are separated voices, \
other labels are names. Lines marked (still speaking) are provisional.

Answer only from the transcript, concisely (a few sentences or short bullets), \
in the language of the question. Mention the time [hh:mm:ss] when you refer to a \
moment. If the transcript doesn't say, say so plainly and don't guess.";

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
        llm::chat(&req.connection, &req.model, req.effort.as_deref(), LIVE_SYSTEM, &messages, 16_000, &mut on_delta)
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
You watch a live meeting transcript and pick out new notable points for the \
person recording it: decisions that were made, action items (with the owner, \
e.g. \"Anna: send the forecast by Friday\"), open questions, and important \
numbers or dates. The transcript comes from speech recognition; \"Me\" is the \
user, \"Others\" everyone else on the call.

Reply with ONLY a JSON array, no other text:
[{\"kind\": \"decision\" | \"action\" | \"question\" | \"fact\", \"text\": \"...\", \"at_secs\": <seconds where it was said>}]

Each text is one short sentence (at most about 20 words) in the language of \
the transcript. Only include what the transcript supports; never invent \
owners or deadlines. Skip small talk and everything already raised. If there \
is nothing new, reply with [].";

/// Asks the "ask" connection for new points in `transcript` (the recent
/// part of the live transcript, lines like "[123s 02:03] Me: …"), leaving
/// out `previous` ones. Errors name the provider.
pub fn live_points(transcript: &str, previous: &[Point]) -> Result<Vec<Point>> {
    let req = Config::load().ask_request()?;
    let mut user = String::new();
    if previous.is_empty() {
        user.push_str("Points already raised: none\n\n");
    } else {
        user.push_str("Points already raised (don't repeat these):\n");
        for p in previous {
            user.push_str(&format!("- [{}] {}\n", p.kind.as_str(), p.text));
        }
        user.push('\n');
    }
    user.push_str(&format!("Recent transcript:\n{transcript}"));
    let done = llm::chat(&req.connection, &req.model, req.effort.as_deref(), POINTS_SYSTEM, &[Message::user(user)], 16_000, &mut |_| {})?;
    let points = parse_points(&done.text).with_context(|| req.connection.name.clone())?;
    Ok(points
        .into_iter()
        .filter(|p| !previous.iter().any(|q| q.text.trim().eq_ignore_ascii_case(p.text.trim())))
        .collect())
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
    let text = answer.trim();
    let text = match text.find("```") {
        Some(start) => {
            let inner = &text[start + 3..];
            let inner = inner.split_once('\n').map_or(inner, |(first, rest)| if first.trim().chars().all(char::is_alphanumeric) { rest } else { inner });
            inner.split("```").next().unwrap_or(inner)
        }
        None => text,
    };
    let value = match (text.find('['), text.rfind(']')) {
        (Some(a), Some(b)) if a < b => serde_json::from_str::<Value>(&text[a..=b]).ok(),
        _ => None,
    };
    let value = match value {
        Some(v) => v,
        None => match (text.find('{'), text.rfind('}')) {
            (Some(a), Some(b)) if a < b => serde_json::from_str::<Value>(&text[a..=b]).ok().and_then(|v| {
                v.as_object().and_then(|o| o.values().find(|x| x.is_array()).cloned())
            }),
            _ => None,
        }
        .with_context(|| tr("Die Antwort war keine gültige Punkteliste (JSON)"))?,
    };
    let Some(items) = value.as_array() else {
        bail!("{}", tr("Die Antwort war keine gültige Punkteliste (JSON)"));
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
}
