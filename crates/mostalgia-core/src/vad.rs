use std::collections::VecDeque;
use std::path::Path;

use anyhow::{Context, Result};
use whisper_rs::{WhisperVadContext, WhisperVadContextParams};

/// Samples per detector frame. Silero works on 512-sample (32 ms) windows
/// at 16 kHz, so the chunker uses the same frame size for both detectors.
pub const FRAME: usize = 512;

/// Frames of history fed to Silero with each new batch. whisper.cpp resets
/// the model's LSTM state on every call, so it needs some audio before the
/// newest frames to warm up (24 × 32 ms ≈ 0.8 s).
const SILERO_CONTEXT_FRAMES: usize = 24;
/// Default probability above which speech starts; it ends again 0.15 below
/// that (hysteresis, as Silero recommends). Lower = more sensitive.
pub const DEFAULT_THRESHOLD: f32 = 0.5;
const HYSTERESIS: f32 = 0.15;

/// Speech gate for the live chunker: Silero VAD when its model is
/// available, otherwise an energy gate that tracks the room's noise floor.
pub enum SpeechDetector {
    Silero(Silero),
    Energy(EnergyGate),
}

impl SpeechDetector {
    /// Loads Silero from `model` if it exists, else falls back to the energy
    /// gate. `threshold` is Silero's speech probability threshold (0–1).
    pub fn load(model: &Path, threshold: f32) -> Result<Self> {
        if !model.exists() {
            eprintln!(
                "[mostalgia] VAD model {} not found — using the energy gate \
                 (run `mostalgia download-models` for Silero VAD)",
                model.display()
            );
            return Ok(Self::Energy(EnergyGate::new()));
        }
        Ok(Self::Silero(Silero::load(model, threshold.clamp(0.2, 0.95))?))
    }

    /// Classifies consecutive `FRAME`-sized frames; returns one flag per frame.
    pub fn classify(&mut self, frames: &[f32]) -> Vec<bool> {
        match self {
            Self::Silero(s) => s.classify(frames),
            Self::Energy(g) => frames.chunks_exact(FRAME).map(|f| g.is_speech(f)).collect(),
        }
    }
}

pub struct Silero {
    ctx: WhisperVadContext,
    history: VecDeque<f32>,
    active: bool,
    on: f32,
}

impl Silero {
    fn load(model: &Path, on: f32) -> Result<Self> {
        let mut params = WhisperVadContextParams::new();
        // The model is tiny; the CPU is faster than a GPU round trip.
        params.set_use_gpu(false);
        params.set_n_threads(1);
        let ctx = WhisperVadContext::new(
            model.to_str().context("VAD model path is not valid UTF-8")?,
            params,
        )
        .context("loading Silero VAD model")?;
        Ok(Self {
            ctx,
            history: VecDeque::new(),
            active: false,
            on,
        })
    }

    fn classify(&mut self, frames: &[f32]) -> Vec<bool> {
        let n = frames.len() / FRAME;
        let mut input: Vec<f32> = self.history.iter().copied().collect();
        input.extend_from_slice(&frames[..n * FRAME]);

        let probs = match self.ctx.detect_speech(&input) {
            Ok(()) => self.ctx.probabilities().to_vec(),
            Err(e) => {
                eprintln!("[mostalgia] VAD error: {e}");
                vec![0.0; n]
            }
        };

        let mut out = Vec::with_capacity(n);
        for &p in &probs[probs.len().saturating_sub(n)..] {
            let threshold = if self.active { self.on - HYSTERESIS } else { self.on };
            self.active = p >= threshold;
            out.push(self.active);
        }

        let keep = SILERO_CONTEXT_FRAMES * FRAME;
        let start = input.len().saturating_sub(keep);
        self.history = input[start..].iter().copied().collect();
        out
    }
}

/// RMS gate relative to a running noise-floor estimate: the floor follows
/// quiet frames quickly and drifts up only slowly during speech, so a noisy
/// mic raises the threshold instead of reading as constant speech.
pub struct EnergyGate {
    noise: f32,
}

/// Speech must be this many times louder than the noise floor (~10 dB).
const ENERGY_MARGIN: f32 = 3.0;
/// Never gate below this RMS (−48 dBFS), even in a dead-quiet room.
const ENERGY_MIN: f32 = 0.004;

impl EnergyGate {
    fn new() -> Self {
        Self { noise: ENERGY_MIN }
    }

    fn is_speech(&mut self, frame: &[f32]) -> bool {
        let rms = rms(frame);
        if rms < self.noise {
            self.noise += (rms - self.noise) * 0.2;
        } else {
            self.noise += (rms - self.noise) * 0.002;
        }
        rms > (self.noise * ENERGY_MARGIN).max(ENERGY_MIN)
    }
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}
