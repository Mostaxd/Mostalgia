//! A short recap of a meeting to paste into a chat, an email or a ticket:
//! title, date, length and who spoke, then the summary's key points,
//! decisions and action items — in the summary's language, without
//! citations, timestamps or links.

use regex::Regex;
use std::sync::OnceLock;

use crate::store::Meeting;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Length {
    /// Key points (the first few), decisions and action items.
    Short,
    /// The summary paragraph and every section.
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Reads well pasted anywhere (Teams, mail): "•" bullets, no markup.
    Plain,
    /// Markdown (Jira, GitHub, Slack-like editors).
    Markdown,
}

/// Key points in a short recap.
const SHORT_POINTS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Summary,
    Points,
    Decisions,
    Actions,
    Questions,
}

/// The summary's sections as (part, heading, lines). Known headings (English,
/// German) name their part; others go by the summary's fixed order, and
/// checkbox lines always mean action items.
pub(crate) fn sections(markdown: &str) -> Vec<(Part, String, Vec<String>)> {
    let order = [Part::Summary, Part::Points, Part::Decisions, Part::Actions, Part::Questions];
    let mut out: Vec<(Part, String, Vec<String>)> = Vec::new();
    for line in markdown.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            let lower = h.trim().to_lowercase();
            let named = match lower.as_str() {
                "summary" | "zusammenfassung" => Some(Part::Summary),
                "key points" | "kernpunkte" => Some(Part::Points),
                "decisions" | "entscheidungen" => Some(Part::Decisions),
                "action items" | "aufgaben" => Some(Part::Actions),
                "open questions" | "offene fragen" => Some(Part::Questions),
                _ => None,
            };
            let part = named.unwrap_or(order[out.len().min(order.len() - 1)]);
            out.push((part, h.trim().to_string(), Vec::new()));
        } else if let Some((_, _, lines)) = out.last_mut()
            && !line.trim().is_empty()
        {
            lines.push(line.trim().to_string());
        }
    }
    for s in &mut out {
        if s.2.iter().any(|l| l.starts_with("- [")) {
            s.0 = Part::Actions;
        }
    }
    out
}

/// Without citations ([[t:12]], [[d:3@4]]), timestamps ("(12:34)",
/// "[00:12:34]"), links (their text stays) and bare URLs.
pub fn clean(text: &str) -> String {
    static RES: OnceLock<[Regex; 4]> = OnceLock::new();
    let [cite, time, link, url] = RES.get_or_init(|| {
        [
            Regex::new(r"\s*\[\[[^\]]*\]\]").unwrap(),
            Regex::new(r"\s*[\(\[]\d{1,2}:\d{2}(:\d{2})?[\)\]]").unwrap(),
            Regex::new(r"\[([^\]]+)\]\([^)]+\)").unwrap(),
            Regex::new(r"\s*https?://\S+").unwrap(),
        ]
    });
    let s = cite.replace_all(text, "");
    let s = time.replace_all(&s, "");
    let s = link.replace_all(&s, "$1");
    let s = url.replace_all(&s, "");
    s.trim().to_string()
}

/// A line for plain text: list markers become "•" / "☐" / "☑", and
/// Markdown emphasis and code marks go.
fn plain(line: &str) -> String {
    let (mark, rest) = if let Some(r) = line.strip_prefix("- [ ] ") {
        ("☐ ", r)
    } else if let Some(r) = line.strip_prefix("- [x] ").or_else(|| line.strip_prefix("- [X] ")) {
        ("☑ ", r)
    } else if let Some(r) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        ("• ", r)
    } else {
        ("", line)
    };
    format!("{mark}{}", rest.replace("**", "").replace("__", "").replace('`', ""))
}

/// The recap, or `None` while the meeting has no summary.
pub fn recap(m: &Meeting, length: Length, style: Style) -> Option<String> {
    let summary = m.summary.as_ref()?;
    let sections = sections(&summary.markdown);
    let de = m.language.starts_with("de");
    let date = m.started_at.format(if de { "%d.%m.%Y, %H:%M" } else { "%Y-%m-%d, %H:%M" }).to_string();
    let mut people: Vec<&str> = Vec::new();
    for s in &m.segments {
        if let Some(name) = s.speaker.as_deref().filter(|n| !crate::voices::is_generic_label(n))
            && !people.contains(&name)
        {
            people.push(name);
        }
    }
    let minutes = (m.duration_secs / 60.0).round() as u64;
    let mut meta = vec![date, if minutes == 0 { "< 1 min".to_string() } else { format!("{minutes} min") }];
    if !people.is_empty() {
        meta.push(people.join(", "));
    }
    let meta = meta.join(" · ");

    let mut out = match style {
        Style::Plain => format!("{}\n{meta}\n", m.title),
        Style::Markdown => format!("**{}**\n_{meta}_\n", m.title),
    };
    let wanted: &[Part] = match length {
        Length::Short => &[Part::Points, Part::Decisions, Part::Actions],
        Length::Full => &[Part::Summary, Part::Points, Part::Decisions, Part::Actions, Part::Questions],
    };
    for (part, heading, lines) in sections.iter().filter(|(p, _, _)| wanted.contains(p)) {
        let mut lines: Vec<String> = lines.iter().map(|l| clean(l)).filter(|l| !l.is_empty() && l != "-").collect();
        if length == Length::Short && *part == Part::Points && lines.len() > SHORT_POINTS {
            lines.truncate(SHORT_POINTS);
        }
        if lines.is_empty() {
            continue;
        }
        out.push('\n');
        match style {
            Style::Plain => {
                out.push_str(heading);
                out.push('\n');
                for l in lines {
                    out.push_str(&plain(&l));
                    out.push('\n');
                }
            }
            Style::Markdown => {
                out.push_str(&format!("**{heading}**\n"));
                for l in lines {
                    out.push_str(&l);
                    out.push('\n');
                }
            }
        }
    }
    Some(out.trim_end().to_string() + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use crate::store::{StoredSegment, Summary};
    use chrono::{Local, TimeZone};

    fn meeting(markdown: Option<&str>) -> Meeting {
        let seg = |speaker: Option<&str>, source| StoredSegment {
            id: 0,
            source,
            speaker: speaker.map(str::to_string),
            start_secs: 0.0,
            end_secs: 1.0,
            text: String::new(),
        };
        Meeting {
            id: 3,
            title: "Jira-Struktur".into(),
            started_at: Local.with_ymd_and_hms(2026, 10, 28, 13, 1, 0).unwrap(),
            duration_secs: 5192.0,
            language: "de".into(),
            dir: Default::default(),
            segments: vec![seg(None, Source::Mic), seg(Some("Markus"), Source::System), seg(Some("Sprecher 1"), Source::System), seg(Some("Tina"), Source::System), seg(Some("Markus"), Source::System)],
            bookmarks: Vec::new(),
            notes: Vec::new(),
            chat: Vec::new(),
            summary: markdown.map(|md| Summary { provider: "deepseek".into(), model: "m".into(), effort: None, created_at: Local::now(), markdown: md.into() }),
            transcript_engine: None,
            transcript_error: None,
            companion: "mori".into(),
        }
    }

    const MD: &str = "# Jira-Struktur\n\n## Zusammenfassung\nDas Team hat Jira festgelegt [[t:12]].\n\n## Kernpunkte\n- Eins (12:34)\n- Zwei, siehe [Doku](https://x.y/z)\n- Drei https://example.com/a\n- Vier\n- Fünf\n- Sechs\n\n## Entscheidungen\n- Jira für alles [[t:90]]\n\n## Aufgaben\n- [ ] Epics anlegen — **Owner:** Markus — **Due:** Freitag\n\n## Offene Fragen\n- Logging?\n";

    #[test]
    fn short_plain_recap() {
        let r = recap(&meeting(Some(MD)), Length::Short, Style::Plain).unwrap();
        assert_eq!(
            r,
            "Jira-Struktur\n28.10.2026, 13:01 · 87 min · Markus, Tina\n\nKernpunkte\n• Eins\n• Zwei, siehe Doku\n• Drei\n• Vier\n• Fünf\n\nEntscheidungen\n• Jira für alles\n\nAufgaben\n☐ Epics anlegen — Owner: Markus — Due: Freitag\n"
        );
    }

    #[test]
    fn full_markdown_recap_keeps_markup_but_not_citations() {
        let r = recap(&meeting(Some(MD)), Length::Full, Style::Markdown).unwrap();
        assert!(r.starts_with("**Jira-Struktur**\n_28.10.2026, 13:01 · 87 min · Markus, Tina_\n"));
        assert!(r.contains("**Zusammenfassung**\nDas Team hat Jira festgelegt.\n"));
        assert!(r.contains("- Sechs\n"), "all points in full");
        assert!(r.contains("- [ ] Epics anlegen — **Owner:** Markus — **Due:** Freitag\n"));
        assert!(r.contains("**Offene Fragen**\n- Logging?"));
        assert!(!r.contains("[[") && !r.contains("http") && !r.contains("12:34"));
    }

    #[test]
    fn other_languages_go_by_section_order() {
        let md = "# Réunion\n\n## Résumé\nTexte.\n\n## Points clés\n- A\n\n## Décisions\n- B\n\n## Actions\n- [ ] C — **Owner:** Léa\n\n## Questions ouvertes\n- D\n";
        let mut m = meeting(Some(md));
        m.language = "fr".into();
        let r = recap(&m, Length::Short, Style::Plain).unwrap();
        assert_eq!(r, "Jira-Struktur\n2026-10-28, 13:01 · 87 min · Markus, Tina\n\nPoints clés\n• A\n\nDécisions\n• B\n\nActions\n☐ C — Owner: Léa\n");
    }

    #[test]
    fn no_summary_no_recap() {
        assert_eq!(recap(&meeting(None), Length::Short, Style::Plain), None);
    }
}
