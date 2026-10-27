use std::path::Path;

use anyhow::{Context, Result};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

pub struct Segment {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

/// Segments Whisper itself rates as more likely silence than speech are
/// dropped; they are almost always hallucinated text.
pub(crate) const MAX_NO_SPEECH_PROB: f32 = 0.6;

/// A speech-to-text engine: local Whisper on this machine, or a cloud
/// model behind one of the AI connections. Which one is in use is always
/// visible ([`Engine::label`]); a failing cloud engine reports its error
/// and is never swapped for another behind the user's back.
pub enum Engine {
    Local(Whisper),
    Cloud(crate::cloud_stt::CloudEngine),
}

impl Engine {
    /// Local Whisper with the given ggml model.
    pub fn load(model: &Path, language: &str) -> Result<Self> {
        Ok(Self::Local(Whisper::load(model, language)?))
    }

    /// A cloud model on `conn`.
    pub fn cloud(conn: &crate::connections::Connection, model: &str, language: &str) -> Result<Self> {
        Ok(Self::Cloud(crate::cloud_stt::CloudEngine::new(conn, model, language)?))
    }

    pub fn is_cloud(&self) -> bool {
        matches!(self, Self::Cloud(_))
    }

    /// "Whisper large-v3-turbo · RTX …" or "OpenAI · gpt-4o-transcribe".
    pub fn label(&self) -> String {
        match self {
            Self::Local(w) => match w.gpu() {
                Some(gpu) => format!("Whisper large-v3-turbo · {gpu}"),
                None => "Whisper large-v3-turbo · CPU".into(),
            },
            Self::Cloud(c) => c.label(),
        }
    }

    pub fn set_language(&mut self, language: &str) -> Result<()> {
        match self {
            Self::Local(w) => w.set_language(language),
            Self::Cloud(c) => {
                c.set_language(language);
                Ok(())
            }
        }
    }

    /// Forgets the languages detected so far (call before a final pass).
    pub fn reset_detected_language(&mut self) {
        match self {
            Self::Local(w) => w.detected.clear(),
            Self::Cloud(c) => c.reset_detected_language(),
        }
    }

    /// The language heard most since the last reset, when transcribing
    /// with "auto" (a Whisper code like "en").
    pub fn detected_language(&self) -> Option<String> {
        let detected = match self {
            Self::Local(w) => &w.detected,
            Self::Cloud(c) => c.detected(),
        };
        detected.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(code, _)| code.clone())
    }

    /// Description of the discrete GPU in use (local Whisper only).
    pub fn gpu(&self) -> Option<&str> {
        match self {
            Self::Local(w) => w.gpu(),
            Self::Cloud(_) => None,
        }
    }

    /// Transcribes one short utterance, independent of earlier ones.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        match self {
            Self::Local(w) => w.transcribe(samples),
            Self::Cloud(c) => c.transcribe(samples),
        }
    }

    /// Transcribes a whole track in one go (see [`Whisper::transcribe_track`]).
    pub fn transcribe_track(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        match self {
            Self::Local(w) => w.transcribe_track(samples),
            Self::Cloud(c) => c.transcribe(samples),
        }
    }
}

/// Thin wrapper around whisper-rs holding one loaded model and a reusable
/// inference state.
pub struct Whisper {
    state: WhisperState,
    /// Seconds of audio per detected language since the last reset (auto mode).
    detected: Vec<(String, f64)>,
    language: Option<String>,
    threads: i32,
    gpu: Option<String>,
}

impl Whisper {
    pub fn load(model: &Path, language: &str) -> Result<Self> {
        quiet_native_logs();
        let mut params = WhisperContextParameters::default();
        let (gpu_device, gpu) = pick_gpu_device();
        params.gpu_device = gpu_device;
        let ctx = WhisperContext::new_with_params(
            model
                .to_str()
                .context("model path is not valid UTF-8")?,
            params,
        )
        .context("loading Whisper model")?;
        let state = ctx.create_state().context("creating Whisper state")?;
        let language = match language {
            "auto" => None,
            l => Some(l.to_string()),
        };
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(8) as i32)
            .unwrap_or(4);
        Ok(Self {
            state,
            detected: Vec::new(),
            language,
            threads,
            gpu,
        })
    }

    /// Switches the transcription language ("de", "en", "auto").
    pub fn set_language(&mut self, language: &str) -> Result<()> {
        self.language = match language {
            "auto" => None,
            l => Some(l.to_string()),
        };
        Ok(())
    }

    /// Description of the discrete GPU in use, if one was found.
    pub fn gpu(&self) -> Option<&str> {
        self.gpu.as_deref()
    }

    /// Transcribes one short live utterance, independent of earlier ones.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        self.run(samples, false)
    }

    /// Transcribes a whole track in one go, carrying context across
    /// Whisper's 30 s windows. Prefer splitting at speech first (see
    /// `finalize`): on long silences this can skip or invent text.
    pub fn transcribe_track(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        self.run(samples, true)
    }

    fn run(&mut self, samples: &[f32], keep_context: bool) -> Result<Vec<Segment>> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(self.language.as_deref());
        params.set_n_threads(self.threads);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        // Each chunk is an independent utterance; carrying context across
        // chunks makes Whisper repeat itself on noisy boundaries.
        params.set_no_context(!keep_context);

        self.state
            .full(params, samples)
            .context("running Whisper inference")?;

        if self.language.is_none() {
            let id = self.state.full_lang_id_from_state();
            if let Some(code) = whisper_rs::get_lang_str(id) {
                let secs = samples.len() as f64 / crate::SAMPLE_RATE as f64;
                match self.detected.iter_mut().find(|(c, _)| c == code) {
                    Some((_, total)) => *total += secs,
                    None => self.detected.push((code.to_string(), secs)),
                }
            }
        }
        let n = self.state.full_n_segments();
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let Some(seg) = self.state.get_segment(i) else {
                continue;
            };
            let text = seg.to_str_lossy().context("reading segment text")?.into_owned();
            if debug_enabled() {
                eprintln!(
                    "[whisper] {:>7.2}-{:>7.2} no_speech={:.2} {}",
                    seg.start_timestamp() as f64 / 100.0,
                    seg.end_timestamp() as f64 / 100.0,
                    seg.no_speech_probability(),
                    text.trim()
                );
            }
            if text.trim().is_empty() || seg.no_speech_probability() > MAX_NO_SPEECH_PROB || is_annotation(&text) {
                continue;
            }
            out.push(Segment {
                // Whisper timestamps are in centiseconds.
                start_secs: seg.start_timestamp() as f64 / 100.0,
                end_secs: seg.end_timestamp() as f64 / 100.0,
                text,
            });
        }
        Ok(out)
    }
}

/// whisper.cpp's `gpu_device` indexes the GPU and integrated-GPU devices in
/// registration order, and Vulkan often lists the iGPU first on hybrid
/// laptops. Prefer the first discrete GPU; fall back to device 0.
fn pick_gpu_device() -> (i32, Option<String>) {
    use std::ffi::CStr;
    use whisper_rs_sys as sys;

    let mut index = 0;
    // SAFETY: plain queries against ggml's static backend registry.
    unsafe {
        for i in 0..sys::ggml_backend_dev_count() {
            let dev = sys::ggml_backend_dev_get(i);
            let ty = sys::ggml_backend_dev_type(dev);
            if ty == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU {
                let desc = CStr::from_ptr(sys::ggml_backend_dev_description(dev));
                return (index, Some(desc.to_string_lossy().into_owned()));
            }
            if ty == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU {
                index += 1;
            }
        }
    }
    (0, None)
}

/// whisper.cpp and ggml log every model load and VAD call at info level.
/// Keep warnings and errors; set MOSTALGIA_DEBUG=1 to see everything.
pub(crate) fn quiet_native_logs() {
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::Once;
    use std::sync::atomic::{AtomicU32, Ordering};
    use whisper_rs_sys as sys;

    static ONCE: Once = Once::new();
    // Level of the last message, so continuation lines follow their parent.
    static LAST_LEVEL: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn log(level: sys::ggml_log_level, text: *const c_char, _: *mut c_void) {
        let level = if level == sys::ggml_log_level_GGML_LOG_LEVEL_CONT {
            LAST_LEVEL.load(Ordering::Relaxed)
        } else {
            LAST_LEVEL.store(level, Ordering::Relaxed);
            level
        };
        if level >= sys::ggml_log_level_GGML_LOG_LEVEL_WARN && !text.is_null() {
            // SAFETY: ggml passes a NUL-terminated string.
            let text = unsafe { CStr::from_ptr(text) };
            eprint!("{}", text.to_string_lossy());
        }
    }

    if debug_enabled() {
        return;
    }
    ONCE.call_once(|| unsafe {
        sys::whisper_log_set(Some(log), std::ptr::null_mut());
        sys::ggml_log_set(Some(log), std::ptr::null_mut());
    });
}

fn debug_enabled() -> bool {
    std::env::var_os("MOSTALGIA_DEBUG").is_some()
}

/// Whisper's sound annotations ("* Musik *", "[Applaus]", "(Lachen)") are
/// never speech.
pub(crate) fn is_annotation(text: &str) -> bool {
    let t = text.trim();
    [('*', '*'), ('[', ']'), ('(', ')'), ('♪', '♪')]
        .iter()
        .any(|(open, close)| t.len() > 1 && t.starts_with(*open) && t.ends_with(*close))
}
