use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use crate::{BLOCK_SAMPLES, SAMPLE_RATE};

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
    pub samples: Vec<f32>,
}

/// Capture one Pulse/PipeWire source at 16 kHz mono f32, archive it to a WAV
/// file, and stream fixed-size blocks to the live transcriber. Pulse does the
/// resampling from the device's native rate for us.
pub fn spawn_capture(
    source: Source,
    device: Option<String>,
    wav_path: PathBuf,
    tx: Sender<Block>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
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

    let wav_spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav =
        hound::WavWriter::create(&wav_path, wav_spec).context("creating WAV writer")?;

    Ok(std::thread::spawn(move || -> Result<()> {
        let mut buf = vec![0u8; BLOCK_SAMPLES * 4];
        while !stop.load(Ordering::SeqCst) {
            pulse.read(&mut buf).context("reading from Pulse stream")?;
            // Keep draining the stream while paused, but record nothing:
            // the paused stretch is simply left out of the tracks.
            if paused.load(Ordering::SeqCst) {
                continue;
            }
            let samples: Vec<f32> = buf
                .chunks_exact(4)
                .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            for &s in &samples {
                let s16 = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                wav.write_sample(s16)?;
            }
            // The receiver going away just means we stop transcribing live.
            let _ = tx.send(Block { source, samples });
        }
        wav.finalize().context("finalizing WAV file")?;
        Ok(())
    }))
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
            return format!("Grafikkarte HDMI{n}");
        }
        return format!("HDMI{n}");
    }
    if builtin {
        if p.contains("headphone") {
            return "Kopfhörer (Klinke)".into();
        }
        if p.contains("headset") {
            return "Headset-Mikrofon (Klinke)".into();
        }
        if p.contains("speaker") {
            return "Laptop-Lautsprecher".into();
        }
        if p.contains("mic") {
            return "Laptop-Mikrofon".into();
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

/// Records `secs` from a source (without saving) and returns the loudest
/// 100 ms RMS — for a "test device" button.
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
