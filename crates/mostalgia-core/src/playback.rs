use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
#[cfg(target_os = "linux")]
use libpulse_binding::def::BufferAttr;
#[cfg(target_os = "linux")]
use libpulse_binding::sample::{Format, Spec};
#[cfg(target_os = "linux")]
use libpulse_binding::stream::Direction;
#[cfg(target_os = "linux")]
use libpulse_simple_binding::Simple;

use crate::{BLOCK_SAMPLES, SAMPLE_RATE};

/// Plays a meeting's tracks mixed together from a given time, on the
/// default output. Dropping it stops playback.
pub struct Player {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    /// Samples (at `rate`) written to the output so far.
    played: Arc<AtomicU64>,
    rate: u32,
    from_secs: f64,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    #[cfg(target_os = "linux")]
    pub fn play(tracks: &[PathBuf], from_secs: f64) -> Result<Self> {
        let readers = open_tracks(tracks, from_secs)?;

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
            rate: SAMPLE_RATE,
            from_secs,
            thread: Some(thread),
        })
    }

    /// Through cpal: a thread mixes and resamples the tracks to the
    /// output's rate and queues them for the output's callback, which
    /// counts what it actually played.
    #[cfg(not(target_os = "linux"))]
    pub fn play(tracks: &[PathBuf], from_secs: f64) -> Result<Self> {
        use crossbeam_channel::TryRecvError;

        let readers = open_tracks(tracks, from_secs)?;
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let played = Arc::new(AtomicU64::new(0));
        let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<u32>>(1);
        let thread = {
            let (stop, done, played) = (stop.clone(), done.clone(), played.clone());
            std::thread::spawn(move || {
                // A few short chunks queued, so stopping is immediate.
                let (chunk_tx, chunk_rx) = crossbeam_channel::bounded::<Vec<f32>>(3);
                let drained = Arc::new(AtomicBool::new(false));
                let mut queue = std::collections::VecDeque::<f32>::new();
                let fill = {
                    let (stop, played, drained) = (stop.clone(), played.clone(), drained.clone());
                    move |out: &mut [f32]| -> usize {
                        if stop.load(Ordering::SeqCst) {
                            return 0;
                        }
                        let mut n = 0;
                        while n < out.len() {
                            if queue.is_empty() {
                                match chunk_rx.try_recv() {
                                    Ok(chunk) => queue.extend(chunk),
                                    Err(TryRecvError::Empty) => break,
                                    Err(TryRecvError::Disconnected) => {
                                        drained.store(true, Ordering::SeqCst);
                                        break;
                                    }
                                }
                            }
                            while n < out.len()
                                && let Some(v) = queue.pop_front()
                            {
                                out[n] = v;
                                n += 1;
                            }
                        }
                        played.fetch_add(n as u64, Ordering::Relaxed);
                        n
                    }
                };
                let (stream, rate) = match crate::audio_cpal::open_output(fill) {
                    Ok(o) => o,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        done.store(true, Ordering::SeqCst);
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(rate));
                let _ = feed_loop(readers, rate, &chunk_tx, &stop);
                drop(chunk_tx);
                // Let the output play what's queued.
                while !stop.load(Ordering::SeqCst) && !drained.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                drop(stream);
                done.store(true, Ordering::SeqCst);
            })
        };
        let rate = match ready_rx.recv() {
            Ok(Ok(rate)) => rate,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                anyhow::bail!("playback thread ended");
            }
        };
        Ok(Self {
            stop,
            done,
            played,
            rate,
            from_secs,
            thread: Some(thread),
        })
    }

    /// Current playback position in seconds from the recording start.
    pub fn position_secs(&self) -> f64 {
        self.from_secs + self.played.load(Ordering::Relaxed) as f64 / self.rate.max(1) as f64
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

type Track = hound::WavReader<BufReader<File>>;

/// The tracks, each placed at `from_secs` (those already over left out).
fn open_tracks(tracks: &[PathBuf], from_secs: f64) -> Result<Vec<Track>> {
    let start = (from_secs.max(0.0) * SAMPLE_RATE as f64) as u32;
    let mut readers = Vec::new();
    for path in tracks {
        let mut r = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
        if start < r.duration() {
            r.seek(start).context("seeking in WAV")?;
            readers.push(r);
        }
    }
    Ok(readers)
}

/// The next 100 ms of the tracks mixed together (as 16-bit sample values
/// in f32), into `mix`; returns how many samples are left (0 at the end).
fn mix_next(readers: &mut [Track], mix: &mut [f32]) -> Result<usize> {
    let gain = 1.0 / readers.len().max(1) as f32;
    mix.fill(0.0);
    let mut longest = 0;
    for r in readers.iter_mut() {
        let mut n = 0;
        for (slot, s) in mix.iter_mut().zip(r.samples::<i16>()) {
            *slot += s? as f32 * gain;
            n += 1;
        }
        longest = longest.max(n);
    }
    Ok(longest)
}

/// Mixes the tracks, resamples them to `rate` and queues them for the
/// output, until the end or `stop`.
#[cfg(not(target_os = "linux"))]
fn feed_loop(mut readers: Vec<Track>, rate: u32, tx: &crossbeam_channel::Sender<Vec<f32>>, stop: &AtomicBool) -> Result<()> {
    use crossbeam_channel::SendTimeoutError;
    let send = |mut chunk: Vec<f32>| -> bool {
        loop {
            match tx.send_timeout(chunk, std::time::Duration::from_millis(50)) {
                Ok(()) => return true,
                Err(SendTimeoutError::Timeout(c)) if !stop.load(Ordering::SeqCst) => chunk = c,
                Err(_) => return false,
            }
        }
    };
    let mut resampler = crate::media_import::Resampler::between(SAMPLE_RATE, rate);
    let mut mix = vec![0f32; BLOCK_SAMPLES];
    while !stop.load(Ordering::SeqCst) {
        let n = mix_next(&mut readers, &mut mix)?;
        if n == 0 {
            break;
        }
        let mut out = Vec::new();
        let scaled: Vec<f32> = mix[..n].iter().map(|v| (v / 32768.0).clamp(-1.0, 1.0)).collect();
        resampler.push(&scaled, &mut out);
        if !send(out) {
            return Ok(());
        }
    }
    let mut tail = Vec::new();
    resampler.finish(&mut tail);
    send(tail);
    Ok(())
}

#[cfg(target_os = "linux")]
fn play_loop(
    pulse: &Simple,
    mut readers: Vec<Track>,
    stop: &AtomicBool,
    played: &AtomicU64,
) -> Result<()> {
    let mut mix = vec![0f32; BLOCK_SAMPLES];
    let mut out = Vec::with_capacity(BLOCK_SAMPLES * 2);
    while !stop.load(Ordering::SeqCst) {
        let longest = mix_next(&mut readers, &mut mix)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, samples: &[i16]) -> Result<()> {
        let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec)?;
        for &s in samples {
            w.write_sample(s)?;
        }
        w.finalize()?;
        Ok(())
    }

    /// Tracks are mixed at equal gain from the chosen time; a track that's
    /// over is left out, the longer one plays to its end.
    #[test]
    fn tracks_mix_from_the_start_time() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("mostalgia-mix-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let (a, b, c) = (dir.join("a.wav"), dir.join("b.wav"), dir.join("c.wav"));
        write(&a, &vec![1000; 2 * BLOCK_SAMPLES])?;
        write(&b, &vec![3000; BLOCK_SAMPLES + 10])?;
        write(&c, &vec![500; 10])?;
        let mut readers = open_tracks(&[a, b, c], BLOCK_SAMPLES as f64 / SAMPLE_RATE as f64)?;
        assert_eq!(readers.len(), 2, "the track that's over is left out");
        let mut mix = vec![0f32; BLOCK_SAMPLES];
        assert_eq!(mix_next(&mut readers, &mut mix)?, BLOCK_SAMPLES);
        assert_eq!(mix[0], 2000.0);
        assert_eq!(mix[10], 500.0, "one track ended, the other still at half gain");
        assert_eq!(mix_next(&mut readers, &mut mix)?, 0);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
