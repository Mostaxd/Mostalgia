//! Importing an existing recording (a Teams recording, a phone voice memo,
//! any audio or video file) as a meeting: its sound is decoded piece by
//! piece (constant memory, also for files of several hours), mixed to mono,
//! resampled to 16 kHz and written as the meeting's one track; then the
//! normal final pass (language, speakers with the known voices) saves it.
//!
//! Decoding is pure Rust (symphonia): MP3, AAC (M4A, MP4, MOV), FLAC, WAV,
//! AIFF, ALAC, Ogg Vorbis, and Matroska/WebM holding those. Opus (most
//! WebM, .opus, WhatsApp/Telegram notes) has no Rust decoder there; it goes
//! through ffmpeg when that is installed, otherwise the error names it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;

use crate::SAMPLE_RATE;
use crate::i18n::{tr, trf};

/// File types offered for import.
pub const EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "mp4", "mov", "aac", "wav", "flac", "ogg", "oga", "opus", "webm", "mkv", "aiff", "aif", "caf", "wma",
];

pub fn is_media(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// What the file says about itself, for the import dialog.
#[derive(Clone, Debug)]
pub struct Info {
    /// Its recording time from the metadata, else the file's time.
    pub started_at: DateTime<Local>,
    /// The file name without extension.
    pub title: String,
}

pub fn info(path: &Path) -> Info {
    let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Recording").replace(['_'], " ");
    let started_at = metadata_date(path)
        .or_else(|| {
            let meta = std::fs::metadata(path).ok()?;
            let t = meta.created().or_else(|_| meta.modified()).ok()?;
            Some(DateTime::<Local>::from(t))
        })
        .unwrap_or_else(Local::now);
    Info { started_at, title }
}

/// A recording date in the file's tags (symphonia), or via ffprobe's
/// `creation_time` (MP4/MOV) when that tool is there.
fn metadata_date(path: &Path) -> Option<DateTime<Local>> {
    let from_tags = (|| {
        let file = std::fs::File::open(path).ok()?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut probed = symphonia::default::get_probe().format(&hint(path), mss, &FormatOptions::default(), &MetadataOptions::default()).ok()?;
        let mut found = None;
        let mut look = |rev: &symphonia::core::meta::MetadataRevision| {
            for tag in rev.tags() {
                if matches!(tag.std_key, Some(StandardTagKey::Date) | Some(StandardTagKey::ReleaseDate)) {
                    found = found.take().or_else(|| parse_date(&tag.value.to_string()));
                }
            }
        };
        if let Some(rev) = probed.metadata.get().as_ref().and_then(|m| m.current().cloned()) {
            look(&rev);
        }
        if let Some(rev) = probed.format.metadata().current() {
            look(rev);
        }
        found
    })();
    from_tags.or_else(|| {
        let out = tool("ffprobe")
            .args(["-v", "quiet", "-show_entries", "format_tags=creation_time", "-of", "default=nw=1:nk=1"])
            .arg(path)
            .output()
            .ok()?;
        parse_date(String::from_utf8_lossy(&out.stdout).trim())
    })
}

/// "2026-10-28T13:01:00.000000Z", "2026-10-28 13:01:00", "2026-10-28".
pub fn parse_date(s: &str) -> Option<DateTime<Local>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Local));
    }
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Local.from_local_datetime(&d).single();
        }
    }
    chrono::NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok().and_then(|d| Local.from_local_datetime(&d.and_hms_opt(12, 0, 0)?).single())
}

fn hint(path: &Path) -> Hint {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    hint
}

/// Streaming windowed-sinc resampler (mono f32), to 16 kHz unless made
/// with [`Resampler::between`]. Also used by live capture and playback
/// where the sound system doesn't resample for us (see `audio_pump`).
pub(crate) struct Resampler {
    ratio: f64,
    /// Input samples not yet consumed, with `pos` the next output's place.
    buf: Vec<f32>,
    pos: f64,
    half: usize,
    cutoff: f64,
    /// Samples taken in and given out so far (for the exact length).
    taken: u64,
    given: u64,
    /// The kernel sampled every 1/`STEPS` input sample over [-half, half].
    table: Vec<f32>,
}

/// Kernel table resolution (per input sample).
const STEPS: usize = 512;

impl Resampler {
    pub(crate) fn new(rate: u32) -> Self {
        Self::between(rate, SAMPLE_RATE)
    }

    /// From `from` Hz to `to` Hz.
    pub(crate) fn between(from: u32, to: u32) -> Self {
        let ratio = from as f64 / to.max(1) as f64;
        // Low-pass below the new Nyquist when downsampling.
        let cutoff = (1.0 / ratio).min(1.0) * 0.95;
        let half = (16.0 * ratio.max(1.0)).ceil() as usize;
        let mut r = Self { ratio, buf: vec![0.0; half], pos: half as f64, half, cutoff, taken: 0, given: 0, table: Vec::new() };
        r.table = (0..=2 * half * STEPS).map(|i| r.kernel(i as f64 / STEPS as f64 - half as f64) as f32).collect();
        r
    }

    fn kernel(&self, x: f64) -> f64 {
        let n = self.half as f64;
        if x.abs() >= n {
            return 0.0;
        }
        let sinc = if x == 0.0 { 1.0 } else { (std::f64::consts::PI * x * self.cutoff).sin() / (std::f64::consts::PI * x) };
        // Blackman window over [-n, n].
        let t = (x + n) / (2.0 * n);
        let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * t).cos() + 0.08 * (4.0 * std::f64::consts::PI * t).cos();
        sinc * w
    }

    pub(crate) fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let before = out.len();
        self.taken += input.len() as u64;
        self.resample(input, out);
        self.given += (out.len() - before) as u64;
    }

    fn resample(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.ratio - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        while self.pos + self.half as f64 + 1.0 < self.buf.len() as f64 {
            let center = self.pos.floor() as isize;
            let mut acc = 0.0f64;
            let mut norm = 0.0f64;
            for i in (center - self.half as isize + 1)..=(center + self.half as isize) {
                let at = ((self.pos - i as f64 + self.half as f64) * STEPS as f64).round() as usize;
                let k = self.table.get(at).copied().unwrap_or(0.0) as f64;
                acc += self.buf[i as usize] as f64 * k;
                norm += k;
            }
            out.push(if norm.abs() > 1e-12 { (acc / norm) as f32 } else { 0.0 });
            self.pos += self.ratio;
        }
        // Keep what the next outputs still need.
        let drop = (self.pos.floor() as usize).saturating_sub(self.half);
        if drop > 0 {
            self.buf.drain(..drop);
            self.pos -= drop as f64;
        }
    }

    /// The tail: as many samples as the input's length calls for.
    pub(crate) fn finish(&mut self, out: &mut Vec<f32>) {
        let want = (self.taken as f64 / self.ratio).round() as u64;
        let missing = want.saturating_sub(self.given) as usize;
        let before = out.len();
        let pad = vec![0.0; self.half * 2 + 2];
        while out.len() - before < missing {
            let n = out.len();
            self.resample(&pad, out);
            if out.len() == n {
                break;
            }
        }
        out.truncate(before + missing);
        self.given += missing as u64;
    }
}

/// Writes `dst` as a 16 kHz mono 16-bit WAV with `src`'s sound; `progress`
/// gets 0..1 when the length is known. Returns its length in seconds.
pub fn convert(src: &Path, dst: &Path, progress: &dyn Fn(Option<f32>)) -> Result<f64> {
    match convert_native(src, dst, progress) {
        Ok(secs) => Ok(secs),
        Err(native) => {
            let _ = std::fs::remove_file(dst);
            if !ffmpeg_available() {
                bail!(
                    "{}",
                    trf(
                        "{} lässt sich nicht lesen ({}). Für dieses Format (z. B. Opus) wird ffmpeg gebraucht – installieren (sudo apt install ffmpeg) und erneut importieren.",
                        &[&src.display(), &format!("{native:#}")]
                    )
                );
            }
            eprintln!("[mostalgia] {} — ffmpeg", format!("{native:#}"));
            convert_ffmpeg(src, dst, progress).with_context(|| trf("{} lässt sich auch mit ffmpeg nicht lesen", &[&src.display()]))
        }
    }
}

fn writer(dst: &Path) -> Result<hound::WavWriter<std::io::BufWriter<std::fs::File>>> {
    let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    hound::WavWriter::create(dst, spec).context("creating the meeting's track")
}

fn write(w: &mut hound::WavWriter<std::io::BufWriter<std::fs::File>>, samples: &[f32]) -> Result<()> {
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    Ok(())
}

fn convert_native(src: &Path, dst: &Path, progress: &dyn Fn(Option<f32>)) -> Result<f64> {
    let file = std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe()
        .format(&hint(src), mss, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .map_err(|e| anyhow!("{}: {e}", tr("unbekanntes Format")))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some())
        .ok_or_else(|| anyhow!(tr("keine Tonspur")))?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow!("{}: {e}", tr("Ton-Codec wird nicht unterstützt")))?;
    let rate = track.codec_params.sample_rate.unwrap_or(SAMPLE_RATE);
    let total = track.codec_params.n_frames.map(|n| n as f64 / rate as f64);
    let mut resampler = Resampler::new(rate);
    let mut out = writer(dst)?;
    let mut written = 0usize;
    let mut decoded_frames = 0u64;
    let mut mono: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    let mut last = std::time::Instant::now();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(anyhow!("{e}")),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A damaged packet: skip it, like players do.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(anyhow!("{e}")),
        };
        let spec = *decoded.spec();
        let frames = decoded.frames();
        let sb = buf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        if sb.capacity() < frames * spec.channels.count() {
            *sb = SampleBuffer::new(decoded.capacity() as u64, spec);
        }
        sb.copy_interleaved_ref(decoded);
        let channels = spec.channels.count().max(1);
        mono.clear();
        mono.extend(sb.samples().chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
        resampled.clear();
        resampler.push(&mono, &mut resampled);
        write(&mut out, &resampled)?;
        written += resampled.len();
        decoded_frames += frames as u64;
        if last.elapsed().as_millis() > 250 {
            last = std::time::Instant::now();
            progress(total.map(|t| (decoded_frames as f64 / rate as f64 / t).min(1.0) as f32));
        }
    }
    resampled.clear();
    resampler.finish(&mut resampled);
    write(&mut out, &resampled)?;
    written += resampled.len();
    out.finalize()?;
    anyhow::ensure!(written > 0, "{}", tr("keine Tonspur"));
    Ok(written as f64 / SAMPLE_RATE as f64)
}

/// ffmpeg / ffprobe, without a console window flashing up on Windows.
fn tool(name: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(name);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn ffmpeg_available() -> bool {
    tool("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// ffmpeg decodes, mixes down and resamples; we stream its raw output.
fn convert_ffmpeg(src: &Path, dst: &Path, progress: &dyn Fn(Option<f32>)) -> Result<f64> {
    let total = tool("ffprobe")
        .args(["-v", "quiet", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(src)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok());
    let mut child = tool("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(src)
        .args(["-vn", "-ac", "1", "-ar", &SAMPLE_RATE.to_string(), "-f", "s16le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting ffmpeg")?;
    let mut stdout = child.stdout.take().context("ffmpeg output")?;
    let mut out = writer(dst)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry: Option<u8> = None;
    let mut samples = 0usize;
    let mut last = std::time::Instant::now();
    loop {
        let n = stdout.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut bytes: Vec<u8> = carry.take().into_iter().collect();
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() % 2 == 1 {
            carry = bytes.pop();
        }
        for pair in bytes.chunks_exact(2) {
            out.write_sample(i16::from_le_bytes([pair[0], pair[1]]))?;
        }
        samples += bytes.len() / 2;
        if last.elapsed().as_millis() > 250 {
            last = std::time::Instant::now();
            progress(total.map(|t| (samples as f64 / SAMPLE_RATE as f64 / t).min(1.0) as f32));
        }
    }
    let status = child.wait()?;
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    anyhow::ensure!(status.success() && samples > 0, "ffmpeg: {}", err.trim());
    out.finalize()?;
    Ok(samples as f64 / SAMPLE_RATE as f64)
}

/// A new session folder for an imported recording started at `started_at`.
pub fn session_dir(recordings: &Path, started_at: &DateTime<Local>) -> Result<PathBuf> {
    let base = format!("session-{}", started_at.timestamp());
    for n in 0.. {
        let dir = recordings.join(if n == 0 { base.clone() } else { format!("{base}-{n}") });
        if !dir.exists() {
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            return Ok(dir);
        }
    }
    unreachable!()
}

/// Stages of an import, for progress displays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    /// Reading the file (`Some` fraction when its length is known).
    Reading(Option<f32>),
    /// Then the final pass.
    Final(crate::finalize::Stage),
}

/// Imports `src` as a meeting titled `title`, started at `started_at`:
/// converts it into a session folder (marked for crash recovery until it
/// is saved), runs the final pass with `engine` and saves it.
pub fn import(
    src: &Path,
    title: &str,
    started_at: DateTime<Local>,
    engine: &mut crate::transcribe::Engine,
    opts: &crate::finalize::FinalPassOptions,
    language: &str,
    progress: &(dyn Fn(Stage) + Sync),
) -> Result<crate::finalize::Saved> {
    let recordings = crate::paths::recordings_dir();
    std::fs::create_dir_all(&recordings)?;
    let dir = session_dir(&recordings, &started_at)?;
    let track = dir.join(crate::Source::System.file_name());
    progress(Stage::Reading(Some(0.0)));
    let secs = match convert(src, &track, &|f| progress(Stage::Reading(f))) {
        Ok(secs) => secs,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    crate::recovery::begin(&dir, &crate::recovery::Manifest { started_at: started_at.timestamp(), pid: std::process::id() })?;
    let rec = crate::recorder::Recording {
        dir: dir.clone(),
        started_at,
        duration_secs: secs,
        tracks: vec![(crate::Source::System, track)],
        live_lines: Vec::new(),
        live_errors: 0,
        languages: [None, None],
    };
    let saved = crate::finalize::save_recording(&rec, engine, opts, language, &[])?;
    if !title.trim().is_empty() {
        crate::store::Store::open(&crate::paths::database())?.set_title(saved.meeting_id, title.trim())?;
    }
    crate::recovery::finish(&dir);
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mostalgia-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Dominant frequency of `samples` (16 kHz) by zero crossings.
    fn frequency(samples: &[f32]) -> f64 {
        let crossings = samples.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        crossings as f64 / (samples.len() as f64 / SAMPLE_RATE as f64)
    }

    /// Every fixture is 2 s of a 440 Hz tone (stereo 44.1/48 kHz): it
    /// comes out as 2 s of 440 Hz at 16 kHz mono.
    #[test]
    fn decodes_the_common_formats() {
        let dir = scratch("formats");
        for name in ["tone.mp3", "tone.m4a", "tone.mp4", "tone.flac", "tone.ogg", "tone.wav", "tone.mkv"] {
            let dst = dir.join(format!("{name}.wav"));
            let secs = convert_native(&fixture(name), &dst, &|_| {}).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!((secs - 2.0).abs() < 0.08, "{name}: {secs} s");
            let samples = crate::finalize::read_wav(&dst).unwrap();
            let mid = &samples[SAMPLE_RATE as usize / 2..SAMPLE_RATE as usize * 3 / 2];
            let f = frequency(mid);
            assert!((f - 440.0).abs() < 5.0, "{name}: {f} Hz");
            let rms = (mid.iter().map(|s| s * s).sum::<f32>() / mid.len() as f32).sqrt();
            assert!(rms > 0.05, "{name}: too quiet ({rms})");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opus has no pure-Rust decoder: with ffmpeg it converts, without it
    /// the error names ffmpeg.
    #[test]
    fn opus_goes_through_ffmpeg() {
        let dir = scratch("opus");
        for name in ["tone.opus", "tone.webm"] {
        let dst = dir.join(format!("{name}.wav"));
        match convert(&fixture(name), &dst, &|_| {}) {
            Ok(secs) => {
                assert!(ffmpeg_available());
                assert!((secs - 2.0).abs() < 0.1, "{secs}");
                assert!((frequency(&crate::finalize::read_wav(&dst).unwrap()[8000..24000]) - 440.0).abs() < 5.0);
            }
            Err(e) => {
                assert!(!ffmpeg_available());
                assert!(format!("{e:#}").contains("ffmpeg"));
            }
        }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resampler_keeps_length_and_pitch() {
        for rate in [8_000u32, 22_050, 44_100, 48_000, 96_000] {
            let input: Vec<f32> = (0..rate as usize * 3).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / rate as f32).sin() * 0.5).collect();
            let mut r = Resampler::new(rate);
            let mut out = Vec::new();
            // In uneven pieces, as decoders hand them over.
            for piece in input.chunks(1153) {
                r.push(piece, &mut out);
            }
            r.finish(&mut out);
            assert!((out.len() as i64 - 48_000).abs() <= 2, "{rate}: {}", out.len());
            assert!((frequency(&out[8000..40000]) - 1000.0).abs() < 3.0, "{rate}");
        }
    }

    #[test]
    fn resampling_is_fast_enough_for_long_files() {
        // 10 minutes at 48 kHz in well under the time it plays.
        let input = vec![0.1f32; 48_000 * 600];
        let t = std::time::Instant::now();
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        for piece in input.chunks(4096) {
            r.push(piece, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(out.len(), 16_000 * 600);
        assert!(t.elapsed().as_secs_f64() < 20.0, "{:?}", t.elapsed());
    }

    #[test]
    fn dates_from_tags() {
        assert_eq!(parse_date("2026-10-28T12:01:00.000000Z").map(|d| d.timestamp()), Some(1793188860));
        assert!(parse_date("2026-10-28 13:01:00").is_some());
        assert!(parse_date("2026-10-28").is_some());
        assert!(parse_date("yesterday").is_none());
        assert!(is_media(Path::new("/x/Teams Meeting.MP4")) && !is_media(Path::new("notes.txt")));
    }
}
