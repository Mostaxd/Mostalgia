//! Controlling the running app from outside: a Unix socket for
//! `mostalgia toggle` (bind it to a GNOME keyboard shortcut — Wayland doesn't
//! let apps grab global hotkeys) and a tray icon (StatusNotifierItem; GNOME
//! shows it with the AppIndicator extension Ubuntu ships).

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Sender;

#[cfg(target_os = "linux")]
use mostalgia_core::i18n::tr;
use mostalgia_core::paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Start recording, or stop if one is running.
    Toggle,
    Start,
    Stop,
    /// Switch the mic of the recording in progress off or on.
    ToggleMic,
    /// Bring the window to the front.
    Show,
    Quit,
}

impl Command {
    #[cfg_attr(not(unix), allow(dead_code))]
    fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "toggle" => Command::Toggle,
            "start" => Command::Start,
            "stop" => Command::Stop,
            "mic" => Command::ToggleMic,
            "show" => Command::Show,
            "quit" => Command::Quit,
            _ => return None,
        })
    }
}

/// Listens on the control socket on a background thread. Fails if another
/// instance already owns it.
#[cfg(unix)]
pub fn listen(tx: Sender<Command>) -> std::io::Result<()> {
    let path = paths::control_socket();
    if path.exists() {
        // A live socket means another instance runs; a dead one is stale.
        if UnixStream::connect(&path).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "Mostalgia is already running"));
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let reply = match Command::parse(&line) {
                    Some(cmd) => {
                        let _ = tx.send(cmd);
                        "ok\n"
                    }
                    None => "unknown command\n",
                };
                let _ = (&stream).write_all(reply.as_bytes());
            }
        }
    });
    Ok(())
}

#[cfg(not(unix))]
pub fn listen(_tx: Sender<Command>) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no control channel on this system yet"))
}

pub fn cleanup() {
    let _ = std::fs::remove_file(paths::control_socket());
}

pub struct Tray {
    pub tx: Sender<Command>,
    pub recording: bool,
    /// While recording: whether the mic is on (it can be switched mid-recording).
    pub mic_on: bool,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "mostalgia".into()
    }

    fn title(&self) -> String {
        "Mostalgia".into()
    }

    fn icon_name(&self) -> String {
        // While recording, the theme's red record symbol; otherwise the
        // Mostalgia icon from `icon_pixmap`.
        if self.recording { "media-record".into() } else { String::new() }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        if self.recording {
            return Vec::new();
        }
        static ICON: std::sync::OnceLock<Vec<ksni::Icon>> = std::sync::OnceLock::new();
        ICON.get_or_init(|| {
            let Ok(img) = image::load_from_memory(crate::ICON_PNG) else { return Vec::new() };
            let rgba = img.to_rgba8();
            // StatusNotifierItem wants ARGB32 in network byte order.
            let data = rgba.pixels().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
            vec![ksni::Icon {
                width: rgba.width() as i32,
                height: rgba.height() as i32,
                data,
            }]
        })
        .clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: if self.recording { format!("Mostalgia — {}", tr("Aufnahme läuft")) } else { "Mostalgia".into() },
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Command::Show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        let item = |label: &str, icon: &str, cmd: Command| {
            StandardItem {
                label: label.into(),
                icon_name: icon.into(),
                activate: Box::new(move |t: &mut Self| {
                    let _ = t.tx.send(cmd);
                }),
                ..Default::default()
            }
            .into()
        };
        let mut items = vec![if self.recording {
            item(tr("Aufnahme beenden"), "media-playback-stop", Command::Stop)
        } else {
            item(tr("Aufnahme starten"), "media-record", Command::Start)
        }];
        if self.recording {
            items.push(if self.mic_on {
                item(tr("Mikrofon ausschalten"), "microphone-sensitivity-muted", Command::ToggleMic)
            } else {
                item(tr("Mikrofon einschalten"), "audio-input-microphone", Command::ToggleMic)
            });
        }
        items.extend([
            item(tr("Fenster anzeigen"), "window-new", Command::Show),
            ksni::MenuItem::Separator,
            item(tr("Mostalgia beenden"), "application-exit", Command::Quit),
        ]);
        items
    }
}

/// The running tray icon.
#[cfg(target_os = "linux")]
pub struct TrayHandle(ksni::blocking::Handle<Tray>);

#[cfg(target_os = "linux")]
impl TrayHandle {
    pub fn spawn(tray: Tray) -> anyhow::Result<Self> {
        use ksni::blocking::TrayMethods;
        Ok(Self(tray.spawn()?))
    }

    pub fn set_state(&self, recording: bool, mic_on: bool) {
        self.0.update(|t| (t.recording, t.mic_on) = (recording, mic_on));
    }
}

/// No tray icon on this system yet.
#[cfg(not(target_os = "linux"))]
pub struct TrayHandle(std::convert::Infallible);

#[cfg(not(target_os = "linux"))]
impl TrayHandle {
    pub fn spawn(_tray: Tray) -> anyhow::Result<Self> {
        anyhow::bail!("no tray icon on this system yet")
    }

    pub fn set_state(&self, _recording: bool, _mic_on: bool) {
        match self.0 {}
    }
}
