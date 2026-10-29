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
//! Either way, the callback mixes to mono and hands the audio to
//! `audio_pump`, which does what Linux capture does: 16 kHz `BLOCK_SAMPLES`
//! blocks, the WAV track written first, the live transcriber never waited
//! for, silence where the source delivered nothing.

use std::ffi::{CStr, c_void};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender};
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

use crate::SAMPLE_RATE;
use crate::audio::{Block, CaptureCtl, Source, SourceInfo};
use crate::audio_pump::{Opened, RawSink, spawn_pump};

/// Starts capturing the Mac's sound output for the recording, with the
/// contract of `audio::spawn_capture` (through `audio_pump`). `device` is
/// `None` or `audio::DEFAULT_MONITOR`: the tap takes all output.
///
/// Fails right away when neither a process tap nor ScreenCaptureKit can be
/// set up, e.g. without permission.
pub fn spawn_system_capture(
    device: Option<String>,
    wav_path: PathBuf,
    tx: Sender<Block>,
    ctl: CaptureCtl,
) -> Result<JoinHandle<Result<()>>> {
    let _ = device;
    // ScreenCaptureKit sends nothing while nothing plays: fill those gaps.
    spawn_pump(Source::System, wav_path, tx, ctl, true, |raw| {
        let backend = Backend::start(RawSink::new(raw, 1))?;
        Ok(Opened { rate: backend.rate.round() as u32, guard: Box::new(backend) })
    })
}

/// System audio is everything the Mac plays; there is nothing to choose
/// (the device lists offer just the default).
pub fn list_system_sources() -> Result<Vec<SourceInfo>> {
    Ok(Vec::new())
}

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
    fn start(tx: RawSink) -> Result<Self> {
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
    sender: *mut RawSink,
}

impl Tap {
    fn start(tx: RawSink) -> Result<Self> {
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
    let sink = unsafe { &*client.cast::<RawSink>() };
    sink.push_mono(unsafe { mix_down(input.as_ptr()) });
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
    sink: RawSink,
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
            self.ivars().sink.push_mono(unsafe { sample_buffer_mono(buffer) });
        }
    }
);

impl AudioOutput {
    fn new(sink: RawSink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { sink });
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
    fn start(tx: RawSink) -> Result<Self> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixes_interleaved_stereo_to_mono() {
        let mut out = Vec::new();
        mix_into(&mut out, &[0.2, 0.4, 1.0, 0.0], 2);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.6).abs() < 1e-6 && (out[1] - 1.0).abs() < 1e-6, "{out:?}");
    }

    /// The buffer list Core Audio hands the IO callback: two non-interleaved
    /// channels are averaged, a buffer without data is skipped.
    #[test]
    fn mixes_buffer_lists_down() {
        let mut left = [0.5f32, 1.0];
        let mut right = [0.1f32, 0.0];
        #[repr(C)]
        struct List3 {
            n: u32,
            buffers: [AudioBuffer; 3],
        }
        let buf = |data: *mut f32, len: usize| AudioBuffer { mNumberChannels: 1, mDataByteSize: (len * 4) as u32, mData: data.cast() };
        let list = List3 {
            n: 3,
            buffers: [buf(left.as_mut_ptr(), 2), buf(right.as_mut_ptr(), 2), AudioBuffer { mNumberChannels: 1, mDataByteSize: 8, mData: std::ptr::null_mut() }],
        };
        let mono = unsafe { mix_down((&raw const list).cast()) };
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.3).abs() < 1e-6 && (mono[1] - 0.5).abs() < 1e-6, "{mono:?}");
    }

    /// Asks macOS for a tap (or ScreenCaptureKit). On CI it may be refused
    /// (no permission); either way it must answer, not hang or crash.
    #[test]
    fn starting_system_audio_answers() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let started = std::time::Instant::now();
        let result = Backend::start(RawSink::new(tx, 1));
        eprintln!("system audio backend: {:?}", result.as_ref().map(|b| b.rate).map_err(|e| format!("{e:#}")));
        assert!(started.elapsed() < Duration::from_secs(40));
    }
}
