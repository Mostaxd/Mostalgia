//! The decision log: every decision across meetings, newest first. They
//! come from the "Decisions" section each summary has (no extra AI call),
//! tied to the transcript line they were most likely made in — the
//! summary's own citation when it has one, else the line sharing the most
//! words — which also names who said it. Entries can be edited, deleted
//! or marked superseded; edited ones survive a new summary.

use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};
use rusqlite::{OptionalExtension, params};

use crate::recap::{Part, clean, sections};
use crate::store::{Meeting, Store};

/// A decision found in a meeting.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub text: String,
    pub speaker: Option<String>,
    pub at_secs: Option<f64>,
}

/// One entry of the log.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub meeting_id: i64,
    pub meeting_title: String,
    pub started_at: DateTime<Local>,
    pub text: String,
    pub speaker: Option<String>,
    pub at_secs: Option<f64>,
    pub edited: bool,
    pub superseded: bool,
    pub superseded_by: Option<i64>,
}

/// What to show.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Words that must all appear (in the decision, its meeting's title or
    /// who made it), any case.
    pub query: String,
    pub from: Option<DateTime<Local>>,
    pub to: Option<DateTime<Local>>,
    pub meeting: Option<i64>,
    pub person: Option<String>,
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_lowercase)
        .collect()
}

/// The decisions in `m`'s summary, each with the line it came from.
pub fn found_in(m: &Meeting) -> Vec<Found> {
    let Some(summary) = &m.summary else { return Vec::new() };
    let lines: Vec<String> = sections(&summary.markdown).into_iter().filter(|(p, _, _)| *p == Part::Decisions).flat_map(|(_, _, l)| l).collect();
    let segment_words: Vec<Vec<String>> = m.segments.iter().map(|s| words(&s.text)).collect();
    lines
        .iter()
        .filter_map(|line| {
            let raw = line.trim_start_matches(['-', '*', ' ']).trim();
            let cited = crate::ask::cite::parse(raw).sources.into_iter().find_map(|t| match t {
                crate::ask::cite::Target::Time { secs, .. } | crate::ask::cite::Target::Meeting { secs, .. } => Some(secs),
                _ => None,
            });
            let text = clean(raw).replace("**", "");
            if text.is_empty() {
                return None;
            }
            let wanted = words(&text);
            let segment = match cited {
                Some(secs) => m.segments.iter().rev().find(|s| s.start_secs <= secs + 0.5),
                None => {
                    // The line (with its neighbour) sharing the most words.
                    let mut best = (0usize, None);
                    for (i, seg) in m.segments.iter().enumerate() {
                        let mut pool: Vec<&String> = segment_words[i].iter().collect();
                        if let Some(next) = segment_words.get(i + 1) {
                            pool.extend(next);
                        }
                        let hits = wanted.iter().filter(|w| pool.contains(w)).count();
                        if hits > best.0 {
                            best = (hits, Some(seg));
                        }
                    }
                    let enough = (wanted.len() as f64 * 0.3).ceil().max(2.0) as usize;
                    best.1.filter(|_| best.0 >= enough)
                }
            };
            let speaker = segment.and_then(|s| s.speaker.clone()).filter(|n| !crate::voices::is_generic_label(n));
            Some(Found { text, speaker, at_secs: cited.or(segment.map(|s| s.start_secs)) })
        })
        .collect()
}

/// Puts `m`'s decisions in the log, replacing the ones taken from an
/// earlier summary of it; edited or superseded entries stay. Returns how
/// many it added.
pub fn add_meeting(store: &Store, m: &Meeting) -> Result<usize> {
    let conn = store.conn();
    conn.execute("DELETE FROM decisions WHERE meeting_id = ?1 AND origin = 'summary' AND edited = 0 AND superseded = 0", [m.id])?;
    let kept: Vec<String> = {
        let mut stmt = conn.prepare("SELECT text FROM decisions WHERE meeting_id = ?1")?;
        stmt.query_map([m.id], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?
    };
    let mut added = 0;
    for f in found_in(m) {
        if kept.iter().any(|k| k.eq_ignore_ascii_case(&f.text)) {
            continue;
        }
        conn.execute(
            "INSERT INTO decisions (meeting_id, text, speaker, at_secs, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![m.id, f.text, f.speaker, f.at_secs, Local::now().timestamp()],
        )?;
        added += 1;
    }
    Ok(added)
}

/// Meetings with a summary (what "Build the log" goes through).
pub fn summarized_meetings(store: &Store) -> Result<Vec<i64>> {
    let mut stmt = store.conn().prepare("SELECT m.id FROM meetings m JOIN summaries s ON s.meeting_id = m.id ORDER BY m.started_at")?;
    let ids = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

pub fn list(store: &Store, filter: &Filter) -> Result<Vec<Entry>> {
    let mut stmt = store.conn().prepare(
        "SELECT d.id, d.meeting_id, m.title, m.started_at, d.text, d.speaker, d.at_secs, d.edited, d.superseded, d.superseded_by
         FROM decisions d JOIN meetings m ON m.id = d.meeting_id
         ORDER BY m.started_at DESC, COALESCE(d.at_secs, 0) DESC, d.id DESC",
    )?;
    let all = stmt
        .query_map([], |r| {
            Ok(Entry {
                id: r.get(0)?,
                meeting_id: r.get(1)?,
                meeting_title: r.get(2)?,
                started_at: Local.timestamp_opt(r.get(3)?, 0).single().unwrap_or_else(Local::now),
                text: r.get(4)?,
                speaker: r.get(5)?,
                at_secs: r.get(6)?,
                edited: r.get(7)?,
                superseded: r.get(8)?,
                superseded_by: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let terms: Vec<String> = filter.query.split_whitespace().map(str::to_lowercase).collect();
    Ok(all
        .into_iter()
        .filter(|e| filter.meeting.is_none_or(|m| m == e.meeting_id))
        .filter(|e| filter.person.as_ref().is_none_or(|p| e.speaker.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(p))))
        .filter(|e| filter.from.is_none_or(|f| e.started_at >= f))
        .filter(|e| filter.to.is_none_or(|t| e.started_at <= t))
        .filter(|e| {
            let hay = format!("{} {} {}", e.text, e.meeting_title, e.speaker.as_deref().unwrap_or("")).to_lowercase();
            terms.iter().all(|t| hay.contains(t))
        })
        .collect())
}

pub fn edit(store: &Store, id: i64, text: &str) -> Result<()> {
    store.conn().execute("UPDATE decisions SET text = ?2, edited = 1 WHERE id = ?1", params![id, text.trim()])?;
    Ok(())
}

pub fn delete(store: &Store, id: i64) -> Result<()> {
    store.conn().execute("DELETE FROM decisions WHERE id = ?1", [id])?;
    Ok(())
}

/// Marks `id` superseded (by the later decision `by`, if given), or not.
pub fn set_superseded(store: &Store, id: i64, superseded: bool, by: Option<i64>) -> Result<()> {
    store.conn().execute("UPDATE decisions SET superseded = ?2, superseded_by = ?3 WHERE id = ?1", params![id, superseded, by.filter(|_| superseded)])?;
    Ok(())
}

/// The text of one decision (for "superseded by …").
pub fn text_of(store: &Store, id: i64) -> Result<Option<String>> {
    Ok(store.conn().query_row("SELECT text FROM decisions WHERE id = ?1", [id], |r| r.get(0)).optional()?)
}

/// The list as Markdown, grouped by meeting (newest first).
pub fn markdown(entries: &[Entry]) -> String {
    let mut out = String::new();
    let mut last: Option<i64> = None;
    for e in entries {
        if last != Some(e.meeting_id) {
            last = Some(e.meeting_id);
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("**{}** — {}\n", e.meeting_title, e.started_at.format("%Y-%m-%d %H:%M")));
        }
        let who = e.speaker.as_deref().map(|s| format!(" ({s})")).unwrap_or_default();
        if e.superseded {
            out.push_str(&format!("- ~~{}~~{who} — superseded\n", e.text));
        } else {
            out.push_str(&format!("- {}{who}\n", e.text));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use crate::finalize::FinalSegment;
    use crate::store::NewMeeting;

    fn seg(source: Source, speaker: Option<&str>, at: f64, text: &str) -> FinalSegment {
        FinalSegment { source, start_secs: at, end_secs: at + 3.0, text: text.into(), speaker: speaker.map(str::to_string) }
    }

    const SUMMARY: &str = "# Jira\n\n## Zusammenfassung\nText.\n\n## Kernpunkte\n- Punkt\n\n## Entscheidungen\n- Jira wird für alle Vorgänge genutzt; Azure DevOps Work Items werden nicht verwendet.\n- Sprints dauern eine Woche (Dienstag bis Dienstag) [[t:120]].\n\n## Aufgaben\n- [ ] Epics anlegen — **Owner:** Markus\n";

    fn store_with_meeting(name: &str) -> (Store, i64, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("mostalgia-decisions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::open(&root.join("db.sqlite")).unwrap();
        let segments = [
            seg(Source::System, Some("Markus"), 10.0, "Also, wir nutzen Jira für alle Vorgänge,"),
            seg(Source::System, Some("Markus"), 13.0, "die Work Items von Azure DevOps werden nicht verwendet."),
            seg(Source::Mic, None, 60.0, "Okay, klingt gut."),
            seg(Source::System, Some("Tina"), 118.0, "Dann machen wir Sprints von einer Woche, Dienstag bis Dienstag."),
        ];
        let started_at = Local.with_ymd_and_hms(2026, 10, 28, 13, 1, 0).unwrap();
        let id = store.insert_meeting(&NewMeeting { title: "Jira-Struktur", started_at, duration_secs: 600.0, language: "de", dir: &root.join("s") }, &segments).unwrap();
        store.save_summary(id, "deepseek", "m", None, SUMMARY).unwrap();
        (store, id, root)
    }

    #[test]
    fn decisions_come_from_the_summary_with_their_line() {
        let (store, id, root) = store_with_meeting("found");
        let m = store.get(id).unwrap().unwrap();
        let found = found_in(&m);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].speaker.as_deref(), Some("Markus"), "by shared words");
        assert_eq!(found[0].at_secs, Some(10.0));
        assert_eq!(found[1].at_secs, Some(120.0), "the summary's own citation");
        assert_eq!(found[1].speaker.as_deref(), Some("Tina"));
        assert!(!found[1].text.contains("[["));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_supersede_filter_and_rebuild() {
        let (store, id, root) = store_with_meeting("log");
        let m = store.get(id).unwrap().unwrap();
        assert_eq!(add_meeting(&store, &m).unwrap(), 2);
        let all = list(&store, &Filter::default()).unwrap();
        assert_eq!(all.len(), 2);
        let (sprints, jira) = (all[0].clone(), all[1].clone());
        assert!(sprints.text.starts_with("Sprints"), "newest line first");
        edit(&store, jira.id, "Jira für alles.").unwrap();
        set_superseded(&store, sprints.id, true, Some(jira.id)).unwrap();
        // A new summary: the edited and the superseded entries stay, nothing doubles.
        assert_eq!(add_meeting(&store, &m).unwrap(), 1, "the original Jira text again (the edit replaced it)");
        assert_eq!(list(&store, &Filter::default()).unwrap().len(), 3);
        let by_tina = list(&store, &Filter { person: Some("tina".into()), ..Default::default() }).unwrap();
        assert_eq!(by_tina.len(), 1);
        assert!(by_tina[0].superseded && by_tina[0].superseded_by == Some(jira.id));
        assert_eq!(list(&store, &Filter { query: "jira ALLES".into(), ..Default::default() }).unwrap().len(), 1);
        assert!(list(&store, &Filter { from: Some(Local::now()), ..Default::default() }).unwrap().is_empty());
        let md = markdown(&list(&store, &Filter::default()).unwrap());
        assert!(md.starts_with("**Jira-Struktur** — 2026-10-28 13:01\n"));
        assert!(md.contains("~~Sprints dauern eine Woche (Dienstag bis Dienstag).~~ (Tina) — superseded"));
        delete(&store, jira.id).unwrap();
        assert_eq!(list(&store, &Filter::default()).unwrap().len(), 2);
        assert_eq!(summarized_meetings(&store).unwrap(), [id]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
