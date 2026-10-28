//! Messages between the app and Mori's desktop process (`mostalgia-app
//! --mori`): one JSON object per line on the child's stdin / stdout.

use serde::{Deserialize, Serialize};

/// One transcript line (as `mostalgia_core::companion::ContextLine`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireLine {
    pub at: f64,
    pub speaker: String,
    pub text: String,
}

impl From<&mostalgia_core::companion::ContextLine> for WireLine {
    fn from(l: &mostalgia_core::companion::ContextLine) -> Self {
        Self { at: l.at_secs, speaker: l.speaker.clone(), text: l.text.clone() }
    }
}

impl WireLine {
    pub fn to_line(&self, interim: bool) -> mostalgia_core::companion::ContextLine {
        mostalgia_core::companion::ContextLine { at_secs: self.at, speaker: self.speaker.clone(), text: self.text.clone(), interim }
    }
}

/// The recording in progress (`crate::recording::LiveState`, with the
/// instants as "milliseconds ago").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireLive {
    pub session: u64,
    pub paused: bool,
    pub speech_ago_ms: Option<u64>,
    pub speech_since_ago_ms: Option<u64>,
    pub speech_secs: f64,
    pub last_line_end: f64,
    /// Finalized lines so far (she waits for her copy of the transcript
    /// to catch up before asking for points).
    #[serde(default)]
    pub lines: usize,
    /// Recording time so far (without pauses), for her record controls.
    #[serde(default)]
    pub elapsed_secs: f64,
    /// Which sources (mic, system) are being recorded right now (missing:
    /// both).
    #[serde(default)]
    pub sources: Option<[bool; 2]>,
}

/// App → Mori.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToMori {
    /// The recording state (`None` = no recording).
    Live { live: Option<WireLive> },
    /// Finalized lines `from..` of the current session's transcript, and the
    /// provisional text being spoken now.
    Transcript { session: u64, from: usize, lines: Vec<WireLine>, interim: Vec<WireLine> },
    /// A recording was saved as this meeting: store pinned points and the
    /// chat with it.
    Saved { meeting_id: i64 },
    /// The meeting open in the app (id, title), for "This meeting".
    OpenMeeting { meeting: Option<(i64, String)> },
    /// Settings changed (language, theme, Mori's options).
    ReloadConfig,
    /// The recording she asked for (`FromMori::StartRecording`) didn't start.
    StartFailed { message: String },
    Quit,
}

/// Mori → app.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromMori {
    /// The window is up.
    Ready,
    /// Open a meeting at this moment (a citation in an answer).
    OpenMeeting { meeting_id: i64, secs: f64 },
    /// Open a document at a page or section (a citation).
    OpenDocument { doc_id: i64, loc: u32 },
    /// Show the live transcript at this moment (a time cited in a "This
    /// meeting" answer).
    ShowLiveMoment { secs: f64 },
    /// Open Settings → AI & transcription.
    OpenSettings,
    /// Pinned points (and the chat) were stored with this meeting.
    NotesSaved { meeting_id: i64 },
    /// The user wants her back inside the app window.
    BackToApp,
    /// Start a recording from exactly these sources (her record button).
    StartRecording { mic: bool, system: bool },
    /// Pause / resume the recording in progress.
    PauseRecording { paused: bool },
    /// Stop (and save) the recording in progress.
    StopRecording,
    /// Switch a source ("mic" / "system") of the recording in progress off
    /// or on.
    SetSource { source: String, on: bool },
    /// Open Settings → Appearance (her options).
    OpenMoriSettings,
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).unwrap_or_default();
    s.push('\n');
    s
}
