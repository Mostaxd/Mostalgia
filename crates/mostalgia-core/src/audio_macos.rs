//! System audio (the other side of a call) on macOS.
//!
//! STUB: the macOS port replaces this file's contents. Keep these two
//! signatures; `audio.rs` calls them on macOS (the microphone, device list
//! of inputs, level meter and playback already work there through
//! `audio_cpal`):
//!
//! ```ignore
//! pub fn spawn_system_capture(
//!     device: Option<String>,
//!     wav_path: std::path::PathBuf,
//!     tx: crossbeam_channel::Sender<crate::audio::Block>,
//!     ctl: crate::audio::CaptureCtl,
//! ) -> anyhow::Result<std::thread::JoinHandle<anyhow::Result<()>>>;
//!
//! pub fn list_system_sources() -> anyhow::Result<Vec<crate::audio::SourceInfo>>;
//! ```
//!
//! `spawn_system_capture` has the contract of `audio::spawn_capture` for
//! `Source::System`: `device` is `None` or `Some(audio::DEFAULT_MONITOR)`
//! for everything the Mac plays, or a `name` from `list_system_sources`.
//! The easy way to keep that contract is `audio_pump::spawn_pump(Source::System,
//! wav_path, tx, ctl, fill_gaps, open)`: `open` runs on the capture thread,
//! starts the platform stream (ScreenCaptureKit, a Core Audio process tap,
//! …) feeding `audio_pump::RawSink::new(raw, channels)`, and returns
//! `audio_pump::Opened { rate, guard }` (the guard keeps the stream alive).
//! Pass `fill_gaps: true` if the stream goes quiet by delivering nothing.
//!
//! `list_system_sources` gives the entries offered for system audio in the
//! device lists (`is_monitor: true`); an empty list just offers "default".
//! The level meter for a system device goes through `spawn_system_capture`.

use std::path::PathBuf;
use std::thread::JoinHandle;

use anyhow::Result;
use crossbeam_channel::Sender;

use crate::audio::{Block, CaptureCtl, SourceInfo};

/// Captures system audio into `wav_path` (see the module docs).
pub fn spawn_system_capture(
    _device: Option<String>,
    _wav_path: PathBuf,
    _tx: Sender<Block>,
    _ctl: CaptureCtl,
) -> Result<JoinHandle<Result<()>>> {
    anyhow::bail!("{}", crate::i18n::tr("Audioaufnahme ist auf diesem System noch nicht verfügbar"))
}

/// System-audio entries for the device lists (see the module docs).
pub fn list_system_sources() -> Result<Vec<SourceInfo>> {
    Ok(Vec::new())
}
