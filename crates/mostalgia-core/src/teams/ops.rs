//! What the app and the CLI do with Teams, step by step: link a recording
//! to its meeting, name it after the meeting when it's saved, and send its
//! summary and action items back. Every Graph call takes a [`Graph`], so
//! tests run against canned answers.

use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};

use super::actions::{self, ActionItem};
use super::calendar::{self, Event};
use super::graph::Graph;
use super::link::{self, TeamsLink};
use super::vtt;
use super::TeamsConfig;
use crate::i18n::{tr, trf};
use crate::store::{Meeting, Store};

/// Links a recording that started at `started` (its folder `dir`) to the
/// event it belongs to among `events`, and puts the meeting's words in
/// front of the glossary while it's transcribed. `None` when no event
/// fits.
pub fn link_recording(cfg: &TeamsConfig, events: &[Event], started: DateTime<Utc>, dir: &Path) -> Result<Option<TeamsLink>> {
    let Some(event) = calendar::event_for_recording(events, started) else {
        return Ok(None);
    };
    let link = TeamsLink::new(event.clone(), &cfg.account_email, &cfg.account_name);
    link.save(dir)?;
    if cfg.attendee_hints {
        crate::glossary::set_meeting_terms(event.hint_terms(&cfg.account_email));
    }
    Ok(Some(link))
}

/// The calendar as last read (the app reads it every few minutes).
static EVENTS: std::sync::Mutex<Option<(std::time::Instant, Vec<Event>)>> = std::sync::Mutex::new(None);

/// How old the remembered calendar may be for linking a recording.
const EVENTS_FRESH: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Reads the calendar (from 4 hours ago to `hours` ahead) and remembers it.
pub fn refresh_events(cfg: &TeamsConfig, hours: i64) -> Result<Vec<Event>> {
    let events = super::client(cfg)?.upcoming(Utc::now(), hours)?;
    *EVENTS.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), events.clone()));
    Ok(events)
}

/// The calendar as last read, if it's recent.
pub fn cached_events() -> Option<Vec<Event>> {
    let guard = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().filter(|(at, _)| at.elapsed() < EVENTS_FRESH).map(|(_, e)| e.clone())
}

/// Whether Teams is set up to be used for the calendar.
pub fn calendar_ready(cfg: &TeamsConfig) -> bool {
    cfg.calendar_sync && cfg.signed_in() && cfg.client_id().is_some()
}

/// Links a recording that just started (in `dir`) to its meeting, from
/// the remembered calendar or else reading it. Meant for a background
/// thread: it may wait for Microsoft.
pub fn link_new_recording(cfg: &TeamsConfig, dir: &Path, started: DateTime<Utc>) -> Result<Option<TeamsLink>> {
    if !calendar_ready(cfg) {
        return Ok(None);
    }
    let events = match cached_events() {
        Some(e) if calendar::event_for_recording(&e, started).is_some() => e,
        _ => refresh_events(cfg, 12)?,
    };
    link_recording(cfg, &events, started, dir)
}

/// After a summary was written: posts it to the meeting chat when that's
/// turned on and the meeting is linked. `Ok(false)` when nothing was sent.
pub fn after_summary(cfg: &TeamsConfig, store: &Store, meeting_id: i64) -> Result<bool> {
    if !cfg.auto_post_summary || !cfg.signed_in() {
        return Ok(false);
    }
    let Some(meeting) = store.get(meeting_id)? else { return Ok(false) };
    let Some(link) = TeamsLink::load(&meeting.dir) else { return Ok(false) };
    if link.posted_at.is_some() || !link.event.is_teams() {
        return Ok(false);
    }
    post_summary(cfg, &super::client(cfg)?, &meeting)?;
    Ok(true)
}

/// The languages Whisper may pick from for a recording of `event`, when
/// the invite helps: with language detection on and no meeting languages
/// set, the invite's language plus the UI language and English (so a
/// German call with an English invite still works). `None`: leave the
/// settings as they are.
pub fn meeting_languages(event: &Event, config: &crate::config::Config) -> Option<Vec<String>> {
    if config.language != "auto" || !config.meeting_languages.is_empty() || !config.teams.attendee_hints {
        return None;
    }
    let hint = event.language_hint()?;
    let mut langs = vec![hint.to_string()];
    for l in [config.ui_language.as_str(), "en"] {
        if (l.len() == 2) && !langs.iter().any(|x| x == l) {
            langs.push(l.to_string());
        }
    }
    Some(langs)
}

/// The event a recording starting now belongs to, from the remembered
/// calendar (no network).
pub fn current_event(cfg: &TeamsConfig) -> Option<Event> {
    if !calendar_ready(cfg) {
        return None;
    }
    calendar::event_for_recording(&cached_events()?, Utc::now()).cloned()
}

/// What [`after_save`] changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    pub title: Option<String>,
    /// The other person's name in a one-to-one call.
    pub named: Option<String>,
}

/// After a linked recording is saved: names it after the event, and in a
/// call with one other person gives "Andere" their name. Clears the
/// meeting's words from the glossary.
pub fn after_save(cfg: &TeamsConfig, store: &Store, meeting_id: i64) -> Result<Applied> {
    crate::glossary::set_meeting_terms(Vec::new());
    let Some(meeting) = store.get(meeting_id)? else { return Ok(Applied::default()) };
    let Some(link) = TeamsLink::load(&meeting.dir) else { return Ok(Applied::default()) };
    let mut out = Applied::default();
    if cfg.auto_name {
        let title = link.event.recording_title();
        store.set_title(meeting_id, &title)?;
        out.title = Some(title);
    }
    if cfg.attendee_hints
        && let Some(name) = link::single_other(&link)
        && let Some(label) = only_other_label(&meeting)
    {
        store.rename_speaker(meeting_id, &label, &name)?;
        out.named = Some(name);
    }
    Ok(out)
}

/// The one generic label all the others' lines have, if there is just one.
fn only_other_label(meeting: &Meeting) -> Option<String> {
    let mut labels: Vec<&str> = meeting.segments.iter().filter(|s| s.source == crate::Source::System).map(|s| s.speaker_label()).collect();
    labels.sort_unstable();
    labels.dedup();
    match labels.as_slice() {
        [one] if crate::voices::is_generic_label(one) && *one != "Ich" => Some(one.to_string()), // i18n: not UI
        _ => None,
    }
}

/// A linked meeting keeps the event's name when its summary is written.
pub fn keeps_title(cfg: &TeamsConfig, meeting: &Meeting) -> bool {
    cfg.auto_name && TeamsLink::load(&meeting.dir).is_some()
}

/// The meeting's link, or an error saying it has none.
pub fn require_link(meeting: &Meeting) -> Result<TeamsLink> {
    TeamsLink::load(&meeting.dir).with_context(|| tr("Diese Aufnahme gehört zu keiner Teams-Besprechung"))
}

/// Links a saved meeting to the event at its start (for meetings recorded
/// before signing in, or when the calendar wasn't reachable).
pub fn link_saved(cfg: &TeamsConfig, graph: &Graph, meeting: &Meeting) -> Result<TeamsLink> {
    let start = meeting.started_at.with_timezone(&Utc);
    let events = graph.events(start - chrono::Duration::hours(3), start + chrono::Duration::hours(1))?;
    let event = calendar::event_for_recording(&events, start).with_context(|| tr("Zu dieser Zeit steht nichts im Kalender"))?;
    let link = TeamsLink::new(event.clone(), &cfg.account_email, &cfg.account_name);
    link.save(&meeting.dir)?;
    Ok(link)
}

/// Posts the meeting's summary to its Teams chat.
pub fn post_summary(cfg: &TeamsConfig, graph: &Graph, meeting: &Meeting) -> Result<()> {
    let summary = meeting.summary.as_ref().with_context(|| tr("Noch keine Zusammenfassung"))?;
    let mut link = require_link(meeting)?;
    let chat = match &link.chat_id {
        Some(c) => c.clone(),
        None => graph.meeting_chat(&link.event)?,
    };
    let html = actions::chat_message(&meeting.title, &summary.markdown, cfg.post_action_items);
    graph.post_chat_message(&chat, &html)?;
    link.chat_id = Some(chat);
    link.posted_at = Some(Utc::now());
    link.save(&meeting.dir)?;
    Ok(())
}

/// What [`send_to_todo`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TodoResult {
    pub sent: Vec<String>,
    /// Already sent before, done, or someone else's.
    pub skipped: usize,
}

/// The action items that would go to To Do.
pub fn todo_items(cfg: &TeamsConfig, meeting: &Meeting) -> Vec<ActionItem> {
    let Some(summary) = &meeting.summary else { return Vec::new() };
    let link = TeamsLink::load(&meeting.dir);
    let mut me: Vec<String> = link.as_ref().map(TeamsLink::me_names).unwrap_or_default();
    me.push(cfg.account_name.clone());
    actions::parse(&summary.markdown).into_iter().filter(|a| !a.done && (!cfg.todo_only_mine || a.is_mine(&me))).collect()
}

/// Sends the meeting's open action items to the To Do list (each once).
pub fn send_to_todo(cfg: &TeamsConfig, graph: &Graph, meeting: &Meeting) -> Result<TodoResult> {
    let summary = meeting.summary.as_ref().with_context(|| tr("Noch keine Zusammenfassung"))?;
    let all = actions::parse(&summary.markdown);
    let items = todo_items(cfg, meeting);
    let mut link = TeamsLink::load(&meeting.dir);
    let already: Vec<String> = link.as_ref().map(|l| l.todo_sent.clone()).unwrap_or_default();
    let fresh: Vec<&ActionItem> = items.iter().filter(|a| !already.contains(&a.task)).collect();
    let mut out = TodoResult { sent: Vec::new(), skipped: all.len() - fresh.len() };
    if fresh.is_empty() {
        return Ok(out);
    }
    let list = graph.todo_list(cfg.todo_list())?;
    let note = trf("Aus der Besprechung „{}“ vom {}", &[&meeting.title, &crate::i18n::date_time(&meeting.started_at)]);
    for item in fresh {
        let note = match &item.owner {
            Some(o) => format!("{note}\n{}", trf("Verantwortlich: {}", &[o])),
            None => note.clone(),
        };
        graph.add_task(&list, item, &note)?;
        out.sent.push(item.task.clone());
    }
    // Remember what went, so a second press doesn't send it again.
    let l = link.get_or_insert_with(TeamsLink::default);
    l.todo_sent.extend(out.sent.iter().cloned());
    l.save(&meeting.dir)?;
    Ok(out)
}

/// The Teams transcript lined up with ours.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptCheck {
    /// Who Teams heard, in order.
    pub speakers: Vec<String>,
    /// Seconds from Teams' time to ours.
    pub offset: Option<f64>,
    pub suggestions: Vec<vtt::Suggestion>,
}

/// Fetches the meeting's Teams transcript and suggests names for our
/// unnamed speakers from it.
pub fn check_transcript(cfg: &TeamsConfig, graph: &Graph, meeting: &Meeting) -> Result<TranscriptCheck> {
    if !cfg.transcripts {
        bail!("{}", tr("Teams-Transkripte sind in den Einstellungen aus"));
    }
    let mut link = require_link(meeting)?;
    let id = match &link.online_meeting_id {
        Some(id) => id.clone(),
        None => {
            let join = link.event.join_url.clone().with_context(|| tr("Der Termin ist keine Teams-Besprechung"))?;
            let om = graph.online_meeting(&join)?.with_context(|| tr("Teams kennt diese Besprechung nicht (nur Geschäfts- oder Schulkonten)"))?;
            link.online_meeting_id = Some(om.id.clone());
            if link.chat_id.is_none() {
                link.chat_id = om.chat_id;
            }
            link.save(&meeting.dir)?;
            om.id
        }
    };
    let transcripts = graph.transcripts(&id)?;
    // The one that started closest to our recording.
    let start = meeting.started_at.with_timezone(&Utc);
    let t = transcripts
        .iter()
        .min_by_key(|t| t.created.map_or(i64::MAX, |c| (c - start).num_seconds().abs()))
        .with_context(|| tr("Für diese Besprechung gibt es kein Teams-Transkript"))?;
    let cues = vtt::parse(&graph.transcript_vtt(&id, &t.id)?);
    let ours: Vec<vtt::Line> =
        meeting.segments.iter().map(|s| vtt::Line { start: s.start_secs, end: s.end_secs, speaker: s.speaker_label(), text: &s.text }).collect();
    let fallback = t.created.map(|c| (c - start).num_milliseconds() as f64 / 1000.0);
    let offset = vtt::find_offset(&ours, &cues, fallback);
    let suggestions = offset.map(|o| vtt::suggest(&ours, &cues, o, &link.me_name)).unwrap_or_default();
    Ok(TranscriptCheck { speakers: vtt::speakers(&cues), offset, suggestions })
}

/// Renames our speakers as suggested; returns how many lines changed.
pub fn apply_names(store: &Store, meeting: &Meeting, suggestions: &[vtt::Suggestion]) -> Result<usize> {
    let mut changed = 0;
    for s in suggestions {
        changed += store.rename_speaker(meeting.id, &s.label, &s.name)?;
    }
    if let Some(mut link) = TeamsLink::load(&meeting.dir) {
        link.names_from_transcript.extend(suggestions.iter().map(|s| (s.label.clone(), s.name.clone())));
        link.save(&meeting.dir)?;
    }
    Ok(changed)
}

/// Teams meetings of the last `days` days (newest first), e.g. to import
/// the transcript of one nobody recorded.
pub fn recent_meetings(graph: &Graph, days: i64) -> Result<Vec<Event>> {
    let now = Utc::now();
    let mut events: Vec<Event> = graph.events(now - chrono::Duration::days(days), now)?.into_iter().filter(|e| e.is_teams() && e.end <= now).collect();
    events.sort_by(|a, b| b.start.cmp(&a.start));
    Ok(events)
}

/// Adds a meeting nobody recorded from its Teams transcript: named after
/// the event, linked to it, and indexed for search. Returns its id.
pub fn import_transcript(cfg: &TeamsConfig, graph: &Graph, store: &mut Store, event: &Event) -> Result<i64> {
    let join = event.join_url.as_deref().with_context(|| tr("Der Termin ist keine Teams-Besprechung"))?;
    let om = graph.online_meeting(join)?.with_context(|| tr("Teams kennt diese Besprechung nicht (nur Geschäfts- oder Schulkonten)"))?;
    let transcripts = graph.transcripts(&om.id)?;
    let t = transcripts.last().with_context(|| tr("Für diese Besprechung gibt es kein Teams-Transkript"))?;
    let vtt = graph.transcript_vtt(&om.id, &t.id)?;
    let title = event.recording_title();
    let duration = (event.duration().num_seconds() > 0).then(|| event.duration().num_seconds() as f64);
    let imported = crate::text_import::import(
        store,
        &crate::text_import::Request { text: &vtt, title: Some(&title), started_at: event.start.with_timezone(&chrono::Local), duration_secs: duration, language: None },
    )?;
    let id = imported.meeting_id;
    if let Some(m) = store.get(id)? {
        let mut link = TeamsLink::new(event.clone(), &cfg.account_email, &cfg.account_name);
        link.online_meeting_id = Some(om.id);
        link.chat_id = om.chat_id;
        link.save(&m.dir)?;
    }
    Ok(id)
}

/// Speaker-name suggestions for a meeting: its attendees, when it's linked.
pub fn name_suggestions(meeting: &Meeting) -> Vec<String> {
    TeamsLink::load(&meeting.dir).map(|l| l.people_names()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use chrono::{Local, TimeZone};

    use super::*;
    use crate::finalize::FinalSegment;
    use crate::store::NewMeeting;
    use crate::teams::calendar::Attendee;
    use crate::teams::graph::Method;
    use crate::teams::graph::mock::MockHttp;
    use crate::Source;

    struct Lib {
        root: std::path::PathBuf,
        store: Store,
    }

    impl Lib {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("mostalgia-teams-ops-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let store = Store::open(&root.join("db.sqlite")).unwrap();
            Self { root, store }
        }

        fn meeting(&mut self, segments: &[FinalSegment]) -> (i64, std::path::PathBuf) {
            let dir = self.root.join("session");
            std::fs::create_dir_all(&dir).unwrap();
            let started = Local.with_ymd_and_hms(2026, 11, 26, 10, 2, 0).unwrap();
            let id = self
                .store
                .insert_meeting(&NewMeeting { title: "Besprechung 26.11.2026, 10:02", started_at: started, duration_secs: 60.0, language: "de", dir: &dir }, segments)
                .unwrap();
            (id, dir)
        }
    }

    impl Drop for Lib {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn seg(source: Source, start: f64, text: &str) -> FinalSegment {
        FinalSegment { source, start_secs: start, end_secs: start + 4.0, text: text.into(), speaker: None }
    }

    fn one_to_one() -> Event {
        let start = Local.with_ymd_and_hms(2026, 11, 26, 10, 0, 0).unwrap().with_timezone(&Utc);
        Event {
            id: "ev1".into(),
            subject: "Anna / Mosta 1:1".into(),
            start,
            end: start + chrono::Duration::minutes(30),
            provider: "teamsForBusiness".into(),
            join_url: Some("https://teams.microsoft.com/l/meetup-join/1".into()),
            attendees: vec![
                Attendee { name: "Anna Lehmann".into(), email: "anna@x.de".into(), ..Attendee::default() },
                Attendee { name: "Mosta".into(), email: "me@x.de".into(), ..Attendee::default() },
            ],
            ..Event::default()
        }
    }

    fn cfg() -> TeamsConfig {
        TeamsConfig { account_email: "me@x.de".into(), account_name: "Mosta".into(), transcripts: true, ..TeamsConfig::default() }
    }

    #[test]
    fn a_linked_recording_is_named_after_its_meeting() {
        let _turn = crate::glossary::TERMS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut lib = Lib::new("name");
        let (id, dir) = lib.meeting(&[seg(Source::Mic, 0.0, "Hallo Anna"), seg(Source::System, 5.0, "Hallo")]);
        let started = Local.with_ymd_and_hms(2026, 11, 26, 10, 2, 0).unwrap().with_timezone(&Utc);
        let link = link_recording(&cfg(), &[one_to_one()], started, &dir).unwrap().unwrap();
        assert_eq!(link.event.id, "ev1");
        assert!(crate::glossary::meeting_terms().contains(&"Anna Lehmann".to_string()));
        let applied = after_save(&cfg(), &lib.store, id).unwrap();
        assert_eq!(applied.title.as_deref(), Some("Anna / Mosta 1:1"));
        assert_eq!(applied.named.as_deref(), Some("Anna Lehmann"));
        let m = lib.store.get(id).unwrap().unwrap();
        assert_eq!(m.title, "Anna / Mosta 1:1");
        assert_eq!(m.segments[1].speaker_label(), "Anna Lehmann");
        assert_eq!(m.segments[0].speaker_label(), "Ich");
        assert!(keeps_title(&cfg(), &m));
        assert_eq!(name_suggestions(&m), ["Anna Lehmann"]);
    }

    #[test]
    fn several_others_are_not_named() {
        let mut lib = Lib::new("several");
        let mut a = seg(Source::System, 0.0, "a");
        a.speaker = Some("Sprecher 1".into());
        let mut b = seg(Source::System, 5.0, "b");
        b.speaker = Some("Sprecher 2".into());
        let (id, dir) = lib.meeting(&[a, b]);
        TeamsLink::new(one_to_one(), "me@x.de", "Mosta").save(&dir).unwrap();
        let applied = after_save(&cfg(), &lib.store, id).unwrap();
        assert_eq!(applied.named, None);
    }

    #[test]
    fn invite_language_narrows_detection() {
        let mut config = crate::config::Config { language: "auto".into(), ui_language: "de".into(), ..Default::default() };
        let event = Event { subject: "Weekly".into(), body: "Hi all, this is the agenda for our meeting. Please add your topics.".into(), ..Event::default() };
        assert_eq!(meeting_languages(&event, &config).unwrap(), ["en", "de"]);
        config.meeting_languages = vec!["de".into(), "ar".into()];
        assert_eq!(meeting_languages(&event, &config), None, "the user's own choice wins");
        config.meeting_languages.clear();
        config.language = "de".into();
        assert_eq!(meeting_languages(&event, &config), None);
    }

    #[test]
    fn imports_a_teams_transcript_nobody_recorded() {
        let mut lib = Lib::new("import");
        let http = MockHttp::default();
        http.on(Method::Get, "/me/onlineMeetings?", 200, r#"{"value":[{"id":"OM1","chatInfo":{"threadId":"19:chat"}}]}"#);
        http.on(Method::Get, "/content", 200, "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\n<v Anna Lehmann>Hallo zusammen, wir fangen an.</v>\n\n00:00:05.000 --> 00:00:09.000\n<v Jerome Markus>Ich habe die Zahlen dabei.</v>\n");
        http.on(Method::Get, "/transcripts", 200, r#"{"value":[{"id":"T1","createdDateTime":"2026-11-26T09:00:00Z"}]}"#);
        let id = import_transcript(&cfg(), &http.graph(), &mut lib.store, &one_to_one()).unwrap();
        let m = lib.store.get(id).unwrap().unwrap();
        assert_eq!(m.title, "Anna / Mosta 1:1");
        assert_eq!(m.segments.len(), 2);
        assert_eq!(m.segments[1].speaker_label(), "Jerome Markus");
        let link = TeamsLink::load(&m.dir).unwrap();
        assert_eq!(link.online_meeting_id.as_deref(), Some("OM1"));
        let _ = std::fs::remove_dir_all(&m.dir);
    }

    #[test]
    fn no_event_no_link() {
        let lib = Lib::new("none");
        let later = Utc::now() + chrono::Duration::days(3);
        assert!(link_recording(&cfg(), &[one_to_one()], later, &lib.root).unwrap().is_none());
        assert!(TeamsLink::load(&lib.root).is_none());
    }

    #[test]
    fn posts_the_summary_once_the_chat_is_known() {
        let mut lib = Lib::new("post");
        let (id, dir) = lib.meeting(&[seg(Source::Mic, 0.0, "Hallo")]);
        lib.store.save_summary(id, "claude", "m", None, "# T\n\n## Summary\nDone.\n\n## Action items\n- [ ] Send it — **Owner:** Mosta\n").unwrap();
        TeamsLink::new(one_to_one(), "me@x.de", "Mosta").save(&dir).unwrap();
        let http = MockHttp::default();
        http.on(Method::Get, "/me/onlineMeetings?", 200, r#"{"value":[{"id":"OM1","chatInfo":{"threadId":"19:chat"}}]}"#);
        http.on(Method::Post, "/chats/", 201, r#"{"id":"m1"}"#);
        let m = lib.store.get(id).unwrap().unwrap();
        post_summary(&cfg(), &http.graph(), &m).unwrap();
        post_summary(&cfg(), &http.graph(), &m).unwrap();
        let reqs = http.requests();
        assert_eq!(reqs.len(), 3, "the chat is looked up once");
        let link = TeamsLink::load(&dir).unwrap();
        assert_eq!(link.chat_id.as_deref(), Some("19:chat"));
        assert!(link.posted_at.is_some());
    }

    #[test]
    fn todo_gets_each_item_once() {
        let mut lib = Lib::new("todo");
        let (id, dir) = lib.meeting(&[seg(Source::Mic, 0.0, "Hallo")]);
        lib.store
            .save_summary(id, "claude", "m", None, "## Action items\n- [ ] Send it — **Owner:** Mosta\n- [ ] Legal — **Owner:** Anna\n- [x] Old — **Owner:** Mosta\n")
            .unwrap();
        TeamsLink::new(one_to_one(), "me@x.de", "Mosta").save(&dir).unwrap();
        let http = MockHttp::default();
        http.on(Method::Get, "/me/todo/lists", 200, r#"{"value":[{"id":"L","displayName":"Mostalgia"}]}"#);
        http.on(Method::Post, "/tasks", 201, r#"{"id":"T"}"#);
        let m = lib.store.get(id).unwrap().unwrap();
        let mine = TeamsConfig { todo_only_mine: true, ..cfg() };
        let r = send_to_todo(&mine, &http.graph(), &m).unwrap();
        assert_eq!(r.sent, ["Send it"]);
        let r = send_to_todo(&cfg(), &http.graph(), &m).unwrap();
        assert_eq!(r.sent, ["Legal"], "Send it went already");
        let r = send_to_todo(&cfg(), &http.graph(), &m).unwrap();
        assert!(r.sent.is_empty());
        assert_eq!(r.skipped, 3);
    }

    #[test]
    fn transcript_names_our_speakers() {
        let mut lib = Lib::new("vtt");
        let mut lines = Vec::new();
        for (i, (who, text)) in [
            ("Sprecher 1", "Guten Morgen wir starten mit dem Pay Flow Rollout heute"),
            ("Sprecher 2", "die Migration der Kundendaten ist fast fertig es fehlen nur noch zwei Tabellen"),
            ("Sprecher 1", "gut dann machen wir am Freitag den Go live mit dem ganzen Team"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut s = FinalSegment { source: Source::System, start_secs: 30.0 + i as f64 * 8.0, end_secs: 36.0 + i as f64 * 8.0, text: text.into(), speaker: None };
            s.speaker = Some(who.into());
            lines.push(s);
        }
        let (id, dir) = lib.meeting(&lines);
        TeamsLink::new(one_to_one(), "me@x.de", "Mosta").save(&dir).unwrap();
        let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:06.000\n<v Anna Lehmann>Guten Morgen, wir starten mit dem PayFlow Rollout heute.</v>\n\n\
00:00:08.000 --> 00:00:14.000\n<v Jerome Markus>Die Migration der Kundendaten ist fast fertig, es fehlen nur noch zwei Tabellen.</v>\n\n\
00:00:16.000 --> 00:00:22.000\n<v Anna Lehmann>Gut, dann machen wir am Freitag den Go-live mit dem ganzen Team.</v>\n";
        let http = MockHttp::default();
        http.on(Method::Get, "/me/onlineMeetings?", 200, r#"{"value":[{"id":"OM1","chatInfo":{"threadId":"19:chat"}}]}"#);
        http.on(Method::Get, "/content", 200, vtt);
        http.on(Method::Get, "/transcripts", 200, r#"{"value":[{"id":"T1","createdDateTime":"2026-11-26T09:02:30Z"}]}"#);
        let m = lib.store.get(id).unwrap().unwrap();
        let check = check_transcript(&cfg(), &http.graph(), &m).unwrap();
        assert_eq!(check.speakers, ["Anna Lehmann", "Jerome Markus"]);
        let names: Vec<(&str, &str)> = check.suggestions.iter().map(|s| (s.label.as_str(), s.name.as_str())).collect();
        assert_eq!(names, [("Sprecher 1", "Anna Lehmann"), ("Sprecher 2", "Jerome Markus")]);
        assert_eq!(apply_names(&lib.store, &m, &check.suggestions).unwrap(), 3);
        let m = lib.store.get(id).unwrap().unwrap();
        assert_eq!(m.segments[1].speaker_label(), "Jerome Markus");
        assert_eq!(TeamsLink::load(&dir).unwrap().online_meeting_id.as_deref(), Some("OM1"));

        let off = TeamsConfig { transcripts: false, ..cfg() };
        assert!(check_transcript(&off, &http.graph(), &m).is_err());
    }
}
