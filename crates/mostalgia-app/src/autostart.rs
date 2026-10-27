//! Auto-start with calls: turns call start/end events from
//! `mostalgia_core::callwatch` into prompts, desktop notifications and
//! start/stop actions, following the "Anrufe erkennen" settings.

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
                        notify("Aufnahme gestartet", &format!("Anruf in {app} erkannt – mostalgia nimmt auf."), None);
                        actions.push(Action::Start(app));
                    } else {
                        self.prompt = Some(app.clone());
                        notify(
                            &format!("Anruf in {app}"),
                            "Soll mostalgia diese Besprechung aufnehmen?",
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
                            notify("Aufnahme beendet", &format!("Der Anruf in {app} ist vorbei."), None);
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
    let actions: Vec<&str> = if action.is_some() { vec!["start", "Aufnahme starten"] } else { Vec::new() };
    let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    let id: u32 = proxy.call(
        "Notify",
        &("mostalgia", 0u32, "audio-input-microphone", summary, body, actions, hints, -1i32),
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
