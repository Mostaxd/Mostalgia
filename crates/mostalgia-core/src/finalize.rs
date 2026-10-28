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
    /// People in the call, counting you (a `diarize::PEOPLE_CHOICES`
    /// value; 0 = let the speaker separation decide).
    pub people: u8,
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
    let mut notes = Vec::new();
    let out = final_pass_with_notes(engine, tracks, opts, &mut notes)?;
    for note in notes {
        eprintln!("[mostalgia] {note}");
    }
    Ok(out)
}

/// [`final_pass`], collecting what it could not do as requested (no
/// speech filter, speaker separation skipped) into `notes` so callers can
/// show it instead of quietly doing less.
pub fn final_pass_with_notes(
    engine: &mut Engine,
    tracks: &[(Source, PathBuf)],
    opts: &FinalPassOptions,
    notes: &mut Vec<String>,
) -> Result<Vec<FinalSegment>> {
    final_pass_in(engine, tracks, opts, notes, &[None, None])
}

/// [`final_pass_with_notes`] with a language per source (mic, system)
/// picked by the user during the recording; the others are detected.
fn final_pass_in(
    engine: &mut Engine,
    tracks: &[(Source, PathBuf)],
    opts: &FinalPassOptions,
    notes: &mut Vec<String>,
    picked: &[Option<String>; 2],
) -> Result<Vec<FinalSegment>> {
    engine.reset_detected_language();
    if opts.vad_model.is_none() {
        notes.push(crate::i18n::tr("Ohne Sprachfilter (Silero-VAD-Modell fehlt — scripts/download-model.sh): Stille kann zu erfundenem Text führen").to_string());
    }
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
        let spans: Vec<(f64, f64)> = match vad.as_mut() {
            Some(vad) => speech_spans(vad, &samples)?,
            None => vec![(0.0, samples.len() as f64 / SAMPLE_RATE as f64)],
        }
        .into_iter()
        .filter(|&(start, end)| {
            let audio = &samples[sample_range(start, end, samples.len())];
            !audio.is_empty() && crate::vad::rms(audio) >= gate
        })
        .collect();
        let mut language = track_language(engine, &samples, &spans, picked[*source as usize].clone())?;
        if let Some(status) = language.status() {
            eprintln!("[mostalgia] language {}: {}", source.as_str(), describe(&status));
        }
        for &(start, end) in &spans {
            let audio = &samples[sample_range(start, end, samples.len())];
            let lang = if engine.is_auto() {
                let (lang, changed) = language.choose(audio, start, |a| engine.detect_language(a))?;
                if changed && let Some(status) = language.status() {
                    eprintln!("[mostalgia] language {} from {}: {}", source.as_str(), crate::chunker::format_ts(start), describe(&status));
                }
                lang
            } else {
                None
            };
            let segments = if vad.is_some() {
                engine.transcribe_as(audio, lang.as_deref())?
            } else {
                engine.transcribe_track_as(audio, lang.as_deref())?
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
            let has_mic = tracks.iter().any(|(s, _)| *s == Source::Mic);
            let speakers = crate::diarize::Speakers::on_system_track(opts.people, has_mic);
            if let Err(e) = crate::diarize::label_track(&samples, &mut lines, speakers) {
                notes.push(crate::i18n::trf("Sprechertrennung übersprungen: {}", &[&format!("{e:#}")]));
            }
        }
    }
    out.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
    Ok(out)
}

/// A track's language in auto mode: the user's pick, or detected on its
/// first [`crate::language::FINAL_WINDOW_SECS`] of speech (the speech
/// spans joined) and locked; re-checked along the way for meetings that
/// change language (see [`crate::language::LanguageLock`]).
fn track_language(engine: &mut Engine, samples: &[f32], spans: &[(f64, f64)], picked: Option<String>) -> Result<crate::language::LanguageLock> {
    let mut lock = crate::language::LanguageLock::new();
    if !engine.is_auto() {
        return Ok(lock);
    }
    if picked.is_some() {
        lock.set_manual(picked);
        return Ok(lock);
    }
    let max = (crate::language::FINAL_WINDOW_SECS * SAMPLE_RATE as f64) as usize;
    let mut window = Vec::with_capacity(max);
    for &(start, end) in spans {
        if window.len() >= max {
            break;
        }
        let audio = &samples[sample_range(start, end, samples.len())];
        window.extend_from_slice(&audio[..audio.len().min(max - window.len())]);
    }
    // Detected on the whole window, locked from the first span on.
    if !window.is_empty()
        && let Some(probs) = engine.detect_language(&window)?
    {
        lock.lock(probs, spans.first().map_or(0.0, |s| s.0));
    }
    Ok(lock)
}

/// "de (de 0.93, en 0.06)" for the log.
fn describe(status: &crate::language::Status) -> String {
    let probs: Vec<String> = status.probs.iter().take(3).map(|(c, p)| format!("{c} {p:.2}")).collect();
    let how = if status.manual { "picked" } else if status.locked { "locked" } else { "detecting" };
    format!("{} {how} ({})", status.language, probs.join(", "))
}

/// Background level of a track: the 20th percentile of 100 ms block RMS.
/// Digital silence — a source switched off mid-recording, or the padding
/// before it joined — isn't the room and doesn't count.
fn noise_floor(samples: &[f32]) -> f32 {
    let mut blocks: Vec<f32> = samples.chunks(crate::BLOCK_SAMPLES).map(crate::vad::rms).filter(|&r| r > 0.0).collect();
    if blocks.is_empty() {
        return 0.0;
    }
    blocks.sort_by(f32::total_cmp);
    blocks[blocks.len() / 5]
}

fn load_vad(path: &Path) -> Result<WhisperVadContext> {
    crate::transcribe::quiet_native_logs();
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

/// A speech-to-text engine for `choice`: local Whisper from the config's
/// model, or the chosen cloud model. Errors name the connection.
pub fn engine_for(config: &crate::config::Config, choice: &crate::config::SttChoice) -> Result<Engine> {
    if choice.is_local() {
        let model = config.whisper_model_path();
        anyhow::ensure!(
            model.exists(),
            "{}",
            crate::i18n::trf("Whisper-Modell nicht gefunden: {} — scripts/download-model.sh ausführen", &[&model.display()])
        );
        let mut engine = Engine::load(&model, &config.language)?;
        engine.set_meeting_languages(&config.meeting_languages);
        return Ok(engine);
    }
    let conn = config.connection(&choice.connection).with_context(|| {
        crate::i18n::trf("Transkriptions-Verbindung \"{}\" gibt es nicht mehr — in den Einstellungen wählen", &[&choice.connection])
    })?;
    let mut engine = Engine::cloud(conn, &choice.model, &config.language)?;
    engine.set_meeting_languages(&config.meeting_languages);
    Ok(engine)
}

/// What [`save_recording`] stored.
pub struct Saved {
    pub meeting_id: i64,
    /// Set when the final pass failed (the meeting holds the live transcript
    /// instead) or could not do everything asked; says why.
    pub error: Option<String>,
}

/// Runs the final pass over a finished recording and saves it to the
/// library. If the final pass fails, nothing is thrown away: the meeting is
/// saved with the live transcript, marked with the engine's error, so it
/// can be transcribed again later with an engine the user picks.
pub fn save_recording(
    rec: &crate::recorder::Recording,
    engine: &mut Engine,
    opts: &FinalPassOptions,
    language: &str,
    bookmarks: &[crate::store::Bookmark],
) -> Result<Saved> {
    use crate::i18n::trf;
    let mut notes = Vec::new();
    let (segments, engine_label, error) = match final_pass_in(engine, &rec.tracks, opts, &mut notes, &rec.languages) {
        Ok(segments) => (segments, engine.label(), (!notes.is_empty()).then(|| notes.join(" · "))),
        Err(e) => (
            rec.live_lines.clone(),
            trf("Live-Transkript ({})", &[&engine.label()]),
            Some(trf("Feinschliff mit {} fehlgeschlagen: {}", &[&engine.label(), &format!("{e:#}")])),
        ),
    };
    let mut store = crate::store::Store::open(&crate::paths::database())?;
    let title = trf("Besprechung {}", &[&crate::i18n::date_time(&rec.started_at)]);
    let id = store.insert_meeting(
        &crate::store::NewMeeting {
            title: &title,
            started_at: rec.started_at,
            duration_secs: rec.duration_secs,
            language,
            dir: &rec.dir,
        },
        &segments,
    )?;
    store.set_transcript_info(id, &engine_label, error.as_deref())?;
    store_detected_language(&store, id, language, engine)?;
    store.add_bookmarks(id, bookmarks)?;
    if let Err(e) = crate::ask::index_meeting(id) {
        eprintln!("[mostalgia] search index: {e:#}");
    }
    Ok(Saved { meeting_id: id, error })
}

/// Transcribes a saved meeting again from its WAV tracks with `engine`,
/// replacing its transcript (speaker renames are reset) and clearing a
/// previous engine error. Returns the number of lines.
pub fn retranscribe(meeting_id: i64, engine: &mut Engine, opts: &FinalPassOptions) -> Result<usize> {
    use crate::i18n::tr;
    let mut store = crate::store::Store::open(&crate::paths::database())?;
    let meeting = store.get(meeting_id)?.context(tr("Besprechung nicht gefunden"))?;
    anyhow::ensure!(!meeting.is_text_import(), "{}", tr("Diese Besprechung wurde aus Text importiert – es gibt kein Audio zum Transkribieren."));
    let tracks = meeting.tracks();
    anyhow::ensure!(!tracks.is_empty(), "{}", tr("Die Audiodateien dieser Besprechung gibt es nicht mehr"));
    let mut notes = Vec::new();
    let segments = final_pass_with_notes(engine, &tracks, opts, &mut notes)?;
    store.replace_segments(meeting_id, &segments)?;
    let note = (!notes.is_empty()).then(|| notes.join(" · "));
    store.set_transcript_info(meeting_id, &engine.label(), note.as_deref())?;
    let configured = if engine.is_auto() { "auto" } else { meeting.language.as_str() };
    store_detected_language(&store, meeting_id, configured, engine)?;
    drop(store);
    if let Err(e) = crate::ask::index_meeting(meeting_id) {
        eprintln!("[mostalgia] search index: {e:#}");
    }
    Ok(segments.len())
}

/// With "auto", remember the language Whisper heard most, so summaries can
/// be written in it without guessing.
fn store_detected_language(store: &crate::store::Store, meeting_id: i64, configured: &str, engine: &Engine) -> Result<()> {
    if configured != "auto" && configured != "" && !configured.starts_with("auto") {
        return Ok(());
    }
    if let Some(code) = engine.detected_language() {
        store.conn().execute("UPDATE meetings SET language = ?2 WHERE id = ?1", rusqlite::params![meeting_id, code])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digital_silence_is_not_the_room() {
        // A mic track that was off for most of the recording: mostly exact
        // zeros, then a room with a little noise.
        let mut track = vec![0.0f32; 80 * crate::BLOCK_SAMPLES];
        track.extend((0..20 * crate::BLOCK_SAMPLES).map(|i| if i % 2 == 0 { 0.01 } else { -0.01 }));
        assert!((noise_floor(&track) - 0.01).abs() < 1e-4);
        assert_eq!(noise_floor(&vec![0.0; 10 * crate::BLOCK_SAMPLES]), 0.0, "all off: no floor, nothing breaks");
        assert_eq!(noise_floor(&[]), 0.0);
    }
}
