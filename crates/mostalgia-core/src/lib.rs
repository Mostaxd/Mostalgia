//! mostalgia's engine: audio capture, live and final Whisper transcription,
//! meeting storage and summaries. Front ends (the CLI, later the desktop
//! app) drive a [`recorder::Recorder`] and consume its [`recorder::Event`]s.

pub mod ask;
pub mod assist;
pub mod audio;
/// Microphone, device list and playback through cpal (and system audio on
/// Windows); see `audio::spawn_capture`.
#[cfg(not(target_os = "linux"))]
pub mod audio_cpal;
/// System-audio capture on macOS (see `audio::spawn_capture`).
#[cfg(target_os = "macos")]
pub mod audio_macos;
pub mod audio_pump;
pub mod callwatch;
pub mod chunker;
pub mod cleanup;
pub mod companion;
pub mod cloud_stt;
pub mod config;
pub mod connections;
pub mod control_channel;
pub mod decisions;
pub mod denoise;
pub mod diarize;
pub mod docs;
pub mod export;
pub mod finalize;
pub mod glossary;
pub mod gpu_check;
pub mod i18n;
pub mod language;
pub mod llm;
pub mod media_import;
pub mod models;
pub mod paths;
pub mod playback;
pub mod recorder;
pub mod recap;
pub mod recovery;
pub mod self_update;
pub mod store;
pub mod summary;
pub mod text_import;
pub mod transcribe;
pub mod translate;
pub mod vad;
pub mod voices;

pub use audio::Source;

/// Sample rate everything runs at. Whisper expects 16 kHz mono,
/// and PulseAudio/PipeWire resamples for us on capture.
pub const SAMPLE_RATE: u32 = 16_000;
/// Samples per capture block (100 ms at 16 kHz).
pub const BLOCK_SAMPLES: usize = 1_600;
