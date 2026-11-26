//! Which Teams event a meeting belongs to, kept as `teams.json` in the
//! meeting's folder (next to its audio) so the library's schema stays as
//! it is, and what was already sent to Teams for it.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::calendar::Event;
use super::chat::ChatLine;

pub const FILE_NAME: &str = "teams.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamsLink {
    pub event: Event,
    /// The signed-in account when it was linked (to tell "me" apart).
    pub me_email: String,
    pub me_name: String,
    /// Graph's online meeting id (for transcripts), once looked up.
    pub online_meeting_id: Option<String>,
    /// The meeting chat, once looked up.
    pub chat_id: Option<String>,
    /// When the summary was last posted to the chat.
    pub posted_at: Option<DateTime<Utc>>,
    /// Action items already sent to To Do (their text), so they aren't sent twice.
    pub todo_sent: Vec<String>,
    /// Speaker names taken over from the Teams transcript.
    pub names_from_transcript: Vec<(String, String)>,
    /// The id of the summary message Mostalgia posted (left out of the chat).
    pub posted_message_id: Option<String>,
    /// What was typed in the meeting chat during the call.
    pub chat: Vec<ChatLine>,
    /// When the chat was last read.
    pub chat_read_at: Option<DateTime<Utc>>,
}

impl TeamsLink {
    pub fn new(event: Event, me_email: &str, me_name: &str) -> Self {
        Self { event, me_email: me_email.into(), me_name: me_name.into(), ..Self::default() }
    }

    /// The link of the meeting in `dir`, if it has one.
    pub fn load(dir: &Path) -> Option<Self> {
        std::fs::read_to_string(dir.join(FILE_NAME)).ok().and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).context("creating the meeting folder")?;
        let path = dir.join(FILE_NAME);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?).context("writing teams.json")?;
        std::fs::rename(&tmp, &path).context("saving teams.json")?;
        Ok(())
    }

    pub fn remove(dir: &Path) -> Result<()> {
        match std::fs::remove_file(dir.join(FILE_NAME)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).context("removing teams.json"),
            _ => Ok(()),
        }
    }

    /// The other people in the meeting (speaker-name suggestions).
    pub fn people_names(&self) -> Vec<String> {
        self.event.people_names(&self.me_email)
    }

    /// Names that mean the user ("Me" in action items and transcripts).
    pub fn me_names(&self) -> Vec<String> {
        [self.me_name.clone(), self.me_email.split('@').next().unwrap_or_default().to_string()]
            .into_iter()
            .filter(|n| !n.trim().is_empty())
            .collect()
    }
}

/// A one-to-one call: exactly one other person was invited.
pub fn single_other(link: &TeamsLink) -> Option<String> {
    let people = link.people_names();
    (people.len() == 1).then(|| people[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teams::calendar::Attendee;

    #[test]
    fn saves_next_to_the_meeting() {
        let dir = std::env::temp_dir().join(format!("mostalgia-teams-link-{}", std::process::id()));
        let event = Event {
            subject: "1:1".into(),
            attendees: vec![
                Attendee { name: "Me".into(), email: "me@x.de".into(), ..Attendee::default() },
                Attendee { name: "Anna Lehmann".into(), email: "anna@x.de".into(), ..Attendee::default() },
            ],
            ..Event::default()
        };
        let mut link = TeamsLink::new(event, "me@x.de", "Mosta Ahmed");
        link.todo_sent.push("Send the offer".into());
        link.save(&dir).unwrap();
        let back = TeamsLink::load(&dir).unwrap();
        assert_eq!(back, link);
        assert_eq!(single_other(&back).as_deref(), Some("Anna Lehmann"));
        assert_eq!(back.me_names(), ["Mosta Ahmed", "me"]);
        TeamsLink::remove(&dir).unwrap();
        assert!(TeamsLink::load(&dir).is_none());
        TeamsLink::remove(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
