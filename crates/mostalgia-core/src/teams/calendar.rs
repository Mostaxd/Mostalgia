//! Calendar events from Graph, and what Mostalgia takes from them: which
//! event a recording belongs to, its title, the people in it, words for
//! Whisper and the language the invite is written in.

use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How early a recording may start and still belong to the next meeting.
pub const EARLY_START: Duration = Duration::minutes(15);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attendee {
    pub name: String,
    pub email: String,
    /// "required", "optional", "resource" or "organizer".
    pub kind: String,
    /// "accepted", "declined", "tentativelyAccepted", "none", …
    pub response: String,
}

impl Attendee {
    /// A meeting room or other resource, not a person.
    pub fn is_resource(&self) -> bool {
        self.kind == "resource"
    }

    /// The name to show: the display name, else the address's local part.
    pub fn label(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() && !name.contains('@') {
            return name.to_string();
        }
        let mail = if name.contains('@') { name } else { self.email.trim() };
        let local = mail.split('@').next().unwrap_or_default();
        // "anna.lehmann" → "Anna Lehmann"
        local
            .split(['.', '_', '-'])
            .filter(|p| !p.is_empty())
            .map(|p| {
                let mut c = p.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn same(&self, email: &str) -> bool {
        !email.is_empty() && self.email.eq_ignore_ascii_case(email.trim())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub id: String,
    pub subject: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub organizer: Option<Attendee>,
    pub attendees: Vec<Attendee>,
    /// The Teams join link, when it's an online meeting.
    pub join_url: Option<String>,
    /// "teamsForBusiness", "skypeForConsumer", "unknown", …
    pub provider: String,
    /// The invite's text (agenda), plain.
    pub body: String,
    pub web_link: Option<String>,
    pub location: String,
    pub cancelled: bool,
}

/// An event from a Graph `event` resource (times in UTC, see
/// `Prefer: outlook.timezone="UTC"`). `None` without times.
pub fn parse_event(v: &Value) -> Option<Event> {
    let start = graph_time(&v["start"])?;
    let end = graph_time(&v["end"]).unwrap_or(start);
    let person = |p: &Value, kind: &str| Attendee {
        name: p["emailAddress"]["name"].as_str().unwrap_or_default().trim().into(),
        email: p["emailAddress"]["address"].as_str().unwrap_or_default().trim().into(),
        kind: p["type"].as_str().unwrap_or(kind).into(),
        response: p["status"]["response"].as_str().unwrap_or_default().into(),
    };
    let organizer = v.get("organizer").filter(|o| o.is_object()).map(|o| person(o, "organizer"));
    let attendees = v["attendees"].as_array().map(|a| a.iter().map(|p| person(p, "required")).collect()).unwrap_or_default();
    let join_url = v["onlineMeeting"]["joinUrl"]
        .as_str()
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .or_else(|| find_join_link(v["body"]["content"].as_str().unwrap_or_default()));
    let body = v["body"]["content"].as_str().or(v["bodyPreview"].as_str()).unwrap_or_default();
    Some(Event {
        id: v["id"].as_str().unwrap_or_default().into(),
        subject: v["subject"].as_str().unwrap_or_default().trim().into(),
        start,
        end,
        organizer,
        attendees,
        join_url,
        provider: v["onlineMeetingProvider"].as_str().unwrap_or_default().into(),
        body: clean_body(body),
        web_link: v["webLink"].as_str().map(str::to_string),
        location: v["location"]["displayName"].as_str().unwrap_or_default().into(),
        cancelled: v["isCancelled"].as_bool().unwrap_or(false),
    })
}

/// `{"dateTime": "2026-11-26T09:00:00.0000000", "timeZone": "UTC"}`.
fn graph_time(v: &Value) -> Option<DateTime<Utc>> {
    let s = v["dateTime"].as_str()?;
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")).ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

/// A Teams join link in an invite's text (events created outside Outlook).
fn find_join_link(text: &str) -> Option<String> {
    let at = text.find("https://teams.microsoft.com/l/meetup-join/").or_else(|| text.find("https://teams.live.com/meet/"))?;
    let link: String = text[at..].chars().take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '<' | '>' | ')')).collect();
    Some(link)
}

/// An invite's text without Teams' join boilerplate and blank runs.
fn clean_body(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        // Teams' block starts with a long underscore rule.
        if l.starts_with("________________") {
            break;
        }
        if l.is_empty() && out.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        out.push(l);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

impl Event {
    /// A Teams meeting (has a Teams join link or provider).
    pub fn is_teams(&self) -> bool {
        self.provider == "teamsForBusiness"
            || self.join_url.as_deref().is_some_and(|u| u.contains("teams.microsoft.com") || u.contains("teams.live.com"))
    }

    pub fn duration(&self) -> Duration {
        self.end - self.start
    }

    /// Running at `t`, or starting within [`EARLY_START`].
    pub fn covers(&self, t: DateTime<Utc>) -> bool {
        self.start - EARLY_START <= t && t < self.end
    }

    /// The title for a recording of it.
    pub fn recording_title(&self) -> String {
        let s = self.subject.trim();
        if s.is_empty() { crate::i18n::tr("Teams-Besprechung").into() } else { s.chars().take(120).collect() }
    }

    /// Organizer and attendees except `me` and rooms, without duplicates.
    pub fn people(&self, me_email: &str) -> Vec<Attendee> {
        let mut out: Vec<Attendee> = Vec::new();
        for p in self.organizer.iter().chain(&self.attendees) {
            if p.is_resource() || p.same(me_email) || p.label().is_empty() {
                continue;
            }
            let dup = out.iter().any(|q| (!p.email.is_empty() && q.email.eq_ignore_ascii_case(&p.email)) || q.label() == p.label());
            if !dup {
                out.push(p.clone());
            }
        }
        out
    }

    /// The names of the other people, as speaker-name suggestions.
    pub fn people_names(&self, me_email: &str) -> Vec<String> {
        self.people(me_email).iter().map(Attendee::label).collect()
    }

    /// Words for the transcription hint: people's names, then the
    /// subject's distinctive words (product names, acronyms, codes).
    pub fn hint_terms(&self, me_email: &str) -> Vec<String> {
        let mut terms: Vec<String> = self.people_names(me_email);
        for word in distinctive_words(&self.subject).into_iter().chain(distinctive_words(&self.body)) {
            if !terms.iter().any(|t| t.eq_ignore_ascii_case(&word) || t.split(' ').any(|p| p.eq_ignore_ascii_case(&word))) {
                terms.push(word);
            }
            if terms.len() >= MAX_HINT_TERMS {
                break;
            }
        }
        terms.truncate(MAX_HINT_TERMS);
        terms
    }

    /// The language the invite is written in, when it's clear.
    pub fn language_hint(&self) -> Option<&'static str> {
        guess_language(&format!("{}\n{}", self.subject, self.body))
    }
}

/// Most words one meeting adds to the hint (it shares Whisper's prompt
/// with the glossary).
pub const MAX_HINT_TERMS: usize = 12;

/// Words that look like names of things: CamelCase ("PayFlow"),
/// acronyms ("CRM", "Q4"), with digits ("v2", "JIRA-123").
pub fn distinctive_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '(' | ')' | '[' | ']' | '"' | '/' | '|' | '!' | '?')) {
        let w = raw.trim_matches(|c: char| !c.is_alphanumeric());
        let chars: Vec<char> = w.chars().collect();
        if chars.len() < 2 || chars.len() > 30 || w.contains("http") || w.contains('@') {
            continue;
        }
        let upper = chars.iter().filter(|c| c.is_uppercase()).count();
        let lower = chars.iter().filter(|c| c.is_lowercase()).count();
        let digits = chars.iter().filter(|c| c.is_ascii_digit()).count();
        let camel = upper >= 2 && lower >= 1 && chars[1..].iter().any(|c| c.is_uppercase());
        let acronym = upper >= 2 && lower == 0;
        let code = digits > 0 && (upper + lower) > 0;
        if (camel || acronym || code) && !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// The event a recording that started at `t` belongs to: a running (or
/// about to start) event, Teams meetings first, then the one whose start
/// is closest to `t`.
pub fn event_for_recording(events: &[Event], t: DateTime<Utc>) -> Option<&Event> {
    events
        .iter()
        .filter(|e| e.covers(t) && !e.cancelled)
        .min_by_key(|e| (!e.is_teams(), (e.start - t).num_seconds().abs(), e.duration().num_seconds()))
}

/// The next Teams meeting that hasn't ended at `now`.
pub fn next_teams_meeting(events: &[Event], now: DateTime<Utc>) -> Option<&Event> {
    events.iter().filter(|e| e.is_teams() && e.end > now && !e.cancelled).min_by_key(|e| e.start)
}

/// Common words per language, for [`guess_language`].
const STOP_WORDS: &[(&str, &[&str])] = &[ // i18n: not UI
    ("de", &["und", "der", "die", "das", "ist", "nicht", "mit", "für", "wir", "ich", "auf", "zu", "den", "ein", "eine", "bitte", "besprechung", "termin", "uns", "über"]),
    ("en", &["the", "and", "is", "to", "of", "for", "we", "with", "on", "this", "please", "meeting", "our", "will", "be", "are", "agenda", "you", "let's", "about"]),
    ("fr", &["le", "la", "les", "et", "est", "pour", "nous", "avec", "des", "une", "un", "réunion", "sur", "pas", "vous", "ordre"]),
    ("es", &["el", "la", "los", "las", "y", "es", "para", "con", "una", "reunión", "por", "que", "nosotros", "del", "sobre"]),
    ("it", &["il", "la", "e", "è", "per", "con", "che", "una", "riunione", "del", "della", "noi", "sono", "non"]),
    ("nl", &["de", "het", "en", "is", "voor", "met", "een", "wij", "we", "van", "vergadering", "niet", "ons", "over"]),
];

/// The language of a short text (an invite) by its common words; `None`
/// when there's too little text or no clear winner. Arabic by its script.
pub fn guess_language(text: &str) -> Option<&'static str> {
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    let arabic = text.chars().filter(|c| ('\u{0600}'..='\u{06FF}').contains(c)).count();
    if letters >= 8 && arabic * 2 > letters {
        return Some("ar");
    }
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut scores: Vec<(&'static str, usize)> =
        STOP_WORDS.iter().map(|(code, list)| (*code, words.iter().filter(|w| list.contains(&w.as_str())).count())).collect();
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    let (best, n) = scores[0];
    let second = scores[1].1;
    (n >= 3 && n >= second * 2).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn sample() -> Value {
        json!({
            "id": "AAMk1",
            "subject": "PayFlow Weekly – Q4 rollout",
            "start": {"dateTime": "2026-11-26T09:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-26T09:30:00.0000000", "timeZone": "UTC"},
            "organizer": {"emailAddress": {"name": "Anna Lehmann", "address": "anna@firma.de"}},
            "attendees": [
                {"type": "required", "status": {"response": "accepted"}, "emailAddress": {"name": "Mosta", "address": "me@firma.de"}},
                {"type": "required", "status": {"response": "none"}, "emailAddress": {"name": "anna@firma.de", "address": "anna@firma.de"}},
                {"type": "optional", "status": {"response": "tentativelyAccepted"}, "emailAddress": {"name": "", "address": "jerome.markus@firma.de"}},
                {"type": "resource", "emailAddress": {"name": "Raum Elbe", "address": "elbe@firma.de"}}
            ],
            "isOnlineMeeting": true,
            "onlineMeetingProvider": "teamsForBusiness",
            "onlineMeeting": {"joinUrl": "https://teams.microsoft.com/l/meetup-join/19%3ameeting"},
            "body": {"contentType": "text", "content": "Agenda:\n\n\n1. Status CRM-Migration\n2. Go-live\n________________________________________________________________________________\nMicrosoft Teams Need help?"},
        })
    }

    #[test]
    fn parses_a_graph_event() {
        let e = parse_event(&sample()).unwrap();
        assert_eq!(e.subject, "PayFlow Weekly – Q4 rollout");
        assert_eq!(e.start, t("2026-11-26T09:00:00Z"));
        assert_eq!(e.duration(), Duration::minutes(30));
        assert!(e.is_teams());
        assert_eq!(e.body, "Agenda:\n\n1. Status CRM-Migration\n2. Go-live");
        assert_eq!(e.organizer.as_ref().unwrap().kind, "organizer");
    }

    #[test]
    fn people_skip_me_rooms_and_duplicates() {
        let e = parse_event(&sample()).unwrap();
        assert_eq!(e.people_names("ME@firma.de"), ["Anna Lehmann", "Jerome Markus"]);
    }

    #[test]
    fn hint_terms_have_names_then_product_words() {
        let e = parse_event(&sample()).unwrap();
        let terms = e.hint_terms("me@firma.de");
        assert_eq!(terms[..2], ["Anna Lehmann", "Jerome Markus"]);
        assert!(terms.contains(&"PayFlow".to_string()), "{terms:?}");
        assert!(terms.contains(&"Q4".to_string()), "{terms:?}");
        assert!(terms.contains(&"CRM-Migration".to_string()), "{terms:?}");
        assert!(!terms.contains(&"Weekly".to_string()));
        assert!(terms.len() <= MAX_HINT_TERMS);
    }

    #[test]
    fn join_link_from_body_when_not_an_online_meeting() {
        let mut v = sample();
        v["onlineMeeting"] = Value::Null;
        v["onlineMeetingProvider"] = json!("unknown");
        v["body"]["content"] = json!("Join: https://teams.microsoft.com/l/meetup-join/abc%40thread.v2/0?context=x\nThanks");
        let e = parse_event(&v).unwrap();
        assert_eq!(e.join_url.as_deref(), Some("https://teams.microsoft.com/l/meetup-join/abc%40thread.v2/0?context=x"));
        assert!(e.is_teams());
    }

    #[test]
    fn recording_matches_the_running_teams_meeting() {
        let teams = parse_event(&sample()).unwrap();
        let lunch = Event { id: "lunch".into(), subject: "Lunch".into(), start: t("2026-11-26T08:30:00Z"), end: t("2026-11-26T10:00:00Z"), ..Event::default() };
        let later = Event { id: "later".into(), start: t("2026-11-26T11:00:00Z"), end: t("2026-11-26T12:00:00Z"), provider: "teamsForBusiness".into(), ..Event::default() };
        let events = vec![lunch, teams, later];
        assert_eq!(event_for_recording(&events, t("2026-11-26T08:50:00Z")).unwrap().id, "AAMk1", "10 min early still counts, Teams first");
        assert_eq!(event_for_recording(&events, t("2026-11-26T09:40:00Z")).unwrap().id, "lunch");
        assert!(event_for_recording(&events, t("2026-11-26T10:30:00Z")).is_none());
        assert_eq!(event_for_recording(&events, t("2026-11-26T10:50:00Z")).unwrap().id, "later");
        assert_eq!(next_teams_meeting(&events, t("2026-11-26T09:45:00Z")).unwrap().id, "later");
    }

    #[test]
    fn attendee_labels() {
        let a = Attendee { email: "anna.lehmann@x.de".into(), ..Attendee::default() };
        assert_eq!(a.label(), "Anna Lehmann");
        let b = Attendee { name: "Jérôme".into(), ..Attendee::default() };
        assert_eq!(b.label(), "Jérôme");
    }

    #[test]
    fn guesses_the_invite_language() {
        assert_eq!(guess_language("Hallo zusammen, wir sprechen über die Planung und den Termin für das Release."), Some("de"));
        assert_eq!(guess_language("Hi all, this is the agenda for our meeting. Please add your topics to the list."), Some("en"));
        assert_eq!(guess_language("Sync"), None);
        assert_eq!(guess_language("اجتماع الفريق الأسبوعي لمناقشة الخطة"), Some("ar"));
    }

    #[test]
    fn distinctive_words_skip_plain_words() {
        assert_eq!(distinctive_words("Weekly sync about the iOS app, JIRA-123 and v2 (OKR)"), ["iOS", "JIRA-123", "v2", "OKR"]);
    }
}
