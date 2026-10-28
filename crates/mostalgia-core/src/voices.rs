//! Known voices: when a speaker on the system track gets a name in a
//! transcript ("Sprecher 2" → "Markus"), a voice print of their lines is
//! kept here, and later meetings label that voice "Markus" by themselves
//! (final pass and, approximately, live).
//!
//! Stored in `voices.json` in the data directory: per name, up to
//! [`MAX_SAMPLES`] voice prints (speaker embeddings, one per meeting they
//! were learned from). A voice matches by its closest print, so the same
//! person on a headset and on a laptop mic can both be recognised.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::SAMPLE_RATE;
use crate::audio::Source;

/// Prints kept per voice; the oldest goes first.
const MAX_SAMPLES: usize = 12;
/// Speech used for one print (more doesn't make it better, only slower).
const MAX_LEARN_SECS: f64 = 90.0;
/// Below this, a print is too unreliable to keep.
pub const MIN_LEARN_SECS: f64 = 4.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Voices {
    pub voices: Vec<KnownVoice>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnownVoice {
    pub name: String,
    pub samples: Vec<VoiceSample>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceSample {
    /// The meeting it was learned from (renaming there again replaces it).
    pub meeting_id: Option<i64>,
    /// Seconds of speech it was taken from.
    pub secs: f64,
    /// Unix time it was learned.
    pub learned_at: i64,
    /// L2-normalized speaker embedding.
    pub embedding: Vec<f32>,
}

impl KnownVoice {
    /// Total seconds of speech this voice was learned from.
    pub fn secs(&self) -> f64 {
        self.samples.iter().map(|s| s.secs).sum()
    }

    /// When it was last learned (Unix time).
    pub fn learned_at(&self) -> i64 {
        self.samples.iter().map(|s| s.learned_at).max().unwrap_or(0)
    }

    fn similarity(&self, emb: &[f32]) -> f32 {
        self.samples.iter().map(|s| dot(&s.embedding, emb)).fold(f32::MIN, f32::max)
    }
}

/// Generic labels that aren't a person: never learned as a voice.
pub fn is_generic_label(name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() || name == "Ich" || name == "Andere" {
        return true;
    }
    [Source::Mic, Source::System].iter().any(|s| crate::i18n::speaker(s.speaker_label()) == name)
        || ["Sprecher ", "Speaker "] // i18n: not UI
            .iter()
            .any(|p| name.strip_prefix(p).is_some_and(|n| n.parse::<usize>().is_ok()))
}

impl Voices {
    pub fn path() -> PathBuf {
        crate::paths::data_dir().join("voices.json")
    }

    /// The known voices (none if the file is missing or unreadable).
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("creating data directory")?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string(self)?).context("writing voices")?;
        std::fs::rename(&tmp, &path).context("saving voices")?;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.voices.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&KnownVoice> {
        self.voices.iter().find(|v| v.name == name)
    }

    /// The known voice closest to `embedding` (normalized) if it is at
    /// least `threshold` similar, with its similarity.
    pub fn best_match(&self, embedding: &[f32], threshold: f32) -> Option<(&str, f32)> {
        self.voices
            .iter()
            .map(|v| (v.name.as_str(), v.similarity(embedding)))
            .filter(|(_, sim)| *sim >= threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Adds a print for `name` (replacing one from the same meeting).
    pub fn learn(&mut self, name: &str, meeting_id: Option<i64>, embedding: Vec<f32>, secs: f64) {
        let sample = VoiceSample {
            meeting_id,
            secs,
            learned_at: Local::now().timestamp(),
            embedding: normalize(embedding),
        };
        let voice = match self.voices.iter().position(|v| v.name == name) {
            Some(i) => &mut self.voices[i],
            None => {
                self.voices.push(KnownVoice { name: name.to_string(), samples: Vec::new() });
                self.voices.last_mut().unwrap()
            }
        };
        if meeting_id.is_some() {
            voice.samples.retain(|s| s.meeting_id != meeting_id);
        }
        voice.samples.push(sample);
        if voice.samples.len() > MAX_SAMPLES {
            voice.samples.remove(0);
        }
    }

    /// Drops what `name` learned from one meeting (it was the wrong
    /// person there); a voice left without prints is forgotten.
    pub fn unlearn_meeting(&mut self, name: &str, meeting_id: i64) -> bool {
        let Some(voice) = self.voices.iter_mut().find(|v| v.name == name) else { return false };
        let before = voice.samples.len();
        voice.samples.retain(|s| s.meeting_id != Some(meeting_id));
        let changed = voice.samples.len() != before;
        self.voices.retain(|v| !v.samples.is_empty());
        changed
    }

    /// Forgets a voice entirely.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.voices.len();
        self.voices.retain(|v| v.name != name);
        self.voices.len() != before
    }

    /// Renames a voice; merges into `to` if that name is known already.
    pub fn rename(&mut self, from: &str, to: &str) -> bool {
        let to = to.trim();
        if to.is_empty() || from == to || is_generic_label(to) {
            return false;
        }
        let Some(i) = self.voices.iter().position(|v| v.name == from) else { return false };
        let moved = self.voices.remove(i);
        match self.voices.iter_mut().find(|v| v.name == to) {
            Some(existing) => {
                existing.samples.extend(moved.samples);
                existing.samples.sort_by_key(|s| s.learned_at);
                let extra = existing.samples.len().saturating_sub(MAX_SAMPLES);
                existing.samples.drain(..extra);
            }
            None => self.voices.push(KnownVoice { name: to.to_string(), ..moved }),
        }
        true
    }
}

/// What [`learn_from_meeting`] did.
#[derive(Debug, PartialEq)]
pub enum Learned {
    /// A voice print of `secs` seconds was stored for the name.
    Voice { secs: f64 },
    /// The name had only lines from the microphone (that's you).
    NotOnSystemTrack,
    /// Too little of their speech to tell the voice by.
    TooShort { secs: f64 },
    /// Nothing to learn (generic label, or the name was cleared).
    Nothing,
}

/// After a speaker was renamed from `previous` to `name` in a meeting:
/// learns `name`'s voice from all their lines on the system track there,
/// and drops what `previous` had learned from this meeting (it was someone
/// else). An empty or generic `name` only does the latter.
pub fn learn_from_meeting(meeting_id: i64, previous: &str, name: &str) -> Result<Learned> {
    let name = name.trim();
    let mut voices = Voices::load();
    let mut changed = previous != name && voices.unlearn_meeting(previous, meeting_id);
    let result = if is_generic_label(name) {
        Learned::Nothing
    } else {
        learn_voice(meeting_id, name, &mut voices, &mut changed)?
    };
    if changed {
        voices.save()?;
    }
    Ok(result)
}

fn learn_voice(meeting_id: i64, name: &str, voices: &mut Voices, changed: &mut bool) -> Result<Learned> {
    let store = crate::store::Store::open(&crate::paths::database())?;
    let meeting = store.get(meeting_id)?.context(crate::i18n::tr("Besprechung nicht gefunden"))?;
    let spans: Vec<(f64, f64)> = meeting
        .segments
        .iter()
        .filter(|s| s.source == Source::System && s.speaker_label() == name)
        .map(|s| (s.start_secs, s.end_secs))
        .collect();
    if spans.is_empty() {
        return Ok(Learned::NotOnSystemTrack);
    }
    let path = meeting.dir.join(Source::System.file_name());
    anyhow::ensure!(path.exists(), "{}", crate::i18n::tr("Die Audiodateien dieser Besprechung gibt es nicht mehr"));
    let track = crate::finalize::read_wav(&path)?;
    let audio = gather(&track, &spans, MAX_LEARN_SECS);
    let secs = audio.len() as f64 / SAMPLE_RATE as f64;
    if secs < MIN_LEARN_SECS {
        return Ok(Learned::TooShort { secs });
    }
    let embedding = crate::diarize::voice_print(&audio)?;
    voices.learn(name, Some(meeting_id), embedding, secs);
    *changed = true;
    Ok(Learned::Voice { secs })
}

/// The audio of `spans` (seconds) joined together, longest spans first,
/// up to `max_secs`.
pub(crate) fn gather(track: &[f32], spans: &[(f64, f64)], max_secs: f64) -> Vec<f32> {
    let mut spans = spans.to_vec();
    spans.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
    let max = (max_secs * SAMPLE_RATE as f64) as usize;
    let at = |t: f64| ((t.max(0.0) * SAMPLE_RATE as f64) as usize).min(track.len());
    let mut out = Vec::new();
    for (start, end) in spans {
        let (a, b) = (at(start), at(end));
        if b <= a {
            continue;
        }
        out.extend_from_slice(&track[a..b.min(a + max - out.len())]);
        if out.len() >= max {
            break;
        }
    }
    out
}

pub(crate) fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = dot(&v, &v).sqrt().max(1e-9);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emb(x: f32, y: f32) -> Vec<f32> {
        vec![x, y, 0.0]
    }

    #[test]
    fn matches_closest_print_above_threshold() {
        let mut v = Voices::default();
        v.learn("Markus", Some(1), emb(1.0, 0.0), 10.0);
        v.learn("Markus", Some(2), emb(0.0, 1.0), 10.0);
        v.learn("Lena", Some(1), emb(-1.0, 0.0), 10.0);
        assert_eq!(v.best_match(&normalize(emb(0.1, 1.0)), 0.5).map(|m| m.0), Some("Markus"));
        assert_eq!(v.best_match(&normalize(emb(-1.0, 0.1)), 0.5).map(|m| m.0), Some("Lena"));
        assert_eq!(v.best_match(&normalize(emb(-1.0, -1.0)), 0.9), None);
    }

    #[test]
    fn relearning_a_meeting_replaces_its_print() {
        let mut v = Voices::default();
        v.learn("Markus", Some(1), emb(1.0, 0.0), 10.0);
        v.learn("Markus", Some(1), emb(0.0, 1.0), 20.0);
        assert_eq!(v.get("Markus").unwrap().samples.len(), 1);
        assert_eq!(v.get("Markus").unwrap().secs(), 20.0);
        for m in 0..20 {
            v.learn("Markus", Some(100 + m), emb(1.0, 0.0), 1.0);
        }
        assert_eq!(v.get("Markus").unwrap().samples.len(), MAX_SAMPLES);
    }

    #[test]
    fn unlearn_rename_and_remove() {
        let mut v = Voices::default();
        v.learn("Markus", Some(1), emb(1.0, 0.0), 10.0);
        v.learn("Jonas", Some(2), emb(0.0, 1.0), 10.0);
        assert!(v.unlearn_meeting("Markus", 1));
        assert!(v.get("Markus").is_none(), "a voice without prints is forgotten");
        assert!(v.rename("Jonas", "Jonas K."));
        assert!(!v.rename("Jonas K.", "Sprecher 3"));
        assert!(v.get("Jonas K.").is_some());
        assert!(v.remove("Jonas K."));
        assert!(v.is_empty());
    }

    #[test]
    fn generic_labels_are_not_names() {
        for l in ["", "Ich", "Andere", "Sprecher 2", "Speaker 10"] {
            assert!(is_generic_label(l), "{l}");
        }
        for l in ["Markus", "Sprecher Müller", "Speaker"] {
            assert!(!is_generic_label(l), "{l}");
        }
    }

    #[test]
    fn gather_takes_longest_spans_up_to_limit() {
        let track: Vec<f32> = (0..SAMPLE_RATE as usize * 10).map(|i| i as f32).collect();
        let out = gather(&track, &[(0.0, 1.0), (2.0, 5.0)], 3.5);
        assert_eq!(out.len(), SAMPLE_RATE as usize * 7 / 2);
        assert_eq!(out[0], (2 * SAMPLE_RATE) as f32);
    }
}
