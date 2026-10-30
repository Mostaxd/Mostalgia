//! Audio through cpal everywhere but Linux (WASAPI on Windows, CoreAudio on
//! macOS): microphone capture, the device list, meeting playback, and on
//! Windows the system audio too (WASAPI loopback of an output device).
//!
//! The rest of the capture contract (16 kHz mono blocks, the WAV track,
//! pause/mute, the hand-off to the live transcriber) is in `audio_pump`,
//! shared with any platform-specific system-audio capture (see
//! `audio::spawn_capture` for where each OS plugs its own in).

use std::path::PathBuf;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use cpal::Sample;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Sender;

use crate::audio::{Block, CaptureCtl, DEFAULT_MONITOR, Source, SourceInfo};
use crate::audio_pump::{Opened, RawSink, spawn_pump};

/// Captures a microphone (`device`: a name from [`input_sources`], or
/// `None` for the default one).
pub fn spawn_input_capture(device: Option<String>, wav_path: PathBuf, tx: Sender<Block>, ctl: CaptureCtl) -> Result<JoinHandle<Result<()>>> {
    spawn_device_capture(Source::Mic, device, false, wav_path, tx, ctl)
}

/// Captures what an output device plays (WASAPI loopback). `device`: a
/// name from [`loopback_sources`], or `None` / [`DEFAULT_MONITOR`] for the
/// default output (which follows the system's default when it changes).
#[cfg(target_os = "windows")]
pub fn spawn_loopback_capture(device: Option<String>, wav_path: PathBuf, tx: Sender<Block>, ctl: CaptureCtl) -> Result<JoinHandle<Result<()>>> {
    spawn_device_capture(Source::System, device, true, wav_path, tx, ctl)
}

/// `loopback`: open an output device as an input (Windows) — it then
/// delivers nothing while nothing plays, so the pump fills in silence.
fn spawn_device_capture(
    source: Source,
    device: Option<String>,
    loopback: bool,
    wav_path: PathBuf,
    tx: Sender<Block>,
    ctl: CaptureCtl,
) -> Result<JoinHandle<Result<()>>> {
    spawn_pump(source, wav_path, tx, ctl, loopback, move |raw| {
        let host = cpal::default_host();
        let dev = find_device(&host, device.as_deref(), loopback)?;
        let config = if loopback { dev.default_output_config() } else { dev.default_input_config() }
            .with_context(|| format!("reading the audio format of {dev}"))?;
        let sink = RawSink::new(raw, config.channels() as usize);
        let stream = build_input(&dev, config, sink).with_context(|| match source {
            Source::Mic => format!("opening the microphone ({dev})"),
            Source::System => format!("capturing the system audio of {dev}"),
        })?;
        stream.play().with_context(|| format!("starting the audio stream of {dev}"))?;
        Ok(Opened { rate: config.sample_rate(), guard: Box::new(stream) })
    })
}

/// A device by the name the device list gave it (its stable id), or by
/// its display name (as typed on the command line); `None` (or Pulse's
/// [`DEFAULT_MONITOR`] for an output) is the default device.
pub fn find_device(host: &cpal::Host, name: Option<&str>, output: bool) -> Result<cpal::Device> {
    let default = || if output { host.default_output_device() } else { host.default_input_device() };
    let name = match name {
        None | Some("") | Some(DEFAULT_MONITOR) => {
            return default().with_context(|| if output { "no audio output found" } else { "no microphone found" });
        }
        Some(name) => name,
    };
    if let Ok(id) = name.parse::<cpal::DeviceId>()
        && let Some(dev) = host.device_by_id(&id)
    {
        return Ok(dev);
    }
    let devices: Vec<cpal::Device> = if output {
        host.output_devices().context("listing audio devices")?.collect()
    } else {
        host.input_devices().context("listing audio devices")?.collect()
    };
    devices
        .into_iter()
        .find(|d| d.to_string() == name)
        .with_context(|| format!("audio device not found: {name}"))
}

fn on_stream_error(sink: &RawSink, e: cpal::Error) {
    match e.kind() {
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => sink.fail(format!("audio device lost: {e}")),
        // Over- and underruns, a rerouted default device, no real-time
        // priority: the stream keeps going.
        _ => eprintln!("[mostalgia] audio stream: {e}"),
    }
}

fn build_input(dev: &cpal::Device, config: cpal::SupportedStreamConfig, sink: RawSink) -> Result<cpal::Stream> {
    use cpal::SampleFormat as F;
    let cfg = config.config();
    macro_rules! build {
        ($t:ty) => {{
            let data = sink.clone();
            let errors = sink.clone();
            dev.build_input_stream::<$t, _, _>(
                cfg,
                move |samples: &[$t], _| data.push_interleaved(samples.iter().map(|s| s.to_sample::<f32>())),
                move |e| on_stream_error(&errors, e),
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        F::F32 => build!(f32),
        F::F64 => build!(f64),
        F::I8 => build!(i8),
        F::I16 => build!(i16),
        F::I32 => build!(i32),
        F::U8 => build!(u8),
        F::U16 => build!(u16),
        F::U32 => build!(u32),
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    Ok(stream?)
}

/// Microphones and other inputs.
pub fn input_sources() -> Result<Vec<SourceInfo>> {
    sources(false)
}

/// Outputs whose sound can be recorded (WASAPI loopback), as monitors.
#[cfg(target_os = "windows")]
pub fn loopback_sources() -> Result<Vec<SourceInfo>> {
    sources(true)
}

fn sources(output: bool) -> Result<Vec<SourceInfo>> {
    let host = cpal::default_host();
    let default = if output { host.default_output_device() } else { host.default_input_device() };
    let default_id = default.and_then(|d| d.id().ok());
    let devices: Vec<cpal::Device> = if output {
        host.output_devices().context("listing audio devices")?.collect()
    } else {
        host.input_devices().context("listing audio devices")?.collect()
    };
    Ok(devices
        .into_iter()
        .filter_map(|d| {
            let id = d.id().ok()?;
            let description = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| d.to_string());
            Some(SourceInfo {
                label: short_label(&description),
                is_default: default_id.as_ref() == Some(&id),
                name: id.to_string(),
                description,
                is_monitor: output,
                available: true,
            })
        })
        .collect())
}

/// "Mikrofon (Yeti Nano)" → "Yeti Nano", "Lautsprecher (Realtek(R) Audio)"
/// stays as it is: Windows names endpoints "<kind> (<device>)", and for a
/// USB or Bluetooth device its own name is what tells it apart.
pub fn short_label(friendly: &str) -> String {
    let Some(open) = friendly.find(" (") else { return friendly.to_string() };
    let inner = friendly[open + 2..].strip_suffix(')').unwrap_or(&friendly[open + 2..]);
    let generic = ["Realtek", "High Definition Audio", "Intel", "AMD", "NVIDIA", "Conexant", "IDT", "Smart Sound", "SST"];
    if inner.is_empty() || generic.iter().any(|g| inner.contains(g)) {
        friendly.to_string()
    } else {
        inner.to_string()
    }
}

/// Opens the default output for mono playback: `fill` gets a buffer of
/// mono frames to fill (pre-zeroed) and returns how many it filled; each
/// goes to every channel. Returns the running stream and its rate.
pub fn open_output<F>(mut fill: F) -> Result<(cpal::Stream, u32)>
where
    F: FnMut(&mut [f32]) -> usize + Send + 'static,
{
    use cpal::SampleFormat as Fmt;
    let host = cpal::default_host();
    let dev = host.default_output_device().context("no audio output found")?;
    let config = dev.default_output_config().with_context(|| format!("reading the audio format of {dev}"))?;
    let channels = (config.channels() as usize).max(1);
    let cfg = config.config();
    macro_rules! build {
        ($t:ty) => {{
            let mut mono: Vec<f32> = Vec::new();
            dev.build_output_stream::<$t, _, _>(
                cfg,
                move |out: &mut [$t], _| {
                    let frames = out.len() / channels;
                    mono.clear();
                    mono.resize(frames, 0.0);
                    let n = fill(&mut mono).min(frames);
                    mono[n..].fill(0.0);
                    for (frame, &v) in out.chunks_mut(channels).zip(&mono) {
                        frame.fill(<$t as Sample>::from_sample(v));
                    }
                },
                |e| eprintln!("[mostalgia] playback: {e}"),
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        Fmt::F32 => build!(f32),
        Fmt::F64 => build!(f64),
        Fmt::I8 => build!(i8),
        Fmt::I16 => build!(i16),
        Fmt::I32 => build!(i32),
        Fmt::U8 => build!(u8),
        Fmt::U16 => build!(u16),
        Fmt::U32 => build!(u32),
        other => anyhow::bail!("unsupported sample format {other:?}"),
    }
    .context("opening audio output")?;
    stream.play().context("starting audio output")?;
    Ok((stream, config.sample_rate()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_labels_keep_the_device_name() {
        assert_eq!(short_label("Mikrofon (Yeti Nano)"), "Yeti Nano");
        assert_eq!(short_label("Headset (WH-1000XM4 Hands-Free)"), "WH-1000XM4 Hands-Free");
        assert_eq!(short_label("Lautsprecher (Realtek(R) Audio)"), "Lautsprecher (Realtek(R) Audio)");
        assert_eq!(short_label("Stereo Mix"), "Stereo Mix");
    }
}
