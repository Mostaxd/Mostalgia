use anyhow::Result;

use crate::SAMPLE_RATE;
use crate::transcribe::{Engine, Segment};
use crate::vad::{self, FRAME, SpeechDetector};

/// Frames (of `FRAME` samples) of non-speech that end a chunk.
const SILENCE_FRAMES_TO_CUT: usize = 16; // 16 × 32 ms ≈ 500 ms
/// Non-speech frames kept before the first speech frame as lead-in.
const LEAD_IN_FRAMES: usize = 8; // ≈ 256 ms
/// Don't cut before a chunk holds this much audio.
const MIN_CHUNK_SECS: f32 = 1.5;
/// Force a cut at this length even mid-speech, so the transcript keeps up.
const MAX_CHUNK_SECS: f32 = 8.0;
/// A forced cut lands on the quietest frame within this many final frames
/// (≈ 3 s), so it falls between words rather than inside one.
const CUT_SEARCH_FRAMES: usize = 96;
/// Interim text is only worth computing once a chunk holds this much audio.
const MIN_INTERIM_SECS: f64 = 0.8;
/// A chunk quieter than this (RMS, ≈ −34 dBFS) is "low energy": if Whisper
/// returns only a known filler phrase for it, that's a hallucination.
const LOW_ENERGY_RMS: f32 = 0.02;
/// An utterance must be this many times louder (RMS) than the source's
/// background noise to reach Whisper (~8 dB): room noise and earphone bleed
/// stay out, while quiet speech — e.g. after a call app turned the mic gain
/// down — still gets through. Relative, because absolute levels vary
/// wildly between mics and gain settings.
pub const SPEECH_OVER_NOISE: f32 = 2.5;
/// Absolute floor for that gate (≈ −52 dBFS), for near-silent sources.
pub const MIN_UTTERANCE_RMS: f32 = 0.0025;

/// The loudness an utterance needs over a background of `noise` RMS.
pub fn speech_gate(noise: f32) -> f32 {
    (noise * SPEECH_OVER_NOISE).max(MIN_UTTERANCE_RMS)
}

/// A span of speech cut from one source, ready for Whisper.
pub struct Utterance {
    /// Seconds from the start of the recording.
    pub offset_secs: f64,
    pub samples: Vec<f32>,
    /// Background noise level (RMS) of the source when it was cut.
    pub noise: f32,
}

impl Utterance {
    /// Loud enough over the source's background to be speech.
    pub fn is_loud_enough(&self) -> bool {
        vad::rms(&self.samples) >= speech_gate(self.noise)
    }
}

/// Accumulates one source's live audio and cuts it into utterances at
/// pauses in speech.
pub struct Chunker {
    detector: SpeechDetector,
    chunk: Vec<f32>,
    pending: Vec<f32>,
    silent_frames: usize,
    heard_speech: bool,
    /// Samples consumed before the current chunk started (for timestamps).
    consumed: u64,
    /// Running background level (RMS of frames the VAD calls non-speech).
    noise: f32,
}

impl Chunker {
    pub fn new(detector: SpeechDetector) -> Self {
        Self {
            detector,
            chunk: Vec::new(),
            pending: Vec::new(),
            silent_frames: 0,
            heard_speech: false,
            consumed: 0,
            noise: 0.003,
        }
    }

    /// Feeds captured audio; returns any utterances completed by it.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Utterance> {
        self.pending.extend_from_slice(samples);
        let whole = self.pending.len() / FRAME * FRAME;
        let mut done = Vec::new();
        if whole == 0 {
            return done;
        }
        let frames: Vec<f32> = self.pending.drain(..whole).collect();
        let speech = self.detector.classify(&frames);
        for (frame, is_speech) in frames.chunks_exact(FRAME).zip(speech) {
            done.extend(self.push_frame(frame, is_speech));
        }
        done
    }

    fn push_frame(&mut self, frame: &[f32], is_speech: bool) -> Option<Utterance> {
        if !is_speech {
            // Falls fast, rises slowly: a quiet room is learned within a
            // second, a burst of noise barely moves it.
            let rms = vad::rms(frame);
            let rate = if rms < self.noise { 0.1 } else { 0.01 };
            self.noise += (rms - self.noise) * rate;
        }
        if !is_speech && !self.heard_speech {
            // Leading silence: keep only a short tail as lead-in, so the
            // chunk's timestamps stay close to the first word.
            self.chunk.extend_from_slice(frame);
            let keep = FRAME * LEAD_IN_FRAMES;
            if self.chunk.len() > keep {
                let drop = self.chunk.len() - keep;
                self.chunk.drain(..drop);
                self.consumed += drop as u64;
            }
            return None;
        }

        self.chunk.extend_from_slice(frame);
        if is_speech {
            self.heard_speech = true;
            self.silent_frames = 0;
        } else {
            self.silent_frames += 1;
        }

        let secs = self.chunk.len() as f32 / SAMPLE_RATE as f32;
        let pause = self.silent_frames >= SILENCE_FRAMES_TO_CUT;
        if pause && secs >= MIN_CHUNK_SECS {
            return self.cut();
        }
        if secs >= MAX_CHUNK_SECS {
            return self.cut_at_quietest();
        }
        None
    }

    /// Mid-speech cut: split at the quietest recent frame and keep the
    /// rest as the start of the next chunk.
    fn cut_at_quietest(&mut self) -> Option<Utterance> {
        let frames = self.chunk.len() / FRAME;
        let from = frames.saturating_sub(CUT_SEARCH_FRAMES).max(1);
        let quietest = (from..frames)
            .min_by(|a, b| {
                let rms = |i: usize| vad::rms(&self.chunk[i * FRAME..(i + 1) * FRAME]);
                rms(*a).total_cmp(&rms(*b))
            })
            .unwrap_or(frames);
        // Cut after the quiet frame; what follows starts the next chunk.
        let at = ((quietest + 1) * FRAME).min(self.chunk.len());
        let rest = self.chunk.split_off(at);
        let utt = self.cut();
        self.chunk = rest;
        self.heard_speech = !self.chunk.is_empty();
        utt
    }

    fn cut(&mut self) -> Option<Utterance> {
        let samples = std::mem::take(&mut self.chunk);
        let start = self.consumed;
        self.consumed += samples.len() as u64;
        self.silent_frames = 0;
        if !std::mem::take(&mut self.heard_speech) {
            return None;
        }
        Some(Utterance {
            offset_secs: start as f64 / SAMPLE_RATE as f64,
            samples,
            noise: self.noise,
        })
    }

    /// The chunk still being collected, for interim (provisional) text:
    /// its start in seconds and its audio, once it holds some speech.
    pub fn open_chunk(&self) -> Option<Utterance> {
        let secs = self.chunk.len() as f64 / SAMPLE_RATE as f64;
        (self.heard_speech && secs >= MIN_INTERIM_SECS).then(|| Utterance {
            offset_secs: self.consumed as f64 / SAMPLE_RATE as f64,
            samples: self.chunk.clone(),
            noise: self.noise,
        })
    }

    /// Samples in the open chunk (to tell whether it grew).
    pub fn open_len(&self) -> usize {
        if self.heard_speech { self.chunk.len() } else { 0 }
    }

    /// Flushes whatever is left when recording stops.
    pub fn finish(mut self) -> Vec<Utterance> {
        let mut done = Vec::new();
        if !self.pending.is_empty() {
            // Pad the last partial frame so the detector sees all of it.
            let mut rest = std::mem::take(&mut self.pending);
            rest.resize(FRAME, 0.0);
            done = self.push(&rest);
        }
        if self.heard_speech {
            done.extend(self.cut());
        }
        done
    }
}

/// Phrases Whisper is known to invent for near-silent audio (lowercase,
/// punctuation stripped).
const FILLER_PHRASES: &[&str] = &[ // i18n: not UI (what Whisper invents on silence)
    "danke",
    "vielen dank",
    "danke schön",
    "dankeschön",
    "vielen dank fürs zuschauen",
    "vielen dank für ihre aufmerksamkeit",
    "tschüss",
    "thank you",
    "thanks for watching",
    "thank you for watching",
    "bye",
    "byebye",
    "bye bye",
    "you",
];

/// True when `text` (one segment, alone in its stretch of audio) is a known
/// filler phrase or subtitle credit and `samples` — the audio it came from —
/// are quiet. Real speech that happens to be "Danke." is loud enough to pass.
pub fn is_filler_hallucination(text: &str, samples: &[f32]) -> bool {
    if vad::rms(samples) >= LOW_ENERGY_RMS {
        return false;
    }
    let text: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    FILLER_PHRASES.contains(&text.as_str()) || text.starts_with("untertitel") || text.starts_with("subtitles")
}

/// Transcribes one live utterance. Returns its segments with timestamps
/// relative to the recording start, or an empty list when Whisper's whole
/// output was a filler hallucination.
pub fn transcribe_utterance(engine: &mut Engine, utt: &Utterance) -> Result<Vec<Segment>> {
    if !utt.is_loud_enough() {
        return Ok(Vec::new());
    }
    let mut segments = engine.transcribe(&utt.samples)?;
    if let [seg] = segments.as_slice() {
        if is_filler_hallucination(&seg.text, &utt.samples) {
            return Ok(Vec::new());
        }
    }
    for seg in &mut segments {
        seg.start_secs += utt.offset_secs;
        seg.end_secs += utt.offset_secs;
        seg.text = seg.text.trim().to_string();
    }
    Ok(segments)
}

pub fn format_ts(secs: f64) -> String {
    let s = secs as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filler_only_when_quiet() {
        let quiet = vec![0.001f32; 16_000];
        let loud = vec![0.1f32; 16_000];
        assert!(is_filler_hallucination("Vielen Dank.", &quiet));
        assert!(is_filler_hallucination(" Bye-bye. ", &quiet));
        assert!(is_filler_hallucination("Untertitel im Auftrag des ZDF", &quiet));
        assert!(!is_filler_hallucination("Vielen Dank.", &loud));
        assert!(!is_filler_hallucination("Vielen Dank für das Budget.", &quiet));
    }
}
