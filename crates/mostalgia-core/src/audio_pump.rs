//! The platform-independent half of capturing audio where the sound system
//! doesn't hand us 16 kHz mono blocks itself (everywhere but Linux, where
//! PulseAudio does): an audio callback pushes whatever it gets (any rate,
//! any channel count) into a [`RawSink`]; [`spawn_pump`] turns that into
//! the same 100 ms blocks, WAV track and live-transcriber hand-off as the
//! PulseAudio capture, with the same [`CaptureCtl`] switches.
//!
//! A platform's capture only has to open its stream inside the `open`
//! closure given to [`spawn_pump`]. The stream lives (and is dropped) on
//! the pump's own thread, so it doesn't have to be `Send`.
#![cfg_attr(target_os = "linux", allow(dead_code))]

use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::audio::{Block, CaptureCtl, Feeder, Source, TrackWriter};
use crate::media_import::Resampler;
use crate::{BLOCK_SAMPLES, SAMPLE_RATE};

/// What a capture callback hands to the pump.
pub enum Raw {
    /// Mono samples at the stream's rate.
    Audio(Vec<f32>),
    /// The stream broke for good (device unplugged, …); ends the capture.
    Fatal(String),
}

/// Where an audio callback puts what it captured. Cheap to clone, never
/// blocks (the pump's queue is unbounded), so it's safe on a real-time
/// audio thread.
#[derive(Clone)]
pub struct RawSink {
    tx: Sender<Raw>,
    channels: usize,
}

impl RawSink {
    /// A sink for interleaved frames of `channels` channels.
    pub fn new(tx: Sender<Raw>, channels: usize) -> Self {
        Self { tx, channels: channels.max(1) }
    }

    /// Interleaved samples, mixed down to mono.
    pub fn push_interleaved(&self, samples: impl IntoIterator<Item = f32>) {
        let mono = downmix(samples, self.channels);
        if !mono.is_empty() {
            let _ = self.tx.send(Raw::Audio(mono));
        }
    }

    /// Mono samples as they are.
    pub fn push_mono(&self, samples: Vec<f32>) {
        if !samples.is_empty() {
            let _ = self.tx.send(Raw::Audio(samples));
        }
    }

    /// The stream broke for good: the capture ends with this error.
    pub fn fail(&self, message: impl Into<String>) {
        let _ = self.tx.send(Raw::Fatal(message.into()));
    }
}

/// Averages each frame of `channels` interleaved samples into one.
pub fn downmix(samples: impl IntoIterator<Item = f32>, channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let iter = samples.into_iter();
    let mut out = Vec::with_capacity(iter.size_hint().0 / channels);
    let (mut acc, mut n) = (0f32, 0usize);
    for s in iter {
        acc += s;
        n += 1;
        if n == channels {
            out.push(acc / channels as f32);
            acc = 0.0;
            n = 0;
        }
    }
    out
}

/// Turns mono audio at any rate into exact `BLOCK_SAMPLES` blocks at 16 kHz.
pub struct BlockConverter {
    resampler: Resampler,
    pending: Vec<f32>,
    scratch: Vec<f32>,
}

impl BlockConverter {
    pub fn new(rate: u32) -> Self {
        Self { resampler: Resampler::new(rate), pending: Vec::with_capacity(2 * BLOCK_SAMPLES), scratch: Vec::new() }
    }

    /// Mono audio at the input rate; returns the blocks completed by it.
    pub fn push(&mut self, input: &[f32]) -> Vec<Vec<f32>> {
        self.scratch.clear();
        self.resampler.push(input, &mut self.scratch);
        self.pending.extend_from_slice(&self.scratch);
        self.take_blocks()
    }

    /// `n` samples (at 16 kHz) of silence, e.g. for a stretch where the
    /// sound system delivered nothing.
    pub fn push_silence(&mut self, n: usize) -> Vec<Vec<f32>> {
        self.pending.resize(self.pending.len() + n, 0.0);
        self.take_blocks()
    }

    fn take_blocks(&mut self) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        while self.pending.len() >= BLOCK_SAMPLES {
            out.push(self.pending.drain(..BLOCK_SAMPLES).collect());
        }
        out
    }
}

/// Keeps a track on the wall clock when the sound system goes quiet by
/// delivering nothing at all: Windows' loopback capture sends no packets
/// while nothing plays, which would leave the system-audio track shorter
/// than the mic's. Paused time doesn't count.
pub struct GapClock {
    last: Option<Instant>,
    /// Running (unpaused) time so far.
    active: Duration,
    /// Samples (16 kHz) produced so far, live or filled in.
    produced: u64,
}

/// Fill in only once this far behind, so ordinary jitter and slight clock
/// drift never put silence in the middle of speech.
pub const GAP_FILL_AFTER: Duration = Duration::from_millis(400);

impl Default for GapClock {
    fn default() -> Self {
        Self::new()
    }
}

impl GapClock {
    pub fn new() -> Self {
        Self { last: None, active: Duration::ZERO, produced: 0 }
    }

    /// Advances the clock to `now`; time while `paused` is left out.
    pub fn tick(&mut self, now: Instant, paused: bool) {
        if let Some(last) = self.last
            && !paused
        {
            self.active += now.saturating_duration_since(last);
        }
        self.last = Some(now);
    }

    pub fn produced(&mut self, n: usize) {
        self.produced += n as u64;
    }

    /// Whole blocks of silence to fill in now (0 unless the track is more
    /// than [`GAP_FILL_AFTER`] behind the clock).
    pub fn missing(&self) -> usize {
        let due = (self.active.as_secs_f64() * SAMPLE_RATE as f64) as u64;
        let behind = due.saturating_sub(self.produced);
        if behind < (GAP_FILL_AFTER.as_secs_f64() * SAMPLE_RATE as f64) as u64 {
            return 0;
        }
        (behind as usize / BLOCK_SAMPLES) * BLOCK_SAMPLES
    }
}

/// An opened stream: its sample rate, and whatever has to stay alive for
/// it to keep running (dropped when the capture stops).
pub struct Opened {
    pub rate: u32,
    pub guard: Box<dyn Any>,
}

/// Starts a capture of `source` into `wav_path`, with the contract of
/// [`crate::audio::spawn_capture`]: 16 kHz mono, 100 ms blocks offered to
/// `tx` without ever waiting, pause/mute/late join through `ctl`.
///
/// `open` runs on the capture's thread: it opens the platform stream,
/// sends what it captures to the sink it builds from the given sender
/// (`RawSink::new(tx, channels)`) and returns the stream's rate. Its error
/// is returned from here, like a failed connect on Linux.
///
/// `fill_gaps`: the stream may go silent by delivering nothing (Windows
/// loopback); fill such stretches with silence to stay on the wall clock.
pub fn spawn_pump<F>(
    source: Source,
    wav_path: PathBuf,
    tx: Sender<Block>,
    ctl: CaptureCtl,
    fill_gaps: bool,
    open: F,
) -> Result<JoinHandle<Result<()>>>
where
    F: FnOnce(Sender<Raw>) -> Result<Opened> + Send + 'static,
{
    let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<Raw>();
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<()>>(1);
    let handle = std::thread::Builder::new()
        .name(format!("mostalgia-capture-{}", source.as_str()))
        .spawn(move || -> Result<()> {
            let opened = match open(raw_tx) {
                Ok(o) => o,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return Ok(());
                }
            };
            let track = match TrackWriter::create(&wav_path, source, ctl.written.clone(), ctl.join_at.clone()) {
                Ok(t) => t,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return Ok(());
                }
            };
            let _ = ready_tx.send(Ok(()));
            let result = pump(source, track, raw_rx, tx, &ctl, opened.rate, fill_gaps);
            drop(opened.guard);
            result
        })
        .context("starting the capture thread")?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => match handle.join() {
            Ok(r) => Err(r.err().unwrap_or_else(|| anyhow::anyhow!("capture thread ended"))),
            Err(_) => anyhow::bail!("capture thread panicked"),
        },
    }
}

fn pump(
    source: Source,
    mut track: TrackWriter,
    rx: Receiver<Raw>,
    tx: Sender<Block>,
    ctl: &CaptureCtl,
    rate: u32,
    fill_gaps: bool,
) -> Result<()> {
    let mut conv = BlockConverter::new(rate);
    let mut feeder = Feeder::new(source, tx);
    let mut clock = GapClock::new();
    let mut write = |blocks: Vec<Vec<f32>>, clock: &mut GapClock, track: &mut TrackWriter| -> Result<()> {
        for samples in blocks {
            clock.produced(samples.len());
            for block in track.push(samples, ctl.muted.load(Ordering::SeqCst))? {
                feeder.offer(block);
            }
        }
        Ok(())
    };
    let result = loop {
        if ctl.stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        let paused = ctl.paused.load(Ordering::SeqCst);
        match rx.recv_timeout(Duration::from_millis(50)) {
            // Keep draining while paused, but record nothing: the paused
            // stretch is simply left out of the tracks.
            Ok(Raw::Audio(_)) if paused => {}
            Ok(Raw::Audio(samples)) => {
                if let Err(e) = write(conv.push(&samples), &mut clock, &mut track) {
                    break Err(e);
                }
            }
            Ok(Raw::Fatal(message)) => break Err(anyhow::anyhow!(message)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Err(anyhow::anyhow!("the audio stream ended")),
        }
        clock.tick(Instant::now(), paused);
        if fill_gaps && !paused {
            let missing = clock.missing();
            if missing > 0
                && let Err(e) = write(conv.push_silence(missing), &mut clock, &mut track)
            {
                break Err(e);
            }
        }
    };
    // Whatever happened, leave a finished, readable track behind.
    let finalized = track.finalize();
    result.and(finalized)
}

/// Records `secs` through `capture` (a `spawn_capture` for one source)
/// without keeping anything, and returns the loudest 100 ms RMS — for a
/// "test device" button.
pub fn measure_with<F>(secs: f32, capture: F) -> Result<f32>
where
    F: FnOnce(PathBuf, Sender<Block>, CaptureCtl) -> Result<JoinHandle<Result<()>>>,
{
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "mostalgia-level-{}-{}.wav",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let (tx, rx) = crossbeam_channel::bounded::<Block>(64);
    let ctl = CaptureCtl {
        stop: Arc::new(AtomicBool::new(false)),
        paused: Arc::new(AtomicBool::new(false)),
        muted: Arc::new(AtomicBool::new(false)),
        written: Arc::new(AtomicU64::new(0)),
        join_at: None,
    };
    let handle = capture(path.clone(), tx, ctl.clone())?;
    let end = Instant::now() + Duration::from_secs_f32(secs.max(0.1));
    let mut peak = 0f32;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(block) => peak = peak.max(crate::vad::rms(&block.samples)),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    ctl.stop.store(true, Ordering::SeqCst);
    let joined = handle.join();
    let _ = std::fs::remove_file(&path);
    match joined {
        Ok(r) => r?,
        Err(_) => anyhow::bail!("capture thread panicked"),
    }
    Ok(peak)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use super::*;

    #[test]
    fn downmix_averages_frames_and_drops_a_partial_one() {
        assert_eq!(downmix([1.0, 0.0, 0.5, 0.5, 0.2], 2), vec![0.5, 0.5]);
        assert_eq!(downmix([0.1, 0.2], 1), vec![0.1, 0.2]);
        assert_eq!(downmix([0.3, 0.3, 0.3, 0.0, 0.0, 0.0], 3), vec![0.3, 0.0]);
    }

    #[test]
    fn converter_makes_exact_blocks_at_16k() {
        let mut conv = BlockConverter::new(48_000);
        let mut total = 0;
        // One second at 48 kHz in uneven callback-sized pieces.
        let tone: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
        for piece in tone.chunks(441) {
            for b in conv.push(piece) {
                assert_eq!(b.len(), BLOCK_SAMPLES);
                total += b.len();
            }
        }
        // All but the resampler's few-sample lag.
        assert!(total >= 16_000 - BLOCK_SAMPLES && total <= 16_000, "{total}");
        let silent = conv.push_silence(2 * BLOCK_SAMPLES);
        assert!(!silent.is_empty());
    }

    #[test]
    fn gap_clock_fills_long_gaps_only_and_skips_pauses() {
        let t0 = Instant::now();
        let mut c = GapClock::new();
        c.tick(t0, false);
        c.tick(t0 + Duration::from_millis(300), false);
        assert_eq!(c.missing(), 0, "jitter is left alone");
        c.tick(t0 + Duration::from_secs(1), false);
        assert_eq!(c.missing(), 10 * BLOCK_SAMPLES);
        c.produced(10 * BLOCK_SAMPLES);
        assert_eq!(c.missing(), 0);
        // Two paused seconds count for nothing.
        c.tick(t0 + Duration::from_secs(3), true);
        assert_eq!(c.missing(), 0);
        c.tick(t0 + Duration::from_millis(3200), false);
        assert_eq!(c.missing(), 0);
    }

    fn ctl() -> CaptureCtl {
        CaptureCtl {
            stop: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            muted: Arc::new(AtomicBool::new(false)),
            written: Arc::new(AtomicU64::new(0)),
            join_at: None,
        }
    }

    /// A fake 44.1 kHz stereo device: the pump writes a 16 kHz mono track
    /// of the right length and hands the same audio on in 100 ms blocks.
    #[test]
    fn pump_writes_the_track_and_feeds_blocks() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("mostalgia-pump-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("mic.wav");
        let (tx, rx) = crossbeam_channel::bounded::<Block>(256);
        let ctl = ctl();
        let (go_tx, go_rx) = crossbeam_channel::bounded::<RawSink>(1);
        let handle = spawn_pump(Source::Mic, path.clone(), tx, ctl.clone(), false, move |raw| {
            let _ = go_tx.send(RawSink::new(raw, 2));
            Ok(Opened { rate: 44_100, guard: Box::new(()) })
        })?;
        let sink = go_rx.recv()?;
        let frame = |i: usize| (i as f32 * 0.02).sin() * 0.4;
        for chunk in 0..100 {
            // 10 ms of stereo per callback, one second in all.
            sink.push_interleaved((0..441).flat_map(|i| {
                let v = frame(chunk * 441 + i);
                [v, v]
            }));
        }
        let mut live = 0;
        while live < 9 * BLOCK_SAMPLES {
            let b = rx.recv_timeout(Duration::from_secs(5))?;
            assert_eq!(b.samples.len(), BLOCK_SAMPLES);
            assert_eq!(b.source, Source::Mic);
            live += b.samples.len();
        }
        ctl.stop.store(true, Ordering::SeqCst);
        handle.join().unwrap()?;
        let track = crate::finalize::read_wav(&path)?;
        assert!(track.len() >= 9 * BLOCK_SAMPLES && track.len() <= 16_000, "{}", track.len());
        assert_eq!(ctl.written.load(Ordering::SeqCst), track.len() as u64);
        assert!(crate::vad::rms(&track) > 0.1);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn a_failed_open_is_returned_and_writes_nothing() {
        let path = std::env::temp_dir().join(format!("mostalgia-pump-fail-{}.wav", std::process::id()));
        let (tx, _rx) = crossbeam_channel::bounded::<Block>(4);
        let r = spawn_pump(Source::System, path.clone(), tx, ctl(), true, |_| anyhow::bail!("no such device"));
        assert!(format!("{:#}", r.err().unwrap()).contains("no such device"));
        assert!(!path.exists());
    }

    /// A fatal stream error ends the capture with it, after finishing the track.
    #[test]
    fn a_broken_stream_ends_the_capture_with_its_error() -> Result<()> {
        let path = std::env::temp_dir().join(format!("mostalgia-pump-broken-{}.wav", std::process::id()));
        let (tx, _rx) = crossbeam_channel::bounded::<Block>(64);
        let handle = spawn_pump(Source::System, path.clone(), tx, ctl(), false, |raw| {
            let sink = RawSink::new(raw, 1);
            sink.push_mono(vec![0.1; 3200]);
            sink.fail("device unplugged");
            Ok(Opened { rate: 16_000, guard: Box::new(sink) })
        })?;
        let err = handle.join().unwrap().unwrap_err();
        assert!(err.to_string().contains("unplugged"));
        assert_eq!(crate::finalize::read_wav(&path)?.len(), 2 * BLOCK_SAMPLES);
        std::fs::remove_file(&path)?;
        Ok(())
    }

    /// The level meter takes the loudest block through any capture.
    #[test]
    fn measure_with_reports_the_peak() -> Result<()> {
        let level = measure_with(0.5, |path, tx, ctl| {
            spawn_pump(Source::Mic, path, tx, ctl, false, |raw| {
                let sink = RawSink::new(raw, 1);
                sink.push_mono(vec![0.5; 16_000]);
                Ok(Opened { rate: 16_000, guard: Box::new(sink) })
            })
        })?;
        assert!((level - 0.5).abs() < 0.01, "{level}");
        Ok(())
    }
}
