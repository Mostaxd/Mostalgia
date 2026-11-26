//! The meeting chat: what people typed during a Teams call (links,
//! decisions, "I'll send the deck"), kept with the meeting so the summary
//! and Ask can use it.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long before the recording's start chat messages still count.
pub const BEFORE: Duration = Duration::minutes(15);
/// How long after its end.
pub const AFTER: Duration = Duration::minutes(30);
/// At most this many messages are kept for a meeting.
pub const MAX_LINES: usize = 300;
/// A single message is cut after this many characters.
const MAX_CHARS: usize = 1_500;

/// One message of the meeting chat, as plain text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatLine {
    pub id: String,
    pub at: DateTime<Utc>,
    pub from: String,
    pub text: String,
}

/// A Graph `chatMessage`, when it's something a person wrote (not a
/// system event such as "recording started", and not deleted).
pub fn parse_message(v: &Value) -> Option<ChatLine> {
    if v["messageType"].as_str().is_some_and(|t| t != "message") || !v["deletedDateTime"].is_null() {
        return None;
    }
    let at = DateTime::parse_from_rfc3339(v["createdDateTime"].as_str()?).ok()?.with_timezone(&Utc);
    let from = v["from"]["user"]["displayName"]
        .as_str()
        .or(v["from"]["application"]["displayName"].as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let content = v["body"]["content"].as_str().unwrap_or_default();
    let mut text = if v["body"]["contentType"].as_str() == Some("html") { html_to_text(content) } else { content.trim().to_string() };
    // Shared files and links come as attachments ("reference" = a file in
    // SharePoint/OneDrive); the body only has an <attachment> placeholder.
    for a in v["attachments"].as_array().into_iter().flatten() {
        let name = a["name"].as_str().unwrap_or_default().trim();
        let url = a["contentUrl"].as_str().unwrap_or_default().trim();
        let item = match (name, url) {
            ("", "") => continue,
            (n, "") => n.to_string(),
            ("", u) => u.to_string(),
            (n, u) => format!("{n} <{u}>"),
        };
        if !text.contains(url) || url.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!("[{item}]"));
        }
    }
    if text.chars().count() > MAX_CHARS {
        text = text.chars().take(MAX_CHARS).collect::<String>() + " …";
    }
    (!text.trim().is_empty()).then(|| ChatLine { id: v["id"].as_str().unwrap_or_default().into(), at, from, text })
}

/// Teams message HTML as one line of text: links keep their address
/// ("deck <https://…>"), paragraphs and line breaks become " / ".
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut href: Option<String> = None;
    let mut link_text = String::new();
    let mut i = 0;
    while i < html.len() {
        let Some(lt) = html[i..].find('<').map(|p| p + i) else {
            push_text(&mut out, &mut link_text, href.is_some(), &html[i..]);
            break;
        };
        push_text(&mut out, &mut link_text, href.is_some(), &html[i..lt]);
        let Some(gt) = html[lt..].find('>').map(|p| p + lt) else { break };
        let raw = &html[lt + 1..gt];
        let closing = raw.starts_with('/');
        let tag = lower[lt + 1..gt].trim_start_matches('/').split([' ', '\n', '\t', '/']).next().unwrap_or_default().to_string();
        match tag.as_str() {
            "a" if !closing => {
                href = attr(raw, "href");
                link_text.clear();
            }
            "a" => {
                let shown = unescape(link_text.trim());
                let url = href.take().map(|h| unescape(&h)).unwrap_or_default();
                let piece = if url.is_empty() || shown == url {
                    if url.is_empty() { shown } else { url }
                } else if shown.is_empty() {
                    url
                } else {
                    format!("{shown} <{url}>")
                };
                out.push_str(&piece);
                link_text.clear();
            }
            "p" | "div" | "br" | "li" if !out.trim_end().is_empty() && !out.trim_end().ends_with('/') => out.push_str(" / "),
            "img" if !closing => {
                // Emoji come as <img alt="😀">.
                if let Some(alt) = attr(raw, "alt") {
                    out.push_str(&unescape(&alt));
                }
            }
            _ => {}
        }
        i = gt + 1;
    }
    let text = out.split_whitespace().collect::<Vec<_>>().join(" ");
    text.trim().trim_end_matches('/').trim().to_string()
}

fn push_text(out: &mut String, link: &mut String, in_link: bool, s: &str) {
    if in_link {
        link.push_str(s);
    } else {
        out.push_str(&unescape(s));
    }
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let (quote, rest) = match rest.chars().next()? {
        q @ ('"' | '\'') => (q, &rest[1..]),
        _ => (' ', rest),
    };
    let end = rest.find(quote).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// The messages written between `from` and `to`, oldest first, without
/// `skip` (Mostalgia's own posted summary), at most [`MAX_LINES`] (the
/// last ones).
pub fn window(lines: Vec<ChatLine>, from: DateTime<Utc>, to: DateTime<Utc>, skip: &[String]) -> Vec<ChatLine> {
    let mut kept: Vec<ChatLine> = lines.into_iter().filter(|l| l.at >= from && l.at <= to && !skip.contains(&l.id)).collect();
    kept.sort_by_key(|l| l.at);
    kept.dedup_by(|a, b| !a.id.is_empty() && a.id == b.id);
    if kept.len() > MAX_LINES {
        kept.drain(..kept.len() - MAX_LINES);
    }
    kept
}

/// "[12:05] Anna: text", the time counted from `start` like the
/// transcript's (negative: before the recording started, shown as 00:00).
pub fn transcript_line(line: &ChatLine, start: DateTime<Utc>) -> String {
    let secs = (line.at - start).num_seconds().max(0) as f64;
    let from = if line.from.is_empty() { "?" } else { line.from.as_str() };
    format!("[{}] {from}: {}", crate::chunker::format_ts(secs), line.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn html_messages_become_text_with_links() {
        assert_eq!(
            html_to_text(r#"<p>Hier das Deck: <a href="https://x.sharepoint.com/deck.pptx?a=1&amp;b=2">Q4 Deck</a></p><p>Entscheidung: wir nehmen &quot;B&quot;</p>"#),
            r#"Hier das Deck: Q4 Deck <https://x.sharepoint.com/deck.pptx?a=1&b=2> / Entscheidung: wir nehmen "B""#
        );
        assert_eq!(html_to_text(r#"<a href="https://a.b/c">https://a.b/c</a>"#), "https://a.b/c");
        assert_eq!(html_to_text(r#"ok <img alt="👍" src="x">"#), "ok 👍");
        assert_eq!(html_to_text("<div>eins<br>zwei</div>"), "eins / zwei");
    }

    #[test]
    fn system_and_deleted_messages_are_left_out() {
        let msg = |kind: &str, deleted: Value| {
            json!({ "id": "1", "messageType": kind, "deletedDateTime": deleted, "createdDateTime": "2026-11-26T10:05:00Z",
                    "from": { "user": { "displayName": "Anna" } }, "body": { "contentType": "text", "content": "hi" } })
        };
        assert!(parse_message(&msg("message", Value::Null)).is_some());
        assert!(parse_message(&msg("systemEventMessage", Value::Null)).is_none());
        assert!(parse_message(&msg("message", json!("2026-11-26T10:06:00Z"))).is_none());
    }

    #[test]
    fn attachments_are_named() {
        let v = json!({ "id": "2", "messageType": "message", "createdDateTime": "2026-11-26T10:05:00Z",
            "from": { "user": { "displayName": "Ben" } },
            "body": { "contentType": "html", "content": "<p>Protokoll</p><attachment id=\"a\"></attachment>" },
            "attachments": [{ "contentType": "reference", "name": "Protokoll.docx", "contentUrl": "https://x/p.docx" }] });
        let l = parse_message(&v).unwrap();
        assert_eq!(l.text, "Protokoll [Protokoll.docx <https://x/p.docx>]");
        assert_eq!(l.from, "Ben");
    }

    #[test]
    fn window_keeps_the_meeting_and_skips_our_post() {
        let t = |m: i64| DateTime::parse_from_rfc3339("2026-11-26T10:00:00Z").unwrap().with_timezone(&Utc) + Duration::minutes(m);
        let line = |id: &str, m: i64| ChatLine { id: id.into(), at: t(m), from: "A".into(), text: id.into() };
        let lines = vec![line("late", 120), line("b", 20), line("ours", 40), line("old", -60), line("a", 5)];
        let kept = window(lines, t(-15), t(60 + 30), &["ours".to_string()]);
        assert_eq!(kept.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(transcript_line(&kept[0], t(2)), format!("[{}] A: a", crate::chunker::format_ts(180.0)));
        assert_eq!(transcript_line(&kept[0], t(9)), format!("[{}] A: a", crate::chunker::format_ts(0.0)));
    }
}
