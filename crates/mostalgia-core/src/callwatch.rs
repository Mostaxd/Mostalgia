//! Noticing calls: polls for apps recording from a microphone — PipeWire
//! (through its PulseAudio interface) on Linux, the privacy "consent store"
//! in the registry on Windows, Core Audio on macOS (see `callwatch_macos.rs`)
//! — and reports when a call app starts and — after a grace period, so a
//! brief stream restart doesn't count — stops.

#[cfg(target_os = "linux")]
use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use anyhow::Context as _;
#[cfg(not(target_os = "macos"))]
use anyhow::Result;
use crossbeam_channel::Sender;
#[cfg(target_os = "linux")]
use libpulse_binding::callbacks::ListResult;
#[cfg(target_os = "linux")]
use libpulse_binding::context::{Context as PaContext, FlagSet, State};
#[cfg(target_os = "linux")]
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
#[cfg(target_os = "linux")]
use libpulse_binding::operation::{Operation, State as OpState};
#[cfg(target_os = "linux")]
use libpulse_binding::proplist::properties;

/// How often the stream list is checked.
const POLL: Duration = Duration::from_millis(1000);
/// A call only counts as over once its mic stream has been gone this long.
pub const END_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallEvent {
    /// A call app started recording from a microphone (`app` = "Zoom", …).
    Started { app: String },
    /// That app hasn't recorded for [`END_GRACE`].
    Ended { app: String },
}

/// One recording stream as PipeWire reports it.
#[derive(Clone, Debug)]
pub struct CaptureStream {
    pub app_name: String,
    pub binary: String,
    pub media_name: String,
    /// Recording from a monitor (system audio), not a microphone.
    pub from_monitor: bool,
}

/// Known call apps: (label, lowercase substrings of app name or binary).
const CALL_APPS: &[(&str, &[&str])] = &[
    ("Zoom", &["zoom"]),
    ("Microsoft Teams", &["teams"]),
    ("Discord", &["discord"]),
    ("Slack", &["slack"]),
    ("Signal", &["signal"]),
    ("Element", &["element"]),
    ("Jitsi", &["jitsi"]),
    ("Skype", &["skype"]),
    ("Webex", &["webex", "ciscowebex", "ciscocollabhost", "atmgr"]),
    ("Telegram", &["telegram"]),
    ("WhatsApp", &["whatsapp"]),
    // macOS (FaceTime's audio runs in `avconferenced`).
    ("FaceTime", &["facetime", "avconferenced"]),
];

/// Browsers: any microphone capture there is WebRTC (Meet, Teams web, Jitsi…).
const BROWSERS: &[(&str, &[&str])] = &[
    ("Chrome", &["google chrome", "google-chrome", "chrome"]),
    ("Chromium", &["chromium"]),
    ("Brave", &["brave"]),
    ("Edge", &["microsoft edge", "msedge"]),
    ("Vivaldi", &["vivaldi"]),
    ("Opera", &["opera"]),
    ("Firefox", &["firefox"]),
    // macOS: Safari's media runs in WebKit's GPU process.
    ("Safari", &["safari", "com.apple.webkit"]),
];

/// The call app behind a recording stream, if it is one ("Zoom",
/// "Browser-Anruf (Chrome)"). Our own and monitor captures never count.
pub fn classify(s: &CaptureStream) -> Option<String> {
    if s.from_monitor {
        return None;
    }
    let app = s.app_name.to_lowercase();
    let bin = s.binary.to_lowercase();
    if app.starts_with("mostalgia") || bin.starts_with("mostalgia") {
        return None;
    }
    let matches = |needles: &[&str]| needles.iter().any(|n| app.contains(n) || bin.contains(n));
    if let Some((label, _)) = CALL_APPS.iter().find(|(_, n)| matches(n)) {
        return Some(label.to_string());
    }
    BROWSERS
        .iter()
        .find(|(_, n)| matches(n))
        .map(|(label, _)| crate::i18n::trf("Browser-Anruf ({})", &[label]))
}

/// A Pulse connection kept open for polling.
#[cfg(target_os = "linux")]
struct Probe {
    ml: Mainloop,
    ctx: PaContext,
}

#[cfg(target_os = "linux")]
impl Probe {
    fn connect() -> Result<Self> {
        let mut ml = Mainloop::new().context("creating Pulse mainloop")?;
        let mut ctx = PaContext::new(&ml, "mostalgia-callwatch").context("creating Pulse context")?;
        ctx.connect(None, FlagSet::NOFLAGS, None)
            .map_err(|e| anyhow::anyhow!("connecting to PulseAudio/PipeWire: {e}"))?;
        loop {
            Self::iterate(&mut ml)?;
            match ctx.get_state() {
                State::Ready => break,
                State::Failed | State::Terminated => anyhow::bail!("PulseAudio/PipeWire connection failed"),
                _ => {}
            }
        }
        Ok(Self { ml, ctx })
    }

    fn iterate(ml: &mut Mainloop) -> Result<()> {
        match ml.iterate(true) {
            IterateResult::Success(_) => Ok(()),
            _ => anyhow::bail!("Pulse mainloop stopped"),
        }
    }

    fn wait<F: ?Sized>(&mut self, op: Operation<F>) -> Result<()> {
        while op.get_state() == OpState::Running {
            Self::iterate(&mut self.ml)?;
        }
        Ok(())
    }

    fn capture_streams(&mut self) -> Result<Vec<CaptureStream>> {
        let monitors: Rc<RefCell<HashSet<u32>>> = Rc::default();
        {
            let monitors = monitors.clone();
            let op = self.ctx.introspect().get_source_info_list(move |r| {
                if let ListResult::Item(s) = r {
                    if s.monitor_of_sink.is_some() {
                        monitors.borrow_mut().insert(s.index);
                    }
                }
            });
            self.wait(op)?;
        }
        let streams: Rc<RefCell<Vec<CaptureStream>>> = Rc::default();
        {
            let streams = streams.clone();
            let monitors = monitors.clone();
            let op = self.ctx.introspect().get_source_output_info_list(move |r| {
                if let ListResult::Item(o) = r {
                    let get = |k: &str| o.proplist.get_str(k).unwrap_or_default();
                    streams.borrow_mut().push(CaptureStream {
                        app_name: get(properties::APPLICATION_NAME),
                        binary: get(properties::APPLICATION_PROCESS_BINARY),
                        media_name: get(properties::MEDIA_NAME),
                        from_monitor: monitors.borrow().contains(&o.source),
                    });
                }
            });
            self.wait(op)?;
        }
        Ok(streams.take())
    }
}

/// One app's entry in Windows' microphone consent store
/// (`HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone`).
#[derive(Clone, Debug)]
pub struct ConsentEntry {
    /// The key's name: a package family name ("MSTeams_8wekyb3d8bbwe") for
    /// a packaged app, or the executable's path with `#` for `\`
    /// ("C:#Users#me#AppData#Local#Discord#app-1.0.9#Discord.exe") under
    /// `NonPackaged`.
    pub key: String,
    pub packaged: bool,
    /// FILETIMEs of the last use; a start with no stop means in use now.
    pub last_start: u64,
    pub last_stop: u64,
}

/// The apps using a microphone right now, as capture streams.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn consent_streams(entries: &[ConsentEntry]) -> Vec<CaptureStream> {
    entries
        .iter()
        .filter(|e| e.last_start != 0 && e.last_stop == 0)
        .map(|e| {
            let (app_name, binary) = if e.packaged {
                // "Microsoft.SkypeApp_kzf8qxf38zg5c" → "Microsoft.SkypeApp".
                let name = e.key.rsplit_once('_').map_or(e.key.as_str(), |(n, _)| n);
                (name.to_string(), e.key.clone())
            } else {
                let path = e.key.replace('#', "\\");
                let file = path.rsplit('\\').next().unwrap_or(&path).to_string();
                let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(&file).to_string();
                (stem, file)
            };
            CaptureStream { app_name, binary, media_name: String::new(), from_monitor: false }
        })
        .collect()
}

/// Reads the microphone consent store (packaged apps and `NonPackaged`).
#[cfg(windows)]
fn read_consent_store() -> Result<Vec<ConsentEntry>> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    const STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    let root = RegKey::predef(HKEY_CURRENT_USER).open_subkey(STORE)?;
    let entry = |parent: &RegKey, key: String, packaged: bool| -> Option<ConsentEntry> {
        let k = parent.open_subkey(&key).ok()?;
        let last_start: u64 = k.get_value("LastUsedTimeStart").ok()?;
        let last_stop: u64 = k.get_value("LastUsedTimeStop").unwrap_or(0);
        Some(ConsentEntry { key, packaged, last_start, last_stop })
    };
    let mut out = Vec::new();
    for name in root.enum_keys().flatten() {
        if name == "NonPackaged" {
            let Ok(np) = root.open_subkey(&name) else { continue };
            out.extend(np.enum_keys().flatten().filter_map(|k| entry(&np, k, false)));
        } else {
            out.extend(entry(&root, name, true));
        }
    }
    Ok(out)
}

/// Windows keeps who uses the microphone in the registry; nothing to
/// connect to.
#[cfg(windows)]
struct Probe;

#[cfg(windows)]
impl Probe {
    fn connect() -> Result<Self> {
        Ok(Self)
    }

    fn capture_streams(&mut self) -> Result<Vec<CaptureStream>> {
        Ok(consent_streams(&read_consent_store()?))
    }
}

#[cfg(target_os = "macos")]
#[path = "callwatch_macos.rs"]
mod macos;
#[cfg(target_os = "macos")]
use macos::Watcher as Probe;

/// No call detection here yet: connecting always fails, so the watcher
/// just keeps waiting.
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
struct Probe;

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
impl Probe {
    fn connect() -> Result<Self> {
        anyhow::bail!("call detection is not available on this system yet")
    }

    fn capture_streams(&mut self) -> Result<Vec<CaptureStream>> {
        Ok(Vec::new())
    }
}

/// Watches for calls on a background thread until `stop` is set. Each app
/// gets one `Started` when its mic stream appears and one `Ended` once the
/// stream has been gone for [`END_GRACE`].
pub fn spawn(tx: Sender<CallEvent>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut active: HashMap<String, Instant> = HashMap::new();
        let mut pulse: Option<Probe> = None;
        while !stop.load(Ordering::SeqCst) {
            if pulse.is_none() {
                pulse = Probe::connect().ok();
            }
            let streams = match pulse.as_mut().map(Probe::capture_streams) {
                Some(Ok(s)) => s,
                _ => {
                    // Reconnect next time (e.g. after PipeWire restarted).
                    pulse = None;
                    std::thread::sleep(POLL * 3);
                    continue;
                }
            };
            let now = Instant::now();
            for app in streams.iter().filter_map(classify) {
                if active.insert(app.clone(), now).is_none() && tx.send(CallEvent::Started { app }).is_err() {
                    return;
                }
            }
            let ended: Vec<String> = active
                .iter()
                .filter(|(_, seen)| now.duration_since(**seen) >= END_GRACE)
                .map(|(app, _)| app.clone())
                .collect();
            for app in ended {
                active.remove(&app);
                if tx.send(CallEvent::Ended { app }).is_err() {
                    return;
                }
            }
            std::thread::sleep(POLL);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(app: &str, bin: &str, monitor: bool) -> CaptureStream {
        CaptureStream {
            app_name: app.into(),
            binary: bin.into(),
            media_name: "RecordStream".into(),
            from_monitor: monitor,
        }
    }

    #[test]
    fn recognizes_call_apps() {
        assert_eq!(classify(&stream("ZOOM VoiceEngine", "zoom", false)).as_deref(), Some("Zoom"));
        assert_eq!(classify(&stream("teams-for-linux", "teams-for-linux", false)).as_deref(), Some("Microsoft Teams"));
        // Browser labels are translated; another test may switch the language.
        assert!(classify(&stream("Google Chrome", "chrome", false)).is_some_and(|l| l.ends_with("(Chrome)")));
        assert!(classify(&stream("Firefox", "firefox", false)).is_some_and(|l| l.ends_with("(Firefox)")));
        assert_eq!(classify(&stream("discord", "Discord", false)).as_deref(), Some("Discord"));
        // Not calls: our own capture, monitors, other recorders.
        assert_eq!(classify(&stream("mostalgia", "mostalgia-app", false)), None);
        assert_eq!(classify(&stream("zoom", "zoom", true)), None);
        assert_eq!(classify(&stream("Audacity", "audacity", false)), None);
    }

    fn entry(key: &str, packaged: bool, start: u64, stop: u64) -> ConsentEntry {
        ConsentEntry { key: key.into(), packaged, last_start: start, last_stop: stop }
    }

    /// Windows' consent store: only entries in use now (started, not
    /// stopped) count, and both packaged and plain apps are recognized.
    #[test]
    fn consent_store_entries_in_use_become_calls() {
        let entries = [
            entry("C:#Users#me#AppData#Local#Discord#app-1.0.9#Discord.exe", false, 133_000, 0),
            entry("C:#Program Files#Zoom#bin#Zoom.exe", false, 133_000, 133_100),
            entry("MSTeams_8wekyb3d8bbwe", true, 133_000, 0),
            entry("C:#Program Files#Google#Chrome#Application#chrome.exe", false, 133_000, 0),
            entry("C:#Program Files#Mostalgia#mostalgia-app.exe", false, 133_000, 0),
            entry("Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe", true, 133_000, 0),
            entry("C:#Program Files#Audacity#Audacity.exe", false, 0, 0),
        ];
        let streams = consent_streams(&entries);
        assert_eq!(streams.len(), 5, "{streams:?}");
        assert_eq!(streams[0].app_name, "Discord");
        assert_eq!(streams[0].binary, "Discord.exe");
        assert_eq!(streams[1].app_name, "MSTeams");
        let calls: Vec<Option<String>> = streams.iter().map(classify).collect();
        assert_eq!(calls[0].as_deref(), Some("Discord"));
        assert_eq!(calls[1].as_deref(), Some("Microsoft Teams"));
        assert!(calls[2].as_deref().is_some_and(|l| l.ends_with("(Chrome)")));
        assert_eq!(calls[3], None, "our own capture");
        assert_eq!(calls[4], None, "not a call app");
    }
}
