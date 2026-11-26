//! Auto-start with calls: turns call start/end events from
//! `mostalgia_core::callwatch` into prompts, desktop notifications and
//! start/stop actions, following the "Anrufe erkennen" settings.

use mostalgia_core::i18n::{tr, trf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Receiver;
use mostalgia_core::callwatch::{self, CallEvent};
use mostalgia_core::config::Config;

/// What the app should do in response.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Start recording (the call app's label).
    Start(String),
    Stop,
}

pub struct AutoStart {
    events: Receiver<CallEvent>,
    /// "Aufnahme starten" clicked in a desktop notification (app label).
    clicked: Receiver<String>,
    clicked_tx: crossbeam_channel::Sender<String>,
    /// A call is asking whether to record ("fragen" mode): its label.
    pub prompt: Option<String>,
    /// The call whose start started the current recording.
    started_for: Option<String>,
    stop: Arc<AtomicBool>,
}

impl Default for AutoStart {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AutoStart {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl AutoStart {
    /// Starts watching for calls in the background.
    pub fn new() -> Self {
        let (tx, events) = crossbeam_channel::unbounded();
        let (clicked_tx, clicked) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        // Headless renders (the screenshots example) set this.
        if std::env::var_os("MOSTALGIA_NO_CALLWATCH").is_none() {
            callwatch::spawn(tx, stop.clone());
            let stop = stop.clone();
            std::thread::Builder::new().name("teams-calendar".into()).spawn(move || teams_watch(stop)).ok();
        }
        Self {
            events,
            clicked,
            clicked_tx,
            prompt: None,
            started_for: None,
            stop,
        }
    }

    /// Processes pending call events. `active` = a recording is running
    /// (or starting / finishing).
    pub fn tick(&mut self, active: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        if !active && self.started_for.is_some() && self.prompt.is_none() {
            // The recording we started has been stopped by hand meanwhile.
            self.started_for = None;
        }
        for app in self.clicked.try_iter().collect::<Vec<_>>() {
            if !active && actions.is_empty() {
                self.prompt = None;
                self.started_for = Some(app.clone());
                actions.push(Action::Start(app));
            }
        }
        for event in self.events.try_iter().collect::<Vec<_>>() {
            let config = Config::load();
            match event {
                CallEvent::Started { app } => {
                    remember_app(&app);
                    if config.call_autostart == "aus" || config.call_apps_ignored.contains(&app) || active {
                        continue;
                    }
                    if config.call_autostart == "automatisch" {
                        self.started_for = Some(app.clone());
                        notify(tr("Aufnahme gestartet"), &trf("Anruf in {} erkannt – Mostalgia nimmt auf.", &[&app]), None);
                        actions.push(Action::Start(app));
                    } else {
                        self.prompt = Some(app.clone());
                        notify(
                            &trf("Anruf in {}", &[&app]),
                            tr("Soll Mostalgia diese Besprechung aufnehmen?"),
                            Some((app, self.clicked_tx.clone())),
                        );
                    }
                }
                CallEvent::Ended { app } => {
                    if self.prompt.as_deref() == Some(app.as_str()) {
                        self.prompt = None;
                    }
                    if self.started_for.as_deref() == Some(app.as_str()) {
                        self.started_for = None;
                        if config.call_stop_auto && active {
                            notify(tr("Aufnahme beendet"), &trf("Der Anruf in {} ist vorbei.", &[&app]), None);
                            actions.push(Action::Stop);
                        }
                    }
                }
            }
        }
        actions
    }

    /// "Aufnahme starten" in the in-app prompt.
    pub fn accept(&mut self) -> Option<Action> {
        let app = self.prompt.take()?;
        self.started_for = Some(app.clone());
        Some(Action::Start(app))
    }

    /// "Ignorieren" in the in-app prompt.
    pub fn dismiss(&mut self) {
        self.prompt = None;
    }
}

/// Reads the Teams calendar every few minutes while signed in (recordings
/// started for a detected call are linked to the meeting from it) and
/// shows the brief before a meeting. It never starts a recording: that
/// stays with call detection, so meetings you don't join aren't recorded.
fn teams_watch(stop: Arc<AtomicBool>) {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use chrono::Utc;
    use mostalgia_core::teams::{brief, ops};

    const REFRESH: Duration = Duration::from_secs(5 * 60);
    let mut briefed: HashSet<String> = HashSet::new();
    let mut refreshed: Option<Instant> = None;
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        let cfg = Config::load().teams;
        if ops::calendar_ready(&cfg) {
            if refreshed.is_none_or(|t| t.elapsed() > REFRESH) {
                refreshed = Some(Instant::now());
                match ops::refresh_events(&cfg, 12) {
                    Ok(_) => last_error.clear(),
                    Err(e) => {
                        let e = format!("{e:#}");
                        if e != last_error {
                            eprintln!("[mostalgia] teams calendar: {e}");
                            last_error = e;
                        }
                    }
                }
            }
            let now = Utc::now();
            for event in ops::cached_events().unwrap_or_default().iter().filter(|e| e.is_teams()) {
                let key = format!("{}@{}", event.id, event.start);
                let until = event.start - now;
                if cfg.brief_minutes > 0
                    && until > chrono::Duration::minutes(1)
                    && until <= chrono::Duration::minutes(cfg.brief_minutes as i64)
                    && briefed.insert(key)
                {
                    notify_brief(&cfg, event);
                }
            }
        }
        for _ in 0..30 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn notify_brief(cfg: &mostalgia_core::teams::TeamsConfig, event: &mostalgia_core::teams::Event) {
        let minutes = (event.start - Utc::now()).num_minutes().max(1);
        let mut lines: Vec<String> = Vec::new();
        let people = event.people_names(&cfg.account_email);
        if !people.is_empty() {
            lines.push(trf("Mit {}", &[&people.join(", ")]));
        }
        if let Ok(store) = mostalgia_core::store::Store::open(&mostalgia_core::paths::database())
            && let Ok(b) = brief::build(&store, event, &cfg.account_email)
        {
            let open = b.open_actions();
            if !open.is_empty() {
                lines.push(trf("Noch offen vom letzten Mal: {}", &[&open.iter().map(|a| a.task.as_str()).collect::<Vec<_>>().join("; ")]));
            }
        }
        if let Some(agenda) = event.body.lines().find(|l| !l.trim().is_empty()) {
            lines.push(agenda.trim().chars().take(120).collect());
        }
        notify(&trf("In {} Min: {}", &[&minutes, &event.recording_title()]), &lines.join("\n"), None);
    }
}

/// Adds a call app to the settings' list of seen apps.
fn remember_app(app: &str) {
    let mut config = Config::load();
    if !config.call_apps_seen.iter().any(|a| a == app) {
        config.call_apps_seen.push(app.to_string());
        let _ = config.save();
    }
}

/// A desktop notification (freedesktop D-Bus API). With `action`, it has
/// an "Aufnahme starten" button and a click is reported on the channel.
fn notify(summary: &str, body: &str, action: Option<(String, crossbeam_channel::Sender<String>)>) {
    let (summary, body) = (summary.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = send_notification(&summary, &body, action) {
            eprintln!("[mostalgia] notification: {e}");
        }
    });
}

#[cfg(target_os = "linux")]
fn send_notification(summary: &str, body: &str, action: Option<(String, crossbeam_channel::Sender<String>)>) -> zbus::Result<()> {
    use std::collections::HashMap;
    let conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?;
    // Subscribe before showing it, so a quick click isn't missed.
    let signals = action.as_ref().map(|_| proxy.receive_all_signals()).transpose()?;
    let actions: Vec<&str> = if action.is_some() { vec!["start", tr("Aufnahme starten")] } else { Vec::new() };
    let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    let id: u32 = proxy.call(
        "Notify",
        &("Mostalgia", 0u32, "audio-input-microphone", summary, body, actions, hints, -1i32),
    )?;
    let (Some((app, tx)), Some(signals)) = (action, signals) else {
        return Ok(());
    };
    // Wait until this notification's button is clicked or it goes away.
    for msg in signals {
        let header = msg.header();
        match header.member().map(|m| m.as_str()) {
            Some("ActionInvoked") => {
                if let Ok((nid, key)) = msg.body().deserialize::<(u32, String)>() {
                    if nid == id && key == "start" {
                        let _ = tx.send(app);
                        break;
                    }
                }
            }
            Some("NotificationClosed") => {
                if let Ok((nid, _)) = msg.body().deserialize::<(u32, u32)>() {
                    if nid == id {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A toast (Windows) through notify-rust, with the "Aufnahme starten"
/// button working like on Linux.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn send_notification(summary: &str, body: &str, action: Option<(String, crossbeam_channel::Sender<String>)>) -> anyhow::Result<()> {
    let mut n = notify_rust::Notification::new();
    n.appname("Mostalgia").summary(summary).body(body);
    // Shown as "Mostalgia" with its icon (see `win_identity`).
    #[cfg(windows)]
    if let Some(id) = crate::win_identity::toast_app_id() {
        n.app_id(id);
    }
    #[cfg(windows)]
    if action.is_some() {
        n.action("start", tr("Aufnahme starten"));
    }
    let handle = n.show()?;
    #[cfg(windows)]
    if let Some((app, tx)) = action {
        // Blocks this notification's thread until it's clicked or gone.
        handle.wait_for_action(|key| {
            if key == "start" {
                let _ = tx.send(app);
            }
        });
    }
    #[cfg(not(windows))]
    let _ = (handle, action);
    Ok(())
}

/// A Notification Center entry (macOS). With `action`, it has an "Aufnahme
/// starten" button like on Linux; this thread waits for the answer.
#[cfg(target_os = "macos")]
fn send_notification(summary: &str, body: &str, action: Option<(String, crossbeam_channel::Sender<String>)>) -> anyhow::Result<()> {
    use mac_notification_sys::{MainButton, Notification, NotificationResponse};
    static APP: std::sync::Once = std::sync::Once::new();
    APP.call_once(|| {
        // Inside Mostalgia.app the notifications are Mostalgia's; a bare
        // binary (a source build) keeps the library's default sender.
        if std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/")) {
            let _ = mac_notification_sys::set_application("io.github.mostaxd.Mostalgia");
        }
    });
    let mut n = Notification::new();
    n.title(summary).message(body);
    if action.is_some() {
        n.main_button(MainButton::SingleAction(tr("Aufnahme starten"))).close_button(tr("Ignorieren")).wait_for_click(true);
    }
    let response = n.send().map_err(|e| anyhow::anyhow!("{e}"))?;
    if let (Some((app, tx)), NotificationResponse::ActionButton(_)) = (action, response) {
        let _ = tx.send(app);
    }
    Ok(())
}
