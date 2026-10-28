use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
#[cfg(target_os = "linux")]
use libpulse_binding::def::BufferAttr;
#[cfg(target_os = "linux")]
use libpulse_binding::sample::{Format, Spec};
#[cfg(target_os = "linux")]
use libpulse_binding::stream::Direction;
#[cfg(target_os = "linux")]
use libpulse_simple_binding::Simple;

#[cfg(any(target_os = "linux", test))]
use crate::BLOCK_SAMPLES;
use crate::SAMPLE_RATE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Mic,
    System,
}

impl Source {
    /// Stable name used in the database and on disk.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Mic => "mic",
            Source::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mic" => Some(Source::Mic),
            "system" => Some(Source::System),
            _ => None,
        }
    }

    pub fn file_name(self) -> String {
        format!("{}.wav", self.as_str())
    }

    /// Speaker label for lines from this source: you, or the other side.
    pub fn speaker_label(self) -> &'static str {
        match self {
            Source::Mic => "Ich",
            Source::System => "Andere",
        }
    }
}

pub struct Block {
    pub source: Source,
    /// Live audio (empty for a silent block).
    pub samples: Vec<f32>,
    /// Samples of silence written to the track instead of live audio: the
    /// source is switched off, or it joined the recording late and was
    /// padded up to the timeline. Nothing here is transcribed.
    pub silence: usize,
}

/// Shared switches and counters of one source's capture.
#[derive(Clone)]
pub struct CaptureCtl {
    pub stop: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    /// Switched off mid-recording: the stream keeps running, the track gets
    /// silence (so it stays aligned with the other one), nothing is sent
    /// to the transcriber.
    pub muted: Arc<AtomicBool>,
    /// Samples written to this source's track so far.
    pub written: Arc<AtomicU64>,
    /// Set when the source starts after the recording did: the other
    /// source's `written`, which this track is padded up to with silence
    /// before its first live block.
    pub join_at: Option<Arc<AtomicU64>>,
}

/// Silence a source joining late needs in front of its first live block of
/// `block` samples so that block ends where the timeline is now
/// (`timeline` samples written by the other source); `written` is what the
/// track already holds.
pub fn late_pad(timeline: u64, block: u64, written: u64) -> u64 {
    timeline.saturating_sub(block).saturating_sub(written)
}

/// One source's WAV track: live blocks, silence while switched off, and the
/// padding of a source that joined late. Returns what to pass on to the
/// live transcriber.
pub struct TrackWriter {
    source: Source,
    wav: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    /// The same file, to sync it to disk.
    file: std::fs::File,
    /// Samples since the last flush and sync.
    unsynced: u64,
    written: Arc<AtomicU64>,
    join_at: Option<Arc<AtomicU64>>,
}

impl TrackWriter {
    pub fn create(path: &std::path::Path, source: Source, written: Arc<AtomicU64>, join_at: Option<Arc<AtomicU64>>) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let file = std::fs::File::create(path).context("creating WAV file")?;
        let sync = file.try_clone().context("creating WAV file")?;
        let wav = hound::WavWriter::new(std::io::BufWriter::new(file), spec).context("creating WAV writer")?;
        Ok(Self { source, wav, file: sync, unsynced: 0, written, join_at })
    }

    /// Writes one captured block (or as much silence when `muted`).
    pub fn push(&mut self, samples: Vec<f32>, muted: bool) -> Result<Vec<Block>> {
        let mut out = Vec::with_capacity(2);
        if let Some(timeline) = self.join_at.take() {
            let pad = late_pad(timeline.load(Ordering::SeqCst), samples.len() as u64, self.written.load(Ordering::SeqCst));
            if pad > 0 {
                self.silence(pad)?;
                out.push(Block { source: self.source, samples: Vec::new(), silence: pad as usize });
            }
        }
        if muted {
            self.silence(samples.len() as u64)?;
            out.push(Block { source: self.source, samples: Vec::new(), silence: samples.len() });
        } else {
            for &s in &samples {
                let s16 = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                self.wav.write_sample(s16)?;
            }
            self.written.fetch_add(samples.len() as u64, Ordering::SeqCst);
            self.unsynced += samples.len() as u64;
            out.push(Block { source: self.source, samples, silence: 0 });
        }
        if self.unsynced as f64 >= crate::recovery::SYNC_SECS * SAMPLE_RATE as f64 {
            self.sync()?;
        }
        Ok(out)
    }

    /// Flushes what's buffered with the header's sizes brought up to date,
    /// and syncs it to disk: a crash, kill or power loss after this keeps a
    /// readable track (see `recovery`).
    fn sync(&mut self) -> Result<()> {
        self.unsynced = 0;
        self.wav.flush().context("writing WAV file")?;
        self.file.sync_data().context("syncing WAV file to disk")
    }

    fn silence(&mut self, n: u64) -> Result<()> {
        for _ in 0..n {
            self.wav.write_sample(0i16)?;
        }
        self.unsynced += n;
        self.written.fetch_add(n, Ordering::SeqCst);
        Ok(())
    }

    pub fn finalize(self) -> Result<()> {
        self.wav.finalize().context("finalizing WAV file")
    }
}

/// Capture one Pulse/PipeWire source at 16 kHz mono f32, archive it to a WAV
/// file, and stream fixed-size blocks to the live transcriber. Pulse does the
/// resampling from the device's native rate for us.
#[cfg(target_os = "linux")]
pub fn spawn_capture(
    source: Source,
    device: Option<String>,
    wav_path: PathBuf,
    tx: Sender<Block>,
    ctl: CaptureCtl,
) -> Result<JoinHandle<Result<()>>> {
    let spec = Spec {
        format: Format::FLOAT32NE,
        channels: 1,
        rate: SAMPLE_RATE,
    };
    anyhow::ensure!(spec.is_valid(), "invalid sample spec");

    let attr = BufferAttr {
        maxlength: u32::MAX,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        // Ask Pulse to hand us data in ~100 ms fragments.
        fragsize: (BLOCK_SAMPLES * 4) as u32,
    };

    let stream_name = match source {
        Source::Mic => "mostalgia-mic",
        Source::System => "mostalgia-system",
    };
    let pulse = Simple::new(
        None,
        "mostalgia",
        Direction::Record,
        device.as_deref(),
        stream_name,
        &spec,
        None,
        Some(&attr),
    )
    .with_context(|| match source {
        Source::Mic => "connecting to the default microphone via PulseAudio/PipeWire".to_string(),
        Source::System => format!(
            "connecting to the system-audio monitor ({}) via PulseAudio/PipeWire",
            device.as_deref().unwrap_or("@DEFAULT_MONITOR@")
        ),
    })?;

    let mut track = TrackWriter::create(&wav_path, source, ctl.written.clone(), ctl.join_at.clone())?;

    Ok(std::thread::spawn(move || -> Result<()> {
        let mut buf = vec![0u8; BLOCK_SAMPLES * 4];
        let mut feeder = Feeder::new(source, tx);
        while !ctl.stop.load(Ordering::SeqCst) {
            pulse.read(&mut buf).context("reading from Pulse stream")?;
            // Keep draining the stream while paused, but record nothing:
            // the paused stretch is simply left out of the tracks.
            if ctl.paused.load(Ordering::SeqCst) {
                continue;
            }
            let samples: Vec<f32> = buf
                .chunks_exact(4)
                .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            for block in track.push(samples, ctl.muted.load(Ordering::SeqCst))? {
                feeder.offer(block);
            }
        }
        track.finalize()
    }))
}

#[cfg(not(target_os = "linux"))]
pub fn spawn_capture(
    _source: Source,
    _device: Option<String>,
    _wav_path: PathBuf,
    _tx: Sender<Block>,
    _ctl: CaptureCtl,
) -> Result<JoinHandle<Result<()>>> {
    anyhow::bail!("{}", crate::i18n::tr("Audioaufnahme ist auf diesem System noch nicht verfügbar"))
}

/// Hands a capture's blocks to the live transcriber without ever waiting
/// for it: the track on disk is written first, so when the transcriber
/// falls behind (its queue is full) the live path just skips audio. What
/// it skipped arrives as one silent block once there is room again, so
/// its clock stays on the recording's timeline; the final pass reads the
/// track and misses nothing.
pub struct Feeder {
    source: Source,
    tx: Sender<Block>,
    /// Samples not handed over yet (sent as a silent block).
    skipped: usize,
    /// Samples skipped over the whole recording.
    pub skipped_total: u64,
}

impl Feeder {
    pub fn new(source: Source, tx: Sender<Block>) -> Self {
        Self { source, tx, skipped: 0, skipped_total: 0 }
    }

    /// Queues `block`, or skips it if the transcriber's queue is full.
    /// Never blocks. The receiver going away just means no live text.
    pub fn offer(&mut self, block: Block) {
        use crossbeam_channel::TrySendError;
        let len = block.samples.len() + block.silence;
        if self.skipped > 0 {
            let gap = Block { source: self.source, samples: Vec::new(), silence: self.skipped };
            match self.tx.try_send(gap) {
                Ok(()) => self.skipped = 0,
                Err(TrySendError::Full(_)) => {
                    self.skip(len);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        if let Err(TrySendError::Full(_)) = self.tx.try_send(block) {
            self.skip(len);
        }
    }

    fn skip(&mut self, len: usize) {
        self.skipped += len;
        self.skipped_total += len as u64;
    }
}

pub struct SourceInfo {
    pub name: String,
    pub description: String,
    /// Short, distinguishing name for people ("Laptop-Mikrofon", "HDMI 2",
    /// "Yeti Nano"), from the device and its active port.
    pub label: String,
    /// Monitor of an output (system audio) rather than an input.
    pub is_monitor: bool,
    pub is_default: bool,
    /// Whether the active port is plugged in / usable (unknown counts as yes).
    pub available: bool,
}

/// A short German name for a capture source: the port for built-in sound
/// cards (where every port shares one long card name), the card otherwise.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn friendly_label(name: &str, description: &str, port: Option<(&str, &str)>) -> String {
    let card = description.trim_start_matches("Monitor of ").trim();
    let (port_name, port_desc) = port.unwrap_or(("", ""));
    let p = format!("{port_name} {port_desc}").to_lowercase();
    let builtin = card.contains("Chipset") || card.contains("Built-in") || name.contains("sof_sdw") || name.contains("pci-0000_00_1f");
    let hdmi_number = || port_desc.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    if p.contains("hdmi") || p.contains("displayport") {
        let n = hdmi_number();
        let n = if n.is_empty() { String::new() } else { format!(" {n}") };
        if card.contains("High Definition Audio Controller") || name.contains("pci-0000_01") {
            // The discrete GPU's audio function.
            return crate::i18n::trf("Grafikkarte HDMI{}", &[&n]);
        }
        return format!("HDMI{n}");
    }
    if builtin {
        if p.contains("headphone") {
            return crate::i18n::tr("Kopfhörer (Klinke)").into();
        }
        if p.contains("headset") {
            return crate::i18n::tr("Headset-Mikrofon (Klinke)").into();
        }
        if p.contains("speaker") {
            return crate::i18n::tr("Laptop-Lautsprecher").into();
        }
        if p.contains("mic") {
            return crate::i18n::tr("Laptop-Mikrofon").into();
        }
    }
    let mut short = card.to_string();
    for suffix in [" Analog Stereo", " Digital Stereo (HDMI)", " Digital Stereo", " Mono", " Stereo"] {
        if let Some(s) = short.strip_suffix(suffix) {
            short = s.to_string();
        }
    }
    short
}

/// Lists Pulse/PipeWire capture sources, marking the default mic and the
/// monitor of the default output.
#[cfg(target_os = "linux")]
pub fn list_sources() -> Result<Vec<SourceInfo>> {
    use std::cell::RefCell;
    use std::rc::Rc;

    use libpulse_binding::callbacks::ListResult;
    use libpulse_binding::context::{Context as PaContext, FlagSet, State};
    use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
    use libpulse_binding::operation::{Operation, State as OpState};

    let mut ml = Mainloop::new().context("creating Pulse mainloop")?;
    let mut ctx = PaContext::new(&ml, "mostalgia").context("creating Pulse context")?;
    ctx.connect(None, FlagSet::NOFLAGS, None)
        .map_err(|e| anyhow::anyhow!("connecting to PulseAudio/PipeWire: {e}"))?;

    fn iterate(ml: &mut Mainloop) -> Result<()> {
        match ml.iterate(true) {
            IterateResult::Success(_) => Ok(()),
            _ => anyhow::bail!("Pulse mainloop stopped"),
        }
    }
    fn wait<F: ?Sized>(ml: &mut Mainloop, op: Operation<F>) -> Result<()> {
        while op.get_state() == OpState::Running {
            iterate(ml)?;
        }
        Ok(())
    }
    loop {
        iterate(&mut ml)?;
        match ctx.get_state() {
            State::Ready => break,
            State::Failed | State::Terminated => anyhow::bail!("PulseAudio/PipeWire connection failed"),
            _ => {}
        }
    }

    let defaults = Rc::new(RefCell::new((String::new(), String::new())));
    {
        let defaults = defaults.clone();
        let op = ctx.introspect().get_server_info(move |info| {
            *defaults.borrow_mut() = (
                info.default_source_name.as_deref().unwrap_or_default().to_string(),
                info.default_sink_name.as_deref().unwrap_or_default().to_string(),
            );
        });
        wait(&mut ml, op)?;
    }

    // Outputs' active ports, so a monitor can be named and filtered by
    // what it actually plays to.
    type Port = (String, String, bool);
    let sinks: Rc<RefCell<std::collections::HashMap<String, Port>>> = Rc::default();
    {
        let sinks = sinks.clone();
        let op = ctx.introspect().get_sink_info_list(move |r| {
            let ListResult::Item(info) = r else { return };
            if let (Some(name), Some(port)) = (info.name.as_deref(), info.active_port.as_ref()) {
                sinks.borrow_mut().insert(
                    name.to_string(),
                    (
                        port.name.as_deref().unwrap_or_default().to_string(),
                        port.description.as_deref().unwrap_or_default().to_string(),
                        port.available != libpulse_binding::def::PortAvailable::No,
                    ),
                );
            }
        });
        wait(&mut ml, op)?;
    }

    let sources = Rc::new(RefCell::new(Vec::new()));
    {
        let sources = sources.clone();
        let defaults = defaults.clone();
        let sinks = sinks.clone();
        let op = ctx.introspect().get_source_info_list(move |r| {
            let ListResult::Item(info) = r else { return };
            let (default_source, default_sink) = &*defaults.borrow();
            let name = info.name.as_deref().unwrap_or_default().to_string();
            let description = info.description.as_deref().unwrap_or_default().to_string();
            let monitor_of = info.monitor_of_sink_name.as_deref();
            let port: Option<Port> = match monitor_of {
                Some(sink) => sinks.borrow().get(sink).cloned(),
                None => info.active_port.as_ref().map(|p| {
                    (
                        p.name.as_deref().unwrap_or_default().to_string(),
                        p.description.as_deref().unwrap_or_default().to_string(),
                        p.available != libpulse_binding::def::PortAvailable::No,
                    )
                }),
            };
            sources.borrow_mut().push(SourceInfo {
                is_monitor: info.monitor_of_sink.is_some(),
                is_default: match monitor_of {
                    Some(sink) => sink == default_sink,
                    None => &name == default_source,
                },
                label: friendly_label(&name, &description, port.as_ref().map(|(n, d, _)| (n.as_str(), d.as_str()))),
                available: port.as_ref().is_none_or(|(_, _, a)| *a),
                description,
                name,
            });
        });
        wait(&mut ml, op)?;
    }
    ctx.disconnect();
    Ok(sources.take())
}

#[cfg(not(target_os = "linux"))]
pub fn list_sources() -> Result<Vec<SourceInfo>> {
    anyhow::bail!("{}", crate::i18n::tr("Audioaufnahme ist auf diesem System noch nicht verfügbar"))
}

/// Records `secs` from a source (without saving) and returns the loudest
/// 100 ms RMS — for a "test device" button.
#[cfg(target_os = "linux")]
pub fn measure_level(device: Option<&str>, secs: f32) -> Result<f32> {
    let spec = Spec {
        format: Format::FLOAT32NE,
        channels: 1,
        rate: SAMPLE_RATE,
    };
    let pulse = Simple::new(None, "mostalgia", Direction::Record, device, "mostalgia-test", &spec, None, None)
        .context("opening the audio source")?;
    let mut buf = vec![0u8; BLOCK_SAMPLES * 4];
    let mut peak = 0f32;
    for _ in 0..((secs * 10.0) as usize).max(1) {
        pulse.read(&mut buf).context("reading audio")?;
        let samples: Vec<f32> = buf
            .chunks_exact(4)
            .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        peak = peak.max(crate::vad::rms(&samples));
    }
    Ok(peak)
}

#[cfg(not(target_os = "linux"))]
pub fn measure_level(_device: Option<&str>, _secs: f32) -> Result<f32> {
    anyhow::bail!("{}", crate::i18n::tr("Audioaufnahme ist auf diesem System noch nicht verfügbar"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: usize = BLOCK_SAMPLES;

    /// A transcriber stuck for 50 blocks (5 s): capture never waits, and
    /// once it catches up the skipped audio arrives as silence, so its
    /// clock still covers every sample.
    #[test]
    fn stalled_transcriber_never_blocks_capture() {
        let (tx, rx) = crossbeam_channel::bounded::<Block>(4);
        let mut feeder = Feeder::new(Source::System, tx);
        let t = std::time::Instant::now();
        for _ in 0..50 {
            feeder.offer(Block { source: Source::System, samples: vec![0.1; B], silence: 0 });
        }
        assert!(t.elapsed() < std::time::Duration::from_millis(100), "capture waited on the transcriber");
        assert_eq!(rx.len(), 4);
        assert_eq!(feeder.skipped_total, 46 * B as u64);
        // Unstuck: it drains, then the next block brings the gap first.
        let mut live = 0;
        let mut silent = 0;
        let mut take = |rx: &crossbeam_channel::Receiver<Block>| {
            for b in rx.try_iter() {
                live += b.samples.len();
                silent += b.silence;
            }
        };
        take(&rx);
        feeder.offer(Block { source: Source::System, samples: vec![0.1; B], silence: 0 });
        take(&rx);
        assert_eq!(live + silent, 51 * B);
        assert_eq!(silent, 46 * B);
        // A transcriber that went away doesn't stop the capture either.
        drop(rx);
        feeder.offer(Block { source: Source::System, samples: vec![0.1; B], silence: 0 });
    }

    fn tone(n: usize) -> Vec<f32> {
        (0..n).map(|i| 0.3 * (i as f32 * 0.05).sin() + 0.01).collect()
    }

    #[test]
    fn late_pad_lines_the_first_block_up_with_the_timeline() {
        // The other track holds 10 blocks; our first block covers the last one.
        assert_eq!(late_pad(10 * B as u64, B as u64, 0), 9 * B as u64);
        assert_eq!(late_pad(0, B as u64, 0), 0);
        assert_eq!(late_pad(5, B as u64, 0), 0);
        assert_eq!(late_pad(10 * B as u64, B as u64, 4 * B as u64), 5 * B as u64);
    }

    /// Desktop audio from the start, the mic switched on after 10 blocks,
    /// off for 3, on again; desktop audio off for 2: both tracks end up the
    /// same length, silent exactly where they should be, and the silent
    /// stretches reach the transcriber as silence only, never as audio.
    #[test]
    fn tracks_stay_sample_aligned_through_switches() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("mostalgia-align-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let written: [Arc<AtomicU64>; 2] = Default::default();
        let mut system = TrackWriter::create(&dir.join("system.wav"), Source::System, written[1].clone(), None)?;
        for _ in 0..10 {
            system.push(tone(B), false)?;
        }
        let mut mic = TrackWriter::create(&dir.join("mic.wav"), Source::Mic, written[0].clone(), Some(written[1].clone()))?;
        // One more desktop block, then the mic's first block arrives.
        system.push(tone(B), false)?;
        let first = mic.push(tone(B), false)?;
        assert_eq!(first.len(), 2);
        assert!(first[0].samples.is_empty());
        assert_eq!(first[0].silence, 10 * B, "padded from 0 to where the timeline is");
        assert_eq!(first[1].samples.len(), B);
        assert_eq!(written[0].load(Ordering::SeqCst), written[1].load(Ordering::SeqCst));

        let mut sent = Vec::new();
        for step in 0..8 {
            let mic_off = (2..5).contains(&step);
            let sys_off = (5..7).contains(&step);
            sent.extend(system.push(tone(B), sys_off)?);
            sent.extend(mic.push(tone(B), mic_off)?);
        }
        for b in &sent {
            assert!(b.samples.is_empty() != (b.silence == 0), "a block is either audio or silence");
        }
        let silent: Vec<(Source, usize)> = sent.iter().filter(|b| b.silence > 0).map(|b| (b.source, b.silence)).collect();
        assert_eq!(silent.iter().filter(|(s, _)| *s == Source::Mic).count(), 3);
        assert_eq!(silent.iter().filter(|(s, _)| *s == Source::System).count(), 2);
        system.finalize()?;
        mic.finalize()?;

        let m = crate::finalize::read_wav(&dir.join("mic.wav"))?;
        let s = crate::finalize::read_wav(&dir.join("system.wav"))?;
        assert_eq!(m.len(), 19 * B);
        assert_eq!(m.len(), s.len(), "same length, sample for sample");
        let block = |t: &[f32], i: usize| t[i * B..(i + 1) * B].to_vec();
        let zero = |v: Vec<f32>| v.iter().all(|&x| x == 0.0);
        for i in 0..10 {
            assert!(zero(block(&m, i)), "mic padding, block {i}");
        }
        assert!(!zero(block(&m, 10)), "first live mic block right after the padding");
        for i in 11..19 {
            let off = (13..16).contains(&i);
            assert_eq!(zero(block(&m, i)), off, "mic block {i}");
        }
        for i in 0..19 {
            let off = (16..18).contains(&i);
            assert_eq!(zero(block(&s, i)), off, "system block {i}");
        }
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
