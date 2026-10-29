//! Controlling the running app from outside: a Unix socket for
//! `mostalgia toggle` (bind it to a GNOME keyboard shortcut — Wayland doesn't
//! let apps grab global hotkeys; on Windows a token-protected localhost port,
//! see `mostalgia_core::control_channel`) and a tray icon (StatusNotifierItem
//! on Linux — GNOME shows it with the AppIndicator extension Ubuntu ships —
//! and the system tray elsewhere).

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Sender;

#[cfg(target_os = "linux")]
use mostalgia_core::i18n::tr;
#[cfg(unix)]
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

/// Listens on a localhost port announced (with a token) in the data
/// directory. Fails if another instance already listens.
#[cfg(not(unix))]
pub fn listen(tx: Sender<Command>) -> std::io::Result<()> {
    use mostalgia_core::control_channel;
    control_channel::serve(&control_channel::endpoint_file(), move |line| match Command::parse(line) {
        Some(cmd) => {
            let _ = tx.send(cmd);
            true
        }
        None => false,
    })
}

pub fn cleanup() {
    #[cfg(unix)]
    let _ = std::fs::remove_file(paths::control_socket());
    #[cfg(not(unix))]
    mostalgia_core::control_channel::cleanup(&mostalgia_core::control_channel::endpoint_file());
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

/// The tray menu for a state: each entry's command and label (`None` is a
/// separator). The same entries as the Linux tray.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn menu_entries(recording: bool, mic_on: bool) -> Vec<Option<(Command, &'static str)>> {
    let mut items = vec![Some(if recording {
        (Command::Stop, mostalgia_core::i18n::tr("Aufnahme beenden"))
    } else {
        (Command::Start, mostalgia_core::i18n::tr("Aufnahme starten"))
    })];
    if recording {
        items.push(Some(if mic_on {
            (Command::ToggleMic, mostalgia_core::i18n::tr("Mikrofon ausschalten"))
        } else {
            (Command::ToggleMic, mostalgia_core::i18n::tr("Mikrofon einschalten"))
        }));
    }
    items.extend([
        Some((Command::Show, mostalgia_core::i18n::tr("Fenster anzeigen"))),
        None,
        Some((Command::Quit, mostalgia_core::i18n::tr("Mostalgia beenden"))),
    ]);
    items
}

/// The system tray through the `tray-icon` crate (Windows; macOS can use
/// the same, built on its main thread).
#[cfg(not(target_os = "linux"))]
#[cfg_attr(not(windows), allow(dead_code))]
mod native {
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::Command;

    /// The app icon, with a red record dot while recording.
    pub fn icon(recording: bool) -> Option<Icon> {
        let img = image::load_from_memory(crate::ICON_PNG).ok()?;
        let mut rgba = img.to_rgba8();
        if recording {
            let (w, h) = rgba.dimensions();
            let r = w.min(h) as f32 * 0.22;
            let (cx, cy) = (w as f32 - r - 1.0, h as f32 - r - 1.0);
            for (x, y, p) in rgba.enumerate_pixels_mut() {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                if d <= r {
                    *p = image::Rgba([0xe0, 0x1b, 0x24, 0xff]);
                } else if d <= r + 1.5 {
                    *p = image::Rgba([0xff, 0xff, 0xff, 0xff]);
                }
            }
        }
        let (w, h) = rgba.dimensions();
        Icon::from_rgba(rgba.into_raw(), w, h).ok()
    }

    fn tooltip(recording: bool) -> String {
        if recording { format!("Mostalgia — {}", mostalgia_core::i18n::tr("Aufnahme läuft")) } else { "Mostalgia".into() }
    }

    pub struct Native {
        icon: TrayIcon,
        ids: Vec<(MenuId, Command)>,
    }

    impl Native {
        pub fn build(recording: bool, mic_on: bool) -> anyhow::Result<Self> {
            let (menu, ids) = menu(recording, mic_on)?;
            let icon = TrayIconBuilder::new()
                .with_id("mostalgia")
                .with_tooltip(tooltip(recording))
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_icon(icon(recording).ok_or_else(|| anyhow::anyhow!("tray icon image"))?)
                .build()?;
            Ok(Self { icon, ids })
        }

        pub fn set_state(&mut self, recording: bool, mic_on: bool) {
            if let Ok((menu, ids)) = menu(recording, mic_on) {
                self.icon.set_menu(Some(Box::new(menu)));
                self.ids = ids;
            }
            let _ = self.icon.set_icon(icon(recording));
            let _ = self.icon.set_tooltip(Some(tooltip(recording)));
        }

        /// Commands from menu clicks and left clicks on the icon since the
        /// last call.
        pub fn commands(&self) -> Vec<Command> {
            let mut out = Vec::new();
            while let Ok(e) = MenuEvent::receiver().try_recv() {
                if let Some((_, cmd)) = self.ids.iter().find(|(id, _)| *id == e.id) {
                    out.push(*cmd);
                }
            }
            while let Ok(e) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    out.push(Command::Show);
                }
            }
            out
        }
    }

    fn menu(recording: bool, mic_on: bool) -> anyhow::Result<(Menu, Vec<(MenuId, Command)>)> {
        let menu = Menu::new();
        let mut ids = Vec::new();
        for (i, entry) in super::menu_entries(recording, mic_on).into_iter().enumerate() {
            match entry {
                Some((cmd, label)) => {
                    let item = MenuItem::with_id(format!("mostalgia-{i}"), label, true, None);
                    ids.push((item.id().clone(), cmd));
                    menu.append(&item)?;
                }
                None => menu.append(&PredefinedMenuItem::separator())?,
            }
        }
        Ok((menu, ids))
    }
}

/// The running tray icon: on Windows it lives on its own thread, which
/// runs the message loop its hidden window needs.
#[cfg(windows)]
pub struct TrayHandle(std::sync::mpsc::Sender<(bool, bool)>);

#[cfg(windows)]
impl TrayHandle {
    pub fn spawn(tray: Tray) -> anyhow::Result<Self> {
        let (state_tx, state_rx) = std::sync::mpsc::channel::<(bool, bool)>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
        std::thread::Builder::new().name("mostalgia-tray".into()).spawn(move || {
            let mut native = match native::Native::build(tray.recording, tray.mic_on) {
                Ok(n) => {
                    let _ = ready_tx.send(Ok(()));
                    n
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            loop {
                pump_messages();
                loop {
                    match state_rx.try_recv() {
                        Ok((recording, mic_on)) => native.set_state(recording, mic_on),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        // The app dropped the handle: remove the icon.
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    }
                }
                for cmd in native.commands() {
                    let _ = tray.tx.send(cmd);
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
        })?;
        ready_rx.recv().map_err(|_| anyhow::anyhow!("tray thread ended"))??;
        Ok(Self(state_tx))
    }

    pub fn set_state(&self, recording: bool, mic_on: bool) {
        let _ = self.0.send((recording, mic_on));
    }
}

/// Handles the window messages queued for this thread (the tray's).
#[cfg(windows)]
fn pump_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage};
    // SAFETY: plain Win32 message loop on this thread with a zeroed MSG.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// No tray icon here yet (macOS: `native` needs building on the main thread).
#[cfg(not(any(target_os = "linux", windows)))]
pub struct TrayHandle(std::convert::Infallible);

#[cfg(not(any(target_os = "linux", windows)))]
impl TrayHandle {
    pub fn spawn(_tray: Tray) -> anyhow::Result<Self> {
        anyhow::bail!("no tray icon on this system yet")
    }

    pub fn set_state(&self, _recording: bool, _mic_on: bool) {
        match self.0 {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_menu_follows_the_recording_state() {
        let cmds = |r, m| menu_entries(r, m).into_iter().map(|e| e.map(|(c, _)| c)).collect::<Vec<_>>();
        assert_eq!(cmds(false, false), [Some(Command::Start), Some(Command::Show), None, Some(Command::Quit)]);
        assert_eq!(
            cmds(true, true),
            [Some(Command::Stop), Some(Command::ToggleMic), Some(Command::Show), None, Some(Command::Quit)]
        );
    }

    #[test]
    fn commands_parse() {
        assert_eq!(Command::parse("toggle\n"), Some(Command::Toggle));
        assert_eq!(Command::parse(" mic "), Some(Command::ToggleMic));
        assert_eq!(Command::parse("rm -rf"), None);
    }
}
