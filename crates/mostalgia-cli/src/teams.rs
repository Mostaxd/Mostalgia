//! `mostalgia teams …`: the Microsoft Teams integration from the terminal.

use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use clap::Subcommand;
use mostalgia_core::config::Config;
use mostalgia_core::i18n::{tr, trf};
use mostalgia_core::store::Store;
use mostalgia_core::teams::{self, auth, brief, calendar, ops};
use mostalgia_core::paths;

#[derive(Subcommand)]
pub enum TeamsAction {
    /// Show the settings and whether you're signed in (default)
    Status,
    /// Set the app registration's client ID and tenant, and the switches
    Setup {
        /// Application (client) ID from Microsoft Entra ID
        #[arg(long)]
        client_id: Option<String>,
        /// "common", "organizations", a tenant id or a domain
        #[arg(long)]
        tenant: Option<String>,
        /// Read the calendar for Teams meetings (on/off)
        #[arg(long)]
        calendar: Option<Toggle>,
        /// Name recordings after their meeting (on/off)
        #[arg(long)]
        auto_name: Option<Toggle>,
        /// Post the summary to the meeting chat as soon as it's written (on/off)
        #[arg(long)]
        auto_post: Option<Toggle>,
        /// Ask for the Teams transcript permission, needs admin consent (on/off)
        #[arg(long)]
        transcripts: Option<Toggle>,
        /// To Do list for action items
        #[arg(long)]
        todo_list: Option<String>,
        /// Only send my own action items to To Do (on/off)
        #[arg(long)]
        todo_only_mine: Option<Toggle>,
        /// Read the meeting chat into the summary and Ask (on/off)
        #[arg(long)]
        chat: Option<Toggle>,
        /// Recurring meetings: say what changed since last time (on/off)
        #[arg(long)]
        series: Option<Toggle>,
    },
    /// Sign in with Microsoft in the browser
    Login,
    /// Sign out (forgets the stored tokens)
    Logout,
    /// Upcoming meetings from the calendar
    Upcoming {
        /// How far ahead, in hours
        #[arg(long, default_value_t = 24)]
        hours: i64,
        /// All events, not only Teams meetings
        #[arg(long)]
        all: bool,
    },
    /// The brief for the next Teams meeting: agenda, people, what's still open from last time
    Brief,
    /// Link a saved meeting to the calendar event at its start time
    Link { id: i64 },
    /// Post a meeting's summary to its Teams chat
    Post { id: i64 },
    /// Send a meeting's action items to Microsoft To Do
    Todo {
        id: i64,
        /// Only list what would be sent
        #[arg(long)]
        dry_run: bool,
    },
    /// Add a Teams meeting nobody recorded, from its Teams transcript.
    /// Without a number, lists the last week's Teams meetings to pick from
    Import {
        /// The meeting's number in the list
        pick: Option<usize>,
        /// How many days back to look
        #[arg(long, default_value_t = 7)]
        days: i64,
    },
    /// Read what was typed in the meeting chat during the call
    Chat { id: i64 },
    /// The other recordings of a meeting's recurring series
    Series { id: i64 },
    /// Fetch the Teams transcript and suggest names for unnamed speakers
    Transcript {
        id: i64,
        /// Rename the speakers as suggested
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

impl Toggle {
    fn on(self) -> bool {
        self == Toggle::On
    }
}

fn on_off(b: bool) -> &'static str {
    if b { tr("an") } else { tr("aus") }
}

fn meeting(store: &Store, id: i64) -> Result<mostalgia_core::store::Meeting> {
    store.get(id)?.with_context(|| trf("Keine Besprechung #{}", &[&id]))
}

pub fn run(action: Option<TeamsAction>) -> Result<()> {
    let mut config = Config::load();
    match action.unwrap_or(TeamsAction::Status) {
        TeamsAction::Status => {
            let t = &config.teams;
            let id = t.client_id().unwrap_or_else(|| tr("(nicht eingetragen)").into());
            println!("{:<16} {id}", tr("Client-ID"));
            println!("{:<16} {}", tr("Mandant"), t.tenant());
            let account = if t.signed_in() && auth::has_session() { t.account_label() } else { tr("nicht angemeldet").into() };
            println!("{:<16} {account}", tr("Konto"));
            println!("{:<16} {}", tr("Kalender"), on_off(t.calendar_sync));
            println!("{:<16} {}", tr("Automatisch benennen"), on_off(t.auto_name));
            println!("{:<16} {}", tr("Zusammenfassung posten"), on_off(t.auto_post_summary));
            println!("{:<16} {}", tr("Teams-Transkripte"), on_off(t.transcripts));
            println!("{:<16} {}", tr("Besprechungschat"), on_off(t.chat_context));
            println!("{:<16} {}", tr("Serien vergleichen"), on_off(t.series_compare));
            println!("{:<16} {}", tr("To-Do-Liste"), t.todo_list());
            if t.client_id().is_none() {
                println!("\n{}", tr("App-Registrierung: siehe docs/teams-integration.md, dann `mostalgia teams setup --client-id …`"));
            }
        }
        TeamsAction::Setup { client_id, tenant, calendar, auto_name, auto_post, transcripts, todo_list, todo_only_mine, chat, series } => {
            let t = &mut config.teams;
            if let Some(id) = client_id {
                anyhow::ensure!(teams::is_guid(id.trim()), "{}", tr("Die Client-ID ist eine GUID (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)"));
                t.client_id = id.trim().into();
            }
            if let Some(v) = tenant {
                t.tenant = v.trim().into();
            }
            if let Some(v) = calendar {
                t.calendar_sync = v.on();
            }
            if let Some(v) = auto_name {
                t.auto_name = v.on();
            }
            if let Some(v) = auto_post {
                t.auto_post_summary = v.on();
            }
            if let Some(v) = transcripts {
                if v.on() && !t.transcripts {
                    eprintln!("[mostalgia] {}", tr("Neue Berechtigung: bitte `mostalgia teams login` noch einmal ausführen"));
                }
                t.transcripts = v.on();
            }
            if let Some(v) = todo_list {
                t.todo_list = v.trim().into();
            }
            if let Some(v) = todo_only_mine {
                t.todo_only_mine = v.on();
            }
            if let Some(v) = chat {
                if v.on() && !t.chat_context {
                    eprintln!("[mostalgia] {}", tr("Neue Berechtigung: bitte `mostalgia teams login` noch einmal ausführen"));
                }
                t.chat_context = v.on();
            }
            if let Some(v) = series {
                t.series_compare = v.on();
            }
            config.save()?;
            return run(Some(TeamsAction::Status));
        }
        TeamsAction::Login => {
            eprintln!("[mostalgia] {}", tr("Öffne die Microsoft-Anmeldung im Browser …"));
            let open = |url: &str| {
                if auth::open_browser(url).is_err() {
                    eprintln!("{}\n{url}", tr("Bitte diese Adresse im Browser öffnen:"));
                }
                Ok(())
            };
            let account = auth::login(&config.teams, &open, &AtomicBool::new(false))?;
            config.teams.account_name = account.name;
            config.teams.account_email = account.email;
            config.save()?;
            println!("{}", trf("Angemeldet als {}", &[&config.teams.account_label()]));
        }
        TeamsAction::Logout => {
            auth::logout()?;
            config.teams.account_name.clear();
            config.teams.account_email.clear();
            config.save()?;
            println!("{}", tr("Abgemeldet"));
        }
        TeamsAction::Upcoming { hours, all } => {
            let events = teams::client(&config.teams)?.upcoming(Utc::now(), hours)?;
            let shown: Vec<_> = events.iter().filter(|e| all || e.is_teams()).collect();
            if shown.is_empty() {
                println!("{}", tr("Keine Teams-Besprechungen in diesem Zeitraum"));
            }
            for e in shown {
                let start = e.start.with_timezone(&Local);
                let end = e.end.with_timezone(&Local);
                let mark = if e.is_teams() { "T" } else { " " };
                let people = e.people_names(&config.teams.account_email);
                let people = if people.is_empty() { String::new() } else { format!("  ({})", people.join(", ")) };
                println!("{mark} {} – {}  {}{people}", mostalgia_core::i18n::date_time(&start), end.format("%H:%M"), e.recording_title());
            }
        }
        TeamsAction::Brief => {
            let events = teams::client(&config.teams)?.upcoming(Utc::now(), 24 * 7)?;
            let event = calendar::next_teams_meeting(&events, Utc::now()).with_context(|| tr("Keine Teams-Besprechungen in diesem Zeitraum"))?;
            let store = Store::open(&paths::database())?;
            println!("{}", brief::build(&store, event, &config.teams.account_email)?.markdown());
        }
        TeamsAction::Link { id } => {
            let store = Store::open(&paths::database())?;
            let m = meeting(&store, id)?;
            let link = ops::link_saved(&config.teams, &teams::client(&config.teams)?, &m)?;
            println!("{}", trf("Verknüpft mit „{}“", &[&link.event.recording_title()]));
            let applied = ops::after_save(&config.teams, &store, id)?;
            if let Some(name) = applied.named {
                println!("{}", trf("„Andere“ heißt jetzt {}", &[&name]));
            }
        }
        TeamsAction::Post { id } => {
            let store = Store::open(&paths::database())?;
            ops::post_summary(&config.teams, &teams::client(&config.teams)?, &meeting(&store, id)?)?;
            println!("{}", tr("Zusammenfassung im Besprechungschat gepostet"));
        }
        TeamsAction::Todo { id, dry_run } => {
            let store = Store::open(&paths::database())?;
            let m = meeting(&store, id)?;
            if dry_run {
                for item in ops::todo_items(&config.teams, &m) {
                    println!("- {}", item.line());
                }
                return Ok(());
            }
            let r = ops::send_to_todo(&config.teams, &teams::client(&config.teams)?, &m)?;
            println!("{}", trf("{} Aufgaben an To Do geschickt ({} übersprungen)", &[&r.sent.len(), &r.skipped]));
            for t in r.sent {
                println!("- {t}");
            }
        }
        TeamsAction::Import { pick, days } => {
            let graph = teams::client(&config.teams)?;
            let events = ops::recent_meetings(&graph, days)?;
            let Some(n) = pick else {
                if events.is_empty() {
                    println!("{}", tr("Keine Teams-Besprechungen in diesem Zeitraum"));
                }
                for (i, e) in events.iter().enumerate() {
                    println!("{:>3}  {}  {}", i + 1, mostalgia_core::i18n::date_time(&e.start.with_timezone(&Local)), e.recording_title());
                }
                return Ok(());
            };
            let event = events.get(n.wrapping_sub(1)).with_context(|| trf("Keine Besprechung #{}", &[&n]))?;
            let mut store = Store::open(&paths::database())?;
            let id = ops::import_transcript(&config.teams, &graph, &mut store, event)?;
            drop(store);
            if let Err(e) = mostalgia_core::ask::index_meeting(id) {
                eprintln!("[mostalgia] search index: {e:#}");
            }
            println!("{}", trf("Als Besprechung #{} hinzugefügt – `mostalgia summarize {}` schreibt die Zusammenfassung", &[&id, &id]));
        }
        TeamsAction::Chat { id } => {
            let store = Store::open(&paths::database())?;
            let m = meeting(&store, id)?;
            let n = ops::read_chat(&teams::client(&config.teams)?, &m)?;
            drop(store);
            if let Err(e) = mostalgia_core::ask::index_meeting(id) {
                eprintln!("[mostalgia] search index: {e:#}");
            }
            let start = m.started_at.with_timezone(&Utc);
            for line in ops::require_link(&m)?.chat {
                println!("{}", teams::chat::transcript_line(&line, start));
            }
            println!("{}", trf("{} Nachrichten aus dem Besprechungschat gelesen", &[&n]));
        }
        TeamsAction::Series { id } => {
            let store = Store::open(&paths::database())?;
            let m = meeting(&store, id)?;
            let series = ops::series(&store, &m)?;
            if series.is_empty() {
                println!("{}", tr("Keine anderen Aufnahmen dieser Serie"));
            }
            for s in &series {
                let mark = if s.summary.is_some() { " " } else { "·" };
                println!("{:>5} {mark} {}  {}", format!("#{}", s.meeting_id), mostalgia_core::i18n::date_time(&s.started_at), s.title);
            }
            if let Some(prev) = ops::previous_in_series(&store, &m)? {
                println!("\n{}", trf("Die Zusammenfassung vergleicht mit #{} ({})", &[&prev.meeting_id, &mostalgia_core::i18n::date(&prev.started_at)]));
            }
        }
        TeamsAction::Transcript { id, apply } => {
            let store = Store::open(&paths::database())?;
            let m = meeting(&store, id)?;
            let check = ops::check_transcript(&config.teams, &teams::client(&config.teams)?, &m)?;
            println!("{}", trf("Teams hat gehört: {}", &[&check.speakers.join(", ")]));
            if let Some(o) = check.offset {
                println!("{}", trf("Versatz: {} s", &[&format!("{o:+.0}")]));
            }
            if check.suggestions.is_empty() {
                println!("{}", tr("Keine eindeutigen Namen für die Sprecher"));
            }
            for s in &check.suggestions {
                println!("{} → {} ({:.0} s, {:.0} %)", mostalgia_core::i18n::speaker(&s.label), s.name, s.seconds, s.share * 100.0);
            }
            if apply && !check.suggestions.is_empty() {
                let n = ops::apply_names(&store, &m, &check.suggestions)?;
                println!("{}", trf("{} Zeilen umbenannt", &[&n]));
            }
        }
    }
    Ok(())
}
