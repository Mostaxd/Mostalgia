//! mostalgia's engine: audio capture, live and final Whisper transcription,
//! meeting storage and summaries. Front ends (the CLI, later the desktop
//! app) drive a [`recorder::Recorder`] and consume its [`recorder::Event`]s.

pub mod ask;
pub mod audio;
pub mod callwatch;
pub mod chunker;
pub mod config;
pub mod denoise;
pub mod diarize;
pub mod export;
pub mod finalize;
pub mod paths;
pub mod playback;
pub mod recorder;
pub mod store;
pub mod summary;
pub mod transcribe;
pub mod vad;

pub use audio::Source;

/// Sample rate everything runs at. Whisper expects 16 kHz mono,
/// and PulseAudio/PipeWire resamples for us on capture.
pub const SAMPLE_RATE: u32 = 16_000;
/// Samples per capture block (100 ms at 16 kHz).
pub const BLOCK_SAMPLES: usize = 1_600;
