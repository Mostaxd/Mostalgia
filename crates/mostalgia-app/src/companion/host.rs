//! The app's side of Mori on the desktop: starts `mostalgia-app --mori`
//! (with XWayland, see `desktop`), feeds it the recording and restarts it
//! if it crashes — a few times, then it says so instead of looping.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use mostalgia_core::i18n::{tr, trf};

use super::ipc::{self, FromMori, ToMori, WireLine, WireLive};
use crate::recording::RecordingView;

/// Restarts allowed within `RESTART_WINDOW` before giving up.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(600);
/// How often the transcript is sent along (speech activity goes with every poll).
const TRANSCRIPT_EVERY: Duration = Duration::from_millis(500);

type LiveKey = (u64, bool, Option<Instant>, Option<Instant>, u64, u64);

struct Running {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<FromMori>,
    /// Finalized lines of the current session she has.
    sent_lines: usize,
    session: Option<u64>,
    last_live: Option<Option<LiveKey>>,
    last_interim: Vec<(f64, String)>,
    transcript_at: Instant,
}

#[derive(Default)]
pub struct DesktopHost {
    running: Option<Running>,
    restarts: Vec<Instant>,
    /// Why she isn't running although she should (shown in the app).
    pub problem: Option<String>,
}

impl DesktopHost {
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Starts her unless she's running (or gave up after crashes; `fresh`
    /// tries again anyway, e.g. after the settings changed).
    pub fn start(&mut self, fresh: bool) {
        if fresh {
            self.restarts.clear();
            self.problem = None;
        }
        if self.running.is_some() || self.problem.is_some() {
            return;
        }
        if std::env::var_os("DISPLAY").is_none_or(|d| d.is_empty()) {
            self.problem = Some(tr("Mori kann nicht auf den Desktop: XWayland ist nicht verfügbar (DISPLAY ist nicht gesetzt).").to_string());
            return;
        }
        match spawn() {
            Ok(r) => self.running = Some(r),
            Err(e) => self.problem = Some(trf("Mori konnte nicht gestartet werden: {}", &[&format!("{e:#}")]).to_string()),
        }
    }

    /// Ends her process (she's turned off, back in the window, or the app quits).
    pub fn stop(&mut self) {
        let Some(mut r) = self.running.take() else { return };
        let _ = r.stdin.write_all(ipc::encode(&ToMori::Quit).as_bytes());
        let _ = r.stdin.flush();
        drop(r.stdin);
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = r.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = r.child.kill();
        let _ = r.child.wait();
    }

    pub fn send(&mut self, msg: &ToMori) {
        if let Some(r) = &mut self.running
            && (r.stdin.write_all(ipc::encode(msg).as_bytes()).is_err() || r.stdin.flush().is_err())
        {
            // A dead pipe: `poll` notices the exit and restarts her.
            eprintln!("[mostalgia] Mori's process isn't listening");
        }
    }

    /// Her messages since the last call; restarts her if she crashed.
    pub fn poll(&mut self) -> Vec<FromMori> {
        let Some(r) = &mut self.running else { return Vec::new() };
        let events: Vec<FromMori> = r.rx.try_iter().collect();
        if let Ok(Some(status)) = r.child.try_wait() {
            eprintln!("[mostalgia] Mori's desktop process ended: {status}");
            self.running = None;
            let now = Instant::now();
            self.restarts.retain(|t| now - *t < RESTART_WINDOW);
            if self.restarts.len() >= MAX_RESTARTS {
                self.problem = Some(trf("Mori ist auf dem Desktop mehrmals abgestürzt ({}). Schalte sie in den Einstellungen aus und wieder ein, um es erneut zu versuchen.", &[&status]).to_string());
            } else {
                self.restarts.push(now);
                self.start(false);
            }
        }
        events
    }

    /// Passes the recording's state along: speech activity every call, the
    /// transcript at most every `TRANSCRIPT_EVERY`.
    pub fn feed(&mut self, recording: &RecordingView) {
        let Some(r) = &mut self.running else { return };
        let mut out: Vec<ToMori> = Vec::new();
        let now = Instant::now();
        let ago = |t: Option<Instant>| t.map(|t| now.saturating_duration_since(t).as_millis() as u64);
        let src = recording.live_state().zip(recording.session_id());
        // Only when something changed (her copy of the instants stays right).
        let key = src.map(|(l, session)| (session, l.paused, l.speech_at, l.speech_since, l.speech_secs.to_bits(), l.last_line_end.to_bits()));
        let live = src.map(|(l, session)| WireLive {
            session,
            paused: l.paused,
            speech_ago_ms: ago(l.speech_at),
            speech_since_ago_ms: ago(l.speech_since),
            speech_secs: l.speech_secs,
            last_line_end: l.last_line_end,
        });
        if r.last_live != Some(key) {
            out.push(ToMori::Live { live: live.clone() });
            r.last_live = Some(key);
        }
        if let Some(l) = &live {
            if r.session != Some(l.session) {
                r.session = Some(l.session);
                r.sent_lines = 0;
                r.last_interim.clear();
            }
            if now - r.transcript_at >= TRANSCRIPT_EVERY {
                r.transcript_at = now;
                let (lines, interim, total) = recording.transcript_delta(r.sent_lines);
                let interim_key: Vec<(f64, String)> = interim.iter().map(|l| (l.at_secs, l.text.clone())).collect();
                if !lines.is_empty() || interim_key != r.last_interim {
                    out.push(ToMori::Transcript {
                        session: l.session,
                        from: r.sent_lines,
                        lines: lines.iter().map(WireLine::from).collect(),
                        interim: interim.iter().map(WireLine::from).collect(),
                    });
                    r.sent_lines = total;
                    r.last_interim = interim_key;
                }
            }
        }
        for msg in out {
            self.send(&msg);
        }
    }
}

impl Drop for DesktopHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn() -> anyhow::Result<Running> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("--mori")
        // GPUI takes Wayland when it can; she needs X11 (XWayland).
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<FromMori>(&line) {
                Ok(msg) => {
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
                // Anything else on her stdout is a log line.
                Err(_) => eprintln!("[mori] {line}"),
            }
        }
    });
    Ok(Running {
        child,
        stdin,
        rx,
        sent_lines: 0,
        session: None,
        last_live: None,
        last_interim: Vec::new(),
        transcript_at: Instant::now() - TRANSCRIPT_EVERY,
    })
}
