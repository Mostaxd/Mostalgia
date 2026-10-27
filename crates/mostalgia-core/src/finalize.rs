use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::audio::Source;
use crate::chunker::is_filler_hallucination;
use crate::transcribe::Engine;
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};
use crate::SAMPLE_RATE;

/// One line of the archived transcript.
#[derive(Clone, Debug)]
pub struct FinalSegment {
    pub source: Source,
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
    /// A named speaker within the source (from diarization), if known.
    pub speaker: Option<String>,
}

/// Reads a 16 kHz mono WAV (what mostalgia records) as f32 samples.
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.channels == 1 && spec.sample_rate == SAMPLE_RATE,
        "{}: expected {SAMPLE_RATE} Hz mono WAV, got {} Hz {} channel(s)",
        path.display(),
        spec.sample_rate,
        spec.channels
    );
    let samples = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<_, _>>()?
        }
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
    };
    Ok(samples)
}

#[derive(Clone, Debug, Default)]
pub struct FinalPassOptions {
    /// Silero VAD model; without it each track goes to Whisper whole.
    pub vad_model: Option<PathBuf>,
    /// Label lines from the system track by speaker when it holds several people.
    pub diarize: bool,
    /// Run noise suppression on the mic track first (the WAV stays raw).
    pub denoise_mic: bool,
}

/// Each speech region goes to Whisper on its own: given a longer span with
/// pauses in it, Whisper sometimes stops early and drops the rest. Only a
/// region shorter than this is merged into the next one…
const MIN_SPAN_SECS: f64 = 3.0;
/// …if the pause between them is at most this…
const MAX_GAP_SECS: f64 = 1.5;
/// …and the result stays under this (Whisper's window is 30 s).
const MAX_SPAN_SECS: f64 = 25.0;

/// Re-transcribes each full track and merges the tracks into one
/// transcript by time. With a VAD model, Silero finds the speech first and
/// only those regions go to Whisper (each up to 25 s), so silence
/// can't turn into invented text or make Whisper skip ahead. Leftover
/// filler hallucinations are dropped either way.
pub fn final_pass(engine: &mut Engine, tracks: &[(Source, PathBuf)], opts: &FinalPassOptions) -> Result<Vec<FinalSegment>> {
    let mut vad = opts.vad_model.as_deref().map(load_vad).transpose()?;
    let diarize = opts.diarize;
    let mut out = Vec::new();
    for (source, path) in tracks {
        let mut samples = read_wav(path)?;
        if opts.denoise_mic && *source == Source::Mic {
            samples = crate::denoise::denoise_all(&samples);
        }
        let first = out.len();
        let gate = crate::chunker::speech_gate(noise_floor(&samples));
        let spans = match vad.as_mut() {
            Some(vad) => speech_spans(vad, &samples)?,
            None => vec![(0.0, samples.len() as f64 / SAMPLE_RATE as f64)],
        };
        for (start, end) in spans {
            let audio = &samples[sample_range(start, end, samples.len())];
            if audio.is_empty() || crate::vad::rms(audio) < gate {
                continue;
            }
            let segments = if vad.is_some() {
                engine.transcribe(audio)?
            } else {
                engine.transcribe_track(audio)?
            };
            let alone = segments.len() == 1;
            for seg in segments {
                let text = seg.text.trim().to_string();
                let span = sample_range(seg.start_secs, seg.end_secs, audio.len());
                let filler_audio = if alone { audio } else { &audio[span] };
                if text.is_empty() || is_filler_hallucination(&text, filler_audio) {
                    continue;
                }
                out.push(FinalSegment {
                    source: *source,
                    start_secs: start + seg.start_secs,
                    end_secs: start + seg.end_secs,
                    text,
                    speaker: None,
                });
            }
        }
        if diarize && *source == Source::System {
            let mut lines: Vec<&mut FinalSegment> = out[first..].iter_mut().collect();
            if let Err(e) = crate::diarize::label_track(&samples, &mut lines) {
                eprintln!("[mostalgia] diarization skipped: {e:#}");
            }
        }
    }
    out.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
    Ok(out)
}

/// Background level of a track: the 20th percentile of 100 ms block RMS.
fn noise_floor(samples: &[f32]) -> f32 {
    let mut blocks: Vec<f32> = samples.chunks(crate::BLOCK_SAMPLES).map(crate::vad::rms).collect();
    if blocks.is_empty() {
        return 0.0;
    }
    blocks.sort_by(f32::total_cmp);
    blocks[blocks.len() / 5]
}

fn load_vad(path: &Path) -> Result<WhisperVadContext> {
    let mut params = WhisperVadContextParams::new();
    params.set_use_gpu(false);
    params.set_n_threads(4);
    WhisperVadContext::new(path.to_str().context("VAD model path is not valid UTF-8")?, params)
        .context("loading Silero VAD model")
}

/// Speech regions of a track (seconds), merged into Whisper-sized spans.
fn speech_spans(vad: &mut WhisperVadContext, samples: &[f32]) -> Result<Vec<(f64, f64)>> {
    let mut params = WhisperVadParams::new();
    params.set_min_silence_duration(500);
    params.set_speech_pad(200);
    params.set_max_speech_duration(MAX_SPAN_SECS as f32);
    let segments = vad
        .segments_from_samples(params, samples)
        .map_err(|e| anyhow::anyhow!("running VAD: {e}"))?;
    let mut spans: Vec<(f64, f64)> = Vec::new();
    // VAD timestamps are in centiseconds.
    for seg in segments {
        let (start, end) = (seg.start as f64 / 100.0, seg.end as f64 / 100.0);
        match spans.last_mut() {
            Some(last)
                if last.1 - last.0 < MIN_SPAN_SECS
                    && start - last.1 <= MAX_GAP_SECS
                    && end - last.0 <= MAX_SPAN_SECS =>
            {
                last.1 = end
            }
            _ => spans.push((start, end)),
        }
    }
    Ok(spans)
}

fn sample_range(start: f64, end: f64, len: usize) -> std::ops::Range<usize> {
    let at = |t: f64| ((t.max(0.0) * SAMPLE_RATE as f64) as usize).min(len);
    let (a, b) = (at(start), at(end));
    a..b.max(a)
}
