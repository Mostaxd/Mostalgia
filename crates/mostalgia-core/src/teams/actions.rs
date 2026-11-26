//! Action items from a summary, and the summary as a Teams chat message.
//! Summaries write action items as `- [ ] <task> — **Owner:** <who> —
//! **Due:** <when>`, with the labels translated into the summary's
//! language, so the checkbox finds them, not the heading.

use chrono::{Datelike, NaiveDate};

use super::auth::escape_html;
use crate::i18n::tr;

#[derive(Clone, Debug, PartialEq)]
pub struct ActionItem {
    pub task: String,
    /// `None` when nobody took it ("open").
    pub owner: Option<String>,
    /// The deadline as written.
    pub due: Option<String>,
    /// The deadline, when it's a date we can read.
    pub due_date: Option<NaiveDate>,
    pub done: bool,
}

/// Words that mean "nobody yet" in an owner field.
const OPEN_OWNERS: &[&str] = &["open", "offen", "—", "-", "n/a", "none", "keiner", "niemand", "tbd", "ouvert", "abierto", "aperto", "مفتوح"]; // i18n: not UI

/// The action items (`- [ ]` / `- [x]` lines) in a summary.
pub fn parse(markdown: &str) -> Vec<ActionItem> {
    parse_with_year(markdown, chrono::Local::now().year())
}

fn parse_with_year(markdown: &str, year: i32) -> Vec<ActionItem> {
    let mut out = Vec::new();
    for line in markdown.lines() {
        let l = line.trim_start();
        let rest = ["- [ ]", "* [ ]", "- [x]", "- [X]", "* [x]", "* [X]"].iter().find_map(|p| l.strip_prefix(p).map(|r| (p.contains('x') || p.contains('X'), r)));
        let Some((done, rest)) = rest else { continue };
        let parts: Vec<&str> = rest.split(" — ").flat_map(|p| p.split(" -- ")).map(str::trim).collect();
        let task = strip_md(parts.first().copied().unwrap_or_default());
        if task.is_empty() {
            continue;
        }
        let mut owner = None;
        let mut due = None;
        for (i, part) in parts.iter().enumerate().skip(1) {
            // "**Owner:** Anna" (any language): label in bold, then the value.
            let value = match part.split_once(":**") {
                Some((_, v)) => strip_md(v),
                None => strip_md(part.split_once(':').map_or(part, |(_, v)| v)),
            };
            if value.is_empty() {
                continue;
            }
            if i == 1 {
                owner = Some(value);
            } else if due.is_none() {
                due = Some(value);
            }
        }
        let owner = owner.filter(|o| !OPEN_OWNERS.contains(&o.to_lowercase().as_str()));
        let due_date = due.as_deref().and_then(|d| parse_date(d, year));
        out.push(ActionItem { task, owner, due, due_date, done });
    }
    out
}

/// Bold, italics and code marks removed, whitespace trimmed.
fn strip_md(s: &str) -> String {
    s.replace("**", "").replace('`', "").trim().trim_matches('_').trim().to_string()
}

/// "2026-12-01", "01.12.2026", "1.12.", "12/01/2026".
fn parse_date(s: &str, year: i32) -> Option<NaiveDate> {
    let s = s.trim().trim_end_matches('.');
    for word in s.split_whitespace() {
        let w = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-' && c != '/');
        if let Ok(d) = NaiveDate::parse_from_str(w, "%Y-%m-%d") {
            return Some(d);
        }
        if let Ok(d) = NaiveDate::parse_from_str(w, "%d.%m.%Y") {
            return Some(d);
        }
        if let Ok(d) = NaiveDate::parse_from_str(w, "%m/%d/%Y") {
            return Some(d);
        }
        let parts: Vec<&str> = w.trim_end_matches('.').split('.').collect();
        if parts.len() == 2
            && let (Ok(d), Ok(m)) = (parts[0].parse(), parts[1].parse())
            && let Some(date) = NaiveDate::from_ymd_opt(year, m, d)
        {
            return Some(date);
        }
    }
    None
}

impl ActionItem {
    /// Whether it's `me`'s: the owner is one of `me_names` (or "Me"/"Ich"), or nobody.
    pub fn is_mine(&self, me_names: &[String]) -> bool {
        let Some(owner) = &self.owner else { return true };
        let o = owner.to_lowercase();
        ["me", "ich", "moi", "yo", "io"].contains(&o.as_str()) // i18n: not UI
            || me_names.iter().filter(|n| !n.trim().is_empty()).any(|n| {
                let n = n.to_lowercase();
                o == n || o.split_whitespace().next() == n.split_whitespace().next()
            })
    }

    /// One line for a chat or a task note.
    pub fn line(&self) -> String {
        let mut s = self.task.clone();
        if let Some(o) = &self.owner {
            s.push_str(&format!(" ({o})"));
        }
        if let Some(d) = &self.due {
            s.push_str(&format!(" — {d}"));
        }
        s
    }
}

/// Markdown (as summaries write it) as the HTML Teams chats accept.
pub fn markdown_to_html(md: &str) -> String {
    let mut out = String::new();
    let mut in_list = false;
    for line in md.lines() {
        let l = line.trim_end();
        let t = l.trim_start();
        let bullet = ["- [ ] ", "- [x] ", "- [X] ", "- ", "* "].iter().find_map(|p| t.strip_prefix(p).map(|r| (*p, r)));
        if let Some((prefix, rest)) = bullet {
            if !in_list {
                out.push_str("<ul>");
                in_list = true;
            }
            let mark = match prefix {
                "- [ ] " => "☐ ",
                "- [x] " | "- [X] " => "☑ ",
                _ => "",
            };
            out.push_str(&format!("<li>{mark}{}</li>", inline(rest)));
            continue;
        }
        if in_list {
            out.push_str("</ul>");
            in_list = false;
        }
        if t.is_empty() {
            continue;
        }
        let level = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&level) && t[level..].starts_with(' ') {
            // Chats show h1 huge: one step smaller each.
            let h = (level + 1).min(4);
            out.push_str(&format!("<h{h}>{}</h{h}>", inline(t[level..].trim())));
        } else {
            out.push_str(&format!("<p>{}</p>", inline(t)));
        }
    }
    if in_list {
        out.push_str("</ul>");
    }
    out
}

/// `**bold**`, `*italic*` and `` `code` `` in escaped text; time markers
/// like `[[t:12.3]]` are dropped.
fn inline(s: &str) -> String {
    let s = drop_time_markers(s);
    let escaped = escape_html(&s);
    let mut out = String::new();
    let mut rest = escaped.as_str();
    let mut bold = false;
    let mut code = false;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("**") {
            out.push_str(if bold { "</b>" } else { "<b>" });
            bold = !bold;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('`') {
            out.push_str(if code { "</code>" } else { "<code>" });
            code = !code;
            rest = r;
        } else {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    if bold {
        out.push_str("</b>");
    }
    if code {
        out.push_str("</code>");
    }
    out
}

fn drop_time_markers(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find("[[t:") {
        out.push_str(&rest[..at]);
        match rest[at..].find("]]") {
            Some(end) => rest = &rest[at + end + 2..],
            None => {
                rest = &rest[at..];
                break;
            }
        }
    }
    out.push_str(rest);
    out.replace("  ", " ")
}

/// The chat message for a meeting's summary: the summary as HTML, minus
/// its action items when `with_actions` is off.
pub fn chat_message(title: &str, summary_md: &str, with_actions: bool) -> String {
    let md: String = if with_actions {
        summary_md.to_string()
    } else {
        summary_md.lines().filter(|l| !(l.trim_start().starts_with("- [ ]") || l.trim_start().starts_with("- [x]"))).collect::<Vec<_>>().join("\n")
    };
    // The summary's own "# Title" repeats the meeting's.
    let body: String = md.lines().skip_while(|l| l.trim().is_empty()).enumerate().filter(|(i, l)| !(*i == 0 && l.starts_with("# "))).map(|(_, l)| l).collect::<Vec<_>>().join("\n");
    format!(
        "<p><b>{}</b> · {}</p>{}<p><i>{}</i></p>",
        escape_html(title),
        escape_html(tr("Zusammenfassung")),
        markdown_to_html(&body),
        escape_html(tr("Geschrieben von Mostalgia"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMMARY: &str = "# Q4 rollout\n\n## Summary\nWe agreed.\n\n## Action items\n\
- [ ] Send the **offer** to PayFlow — **Owner:** Anna — **Due:** 2026-12-01\n\
- [ ] Book the room — **Owner:** open\n\
- [x] Update the Jira board — **Verantwortlich:** Ich — **Fällig:** 3.12.\n\
- [ ] Check legal — **Owner:** Jerome Markus — **Due:** next Friday\n\n## Open questions\n- Budget?\n";

    #[test]
    fn finds_action_items_in_any_language() {
        let items = parse_with_year(SUMMARY, 2026);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].task, "Send the offer to PayFlow");
        assert_eq!(items[0].owner.as_deref(), Some("Anna"));
        assert_eq!(items[0].due_date, NaiveDate::from_ymd_opt(2026, 12, 1));
        assert_eq!(items[1].owner, None, "open means nobody");
        assert!(items[2].done);
        assert_eq!(items[2].owner.as_deref(), Some("Ich"));
        assert_eq!(items[2].due_date, NaiveDate::from_ymd_opt(2026, 12, 3));
        assert_eq!(items[3].due.as_deref(), Some("next Friday"));
        assert_eq!(items[3].due_date, None);
    }

    #[test]
    fn mine_by_name_first_name_or_me() {
        let items = parse_with_year(SUMMARY, 2026);
        let me = vec!["Anna Lehmann".to_string()];
        assert!(items[0].is_mine(&me), "first name matches");
        assert!(items[1].is_mine(&me), "open items are anyone's");
        assert!(items[2].is_mine(&[]), "Ich");
        assert!(!items[3].is_mine(&me));
    }

    #[test]
    fn markdown_becomes_chat_html() {
        let html = markdown_to_html("## Key points\n- A **bold** <tag> [[t:12.5]]\n- `code`\n\nText & more");
        assert_eq!(html, "<h3>Key points</h3><ul><li>A <b>bold</b> &lt;tag&gt; </li><li><code>code</code></li></ul><p>Text &amp; more</p>");
    }

    #[test]
    fn chat_message_without_actions() {
        let msg = chat_message("Weekly", SUMMARY, false);
        assert!(msg.starts_with("<p><b>Weekly</b>"));
        assert!(!msg.contains("offer"), "{msg}");
        assert!(!msg.contains("<h2>Q4 rollout"), "the summary's own title is left out");
        let with = chat_message("Weekly", SUMMARY, true);
        assert!(with.contains("☐ Send the <b>offer</b>"), "{with}");
    }
}
