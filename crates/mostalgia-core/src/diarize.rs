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
/// `MOSTALGIA_VOICE_MATCH` overrides it.
const KNOWN_MATCH: f32 = 0.6;
/// …and by how much it must beat the next closest known voice: prints of
/// different people over call audio can be 0.8 alike, so a speaker who is
/// about as close to two names gets neither.
const KNOWN_MARGIN: f32 = 0.08;
/// Live: the same, for a voice heard only in short chunks so far.
const LIVE_KNOWN_MATCH: f32 = 0.65;
/// Final pass: speech per speaker used to compare with known voices.
const MATCH_SECS: f64 = 60.0;
/// Final pass: a "speaker" with less speech than this is a fragment of
/// someone else (a laugh, crosstalk, a cough) and goes to the closest real
/// one.
const TINY_CLUSTER_SECS: f64 = 10.0;
/// Final pass: the clustering works on a few seconds at a time and splits
/// one person into several speakers; speakers whose voice prints (up to a
/// minute each) are this alike are one person. Measured on call audio:
/// same person 0.85–0.97, different people 0.65–0.81.
/// `MOSTALGIA_SAME_VOICE` overrides it.
const SAME_VOICE: f32 = 0.85;

pub fn speaker_name(index: usize) -> String {
    format!("Sprecher {}", index + 1)
}

/// How many people a track holds, if the user said.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Speakers {
    /// The clustering threshold decides.
    #[default]
    Auto,
    Exactly(usize),
    AtLeast(usize),
}

/// The "people in the call" choices: 0 = auto, 2…4, and 5 for "5 or more".
pub const PEOPLE_CHOICES: [u8; 5] = [0, 2, 3, 4, 5];

impl Speakers {
    /// The system track's speakers for a call of `people` (a
    /// [`PEOPLE_CHOICES`] value, counting you), when you are on the
    /// microphone track (`mic`) and everyone else on the system track.
    pub fn on_system_track(people: u8, mic: bool) -> Self {
        let others = |n: u8| (n as usize).saturating_sub(mic as usize).max(1);
        match people {
            0 | 1 => Speakers::Auto,
            5.. => Speakers::AtLeast(others(5)),
            n => Speakers::Exactly(others(n)),
        }
    }
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

/// One stretch of one speaker's speech (seconds).
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub start: f32,
    pub end: f32,
    pub speaker: i32,
}

/// A track split into speakers.
#[derive(Debug, Default)]
pub struct Separation {
    /// Speakers numbered 0.. in order of first appearance.
    pub turns: Vec<Turn>,
    /// Speakers the clustering found, before fragments of one person were
    /// joined.
    pub found: usize,
    /// Per speaker: seconds of speech…
    pub secs: Vec<f64>,
    /// …voice print (none with too little speech)…
    pub prints: Vec<Option<Vec<f32>>>,
    /// …and the known voice it is, with the similarity.
    pub names: Vec<Option<(String, f32)>>,
}

impl Separation {
    /// The label of speaker `idx`: its known name, or "Sprecher N" with
    /// unknown voices numbered among themselves.
    pub fn label(&self, idx: usize) -> String {
        match &self.names[idx] {
            Some((name, _)) => name.clone(),
            None => speaker_name(self.names[..idx].iter().filter(|n| n.is_none()).count()),
        }
    }
}

/// [`SAME_VOICE`] for a clustering threshold: the "Trennschärfe" setting
/// ("Mehr Sprecher" 0.4 … "Weniger" 0.6) moves it too, so it keeps meaning
/// what it says (0.4 → 0.88, 0.5 → 0.85, 0.6 → 0.82).
fn same_voice(cluster_threshold: f32) -> f32 {
    (SAME_VOICE + (CLUSTER_THRESHOLD - cluster_threshold) * 0.3).clamp(0.7, 0.95)
}

fn cluster_threshold() -> f32 {
    std::env::var("MOSTALGIA_DIARIZE_THRESHOLD").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        let configured = crate::config::Config::load().diarize_threshold;
        if configured > 0.0 { configured } else { CLUSTER_THRESHOLD }
    })
}

/// Splits a track into speakers and names the ones that are `known`
/// voices (each name at most once).
#[cfg(feature = "diarization")]
pub fn separate(samples: &[f32], speakers: Speakers, known: &crate::voices::Voices) -> Result<Separation> {
    let (seg_model, emb_model) = models().ok_or_else(|| anyhow!(crate::i18n::tr("Modelle für die Sprechertrennung fehlen — scripts/download-model.sh ausführen")))?;
    let threshold = cluster_threshold();
    let same_voice = std::env::var("MOSTALGIA_SAME_VOICE").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| same_voice(threshold));
    let mut extractor = extractor(&emb_model, 4)?;
    let mut turns = run_diarization(&seg_model, &emb_model, threshold, -1, samples)?;
    let found = renumber(&mut turns);
    if found == 0 {
        return Ok(Separation::default());
    }
    let prints = speaker_prints(&turns, found, samples, &mut extractor);
    let secs = speaker_secs(&turns, found);
    let (exactly, at_least) = match speakers {
        Speakers::Auto => (None, 1),
        Speakers::Exactly(n) => (Some(n.max(1)), n.max(1)),
        Speakers::AtLeast(n) => (None, n.max(1)),
    };
    let plan = merge_plan(&secs, &prints, |a, b| time_gap(&turns, a, b), exactly, at_least, same_voice);
    let count = if plan.is_empty() {
        // Fewer real voices than asked for: let the clustering split into
        // exactly that many instead.
        let n = exactly.unwrap_or(at_least) as i32;
        turns = run_diarization(&seg_model, &emb_model, threshold, n, samples)?;
        renumber(&mut turns)
    } else {
        for t in turns.iter_mut() {
            t.speaker = plan[t.speaker as usize] as i32;
        }
        renumber(&mut turns)
    };
    let prints = speaker_prints(&turns, count, samples, &mut extractor);
    let secs = speaker_secs(&turns, count);
    let names = if known.is_empty() {
        vec![None; count]
    } else {
        let threshold = std::env::var("MOSTALGIA_VOICE_MATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(KNOWN_MATCH);
        known.assign(&prints, threshold, KNOWN_MARGIN)
    };
    let sep = Separation { turns, found, secs, prints, names };
    if std::env::var_os("MOSTALGIA_DEBUG").is_some() {
        for t in &sep.turns {
            eprintln!("[diarize] {:>7.2}-{:>7.2} speaker {}", t.start, t.end, t.speaker);
        }
        eprintln!("[diarize] clustering found {found} speakers, {count} after merging");
        for i in 0..count {
            eprintln!("[diarize] speaker {i}: {:.1}s, known voice {:?}", sep.secs[i], sep.names[i]);
        }
    }
    Ok(sep)
}

fn speaker_secs(turns: &[Turn], count: usize) -> Vec<f64> {
    let mut secs = vec![0.0; count];
    for t in turns {
        if let Some(s) = secs.get_mut(t.speaker as usize) {
            *s += (t.end - t.start).max(0.0) as f64;
        }
    }
    secs
}

/// Seconds between the nearest turns of speakers `a` and `b`.
fn time_gap(turns: &[Turn], a: usize, b: usize) -> f32 {
    let of = |s: usize| turns.iter().filter(move |t| t.speaker as usize == s);
    of(a).flat_map(|t| of(b).map(move |u| (u.start - t.end).max(t.start - u.end).max(0.0))).fold(f32::MAX, f32::min)
}

/// Labels `segments` (lines from one track whose audio is `samples`) by
/// speaker. Returns the number of speakers found; with only one that isn't
/// a known voice, the lines are left unlabelled.
#[cfg(feature = "diarization")]
pub fn label_track(samples: &[f32], segments: &mut [&mut FinalSegment], speakers: Speakers) -> Result<usize> {
    if segments.is_empty() {
        return Ok(0);
    }
    let sep = separate(samples, speakers, &crate::voices::Voices::load())?;
    let count = sep.secs.len();
    if count < 2 && sep.names.iter().all(Option::is_none) {
        return Ok(count);
    }
    for seg in segments.iter_mut() {
        // The speaker who talks most during this line.
        let mut best = (0.0f64, None);
        for t in &sep.turns {
            let overlap = (seg.end_secs.min(t.end as f64) - seg.start_secs.max(t.start as f64)).max(0.0);
            if overlap > best.0 {
                best = (overlap, Some(t.speaker));
            }
        }
        if let Some(spk) = best.1 {
            seg.speaker = Some(sep.label(spk as usize));
        }
    }
    Ok(count)
}

/// Renumbers speakers 0.. in order of first appearance; returns how many.
fn renumber(turns: &mut [Turn]) -> usize {
    let mut order: Vec<i32> = Vec::new();
    for t in turns.iter_mut() {
        let idx = match order.iter().position(|s| *s == t.speaker) {
            Some(i) => i,
            None => {
                order.push(t.speaker);
                order.len() - 1
            }
        };
        t.speaker = idx as i32;
    }
    order.len()
}

/// A voice print per speaker (0..count) from up to a minute of their
/// speech; none for a speaker with too little of it.
#[cfg(feature = "diarization")]
fn speaker_prints(turns: &[Turn], count: usize, samples: &[f32], extractor: &mut sherpa_rs::speaker_id::EmbeddingExtractor) -> Vec<Option<Vec<f32>>> {
    (0..count as i32)
        .map(|spk| {
            let spans: Vec<(f64, f64)> = turns.iter().filter(|t| t.speaker == spk).map(|t| (t.start as f64, t.end as f64)).collect();
            let audio = crate::voices::gather(samples, &spans, MATCH_SECS);
            if (audio.len() as f64) < LIVE_MIN_SECS * SAMPLE_RATE as f64 {
                return None;
            }
            embed(extractor, &audio)
        })
        .collect()
}

/// Which of the clustered speakers (with `secs` of speech and voice
/// `prints`) are the same person: the speaker each one becomes.
///
/// A tiny speaker (under [`TINY_CLUSTER_SECS`]) goes to the big one whose
/// voice is closest, or, without a print, the one speaking nearest in time
/// (`gap`). Then the two big speakers with the most alike voices are
/// joined, again and again: down to `exactly` speakers when given,
/// otherwise while they are at least `same_voice` alike, but never below
/// `at_least`. Empty when there are fewer big speakers than `at_least`.
fn merge_plan(
    secs: &[f64],
    prints: &[Option<Vec<f32>>],
    gap: impl Fn(usize, usize) -> f32,
    exactly: Option<usize>,
    at_least: usize,
    same_voice: f32,
) -> Vec<usize> {
    use crate::voices::{dot, normalize};
    let count = secs.len();
    let print = |s: usize| prints.get(s).and_then(Option::as_ref);
    let big: Vec<usize> = (0..count).filter(|&s| secs[s] >= TINY_CLUSTER_SECS && print(s).is_some()).collect();
    if big.len() < at_least.max(1) {
        // Nothing big at all and nothing asked for: keep the clustering's.
        return if big.is_empty() && at_least <= 1 && exactly.is_none() { (0..count).collect() } else { Vec::new() };
    }
    let mut target: Vec<usize> = (0..count).collect();
    for s in (0..count).filter(|s| !big.contains(s)) {
        let by_voice = print(s).and_then(|p| {
            big.iter().map(|&b| (b, dot(p, print(b).unwrap()))).max_by(|a, b| a.1.total_cmp(&b.1)).map(|(b, _)| b)
        });
        target[s] = by_voice.unwrap_or_else(|| *big.iter().min_by(|a, b| gap(s, **a).total_cmp(&gap(s, **b))).unwrap());
    }
    // Groups of big speakers, each with its speech-weighted mean print.
    let mut groups: Vec<(Vec<usize>, Vec<f32>)> = big
        .iter()
        .map(|&b| (vec![b], print(b).unwrap().iter().map(|x| x * secs[b] as f32).collect()))
        .collect();
    let goal = exactly.unwrap_or(at_least).max(1);
    while groups.len() > goal {
        let mut best = (0, 1, f32::MIN);
        for i in 0..groups.len() {
            for j in i + 1..groups.len() {
                let sim = dot(&normalize(groups[i].1.clone()), &normalize(groups[j].1.clone()));
                if sim > best.2 {
                    best = (i, j, sim);
                }
            }
        }
        if exactly.is_none() && best.2 < same_voice {
            break;
        }
        let (members, sum) = groups.remove(best.1);
        groups[best.0].0.extend(members);
        groups[best.0].1.iter_mut().zip(sum).for_each(|(a, b)| *a += b);
    }
    for (members, _) in &groups {
        let head = members[0];
        for t in target.iter_mut() {
            if members.contains(t) {
                *t = head;
            }
        }
    }
    target
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

/// sherpa-onnx offline diarization. Called through the C API directly
/// because sherpa-rs fixes it to one thread, which makes an hour-long
/// meeting take minutes.
#[cfg(feature = "diarization")]
fn run_diarization(seg_model: &std::path::Path, emb_model: &std::path::Path, threshold: f32, num_clusters: i32, samples: &[f32]) -> Result<Vec<Turn>> {
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
        // Without a number of speakers (-1), the threshold decides.
        clustering: sys::SherpaOnnxFastClusteringConfig { num_clusters, threshold },
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
pub fn label_track(_: &[f32], _: &mut [&mut FinalSegment], _: Speakers) -> Result<usize> {
    Err(anyhow!(crate::i18n::tr("ohne Sprechertrennung gebaut (Feature `diarization`)")))
}

#[cfg(not(feature = "diarization"))]
pub fn separate(_: &[f32], _: Speakers, _: &crate::voices::Voices) -> Result<Separation> {
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
            // A name another live voice has already is not taken again.
            if self.voices[idx].1.is_none() {
                let mean = normalize(self.voices[idx].0.clone());
                let taken: Vec<&str> = self.voices.iter().filter_map(|v| v.1.as_deref()).collect();
                let found = self.known.best_match(&mean, LIVE_KNOWN_MATCH, KNOWN_MARGIN);
                self.voices[idx].1 = found.filter(|(n, _)| !taken.contains(n)).map(|(n, _)| n.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(deg: f32) -> Option<Vec<f32>> {
        let r = deg.to_radians();
        Some(vec![r.cos(), r.sin(), 0.0])
    }

    /// Speakers far apart in time: the gap is their index distance.
    fn gap(a: usize, b: usize) -> f32 {
        (a as f32 - b as f32).abs()
    }

    #[test]
    fn people_in_the_call_to_speakers_on_the_system_track() {
        assert_eq!(Speakers::on_system_track(0, true), Speakers::Auto);
        assert_eq!(Speakers::on_system_track(3, true), Speakers::Exactly(2));
        assert_eq!(Speakers::on_system_track(3, false), Speakers::Exactly(3));
        assert_eq!(Speakers::on_system_track(2, true), Speakers::Exactly(1));
        assert_eq!(Speakers::on_system_track(5, true), Speakers::AtLeast(4));
    }

    #[test]
    fn more_speakers_setting_joins_less() {
        assert!((same_voice(0.5) - SAME_VOICE).abs() < 1e-6);
        assert!(same_voice(0.4) > same_voice(0.5) && same_voice(0.5) > same_voice(0.6));
    }

    #[test]
    fn tiny_clusters_join_the_closest_big_one() {
        // Two people (0 and 1, 0.73 alike as over a call) and fragments:
        // 2 sounds like 1, 3 like 0, 4 is too short for a print and sits
        // next to 1 in time.
        let secs = [200.0, 150.0, 4.0, 7.5, 0.8];
        let prints = [at(0.0), at(43.0), at(40.0), at(5.0), None];
        assert_eq!(merge_plan(&secs, &prints, gap, None, 1, SAME_VOICE), [0, 1, 1, 0, 1]);
    }

    #[test]
    fn one_person_split_by_the_clustering_is_joined_again() {
        // 0 and 2 are the same voice (0.94), 1 someone else.
        let secs = [120.0, 80.0, 60.0];
        let prints = [at(0.0), at(43.0), at(20.0)];
        assert_eq!(merge_plan(&secs, &prints, gap, None, 1, SAME_VOICE), [0, 1, 0]);
        // At least three people: nothing is joined.
        assert_eq!(merge_plan(&secs, &prints, gap, None, 3, SAME_VOICE), [0, 1, 2]);
        // Exactly one person: everyone is joined.
        assert_eq!(merge_plan(&secs, &prints, gap, Some(1), 1, SAME_VOICE), [0, 0, 0]);
        // Exactly two: the most alike are joined, whatever the threshold.
        assert_eq!(merge_plan(&secs, &prints, gap, Some(2), 2, 0.99), [0, 1, 0]);
        // More people asked for than real voices found: clustering again.
        assert!(merge_plan(&secs, &prints, gap, Some(4), 4, SAME_VOICE).is_empty());
        // Nothing big and nothing asked: left as it is.
        assert_eq!(merge_plan(&[2.0, 3.0], &[at(0.0), at(90.0)], gap, None, 1, SAME_VOICE), [0, 1]);
    }

    #[test]
    fn speakers_are_numbered_by_first_appearance() {
        let mut turns = vec![
            Turn { start: 0.0, end: 1.0, speaker: 7 },
            Turn { start: 1.0, end: 2.0, speaker: 3 },
            Turn { start: 2.0, end: 3.0, speaker: 7 },
        ];
        assert_eq!(renumber(&mut turns), 2);
        assert_eq!(turns.iter().map(|t| t.speaker).collect::<Vec<_>>(), [0, 1, 0]);
        assert_eq!(speaker_secs(&turns, 2), [2.0, 1.0]);
    }
}
