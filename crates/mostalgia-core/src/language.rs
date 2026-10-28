//! Which language a meeting is transcribed in, when it is set to "auto".
//!
//! Whisper detecting the language on every short chunk goes wrong on
//! German speech full of English terms: a chunk with "Deployment" and
//! "Pull Request" in it comes out as "en", and Whisper then writes the
//! German speech down as English (it translates). So detection here runs
//! on a longer stretch of speech, only chooses among the languages the
//! user said their meetings are in, and is locked per track; later checks
//! switch only when another language is clearly more likely.

/// Language probabilities, most likely first, summing to 1 over the
/// candidates (Whisper codes like "de").
pub type Probs = Vec<(String, f32)>;

/// Speech (seconds) a track's language is detected on before it is locked.
pub const LOCK_SECS: f64 = 20.0;
/// The final pass looks at up to this much speech of a track at once.
pub const FINAL_WINDOW_SECS: f64 = 60.0;
/// Once locked, the language is checked again this often (track time)…
pub const RECHECK_SECS: f64 = 120.0;
/// …on the most recent stretch of speech of this length…
pub const RECHECK_WINDOW_SECS: f64 = 20.0;
/// …and switched only if another language is this much more likely.
pub const SWITCH_MARGIN: f32 = 0.5;

/// Languages offered for "Languages spoken in my meetings" (Whisper code,
/// name in that language).
pub const COMMON: [(&str, &str); 18] = [
    ("de", "Deutsch"),
    ("en", "English"),
    ("ar", "العربية"),
    ("fr", "Français"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("nl", "Nederlands"),
    ("pt", "Português"),
    ("pl", "Polski"),
    ("tr", "Türkçe"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("fa", "فارسی"),
    ("hi", "हिन्दी"),
    ("ur", "اردو"),
    ("ja", "日本語"),
    ("zh", "中文"),
    ("ko", "한국어"),
];

/// A language's name in that language for the common ones, else its
/// English name ("Swahili").
pub fn native_name(code: &str) -> String {
    COMMON
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| crate::summary::language_name(code))
}

/// A starting set for "Languages spoken in my meetings": the app's
/// language, what earlier meetings were in (most frequent first), German
/// and English; only languages Whisper knows, each once. (Earlier
/// meetings alone aren't enough: before this fix German calls were often
/// stored as English.)
pub fn suggest(ui_language: &str, history: &[String]) -> Vec<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for code in history {
        match counts.iter_mut().find(|(c, _)| *c == code.as_str()) {
            Some((_, n)) => *n += 1,
            None => counts.push((code.as_str(), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    let mut out: Vec<String> = Vec::new();
    for code in std::iter::once(ui_language).chain(counts.into_iter().map(|(c, _)| c)).chain(["de", "en"]) {
        // ("auto" etc. would make whisper.cpp log an error.)
        let plausible = (2..=3).contains(&code.len()) && code != "auto" && code.chars().all(|c| c.is_ascii_lowercase());
        if plausible && whisper_rs::get_lang_id(code).is_some() && !out.iter().any(|c| c == code) {
            out.push(code.to_string());
        }
    }
    out
}

/// Whisper's probabilities (indexed by language id) restricted to
/// `allowed` and renormalized; all languages when `allowed` is empty.
/// Unknown codes in `allowed` are ignored; if none is known, all count.
pub fn restrict(all: &[f32], allowed: &[String]) -> Probs {
    let code = |id: usize| whisper_rs::get_lang_str(id as i32);
    restrict_with(all, allowed, code)
}

fn restrict_with<'a>(all: &[f32], allowed: &[String], code: impl Fn(usize) -> Option<&'a str>) -> Probs {
    let pick = |only: bool| -> Probs {
        all.iter()
            .enumerate()
            .filter_map(|(id, &p)| Some((code(id)?, p)))
            .filter(|(c, _)| !only || allowed.iter().any(|a| a == c))
            .map(|(c, p)| (c.to_string(), p.max(0.0)))
            .collect()
    };
    let mut probs = pick(!allowed.is_empty());
    if probs.is_empty() {
        probs = pick(false);
    }
    normalize(probs)
}

fn normalize(mut probs: Probs) -> Probs {
    let total: f32 = probs.iter().map(|(_, p)| p).sum();
    if total > 0.0 {
        for (_, p) in &mut probs {
            *p /= total;
        }
    } else if !probs.is_empty() {
        let n = probs.len() as f32;
        for (_, p) in &mut probs {
            *p = 1.0 / n;
        }
    }
    probs.sort_by(|a, b| b.1.total_cmp(&a.1));
    probs
}

/// Weighted average of several detections (e.g. 30 s windows of a longer
/// stretch, weighted by how much speech each held).
pub fn combine(parts: &[(Probs, f64)]) -> Probs {
    let mut out: Probs = Vec::new();
    for (probs, weight) in parts {
        for (code, p) in probs {
            let add = *p * *weight as f32;
            match out.iter_mut().find(|(c, _)| c == code) {
                Some((_, total)) => *total += add,
                None => out.push((code.clone(), add)),
            }
        }
    }
    normalize(out)
}

/// The language to switch to from `current`, if another is more likely by
/// at least `margin`.
pub fn should_switch(current: &str, probs: &Probs, margin: f32) -> Option<String> {
    let (best, p_best) = probs.first()?;
    let p_current = probs.iter().find(|(c, _)| c == current).map_or(0.0, |(_, p)| *p);
    (best != current && p_best - p_current >= margin).then(|| best.clone())
}

/// What a [`LanguageLock`] knows about its track, for display.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// The language used for the latest speech.
    pub language: String,
    /// Detected on enough speech and fixed for now.
    pub locked: bool,
    /// Picked by the user; never changed by detection.
    pub manual: bool,
    /// The latest detection (empty for a manual pick).
    pub probs: Probs,
}

/// One track's language in auto mode: detected on the first
/// [`LOCK_SECS`] of speech (re-detected on all of it with each new
/// utterance until then), then locked and checked again every
/// [`RECHECK_SECS`] on the latest speech. A manual pick wins.
#[derive(Default)]
pub struct LanguageLock {
    language: Option<String>,
    locked: bool,
    manual: bool,
    /// Speech so far before the lock; afterwards the latest stretch.
    window: Vec<f32>,
    checked_at: f64,
    probs: Probs,
}

impl LanguageLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// The user picked `code` (`None`: back to detection, starting over).
    pub fn set_manual(&mut self, code: Option<String>) {
        *self = Self::default();
        if let Some(code) = code {
            self.language = Some(code);
            self.locked = true;
            self.manual = true;
        }
    }

    /// Locks on a detection made elsewhere (the final pass detects on a
    /// longer look-ahead window first).
    pub fn lock(&mut self, probs: Probs, at_secs: f64) {
        if self.manual {
            return;
        }
        if let Some((code, _)) = probs.first() {
            self.language = Some(code.clone());
            self.locked = true;
            self.checked_at = at_secs;
            self.window.clear();
        }
        self.probs = probs;
    }

    pub fn status(&self) -> Option<Status> {
        Some(Status { language: self.language.clone()?, locked: self.locked, manual: self.manual, probs: self.probs.clone() })
    }

    /// The language to transcribe `samples` (speech at `at_secs` on the
    /// track) in. `detect` gives restricted probabilities for some audio,
    /// or `None` when it can't (then the engine decides: `Ok(None)`).
    /// The second value says whether the language changed.
    pub fn choose(
        &mut self,
        samples: &[f32],
        at_secs: f64,
        mut detect: impl FnMut(&[f32]) -> anyhow::Result<Option<Probs>>,
    ) -> anyhow::Result<(Option<String>, bool)> {
        let rate = crate::SAMPLE_RATE as f64;
        if self.manual {
            return Ok((self.language.clone(), false));
        }
        let before = self.language.clone();
        if !self.locked {
            let room = (LOCK_SECS * rate) as usize;
            let take = samples.len().min(room.saturating_sub(self.window.len()));
            self.window.extend_from_slice(&samples[..take]);
            if let Some(probs) = detect(&self.window)? {
                if self.window.len() >= room {
                    self.lock(probs, at_secs);
                } else {
                    self.language = probs.first().map(|(c, _)| c.clone());
                    self.probs = probs;
                }
            }
        } else {
            let keep = (RECHECK_WINDOW_SECS * rate) as usize;
            self.window.extend_from_slice(samples);
            if self.window.len() > keep {
                self.window.drain(..self.window.len() - keep);
            }
            if at_secs - self.checked_at >= RECHECK_SECS && self.window.len() >= keep / 2 {
                self.checked_at = at_secs;
                if let Some(probs) = detect(&self.window)? {
                    let current = self.language.clone().unwrap_or_default();
                    if let Some(code) = should_switch(&current, &probs, SWITCH_MARGIN) {
                        self.language = Some(code);
                    }
                    self.probs = probs;
                }
            }
        }
        let changed = self.language != before;
        Ok((self.language.clone(), changed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODES: [&str; 4] = ["en", "de", "ar", "nl"];

    fn r(all: &[f32], allowed: &[&str]) -> Probs {
        let allowed: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
        restrict_with(all, &allowed, |id| CODES.get(id).copied())
    }

    fn p(pairs: &[(&str, f32)]) -> Probs {
        pairs.iter().map(|(c, p)| (c.to_string(), *p)).collect()
    }

    #[test]
    fn restricted_choice_only_among_allowed() {
        // Whisper hears mostly English (tech terms), then Dutch, then German.
        let all = [0.5, 0.2, 0.05, 0.25];
        let any = r(&all, &[]);
        assert_eq!(any[0].0, "en");
        assert!((any.iter().map(|x| x.1).sum::<f32>() - 1.0).abs() < 1e-5);
        // Only German and Arabic spoken here: German wins, renormalized.
        let de_ar = r(&all, &["de", "ar"]);
        assert_eq!(de_ar.len(), 2);
        assert_eq!(de_ar[0].0, "de");
        assert!((de_ar[0].1 - 0.8).abs() < 1e-5);
        // Dutch never wins when it isn't allowed.
        assert_eq!(r(&all, &["de", "en"])[0].0, "en");
        // Unknown codes only: fall back to everything.
        assert_eq!(r(&all, &["xx"])[0].0, "en");
        // All zero: evenly split, still a choice.
        let zero = r(&[0.0; 4], &["de", "en"]);
        assert_eq!(zero.len(), 2);
        assert!((zero[0].1 - 0.5).abs() < 1e-5);
    }

    #[test]
    fn suggestion_starts_from_the_app_and_history() {
        let h = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(suggest("de", &h(&["ar", "en", "ar", "auto", ""])), h(&["de", "ar", "en"]));
        assert_eq!(suggest("en", &h(&["en"])), h(&["en", "de"]));
    }

    #[test]
    fn combine_weights_by_speech() {
        let long = p(&[("de", 0.7), ("en", 0.3)]);
        let short = p(&[("en", 0.9), ("de", 0.1)]);
        let both = combine(&[(long, 25.0), (short, 5.0)]);
        assert_eq!(both[0].0, "de");
        assert!((both.iter().map(|x| x.1).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn switch_needs_a_clear_margin() {
        assert_eq!(should_switch("de", &p(&[("en", 0.6), ("de", 0.4)]), SWITCH_MARGIN), None);
        assert_eq!(should_switch("de", &p(&[("en", 0.8), ("de", 0.2)]), SWITCH_MARGIN), Some("en".into()));
        assert_eq!(should_switch("de", &p(&[("de", 0.9), ("en", 0.1)]), SWITCH_MARGIN), None);
        // The current language isn't a candidate at all: its probability is 0.
        assert_eq!(should_switch("fr", &p(&[("de", 0.55), ("en", 0.45)]), SWITCH_MARGIN), Some("de".into()));
        assert_eq!(should_switch("de", &Vec::new(), SWITCH_MARGIN), None);
    }

    fn secs(s: f64) -> Vec<f32> {
        vec![0.1; (s * crate::SAMPLE_RATE as f64) as usize]
    }

    #[test]
    fn locks_after_enough_speech_then_only_switches_clearly() {
        let mut lock = LanguageLock::new();
        let mut answers = vec![
            p(&[("en", 0.6), ("de", 0.4)]), // first short chunk: tech words
            p(&[("de", 0.7), ("en", 0.3)]),
            p(&[("de", 0.8), ("en", 0.2)]), // 20 s reached: lock German
        ]
        .into_iter();
        let mut lens = Vec::new();
        let mut detect = |s: &[f32]| {
            lens.push(s.len());
            Ok(answers.next())
        };
        assert_eq!(lock.choose(&secs(4.0), 0.0, &mut detect).unwrap(), (Some("en".into()), true));
        assert!(!lock.status().unwrap().locked);
        assert_eq!(lock.choose(&secs(8.0), 5.0, &mut detect).unwrap(), (Some("de".into()), true));
        assert_eq!(lock.choose(&secs(10.0), 14.0, &mut detect).unwrap(), (Some("de".into()), false));
        assert!(lock.status().unwrap().locked);
        // Detection saw all speech so far, capped at LOCK_SECS.
        let rate = crate::SAMPLE_RATE as usize;
        assert_eq!(lens, vec![4 * rate, 12 * rate, 20 * rate]);

        // Locked: no detection until RECHECK_SECS have passed.
        let mut calls = 0;
        let mut never = |_: &[f32]| {
            calls += 1;
            Ok(Some(p(&[("en", 0.99), ("de", 0.01)])))
        };
        for t in [30.0, 60.0, 100.0] {
            assert_eq!(lock.choose(&secs(5.0), t, &mut never).unwrap(), (Some("de".into()), false));
        }
        assert_eq!(calls, 0);
        // A re-check that is only somewhat more English keeps German…
        let mut unsure = |_: &[f32]| Ok(Some(p(&[("en", 0.6), ("de", 0.4)])));
        assert_eq!(lock.choose(&secs(15.0), 140.0, &mut unsure).unwrap(), (Some("de".into()), false));
        // …a clear one switches, at the next re-check.
        let mut clear = |_: &[f32]| Ok(Some(p(&[("en", 0.9), ("de", 0.1)])));
        assert_eq!(lock.choose(&secs(15.0), 200.0, &mut clear).unwrap(), (Some("de".into()), false), "too soon");
        assert_eq!(lock.choose(&secs(15.0), 261.0, &mut clear).unwrap(), (Some("en".into()), true));
    }

    #[test]
    fn manual_pick_wins_and_can_be_undone() {
        let mut lock = LanguageLock::new();
        lock.set_manual(Some("ar".into()));
        let mut detect = |_: &[f32]| -> anyhow::Result<Option<Probs>> { panic!("no detection after a manual pick") };
        assert_eq!(lock.choose(&secs(30.0), 500.0, &mut detect).unwrap(), (Some("ar".into()), false));
        let st = lock.status().unwrap();
        assert!(st.manual && st.locked);
        lock.lock(p(&[("de", 1.0)]), 0.0);
        assert_eq!(lock.status().unwrap().language, "ar", "a detection never overrides the user");
        lock.set_manual(None);
        assert!(lock.status().is_none());
        let mut de = |_: &[f32]| Ok(Some(p(&[("de", 0.9), ("en", 0.1)])));
        assert_eq!(lock.choose(&secs(3.0), 501.0, &mut de).unwrap(), (Some("de".into()), true));
    }

    #[test]
    fn without_detection_the_engine_decides() {
        let mut lock = LanguageLock::new();
        let mut none = |_: &[f32]| Ok(None);
        assert_eq!(lock.choose(&secs(25.0), 0.0, &mut none).unwrap(), (None, false));
        assert!(lock.status().is_none());
    }
}
