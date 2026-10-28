//! Runs the final pass over a recording's WAV tracks and prints the
//! transcript to stdout, without touching the library: for checking which
//! language each track is detected and transcribed in.
//!
//!     cargo run --release --features vulkan -p mostalgia-core --example language_check -- <session dir> [de,en,ar]
//!
//! The second argument is the meeting languages ("" or "any" = all; default:
//! the configured ones). The language is always "auto". The detection per
//! track (with probabilities) goes to stderr; MOSTALGIA_DEBUG=1 shows every
//! detection Whisper makes.
//!
//! SLICES=mic:11.3-17.3,system:17.7-28.2 instead transcribes just those
//! stretches one by one the old way (Whisper's own detection per stretch,
//! no meeting languages, no lock) next to the detection used now.

use mostalgia_core::config::Config;
use mostalgia_core::finalize::{self, FinalPassOptions};
use mostalgia_core::transcribe::Engine;
use mostalgia_core::{Source, paths};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: language_check <session dir> [de,en,ar]"));
    let config = Config::load();
    let languages: Vec<String> = match args.next() {
        Some(list) if list.is_empty() || list == "any" => Vec::new(),
        Some(list) => list.split(',').map(|s| s.trim().to_string()).collect(),
        None => config.meeting_languages.clone(),
    };
    let tracks: Vec<(Source, std::path::PathBuf)> = [Source::Mic, Source::System]
        .into_iter()
        .map(|s| (s, dir.join(s.file_name())))
        .filter(|(_, p)| p.exists())
        .collect();
    anyhow::ensure!(!tracks.is_empty(), "no mic.wav / system.wav in {}", dir.display());

    let model = config.whisper_model_path();
    let mut engine = Engine::load(&model, "auto")?;
    engine.set_meeting_languages(&languages);
    eprintln!("[check] {} · meeting languages: {:?}", engine.label(), languages);
    if let Ok(slices) = std::env::var("SLICES") {
        for slice in slices.split(',') {
            let (src, range) = slice.split_once(':').expect("source:start-end");
            let (a, b) = range.split_once('-').expect("start-end");
            let (a, b): (f64, f64) = (a.parse()?, b.parse()?);
            let path = dir.join(format!("{src}.wav"));
            let samples = finalize::read_wav(&path)?;
            let at = |t: f64| ((t * mostalgia_core::SAMPLE_RATE as f64) as usize).min(samples.len());
            let audio = &samples[at(a)..at(b)];
            let mut old = Engine::load(&model, "auto")?;
            let text: Vec<String> = old.transcribe(audio)?.into_iter().map(|s| s.text.trim().to_string()).collect();
            println!("{slice} old: [{}] {}", old.detected_language().unwrap_or_default(), text.join(" "));
            let probs = engine.detect_language(audio)?.unwrap_or_default();
            let top: Vec<String> = probs.iter().take(3).map(|(c, p)| format!("{c} {p:.2}")).collect();
            let lang = probs.first().map(|(c, _)| c.clone());
            let text: Vec<String> = engine.transcribe_as(audio, lang.as_deref())?.into_iter().map(|s| s.text.trim().to_string()).collect();
            println!("{slice} new: [{}] {}", top.join(", "), text.join(" "));
        }
        return Ok(());
    }
    let vad = config.vad_model_path();
    let vad = if vad.exists() { vad } else { paths::model(paths::VAD_MODEL) };
    let opts = FinalPassOptions { vad_model: vad.exists().then_some(vad), ..Default::default() };
    let t = std::time::Instant::now();
    let segments = finalize::final_pass(&mut engine, &tracks, &opts)?;
    for seg in &segments {
        println!("[{}] {}: {}", mostalgia_core::chunker::format_ts(seg.start_secs), seg.source.as_str(), seg.text);
    }
    eprintln!(
        "[check] {} lines in {:.0} s · meeting language: {:?}",
        segments.len(),
        t.elapsed().as_secs_f64(),
        engine.detected_language()
    );
    Ok(())
}
