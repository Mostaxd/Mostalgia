//! Work running on a meeting in the background: the final transcription
//! after a recording, transcribing again, the summary, the notes. The
//! library lists it with a spinner and the meeting screen shows it, from
//! whichever view started it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// What the work is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// The recording that just stopped (not in the library yet).
    Recording,
    /// A meeting's transcript being made again.
    Transcript(i64),
    /// A meeting's summary being written.
    Summary(i64),
    /// A meeting's notes being written.
    Notes(i64),
}

impl Key {
    /// The library meeting this is about.
    pub fn meeting(self) -> Option<i64> {
        match self {
            Key::Recording => None,
            Key::Transcript(id) | Key::Summary(id) | Key::Notes(id) => Some(id),
        }
    }
}

/// One piece of work as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    /// The meeting's title.
    pub title: String,
    /// What is happening ("Transcribing … 42 %").
    pub stage: String,
    /// How far, 0..=1, when known.
    pub fraction: Option<f32>,
}

static JOBS: Mutex<Vec<(Key, Job)>> = Mutex::new(Vec::new());
/// Work that failed, with why, until the meeting's screen shows it.
static FAILED: Mutex<Vec<(Key, String)>> = Mutex::new(Vec::new());
/// Bumped on every change, so views redraw only then.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn jobs() -> std::sync::MutexGuard<'static, Vec<(Key, Job)>> {
    JOBS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts or updates the work on `key`.
pub fn set(key: Key, job: Job) {
    let mut jobs = jobs();
    match jobs.iter_mut().find(|(k, _)| *k == key) {
        Some((_, j)) if *j == job => return,
        Some((_, j)) => *j = job,
        None => jobs.push((key, job)),
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Updates the stage of the work on `key`, if any.
pub fn stage(key: Key, stage: String, fraction: Option<f32>) {
    let job = jobs().iter().find(|(k, _)| *k == key).map(|(_, j)| Job { stage, fraction, ..j.clone() });
    if let Some(job) = job {
        set(key, job);
    }
}

/// The work on `key` is done (or failed).
pub fn clear(key: Key) {
    let mut jobs = jobs();
    let before = jobs.len();
    jobs.retain(|(k, _)| *k != key);
    if jobs.len() != before {
        GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

/// The work on `key` failed: it ends, and `message` waits for the
/// meeting's screen (even one opened later) to show it.
pub fn fail(key: Key, message: String) {
    clear(key);
    let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    failed.retain(|(k, _)| *k != key);
    failed.push((key, message));
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Takes the failures about `meeting_id` (to show them).
pub fn take_failures(meeting_id: i64) -> Vec<(Key, String)> {
    let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    let (mine, rest) = std::mem::take(&mut *failed).into_iter().partition(|(k, _)| k.meeting() == Some(meeting_id));
    *failed = rest;
    mine
}

/// Whether something about `meeting_id` failed and wasn't shown yet.
pub fn has_failure(meeting_id: i64) -> bool {
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(k, _)| k.meeting() == Some(meeting_id))
}

/// The work running on `meeting_id`, if any (the first, when several).
pub fn for_meeting(meeting_id: i64) -> Option<(Key, Job)> {
    jobs().iter().find(|(k, _)| k.meeting() == Some(meeting_id)).cloned()
}

pub fn get(key: Key) -> Option<Job> {
    jobs().iter().find(|(k, _)| *k == key).map(|(_, j)| j.clone())
}

/// Everything running, oldest first.
pub fn all() -> Vec<(Key, Job)> {
    jobs().clone()
}

/// Changes so far (see [`GENERATION`]).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// "42 %" for a fraction.
pub fn percent(fraction: f32) -> String {
    format!("{} %", (fraction.clamp(0.0, 1.0) * 100.0).floor() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_come_and_go() {
        let key = Key::Summary(-7);
        let start = generation();
        set(key, Job { title: "Standup".into(), stage: "Transcribing … 0 %".into(), fraction: Some(0.0) });
        assert!(generation() > start);
        stage(key, "Transcribing … 42 %".into(), Some(0.42));
        assert_eq!(get(key).map(|j| (j.title, j.fraction)), Some(("Standup".into(), Some(0.42))));
        // The same again changes nothing (no redraw).
        let now = generation();
        stage(key, "Transcribing … 42 %".into(), Some(0.42));
        assert_eq!(generation(), now);
        assert!(all().iter().any(|(k, _)| *k == key));
        clear(key);
        assert_eq!(get(key), None);
        stage(key, "late".into(), None);
        assert_eq!(get(key), None, "a stage after the end doesn't bring it back");
        assert_eq!(percent(0.999), "99 %");
    }

    #[test]
    fn a_failure_waits_for_its_meeting() {
        let key = Key::Transcript(-8);
        set(key, Job { title: "Review".into(), stage: "Transcribing … 10 %".into(), fraction: Some(0.1) });
        fail(key, "GPU lost".into());
        assert_eq!(get(key), None, "failed work isn't running any more");
        assert!(has_failure(-8));
        assert!(take_failures(-9).is_empty(), "another meeting's");
        assert_eq!(take_failures(-8), [(key, "GPU lost".to_string())]);
        assert!(!has_failure(-8), "shown once");
    }
}
