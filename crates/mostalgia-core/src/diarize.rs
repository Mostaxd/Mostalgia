//! Telling remote speakers apart (optional): the system-audio track can
//! hold several people, so it's split into "Sprecher 1", "Sprecher 2", …
//!
//! - Final pass: pyannote segmentation + speaker embeddings + clustering
//!   over the whole track (sherpa-onnx). Accurate.
//! - Live: an embedding per chunk, matched against the voices heard so far.
//!   Approximate; the final pass relabels the saved transcript.
//!
//! Both also compare voices with the ones named in earlier meetings
//! ([`crate::voices`]) and use the name when one matches.

use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::finalize::FinalSegment;
use crate::{SAMPLE_RATE, paths};

pub const SEGMENTATION_MODEL: &str = "pyannote-segmentation-3.0.onnx";
pub const EMBEDDING_MODEL: &str = "wespeaker_en_voxceleb_resnet34_LM.onnx";

/// Clustering threshold for the final pass (higher = fewer speakers); the
/// sherpa-onnx default, tuned on real voices. `MOSTALGIA_DIARIZE_THRESHOLD`
/// overrides it (very similar voices, e.g. two TTS voices, need ~0.2).
const CLUSTER_THRESHOLD: f32 = 0.5;
/// Live: cosine similarity at which a chunk counts as a known voice.
const LIVE_MATCH: f32 = 0.55;
/// Live: chunks shorter than this don't carry a reliable voice print.
const LIVE_MIN_SECS: f64 = 1.5;
/// Final pass: cosine similarity at which a speaker is a known voice.
/// Same-person prints from this model are usually 0.6–0.85 apart from
/// different people's at 0.0–0.35. `MOSTALGIA_VOICE_MATCH` overrides it.
const KNOWN_MATCH: f32 = 0.5;
/// Live: the same, for a voice heard only in short chunks so far.
const LIVE_KNOWN_MATCH: f32 = 0.55;
/// Final pass: speech per speaker used to compare with known voices.
const MATCH_SECS: f64 = 60.0;

pub fn speaker_name(index: usize) -> String {
    format!("Sprecher {}", index + 1)
}

fn models() -> Option<(PathBuf, PathBuf)> {
    let seg = paths::model(SEGMENTATION_MODEL);
    let emb = paths::model(EMBEDDING_MODEL);
    (cfg!(feature = "diarization") && seg.exists() && emb.exists()).then_some((seg, emb))
}

/// Whether diarization is built in and its models are downloaded.
pub fn available() -> bool {
    models().is_some()
}

/// Labels `segments` (lines from one track whose audio is `samples`) by
/// speaker. Returns the number of speakers found; with only one, the
/// lines are left unlabelled.
#[cfg(feature = "diarization")]
pub fn label_track(samples: &[f32], segments: &mut [&mut FinalSegment]) -> Result<usize> {
    let (seg_model, emb_model) = models().ok_or_else(|| anyhow!(crate::i18n::tr("Modelle für die Sprechertrennung fehlen — scripts/download-model.sh ausführen")))?;
    if segments.is_empty() {
        return Ok(0);
    }
    let threshold = std::env::var("MOSTALGIA_DIARIZE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            let configured = crate::config::Config::load().diarize_threshold;
            if configured > 0.0 { configured } else { CLUSTER_THRESHOLD }
        });
    let turns = run_diarization(&seg_model, &emb_model, threshold, samples)?;
    if std::env::var_os("MOSTALGIA_DEBUG").is_some() {
        for t in &turns {
            eprintln!("[diarize] {:>7.2}-{:>7.2} speaker {}", t.start, t.end, t.speaker);
        }
    }

    // Number speakers in order of first appearance.
    let mut order: Vec<i32> = Vec::new();
    for t in &turns {
        if !order.contains(&t.speaker) {
            order.push(t.speaker);
        }
    }
    let names = name_speakers(&order, &turns, samples, &emb_model);
    if order.len() < 2 && names.iter().all(Option::is_none) {
        return Ok(order.len());
    }
    for seg in segments.iter_mut() {
        // The speaker who talks most during this line.
        let mut best = (0.0f64, None);
        for t in &turns {
            let overlap = (seg.end_secs.min(t.end as f64) - seg.start_secs.max(t.start as f64)).max(0.0);
            if overlap > best.0 {
                best = (overlap, Some(t.speaker));
            }
        }
        if let Some(spk) = best.1 {
            let idx = order.iter().position(|s| *s == spk).unwrap_or(0);
            seg.speaker = Some(names[idx].clone().unwrap_or_else(|| {
                // Unknown voices are numbered among themselves.
                speaker_name(names[..idx].iter().filter(|n| n.is_none()).count())
            }));
        }
    }
    Ok(order.len())
}

/// The known voice (if any) of each speaker in `order`, from up to a
/// minute of their speech. Failing to compare only loses the names.
#[cfg(feature = "diarization")]
fn name_speakers(order: &[i32], turns: &[Turn], samples: &[f32], emb_model: &std::path::Path) -> Vec<Option<String>> {
    let mut names = vec![None; order.len()];
    let known = crate::voices::Voices::load();
    if known.is_empty() {
        return names;
    }
    let threshold = std::env::var("MOSTALGIA_VOICE_MATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(KNOWN_MATCH);
    let mut extractor = match extractor(emb_model, 4) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[mostalgia] known voices: {e:#}");
            return names;
        }
    };
    let debug = std::env::var_os("MOSTALGIA_DEBUG").is_some();
    for (idx, spk) in order.iter().enumerate() {
        let spans: Vec<(f64, f64)> = turns.iter().filter(|t| t.speaker == *spk).map(|t| (t.start as f64, t.end as f64)).collect();
        let audio = crate::voices::gather(samples, &spans, MATCH_SECS);
        if (audio.len() as f64) < LIVE_MIN_SECS * SAMPLE_RATE as f64 {
            continue;
        }
        let Some(emb) = embed(&mut extractor, &audio) else { continue };
        let found = known.best_match(&emb, threshold);
        if debug {
            eprintln!("[diarize] speaker {spk}: known voice {found:?}");
        }
        names[idx] = found.map(|(name, _)| name.to_string());
    }
    names
}

#[cfg(feature = "diarization")]
fn extractor(emb_model: &std::path::Path, threads: usize) -> Result<sherpa_rs::speaker_id::EmbeddingExtractor> {
    sherpa_rs::speaker_id::EmbeddingExtractor::new(sherpa_rs::speaker_id::ExtractorConfig {
        model: emb_model.to_string_lossy().into_owned(),
        num_threads: Some(threads),
        ..Default::default()
    })
    .map_err(|e| anyhow!("loading speaker model: {e}"))
}

/// A normalized speaker embedding of `audio`.
#[cfg(feature = "diarization")]
fn embed(extractor: &mut sherpa_rs::speaker_id::EmbeddingExtractor, audio: &[f32]) -> Option<Vec<f32>> {
    let emb = extractor.compute_speaker_embedding(audio.to_vec(), SAMPLE_RATE).ok()?;
    Some(crate::voices::normalize(emb))
}

/// A voice print (normalized speaker embedding) of one person's speech,
/// for remembering their voice.
pub fn voice_print(audio: &[f32]) -> Result<Vec<f32>> {
    #[cfg(feature = "diarization")]
    {
        let (_, emb_model) = models().ok_or_else(|| anyhow!(crate::i18n::tr("Modelle für die Sprechertrennung fehlen — scripts/download-model.sh ausführen")))?;
        let mut extractor = extractor(&emb_model, 4)?;
        embed(&mut extractor, audio).ok_or_else(|| anyhow!("computing the voice print failed"))
    }
    #[cfg(not(feature = "diarization"))]
    {
        let _ = audio;
        Err(anyhow!(crate::i18n::tr("ohne Sprechertrennung gebaut (Feature `diarization`)")))
    }
}

#[cfg(feature = "diarization")]
struct Turn {
    start: f32,
    end: f32,
    speaker: i32,
}

/// sherpa-onnx offline diarization. Called through the C API directly
/// because sherpa-rs fixes it to one thread, which makes an hour-long
/// meeting take minutes.
#[cfg(feature = "diarization")]
fn run_diarization(seg_model: &std::path::Path, emb_model: &std::path::Path, threshold: f32, samples: &[f32]) -> Result<Vec<Turn>> {
    use std::ffi::CString;
    use sherpa_rs_sys as sys;

    let threads = std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4) as i32;
    let cstr = |p: &std::path::Path| CString::new(p.to_string_lossy().as_bytes()).expect("path without NUL");
    let (seg, emb, provider) = (cstr(seg_model), cstr(emb_model), CString::new("cpu").unwrap());
    let config = sys::SherpaOnnxOfflineSpeakerDiarizationConfig {
        segmentation: sys::SherpaOnnxOfflineSpeakerSegmentationModelConfig {
            pyannote: sys::SherpaOnnxOfflineSpeakerSegmentationPyannoteModelConfig { model: seg.as_ptr() },
            num_threads: threads,
            debug: 0,
            provider: provider.as_ptr(),
        },
        embedding: sys::SherpaOnnxSpeakerEmbeddingExtractorConfig {
            model: emb.as_ptr(),
            num_threads: threads,
            debug: 0,
            provider: provider.as_ptr(),
        },
        // Let the threshold decide how many speakers there are.
        clustering: sys::SherpaOnnxFastClusteringConfig { num_clusters: -1, threshold },
        min_duration_on: 0.3,
        min_duration_off: 0.5,
    };
    // SAFETY: the config's strings outlive these calls, and every object
    // sherpa returns is destroyed before returning.
    unsafe {
        let sd = sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config);
        if sd.is_null() {
            return Err(anyhow!("loading the diarization models failed"));
        }
        let result = sys::SherpaOnnxOfflineSpeakerDiarizationProcess(sd, samples.as_ptr(), samples.len() as i32);
        let mut turns = Vec::new();
        if !result.is_null() {
            let n = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result);
            let segs = sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result);
            if !segs.is_null() && n > 0 {
                for s in std::slice::from_raw_parts(segs, n as usize) {
                    turns.push(Turn {
                        start: s.start,
                        end: s.end,
                        speaker: s.speaker,
                    });
                }
                sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(segs);
            }
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(result);
        }
        sys::SherpaOnnxDestroyOfflineSpeakerDiarization(sd);
        Ok(turns)
    }
}

#[cfg(not(feature = "diarization"))]
pub fn label_track(_: &[f32], _: &mut [&mut FinalSegment]) -> Result<usize> {
    Err(anyhow!(crate::i18n::tr("ohne Sprechertrennung gebaut (Feature `diarization`)")))
}

/// Live speaker matching for one source.
pub struct LiveSpeakers {
    #[cfg(feature = "diarization")]
    extractor: sherpa_rs::speaker_id::EmbeddingExtractor,
    /// Running mean embedding per speaker (L2-normalized on use), and the
    /// known voice it turned out to be.
    voices: Vec<(Vec<f32>, Option<String>)>,
    /// Voices named in earlier meetings.
    known: crate::voices::Voices,
}

impl LiveSpeakers {
    pub fn new() -> Result<Self> {
        #[cfg(feature = "diarization")]
        {
            let (_, emb_model) = models().ok_or_else(|| anyhow!(crate::i18n::tr("Modelle für die Sprechertrennung fehlen — scripts/download-model.sh ausführen")))?;
            Ok(Self {
                extractor: extractor(&emb_model, 2)?,
                voices: Vec::new(),
                known: crate::voices::Voices::load(),
            })
        }
        #[cfg(not(feature = "diarization"))]
        Err(anyhow!(crate::i18n::tr("ohne Sprechertrennung gebaut (Feature `diarization`)")))
    }

    /// The speaker of a chunk (a known name, or "Sprecher N"), or `None`
    /// when it's too short to tell or only one unknown voice has been heard
    /// so far.
    pub fn identify(&mut self, samples: &[f32]) -> Option<String> {
        if (samples.len() as f64) < LIVE_MIN_SECS * SAMPLE_RATE as f64 {
            return None;
        }
        #[cfg(feature = "diarization")]
        {
            use crate::voices::{dot, normalize};
            let emb = embed(&mut self.extractor, samples)?;
            let best = self
                .voices
                .iter()
                .enumerate()
                .map(|(i, v)| (i, dot(&normalize(v.0.clone()), &emb)))
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let idx = match best {
                Some((i, sim)) if sim >= LIVE_MATCH => {
                    for (v, e) in self.voices[i].0.iter_mut().zip(&emb) {
                        *v += e;
                    }
                    i
                }
                _ => {
                    self.voices.push((emb, None));
                    self.voices.len() - 1
                }
            };
            // An unnamed voice is checked again as its mean gets steadier.
            if self.voices[idx].1.is_none() {
                let mean = normalize(self.voices[idx].0.clone());
                self.voices[idx].1 = self.known.best_match(&mean, LIVE_KNOWN_MATCH).map(|(n, _)| n.to_string());
            }
            if let Some(name) = &self.voices[idx].1 {
                return Some(name.clone());
            }
            let number = self.voices[..idx].iter().filter(|v| v.1.is_none()).count();
            (self.voices.len() > 1).then(|| speaker_name(number))
        }
        #[cfg(not(feature = "diarization"))]
        None
    }
}
