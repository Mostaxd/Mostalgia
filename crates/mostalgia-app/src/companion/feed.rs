//! Where Mori learns about the recording: the recording screen in this
//! window, or messages from the app when she runs on the desktop.

use std::time::{Duration, Instant};

use gpui_kit::*;
use mostalgia_core::companion::ContextLine;

use super::ipc::{WireLine, WireLive};
use crate::recording::{LiveState, RecordingView};

pub enum Feed {
    Window(Entity<RecordingView>),
    Remote(Entity<RemoteFeed>),
}

pub enum FeedRef<'a> {
    Window(&'a RecordingView),
    Remote(&'a RemoteFeed),
}

impl Feed {
    pub fn read<'a>(&self, cx: &'a App) -> FeedRef<'a> {
        match self {
            Feed::Window(e) => FeedRef::Window(e.read(cx)),
            Feed::Remote(e) => FeedRef::Remote(e.read(cx)),
        }
    }
}

impl FeedRef<'_> {
    pub fn is_recording(&self) -> bool {
        match self {
            FeedRef::Window(r) => r.is_recording(),
            FeedRef::Remote(r) => r.live.is_some(),
        }
    }

    pub fn live_state(&self) -> Option<LiveState> {
        match self {
            FeedRef::Window(r) => r.live_state(),
            FeedRef::Remote(r) => r.live_state(),
        }
    }

    /// Identifies the recording in progress.
    pub fn session_id(&self) -> Option<u64> {
        match self {
            FeedRef::Window(r) => r.session_id(),
            FeedRef::Remote(r) => r.live.and(r.session),
        }
    }

    /// Whether the transcript has every finalized line `live` counts. On
    /// the desktop the speech counter arrives at once but the transcript
    /// only every half second: a request made in between would miss the
    /// very line that triggered it.
    pub fn caught_up(&self, live: &LiveState) -> bool {
        match self {
            FeedRef::Window(_) => true,
            FeedRef::Remote(r) => r.lines.len() >= live.lines,
        }
    }

    pub fn transcript_lines(&self, from_secs: f64) -> Vec<ContextLine> {
        match self {
            FeedRef::Window(r) => r.transcript_lines(from_secs),
            FeedRef::Remote(r) => r.transcript_lines(from_secs),
        }
    }
}

/// The recording as the app reported it (desktop mode).
#[derive(Default)]
pub struct RemoteFeed {
    live: Option<LiveState>,
    /// When `live` arrived (its recording time runs on from there).
    live_at: Option<Instant>,
    session: Option<u64>,
    lines: Vec<WireLine>,
    interim: Vec<WireLine>,
}

impl RemoteFeed {
    pub fn set_live(&mut self, live: Option<WireLive>) {
        let now = Instant::now();
        let ago = |ms: Option<u64>| ms.and_then(|ms| now.checked_sub(Duration::from_millis(ms)));
        match live {
            Some(l) => {
                if self.session != Some(l.session) {
                    self.session = Some(l.session);
                    self.lines.clear();
                    self.interim.clear();
                }
                self.live = Some(LiveState {
                    paused: l.paused,
                    speech_at: ago(l.speech_ago_ms),
                    speech_since: ago(l.speech_since_ago_ms),
                    speech_secs: l.speech_secs,
                    last_line_end: l.last_line_end,
                    lines: l.lines,
                    elapsed_secs: l.elapsed_secs,
                });
                self.live_at = Some(now);
            }
            None => {
                self.live = None;
                self.interim.clear();
            }
        }
    }

    pub fn set_transcript(&mut self, session: u64, from: usize, lines: Vec<WireLine>, interim: Vec<WireLine>) {
        if self.session != Some(session) {
            self.session = Some(session);
            self.lines.clear();
        }
        // Lines the app thinks we have but we don't: take what we can.
        if from > self.lines.len() {
            eprintln!("[mori] transcript: lines {}..{from} never arrived", self.lines.len());
        }
        self.lines.truncate(from);
        self.lines.extend(lines);
        self.interim = interim;
    }

    /// The recording as last reported, its time run on since (unless paused).
    fn live_state(&self) -> Option<LiveState> {
        let mut live = self.live?;
        if !live.paused
            && let Some(at) = self.live_at
        {
            live.elapsed_secs += at.elapsed().as_secs_f64();
        }
        Some(live)
    }

    /// How many finalized lines arrived (for the app's next delta).
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Like `RecordingView::transcript_lines`: lines from `from_secs` on
    /// (and the one running into it), then the provisional text.
    fn transcript_lines(&self, from_secs: f64) -> Vec<ContextLine> {
        let first = self.lines.iter().position(|l| l.at >= from_secs).unwrap_or(self.lines.len()).saturating_sub(1);
        let mut out: Vec<ContextLine> = self.lines[first.min(self.lines.len())..].iter().map(|l| l.to_line(false)).collect();
        out.extend(self.interim.iter().map(|l| l.to_line(true)));
        out
    }
}
