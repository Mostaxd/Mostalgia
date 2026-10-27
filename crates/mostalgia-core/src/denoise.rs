//! Optional noise suppression for the mic: RNNoise (via `nnnoiseless`),
//! good at steady noise like fans and decent at keyboard clicks. It runs
//! only on what goes to the live transcriber (and, if asked, the final
//! pass); the recorded WAVs are always the raw signal.

use nnnoiseless::DenoiseState;

/// RNNoise works on 10 ms frames at 48 kHz; we run at 16 kHz, so audio is
/// upsampled ×3 on the way in and decimated on the way out.
const RATIO: usize = 3;
const FRAME_48K: usize = DenoiseState::FRAME_SIZE;
const FRAME_16K: usize = FRAME_48K / RATIO;

pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    /// 16 kHz input not yet filling a frame.
    pending: Vec<f32>,
    /// Last input sample, for interpolating across frame boundaries.
    last: f32,
    up: Vec<f32>,
    out48: Vec<f32>,
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            pending: Vec::new(),
            last: 0.0,
            up: vec![0.0; FRAME_48K],
            out48: vec![0.0; FRAME_48K],
        }
    }

    /// Denoises a stretch of 16 kHz audio (−1..1). Output lags input by up
    /// to one 10 ms frame; [`Self::flush`] returns the remainder.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        self.pending.extend_from_slice(samples);
        let frames = self.pending.len() / FRAME_16K;
        let mut out = Vec::with_capacity(frames * FRAME_16K);
        for f in 0..frames {
            let frame: [f32; FRAME_16K] = self.pending[f * FRAME_16K..(f + 1) * FRAME_16K]
                .try_into()
                .expect("frame size");
            self.process_frame(&frame, &mut out);
        }
        self.pending.drain(..frames * FRAME_16K);
        out
    }

    pub fn flush(&mut self) -> Vec<f32> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let n = self.pending.len();
        let mut frame = [0f32; FRAME_16K];
        frame[..n].copy_from_slice(&self.pending);
        self.pending.clear();
        let mut out = Vec::with_capacity(FRAME_16K);
        self.process_frame(&frame, &mut out);
        out.truncate(n);
        out
    }

    fn process_frame(&mut self, frame: &[f32; FRAME_16K], out: &mut Vec<f32>) {
        // Linear interpolation up to 48 kHz, in RNNoise's i16-scaled range.
        let mut prev = self.last;
        for (i, &s) in frame.iter().enumerate() {
            for k in 0..RATIO {
                let t = (k + 1) as f32 / RATIO as f32;
                self.up[i * RATIO + k] = (prev + (s - prev) * t) * 32768.0;
            }
            prev = s;
        }
        self.last = prev;
        self.state.process_frame(&mut self.out48, &self.up);
        // Average each group of three: a simple low-pass before decimating.
        for chunk in self.out48.chunks_exact(RATIO) {
            out.push((chunk.iter().sum::<f32>() / RATIO as f32 / 32768.0).clamp(-1.0, 1.0));
        }
    }
}

/// Denoises a whole track at once.
pub fn denoise_all(samples: &[f32]) -> Vec<f32> {
    let mut d = Denoiser::new();
    let mut out = d.process(samples);
    out.extend(d.flush());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_length_and_quiets_noise() {
        // 2 s of white-ish noise.
        let mut x = 12345u32;
        let noise: Vec<f32> = (0..32_000)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((x >> 16) as f32 / 32768.0 - 1.0) * 0.05
            })
            .collect();
        let out = denoise_all(&noise);
        assert_eq!(out.len(), noise.len());
        let rms = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt();
        // After the model settles, the noise should be much quieter.
        assert!(rms(&out[16_000..]) < rms(&noise[16_000..]) * 0.5);
    }
}
