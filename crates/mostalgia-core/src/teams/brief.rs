//! The brief before a meeting: its agenda from the invite, who's coming,
//! and what happened the last times with the same people (or the same
//! recurring meeting), with the action items still open from then.

use anyhow::Result;
use chrono::{DateTime, Local};

use super::actions::{self, ActionItem};
use super::calendar::Event;
use super::link::TeamsLink;
use crate::i18n::{tr, trf};
use crate::store::Store;

/// Past meetings looked at for a brief.
const SCAN: usize = 300;
/// Past meetings a brief shows.
const SHOWN: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct PastMeeting {
    pub meeting_id: i64,
    pub title: String,
    pub started_at: DateTime<Local>,
    /// People who were in both.
    pub shared: Vec<String>,
    pub same_series: bool,
    /// The summary's first paragraph.
    pub gist: Option<String>,
    pub open_actions: Vec<ActionItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Brief {
    pub event: Event,
    pub people: Vec<String>,
    pub past: Vec<PastMeeting>,
}

/// Same meeting series: same subject, ignoring case and numbers ("Weekly #12").
fn series_key(subject: &str) -> String {
    subject.chars().filter(|c| c.is_alphabetic() || c.is_whitespace()).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The first paragraph of a summary that isn't a heading.
pub fn gist(markdown: &str) -> Option<String> {
    let mut para: Vec<&str> = Vec::new();
    for line in markdown.lines() {
        let l = line.trim();
        if l.starts_with('#') || l.starts_with("- ") || l.starts_with("* ") {
            if para.is_empty() {
                continue;
            }
            break;
        }
        if l.is_empty() {
            if para.is_empty() {
                continue;
            }
            break;
        }
        para.push(l);
    }
    (!para.is_empty()).then(|| para.join(" "))
}

/// The brief for `event` from the library's linked meetings.
pub fn build(store: &Store, event: &Event, me_email: &str) -> Result<Brief> {
    let people = event.people(me_email);
    let emails: Vec<String> = people.iter().map(|p| p.email.to_lowercase()).filter(|e| !e.is_empty()).collect();
    let key = series_key(&event.subject);
    let mut past = Vec::new();
    for (id, title, started_at, dir) in store.folders(SCAN)? {
        if past.len() >= SHOWN {
            break;
        }
        let Some(link) = TeamsLink::load(&dir) else { continue };
        if !event.id.is_empty() && link.event.id == event.id && link.event.start == event.start {
            continue; // this very meeting (recorded already)
        }
        let theirs = link.event.people(&link.me_email);
        let shared: Vec<String> =
            theirs.iter().filter(|p| !p.email.is_empty() && emails.contains(&p.email.to_lowercase())).map(|p| p.label()).collect();
        let same_series = !key.is_empty() && series_key(&link.event.subject) == key;
        // Small meetings: one shared person counts; bigger ones need half.
        let enough = !shared.is_empty() && shared.len() * 2 >= emails.len().min(theirs.len()).max(1);
        if !(same_series || enough) {
            continue;
        }
        let meeting = store.get(id)?;
        let summary = meeting.as_ref().and_then(|m| m.summary.as_ref()).map(|s| s.markdown.clone()).unwrap_or_default();
        past.push(PastMeeting {
            meeting_id: id,
            title,
            started_at,
            shared,
            same_series,
            gist: gist(&summary),
            open_actions: actions::parse(&summary).into_iter().filter(|a| !a.done).collect(),
        });
    }
    Ok(Brief { event: event.clone(), people: people.iter().map(|p| p.label()).collect(), past })
}

impl Brief {
    /// The brief as Markdown (shown in the app, printed by the CLI).
    pub fn markdown(&self) -> String {
        let mut out = format!("# {}\n\n", self.event.recording_title());
        let start = self.event.start.with_timezone(&Local);
        let end = self.event.end.with_timezone(&Local);
        out.push_str(&format!("{} – {}\n\n", crate::i18n::date_time(&start), end.format("%H:%M")));
        if !self.people.is_empty() {
            out.push_str(&format!("**{}:** {}\n\n", tr("Teilnehmende"), self.people.join(", ")));
        }
        if !self.event.body.trim().is_empty() {
            out.push_str(&format!("## {}\n\n{}\n\n", tr("Agenda"), self.event.body.trim()));
        }
        if self.past.is_empty() {
            out.push_str(&format!("_{}_\n", tr("Keine früheren Aufnahmen mit diesen Leuten.")));
            return out;
        }
        out.push_str(&format!("## {}\n\n", tr("Letzte Male")));
        for p in &self.past {
            out.push_str(&format!("### {} ({})\n\n", p.title, crate::i18n::date(&p.started_at)));
            if !p.shared.is_empty() {
                out.push_str(&format!("{}\n\n", trf("Mit {}", &[&p.shared.join(", ")])));
            }
            if let Some(g) = &p.gist {
                out.push_str(&format!("{g}\n\n"));
            }
            if !p.open_actions.is_empty() {
                out.push_str(&format!("**{}**\n\n", tr("Noch offen:")));
                for a in &p.open_actions {
                    out.push_str(&format!("- [ ] {}\n", a.line()));
                }
                out.push('\n');
            }
        }
        out
    }

    /// Open action items from earlier meetings, all together.
    pub fn open_actions(&self) -> Vec<&ActionItem> {
        self.past.iter().flat_map(|p| &p.open_actions).collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::store::NewMeeting;
    use crate::teams::calendar::Attendee;

    fn person(name: &str, email: &str) -> Attendee {
        Attendee { name: name.into(), email: email.into(), kind: "required".into(), ..Attendee::default() }
    }

    #[test]
    fn gist_is_the_first_paragraph() {
        assert_eq!(gist("# T\n\n## Summary\nWe met.\nIt went well.\n\n## Key points\n- a").as_deref(), Some("We met. It went well."));
        assert_eq!(gist("# T\n- only bullets"), None);
    }

    #[test]
    fn series_ignores_numbers() {
        assert_eq!(series_key("Weekly #12"), series_key("weekly #13"));
    }

    #[test]
    fn finds_earlier_meetings_with_the_same_people() {
        let root = std::env::temp_dir().join(format!("mostalgia-brief-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::open(&root.join("db.sqlite")).unwrap();
        let mk = |store: &mut Store, name: &str, day: u32, people: Vec<Attendee>, subject: &str, summary: Option<&str>| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let started = Local.with_ymd_and_hms(2026, 11, day, 10, 0, 0).unwrap();
            let id = store.insert_meeting(&NewMeeting { title: name, started_at: started, duration_secs: 60.0, language: "de", dir: &dir }, &[]).unwrap();
            if let Some(md) = summary {
                store.save_summary(id, "claude", "m", None, md).unwrap();
            }
            let event = Event { id: format!("ev-{name}"), subject: subject.into(), attendees: people, ..Event::default() };
            TeamsLink::new(event, "me@x.de", "Me").save(&dir).unwrap();
            id
        };
        let anna = person("Anna Lehmann", "anna@x.de");
        let jerome = person("Jerome Markus", "jerome@x.de");
        let bob = person("Bob", "bob@other.de");
        let with_anna = mk(&mut store, "with-anna", 20, vec![anna.clone()], "Offer", Some("# Offer\n\n## Summary\nWe talked prices.\n\n## Action items\n- [ ] Send the offer — **Owner:** Me\n- [x] Call legal — **Owner:** Anna\n"));
        mk(&mut store, "with-bob", 21, vec![bob.clone()], "Other", None);
        let weekly = mk(&mut store, "weekly", 22, vec![bob], "Team Weekly #3", None);

        let event = Event { id: "now".into(), subject: "Team Weekly #4".into(), attendees: vec![anna, jerome, person("Me", "me@x.de")], body: "1. Status".into(), ..Event::default() };
        let brief = build(&store, &event, "me@x.de").unwrap();
        assert_eq!(brief.people, ["Anna Lehmann", "Jerome Markus"]);
        let ids: Vec<i64> = brief.past.iter().map(|p| p.meeting_id).collect();
        assert_eq!(ids, [weekly, with_anna], "newest first; Bob's other meeting isn't related");
        assert!(brief.past[0].same_series);
        assert_eq!(brief.past[1].gist.as_deref(), Some("We talked prices."));
        assert_eq!(brief.open_actions().len(), 1);
        let md = brief.markdown();
        assert!(md.contains("Send the offer"), "{md}");
        assert!(md.contains("1. Status"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
