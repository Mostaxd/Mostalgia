use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use crate::{BLOCK_SAMPLES, SAMPLE_RATE};

/// Plays a meeting's tracks mixed together from a given time, on the
/// default output. Dropping it stops playback.
pub struct Player {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    /// Samples written to the output so far.
    played: Arc<AtomicU64>,
    from_secs: f64,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn play(tracks: &[PathBuf], from_secs: f64) -> Result<Self> {
        let start = (from_secs.max(0.0) * SAMPLE_RATE as f64) as u32;
        let mut readers = Vec::new();
        for path in tracks {
            let mut r = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
            if start < r.duration() {
                r.seek(start).context("seeking in WAV")?;
                readers.push(r);
            }
        }

        let spec = Spec {
            format: Format::S16NE,
            channels: 1,
            rate: SAMPLE_RATE,
        };
        // A short buffer (~100 ms) so stopping is immediate.
        let bytes = (BLOCK_SAMPLES * 2) as u32;
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: bytes,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };
        let pulse = Simple::new(None, "mostalgia", Direction::Playback, None, "mostalgia-playback", &spec, None, Some(&attr))
            .context("opening audio output")?;

        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let played = Arc::new(AtomicU64::new(0));
        let thread = {
            let (stop, done, played) = (stop.clone(), done.clone(), played.clone());
            std::thread::spawn(move || {
                let _ = play_loop(&pulse, readers, &stop, &played);
                if stop.load(Ordering::SeqCst) {
                    let _ = pulse.flush();
                } else {
                    let _ = pulse.drain();
                }
                done.store(true, Ordering::SeqCst);
            })
        };
        Ok(Self {
            stop,
            done,
            played,
            from_secs,
            thread: Some(thread),
        })
    }

    /// Current playback position in seconds from the recording start.
    pub fn position_secs(&self) -> f64 {
        self.from_secs + self.played.load(Ordering::Relaxed) as f64 / SAMPLE_RATE as f64
    }

    pub fn is_finished(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn play_loop(
    pulse: &Simple,
    mut readers: Vec<hound::WavReader<BufReader<File>>>,
    stop: &AtomicBool,
    played: &AtomicU64,
) -> Result<()> {
    let gain = 1.0 / readers.len().max(1) as f32;
    let mut mix = vec![0f32; BLOCK_SAMPLES];
    let mut out = Vec::with_capacity(BLOCK_SAMPLES * 2);
    while !stop.load(Ordering::SeqCst) {
        mix.fill(0.0);
        let mut longest = 0;
        for r in &mut readers {
            let mut n = 0;
            for (slot, s) in mix.iter_mut().zip(r.samples::<i16>()) {
                *slot += s? as f32 * gain;
                n += 1;
            }
            longest = longest.max(n);
        }
        if longest == 0 {
            break;
        }
        out.clear();
        for &v in &mix[..longest] {
            out.extend_from_slice(&(v.clamp(i16::MIN as f32, i16::MAX as f32) as i16).to_ne_bytes());
        }
        pulse.write(&out).context("writing to audio output")?;
        played.fetch_add(longest as u64, Ordering::Relaxed);
    }
    Ok(())
}

/// Peak level (0–1) of `buckets` equal slices of a WAV track, for drawing
/// a waveform.
pub fn peaks(path: &Path, buckets: usize) -> Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let total = r.duration() as usize;
    let buckets = buckets.max(1);
    let per = total.div_ceil(buckets).max(1);
    let mut out = vec![0f32; buckets];
    for (i, s) in r.samples::<i16>().enumerate() {
        let v = (s? as f32 / i16::MAX as f32).abs();
        let b = &mut out[(i / per).min(buckets - 1)];
        if v > *b {
            *b = v;
        }
    }
    Ok(out)
}
