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

    /// Languages the meetings are held in (Whisper codes; empty = any).
    /// In auto mode the engine only ever picks one of these.
    pub fn set_meeting_languages(&mut self, languages: &[String]) {
        match self {
            Self::Local(w) => w.allowed = languages.to_vec(),
            Self::Cloud(c) => c.set_meeting_languages(languages),
        }
    }

    /// The configured language is "auto".
    pub fn is_auto(&self) -> bool {
        match self {
            Self::Local(w) => w.language.is_none(),
            Self::Cloud(c) => c.is_auto(),
        }
    }

    /// Probabilities of the meeting languages for `samples` (most likely
    /// first). `None` with a fixed language, or when the engine can't tell
    /// yet: a cloud engine only knows what it reported for earlier audio.
    pub fn detect_language(&mut self, samples: &[f32]) -> Result<Option<crate::language::Probs>> {
        match self {
            Self::Local(w) if w.language.is_none() => w.detect_language(samples).map(Some),
            Self::Local(_) => Ok(None),
            Self::Cloud(c) => Ok(c.reported_probs()),
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
        self.transcribe_as(samples, None)
    }

    /// [`Engine::transcribe`] in `language` (a Whisper code) when the
    /// engine is set to "auto"; a fixed configured language always wins.
    /// `None`: the engine detects it (among the meeting languages).
    pub fn transcribe_as(&mut self, samples: &[f32], language: Option<&str>) -> Result<Vec<Segment>> {
        match self {
            Self::Local(w) => w.run(samples, false, language),
            Self::Cloud(c) => c.transcribe_as(samples, language),
        }
    }

    /// Transcribes a whole track in one go (see [`Whisper::transcribe_track`]).
    pub fn transcribe_track(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        self.transcribe_track_as(samples, None)
    }

    /// [`Engine::transcribe_track`] in `language` (see [`Engine::transcribe_as`]).
    pub fn transcribe_track_as(&mut self, samples: &[f32], language: Option<&str>) -> Result<Vec<Segment>> {
        match self {
            Self::Local(w) => w.run(samples, true, language),
            Self::Cloud(c) => c.transcribe_as(samples, language),
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
    /// Meeting languages auto mode chooses among (empty = any).
    allowed: Vec<String>,
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
            allowed: Vec::new(),
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
        self.run(samples, false, None)
    }

    /// Transcribes a whole track in one go, carrying context across
    /// Whisper's 30 s windows. Prefer splitting at speech first (see
    /// `finalize`): on long silences this can skip or invent text.
    pub fn transcribe_track(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        self.run(samples, true, None)
    }

    /// Probabilities of the meeting languages for `samples`, most likely
    /// first. Whisper looks at 30 s at a time; longer audio is detected per
    /// 30 s window and the results averaged by length.
    pub fn detect_language(&mut self, samples: &[f32]) -> Result<crate::language::Probs> {
        const WINDOW: usize = 30 * crate::SAMPLE_RATE as usize;
        // A last window shorter than this adds nothing but noise.
        const MIN_TAIL: usize = 5 * crate::SAMPLE_RATE as usize;
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        let threads = self.threads.max(1) as usize;
        self.state.pcm_to_mel(samples, threads).context("computing the spectrogram for language detection")?;
        let mut parts = Vec::new();
        let mut offset = 0;
        while offset < samples.len() {
            let len = (samples.len() - offset).min(WINDOW);
            if len < MIN_TAIL && offset > 0 {
                break;
            }
            let offset_ms = offset * 1000 / crate::SAMPLE_RATE as usize;
            let (_, probs) = self.state.lang_detect(offset_ms, threads).context("detecting the language")?;
            parts.push((crate::language::restrict(&probs, &self.allowed), len.max(1) as f64));
            offset += WINDOW;
        }
        let probs = crate::language::combine(&parts);
        if debug_enabled() {
            let top: Vec<String> = probs.iter().take(4).map(|(c, p)| format!("{c} {p:.2}")).collect();
            eprintln!("[whisper] language over {:.1} s: {}", samples.len() as f64 / crate::SAMPLE_RATE as f64, top.join(", "));
        }
        Ok(probs)
    }

    /// `language` applies in auto mode only (a fixed language wins).
    fn run(&mut self, samples: &[f32], keep_context: bool, language: Option<&str>) -> Result<Vec<Segment>> {
        let mut language = self.language.clone().or_else(|| language.map(str::to_string));
        // Auto without a language from the caller: still only one of the
        // meeting languages, never whatever this snippet sounds like most.
        if language.is_none() && !self.allowed.is_empty() && !samples.is_empty() {
            language = self.detect_language(samples)?.first().map(|(c, _)| c.clone());
        }
        let params = full_params(language.as_deref(), self.threads, keep_context);

        self.state
            .full(params, samples)
            .context("running Whisper inference")?;

        if self.language.is_none() {
            let id = self.state.full_lang_id_from_state();
            let heard = language.as_deref().or_else(|| whisper_rs::get_lang_str(id));
            if let Some(code) = heard {
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

/// Whisper's settings for one pass in `language` (`None`: Whisper detects it).
fn full_params(language: Option<&str>, threads: i32, keep_context: bool) -> FullParams<'_, 'static> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(language);
    // Transcribe, never translate: German speech stays German.
    params.set_translate(false);
    params.set_n_threads(threads);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    // Each chunk is an independent utterance; carrying context across
    // chunks makes Whisper repeat itself on noisy boundaries.
    params.set_no_context(!keep_context);
    params
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_translates() {
        for language in [Some("de"), Some("en"), None] {
            let params = format!("{:?}", full_params(language, 4, false));
            assert!(params.contains("translate: false"), "{params}");
            assert!(!params.contains("translate: true"));
        }
    }
}
