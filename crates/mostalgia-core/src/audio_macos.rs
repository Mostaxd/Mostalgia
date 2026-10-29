//! System audio on macOS: what the other side of a call says, i.e.
//! everything the Mac plays (our own process left out).
//!
//! - macOS 14.2 and newer: a Core Audio process tap. A global mono tap of
//!   all processes' output (unmuted: you keep hearing the call) is put into
//!   a private aggregate device, whose IO callback hands us the mix. Needs
//!   the "system audio recording" permission (`NSAudioCaptureUsageDescription`),
//!   which macOS asks for on first use.
//! - macOS 13 to 14.1: ScreenCaptureKit's audio of the main display. Needs
//!   the "screen recording" permission (System Settings → Privacy &
//!   Security), as macOS has no audio-only permission there.
//!
//! Either way, the audio goes the same way as on Linux: resampled to
//! 16 kHz mono f32, cut into `BLOCK_SAMPLES` blocks, written to the WAV
//! track first, then offered to the live transcriber without ever waiting
//! for it (`Feeder`).

use std::ffi::{CStr, c_void};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertySelector, CATapDescription,
    CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectSystemObject, kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat};
use objc2_core_foundation::{CFDictionary, CFRetained};
use objc2_core_media::{CMBlockBuffer, CMSampleBuffer, CMTime};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString, NSUUID};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType, SCWindow,
};

use crate::audio::{Block, CaptureCtl, Feeder, Source, TrackWriter};
use crate::{BLOCK_SAMPLES, SAMPLE_RATE};

/// Starts capturing the Mac's sound output for the recording. `device` (an
/// output device's UID) is not used yet: the tap takes all output.
///
/// Fails right away (before a thread is left running) when neither a
/// process tap nor ScreenCaptureKit can be set up, e.g. without permission.
pub fn spawn_system_capture(
    device: Option<String>,
    wav_path: PathBuf,
    tx: Sender<Block>,
    ctl: CaptureCtl,
) -> Result<JoinHandle<Result<()>>> {
    let _ = device;
    let mut track = TrackWriter::create(&wav_path, Source::System, ctl.written.clone(), ctl.join_at.clone())?;
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<f64>>(1);
    let stop = ctl.stop.clone();
    let handle = std::thread::Builder::new().name("mostalgia-system-audio".into()).spawn(move || -> Result<()> {
        // Audio from the IO callback / the capture queue: mono at `rate`.
        let (audio_tx, audio_rx) = crossbeam_channel::unbounded::<Vec<f32>>();
        let backend = match Backend::start(audio_tx) {
            Ok(b) => b,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return Ok(());
            }
        };
        let _ = ready_tx.send(Ok(backend.rate));
        let mut feeder = Feeder::new(Source::System, tx);
        let result = pump(&audio_rx, backend.rate, &ctl, |samples, muted| {
            for block in track.push(samples, muted)? {
                feeder.offer(block);
            }
            Ok(())
        });
        drop(backend);
        result?;
        track.finalize()
    })?;
    match ready_rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(_rate)) => Ok(handle),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            bail!("system audio did not start (no answer from macOS)")
        }
    }
}

/// Moves audio from `rx` (mono at `rate`) into 16 kHz blocks handed to
/// `push` until `ctl.stop`. Paused audio is dropped; when the source goes
/// quiet (nothing plays and the backend sends nothing, or a device switch),
/// the gap is filled with silence so the track keeps the recording's time.
fn pump(rx: &Receiver<Vec<f32>>, rate: f64, ctl: &CaptureCtl, mut push: impl FnMut(Vec<f32>, bool) -> Result<()>) -> Result<()> {
    let mut resampler = Resampler::new(rate, SAMPLE_RATE as f64);
    let mut pending: Vec<f32> = Vec::with_capacity(BLOCK_SAMPLES * 4);
    let mut last_audio = Instant::now();
    while !ctl.stop.load(Ordering::SeqCst) {
        let paused = ctl.paused.load(Ordering::SeqCst);
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(samples) => {
                last_audio = Instant::now();
                if !paused {
                    resampler.process(&samples, &mut pending);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let quiet = last_audio.elapsed();
                if paused {
                    last_audio = Instant::now();
                } else if quiet >= GAP {
                    pending.resize(pending.len() + (quiet.as_secs_f64() * SAMPLE_RATE as f64) as usize, 0.0);
                    last_audio = Instant::now();
                }
            }
            Err(RecvTimeoutError::Disconnected) => bail!("the system audio stream ended"),
        }
        while pending.len() >= BLOCK_SAMPLES {
            let block: Vec<f32> = pending.drain(..BLOCK_SAMPLES).collect();
            push(block, ctl.muted.load(Ordering::SeqCst))?;
        }
    }
    Ok(())
}

/// How long the source may send nothing before the gap counts as silence.
const GAP: Duration = Duration::from_millis(400);

/// A running capture; stops and tears down when dropped.
struct Backend {
    rate: f64,
    _inner: Inner,
}

enum Inner {
    Tap(Tap),
    Sck(Sck),
}

impl Backend {
    fn start(tx: Sender<Vec<f32>>) -> Result<Self> {
        match Tap::start(tx.clone()) {
            Ok(tap) => Ok(Self { rate: tap.rate, _inner: Inner::Tap(tap) }),
            Err(tap_err) => {
                eprintln!("[mostalgia] Core Audio tap unavailable ({tap_err:#}); trying ScreenCaptureKit");
                let sck = Sck::start(tx).with_context(|| format!("Core Audio tap: {tap_err:#}"))?;
                Ok(Self { rate: sck.rate, _inner: Inner::Sck(sck) })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Core Audio process tap (macOS 14.2+)

/// `AudioHardwareCreateProcessTap` / `AudioHardwareDestroyProcessTap`, looked
/// up at run time: linked directly, the app wouldn't start before 14.2.
type CreateTapFn = unsafe extern "C" fn(*const AnyObject, *mut AudioObjectID) -> i32;
type DestroyTapFn = unsafe extern "C" fn(AudioObjectID) -> i32;

fn symbol<T>(name: &CStr) -> Option<T> {
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    // SAFETY: `T` is the function pointer type of that symbol.
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
}

struct Tap {
    rate: f64,
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    destroy_tap: DestroyTapFn,
    /// The IO callback's data (owned here, freed after the callback is gone).
    sender: *mut Sender<Vec<f32>>,
}

impl Tap {
    fn start(tx: Sender<Vec<f32>>) -> Result<Self> {
        let create: CreateTapFn = symbol(c"AudioHardwareCreateProcessTap").context("needs macOS 14.2 or newer")?;
        let destroy_tap: DestroyTapFn = symbol(c"AudioHardwareDestroyProcessTap").context("needs macOS 14.2 or newer")?;
        anyhow::ensure!(AnyClass::get(c"CATapDescription").is_some(), "needs macOS 14.2 or newer");

        // All processes' output but our own (playback of a recording, Mori).
        let mut exclude = Vec::new();
        if let Ok(me) = get_property::<AudioObjectID, i32>(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyTranslatePIDToProcessObject,
            Some(&(std::process::id() as i32)),
        ) && me != 0
        {
            exclude.push(NSNumber::new_u32(me));
        }
        let desc = unsafe {
            let d = CATapDescription::initMonoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &NSArray::from_retained_slice(&exclude));
            d.setName(&NSString::from_str("Mostalgia"));
            d.setPrivate(true);
            d.setMuteBehavior(CATapMuteBehavior::Unmuted);
            d
        };
        let tap_uid = unsafe { desc.UUID().UUIDString() };
        let mut tap: AudioObjectID = 0;
        let status = unsafe { create(Retained::as_ptr(&desc).cast(), &mut tap) };
        anyhow::ensure!(status == 0 && tap != 0, "creating the process tap failed (OSStatus {status}); is system audio recording allowed for Mostalgia?");

        let mut this = Self { rate: 48_000.0, tap, aggregate: 0, proc_id: None, destroy_tap, sender: std::ptr::null_mut() };
        let format: AudioStreamBasicDescription = get_property(tap, kAudioTapPropertyFormat, None::<&()>).context("reading the tap's format")?;
        anyhow::ensure!(
            format.mFormatFlags & kAudioFormatFlagIsFloat != 0 && format.mBitsPerChannel == 32,
            "unexpected tap format (flags {:#x}, {} bits)",
            format.mFormatFlags,
            format.mBitsPerChannel
        );
        if format.mSampleRate > 0.0 {
            this.rate = format.mSampleRate;
        }

        // A private aggregate device with the tap as its input, clocked by
        // the default output device (so it runs, and delivers silence,
        // while nothing plays).
        let output_uid = default_output_uid();
        let key = |k: &CStr| NSString::from_str(k.to_str().unwrap_or_default());
        let dict = |pairs: &[(&CStr, &AnyObject)]| -> Retained<NSDictionary<NSString, AnyObject>> {
            let keys: Vec<Retained<NSString>> = pairs.iter().map(|(k, _)| key(k)).collect();
            let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let values: Vec<&AnyObject> = pairs.iter().map(|(_, v)| *v).collect();
            NSDictionary::from_slices(&keys, &values)
        };
        let yes = NSNumber::new_bool(true);
        let no = NSNumber::new_bool(false);
        let sub_tap = dict(&[(kAudioSubTapUIDKey, &tap_uid), (kAudioSubTapDriftCompensationKey, &yes)]);
        let taps = NSArray::from_retained_slice(&[sub_tap]);
        let name = NSString::from_str("Mostalgia System Audio");
        let uid = NSUUID::new().UUIDString();
        let mut pairs: Vec<(&CStr, &AnyObject)> = vec![
            (kAudioAggregateDeviceNameKey, &name),
            (kAudioAggregateDeviceUIDKey, &uid),
            (kAudioAggregateDeviceIsPrivateKey, &yes),
            (kAudioAggregateDeviceIsStackedKey, &no),
            (kAudioAggregateDeviceTapAutoStartKey, &yes),
            (kAudioAggregateDeviceTapListKey, &taps),
        ];
        let subdevices = output_uid.as_ref().map(|out| NSArray::from_retained_slice(&[dict(&[(kAudioSubDeviceUIDKey, &**out)])]));
        if let (Some(out), Some(subs)) = (&output_uid, &subdevices) {
            pairs.push((kAudioAggregateDeviceMainSubDeviceKey, &**out));
            pairs.push((kAudioAggregateDeviceSubDeviceListKey, &**subs));
        }
        let description = dict(&pairs);
        let mut aggregate: AudioObjectID = 0;
        // SAFETY: NSDictionary is toll-free bridged to CFDictionary.
        let status = unsafe {
            let cf: &CFDictionary = &*Retained::as_ptr(&description).cast::<CFDictionary>();
            AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut aggregate))
        };
        anyhow::ensure!(status == 0 && aggregate != 0, "creating the aggregate device failed (OSStatus {status})");
        this.aggregate = aggregate;

        this.sender = Box::into_raw(Box::new(tx));
        let mut proc_id: AudioDeviceIOProcID = None;
        let status = unsafe { AudioDeviceCreateIOProcID(aggregate, Some(io_proc), this.sender.cast(), NonNull::from(&mut proc_id)) };
        anyhow::ensure!(status == 0 && proc_id.is_some(), "registering the audio callback failed (OSStatus {status})");
        this.proc_id = proc_id;
        let status = unsafe { AudioDeviceStart(aggregate, proc_id) };
        anyhow::ensure!(status == 0, "starting the tap failed (OSStatus {status})");
        Ok(this)
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            if self.aggregate != 0 {
                if self.proc_id.is_some() {
                    AudioDeviceStop(self.aggregate, self.proc_id);
                    AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
                }
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            (self.destroy_tap)(self.tap);
            if !self.sender.is_null() {
                drop(Box::from_raw(self.sender));
            }
        }
    }
}

/// The aggregate device's IO callback (Core Audio's real-time thread):
/// mixes the input down to mono and passes it on. Never blocks.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let tx = unsafe { &*client.cast::<Sender<Vec<f32>>>() };
    let mono = unsafe { mix_down(input.as_ptr()) };
    if !mono.is_empty() {
        let _ = tx.try_send(mono);
    }
    0
}

/// All channels of all buffers of a float32 buffer list, averaged to mono.
unsafe fn mix_down(list: *const AudioBufferList) -> Vec<f32> {
    let count = unsafe { (*list).mNumberBuffers } as usize;
    let buffers: &[AudioBuffer] = unsafe { std::slice::from_raw_parts((&raw const (*list).mBuffers).cast::<AudioBuffer>(), count) };
    let mut out: Vec<f32> = Vec::new();
    let mut channels = 0usize;
    for b in buffers {
        let ch = b.mNumberChannels as usize;
        if b.mData.is_null() || ch == 0 {
            continue;
        }
        let data = unsafe { std::slice::from_raw_parts(b.mData.cast::<f32>(), b.mDataByteSize as usize / 4) };
        mix_into(&mut out, data, ch);
        channels += ch;
    }
    if channels > 1 {
        let scale = 1.0 / channels as f32;
        out.iter_mut().for_each(|s| *s *= scale);
    }
    out
}

/// Adds the frames of interleaved `data` (`ch` channels) to `out`, summed.
fn mix_into(out: &mut Vec<f32>, data: &[f32], ch: usize) {
    let frames = data.len() / ch;
    if out.len() < frames {
        out.resize(frames, 0.0);
    }
    for (o, frame) in out.iter_mut().zip(data.chunks_exact(ch)) {
        *o += frame.iter().sum::<f32>();
    }
}

fn address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain }
}

/// A fixed-size property of an audio object (global scope).
pub(crate) fn get_property<T: Copy, Q>(object: AudioObjectID, selector: AudioObjectPropertySelector, qualifier: Option<&Q>) -> Result<T> {
    let addr = address(selector);
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    let mut size = std::mem::size_of::<T>() as u32;
    let (q_size, q_ptr) = match qualifier {
        Some(q) => (std::mem::size_of::<Q>() as u32, (q as *const Q).cast::<c_void>()),
        None => (0, std::ptr::null()),
    };
    let status = unsafe {
        AudioObjectGetPropertyData(object, NonNull::from(&addr), q_size, q_ptr, NonNull::from(&mut size), NonNull::new_unchecked(value.as_mut_ptr().cast()))
    };
    anyhow::ensure!(status == 0, "audio property {selector:#x} of object {object}: OSStatus {status}");
    Ok(unsafe { value.assume_init() })
}

/// A list property (e.g. the process objects) of an audio object.
pub(crate) fn get_list<T: Copy>(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Result<Vec<T>> {
    let addr = address(selector);
    let mut size = 0u32;
    let status = unsafe { AudioObjectGetPropertyDataSize(object, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size)) };
    anyhow::ensure!(status == 0, "audio property {selector:#x} size: OSStatus {status}");
    let n = size as usize / std::mem::size_of::<T>();
    let mut items: Vec<T> = Vec::with_capacity(n);
    if n == 0 {
        return Ok(items);
    }
    let status = unsafe {
        AudioObjectGetPropertyData(object, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::new_unchecked(items.as_mut_ptr().cast()))
    };
    anyhow::ensure!(status == 0, "audio property {selector:#x}: OSStatus {status}");
    unsafe { items.set_len((size as usize / std::mem::size_of::<T>()).min(n)) };
    Ok(items)
}

/// A CFString property (returned +1) as an NSString.
pub(crate) fn get_string(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Option<Retained<NSString>> {
    let ptr: *mut NSString = get_property::<*mut NSString, ()>(object, selector, None).ok()?;
    // SAFETY: Core Audio hands out a retained CFString (toll-free bridged).
    unsafe { Retained::from_raw(ptr) }
}

fn default_output_uid() -> Option<Retained<NSString>> {
    let device: AudioObjectID = get_property(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyDefaultOutputDevice, None::<&()>).ok()?;
    (device != 0).then(|| get_string(device, kAudioDevicePropertyDeviceUID)).flatten()
}

// ---------------------------------------------------------------------------
// ScreenCaptureKit (macOS 13+)

struct OutputIvars {
    tx: Sender<Vec<f32>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[name = "MostalgiaSystemAudioOutput"]
    #[ivars = OutputIvars]
    struct AudioOutput;

    unsafe impl NSObjectProtocol for AudioOutput {}

    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            let mono = unsafe { sample_buffer_mono(buffer) };
            if !mono.is_empty() {
                let _ = self.ivars().tx.try_send(mono);
            }
        }
    }
);

impl AudioOutput {
    fn new(tx: Sender<Vec<f32>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { tx });
        unsafe { msg_send![super(this), init] }
    }
}

/// A sample buffer's audio (float32), mixed to mono.
unsafe fn sample_buffer_mono(buffer: &CMSampleBuffer) -> Vec<f32> {
    // Room for an AudioBufferList with up to 8 buffers, 16-byte aligned.
    let mut storage = [0u128; 16];
    let list = storage.as_mut_ptr().cast::<AudioBufferList>();
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    let status = unsafe {
        buffer.audio_buffer_list_with_retained_block_buffer(
            std::ptr::null_mut(),
            list,
            std::mem::size_of_val(&storage),
            None,
            None,
            1, // kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment
            &mut block,
        )
    };
    if status != 0 {
        return Vec::new();
    }
    let mono = unsafe { mix_down(list) };
    // The block buffer backs the list's data; release it after reading.
    if let Some(block) = NonNull::new(block) {
        drop(unsafe { CFRetained::from_raw(block) });
    }
    mono
}

struct Sck {
    rate: f64,
    stream: Retained<SCStream>,
    _output: Retained<AudioOutput>,
}

/// Waits for a completion handler's result.
fn wait<T>(rx: &Receiver<T>, what: &str) -> Result<T> {
    rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow::anyhow!("{what}: no answer from ScreenCaptureKit"))
}

fn ns_error(e: *mut NSError) -> Option<String> {
    (!e.is_null()).then(|| unsafe { (*e).localizedDescription() }.to_string())
}

impl Sck {
    fn start(tx: Sender<Vec<f32>>) -> Result<Self> {
        anyhow::ensure!(AnyClass::get(c"SCStream").is_some(), "ScreenCaptureKit needs macOS 13 or newer");
        // The displays (asks for the screen recording permission).
        let (content_tx, content_rx) = crossbeam_channel::bounded::<Result<Retained<SCShareableContent>, String>>(1);
        let handler = block2::RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = match (unsafe { Retained::retain(content) }, ns_error(error)) {
                (Some(c), _) => Ok(c),
                (None, e) => Err(e.unwrap_or_else(|| "no shareable content".into())),
            };
            let _ = content_tx.try_send(result);
        });
        unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
        let content = wait(&content_rx, "listing displays")?
            .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: {e} (is screen recording allowed for Mostalgia?)"))?;
        let displays = unsafe { content.displays() };
        let display = displays.firstObject().context("ScreenCaptureKit: no display")?;

        let filter = unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::<SCWindow>::new()) };
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setCapturesAudio(true);
            config.setSampleRate(SAMPLE_RATE as isize);
            config.setChannelCount(1);
            config.setExcludesCurrentProcessAudio(true);
            // The video can't be switched off: keep it tiny and rare.
            config.setWidth(2);
            config.setHeight(2);
            config.setMinimumFrameInterval(CMTime::new(1, 1));
            config.setShowsCursor(false);
        }
        let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None) };
        let output = AudioOutput::new(tx);
        let queue = dispatch2::DispatchQueue::new("io.github.mostaxd.Mostalgia.system-audio", None);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(objc2::runtime::ProtocolObject::from_ref(&*output), SCStreamOutputType::Audio, Some(&queue))
                .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: {}", e.localizedDescription()))?;
        }
        let (started_tx, started_rx) = crossbeam_channel::bounded::<Option<String>>(1);
        let handler = block2::RcBlock::new(move |error: *mut NSError| {
            let _ = started_tx.try_send(ns_error(error));
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&handler)) };
        if let Some(e) = wait(&started_rx, "starting the capture")? {
            bail!("ScreenCaptureKit: {e} (is screen recording allowed for Mostalgia?)");
        }
        Ok(Self { rate: SAMPLE_RATE as f64, stream, _output: output })
    }
}

impl Drop for Sck {
    fn drop(&mut self) {
        let (tx, rx) = crossbeam_channel::bounded::<()>(1);
        let handler = block2::RcBlock::new(move |_error: *mut NSError| {
            let _ = tx.try_send(());
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&handler)) };
        let _ = rx.recv_timeout(Duration::from_secs(3));
    }
}

// ---------------------------------------------------------------------------
// Resampling

/// Streaming resampler for speech: a windowed-sinc low-pass below the
/// target's Nyquist frequency, then linear interpolation.
pub(crate) struct Resampler {
    /// Input samples per output sample.
    step: f64,
    taps: Vec<f32>,
    /// The last `taps.len() - 1` input samples.
    history: Vec<f32>,
    /// Next output position, in input samples from the current chunk's start.
    pos: f64,
    /// The previous chunk's last filtered sample.
    prev: f32,
}

impl Resampler {
    pub(crate) fn new(from: f64, to: f64) -> Self {
        let step = from / to;
        let taps = if step > 1.0 {
            // Cut-off a bit below the target's Nyquist frequency.
            let cutoff = 0.45 / step;
            let n = 64usize;
            let mid = (n - 1) as f64 / 2.0;
            let mut taps: Vec<f32> = (0..n)
                .map(|i| {
                    let x = i as f64 - mid;
                    let sinc = if x == 0.0 { 2.0 * cutoff } else { (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x) };
                    let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()
                        + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
                    (sinc * w) as f32
                })
                .collect();
            let sum: f32 = taps.iter().sum();
            taps.iter_mut().for_each(|t| *t /= sum);
            taps
        } else {
            vec![1.0]
        };
        let history = vec![0.0; taps.len() - 1];
        Self { step, taps, history, pos: 0.0, prev: 0.0 }
    }

    /// Resamples `input`, appending to `out`.
    pub(crate) fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if (self.step - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        let k = self.taps.len();
        let mut padded = Vec::with_capacity(self.history.len() + input.len());
        padded.extend_from_slice(&self.history);
        padded.extend_from_slice(input);
        let filtered: Vec<f32> = (0..input.len())
            .map(|i| padded[i..i + k].iter().zip(self.taps.iter().rev()).map(|(s, t)| s * t).sum())
            .collect();
        self.history.copy_from_slice(&padded[padded.len() - (k - 1)..]);
        let n = filtered.len();
        // Index -1 is the previous chunk's last sample.
        let at = |j: isize| if j < 0 { self.prev } else { filtered[j as usize] };
        while self.pos < (n - 1) as f64 {
            let i = self.pos.floor();
            let frac = (self.pos - i) as f32;
            let i = i as isize;
            out.push(at(i) * (1.0 - frac) + at(i + 1) * frac);
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = filtered[n - 1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin() as f32 * 0.5).collect()
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt()
    }

    /// 48 kHz in uneven chunks: a third as many samples come out, speech
    /// frequencies keep their level, ones above 8 kHz don't fold back in.
    #[test]
    fn resamples_48k_to_16k_without_aliasing() {
        for (freq, passes) in [(1000.0, true), (3000.0, true), (13000.0, false)] {
            let input = sine(freq, 48_000.0, 48_000);
            let mut r = Resampler::new(48_000.0, 16_000.0);
            let mut out = Vec::new();
            for chunk in input.chunks(487) {
                r.process(chunk, &mut out);
            }
            assert!((out.len() as i64 - 16_000).abs() <= 2, "{} samples", out.len());
            let level = rms(&out[800..]);
            if passes {
                assert!((level - 0.3535).abs() < 0.03, "{freq} Hz at {level}");
            } else {
                assert!(level < 0.02, "{freq} Hz aliased at {level}");
            }
        }
    }

    #[test]
    fn resamples_44k1_and_passes_16k_through() {
        let input = sine(440.0, 44_100.0, 44_100);
        let mut r = Resampler::new(44_100.0, 16_000.0);
        let mut out = Vec::new();
        for chunk in input.chunks(512) {
            r.process(chunk, &mut out);
        }
        assert!((out.len() as i64 - 16_000).abs() <= 2, "{} samples", out.len());
        let mut same = Resampler::new(16_000.0, 16_000.0);
        let mut out = Vec::new();
        same.process(&[0.1, 0.2, 0.3], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn mixes_interleaved_stereo_to_mono() {
        let mut out = Vec::new();
        mix_into(&mut out, &[0.2, 0.4, 1.0, 0.0], 2);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.6).abs() < 1e-6 && (out[1] - 1.0).abs() < 1e-6, "{out:?}");
    }

    /// Nothing arrives for a while (nothing plays): the track still gets
    /// that time, as silence, and blocks have the fixed size.
    #[test]
    fn pump_fills_gaps_with_silence() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64};
        let ctl = CaptureCtl {
            stop: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            muted: Arc::new(AtomicBool::new(false)),
            written: Arc::new(AtomicU64::new(0)),
            join_at: None,
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(vec![0.1; 48_00]).unwrap();
        let stop = ctl.stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1200));
            stop.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
            drop(tx);
        });
        let mut total = 0;
        pump(&rx, 48_000.0, &ctl, |b, _| {
            assert_eq!(b.len(), BLOCK_SAMPLES);
            total += b.len();
            Ok(())
        })
        .unwrap();
        // 0.1 s of audio plus about 1 s of silence.
        assert!(total >= 8 * BLOCK_SAMPLES, "{total}");
    }
}
