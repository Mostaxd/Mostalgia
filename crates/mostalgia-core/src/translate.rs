//! Live translation: finished transcript lines are translated into one
//! language by the chosen AI connection while the meeting goes on. Only
//! text is sent, never audio. Lines are batched (a few at a time, as they
//! arrive) and the last translated lines go along as context, so names and
//! terms stay consistent; terms the caller names (the user's own words)
//! are kept as written.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::summary::SummaryRequest;

/// Most lines in one request.
const MAX_BATCH: usize = 6;
/// How long to wait for more lines after the first one before sending.
const GATHER: Duration = Duration::from_millis(500);
/// Earlier lines (original and translation) sent as context.
const CONTEXT_LINES: usize = 4;
/// After this many failed requests in a row, wait before trying again.
const FAILS_BEFORE_PAUSE: usize = 3;
const PAUSE_AFTER_FAILS: Duration = Duration::from_secs(15);

/// A line to translate: an id the caller chooses (its index in the live
/// transcript) and who said what.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: usize,
    pub speaker: String,
    pub text: String,
}

/// What the worker reports back.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The translation of line `id`; `None` when the line already is in the
    /// target language (nothing to show).
    Done { id: usize, text: Option<String> },
    /// A request failed (the lines in it stay untranslated). Sent once per
    /// run of failures; translating goes on with the next lines.
    Failed(String),
    /// Requests work again after a failure.
    Recovered,
}

/// Sends text to the model and returns its reply: (system, user) → text.
pub type Ask = Box<dyn FnMut(&str, &str) -> Result<String> + Send>;

/// A background worker translating lines into `target` (a language code).
/// Dropping it stops the worker after the request in flight.
pub struct LiveTranslator {
    jobs: Sender<Job>,
    outcomes: Receiver<Outcome>,
    target: String,
}

impl LiveTranslator {
    /// Translates with the connection and model in `req`, keeping `terms`
    /// as written.
    pub fn start(req: SummaryRequest, target: &str, terms: Vec<String>) -> Self {
        let ask: Ask = Box::new(move |system, user| {
            let max_tokens = 1200;
            crate::llm::chat_quick(&req.connection, &req.model, system, user, 0, max_tokens, false, &mut |_| {})
                .map(|c| c.text)
        });
        Self::with(ask, target, terms)
    }

    /// Translates with `ask` (tests use a stand-in).
    pub fn with(ask: Ask, target: &str, terms: Vec<String>) -> Self {
        let (jobs, job_rx) = channel::<Job>();
        let (out_tx, outcomes) = channel::<Outcome>();
        let target_name = crate::summary::language_name(target);
        std::thread::Builder::new()
            .name("live-translate".into())
            .spawn(move || run(ask, &target_name, &terms, job_rx, out_tx))
            .expect("spawning the translation thread");
        Self { jobs, outcomes, target: target.to_string() }
    }

    /// The target language code.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Queues a line. Empty lines are skipped.
    pub fn push(&self, id: usize, speaker: &str, text: &str) {
        let text = text.trim();
        if text.chars().any(char::is_alphabetic) {
            let _ = self.jobs.send(Job { id, speaker: speaker.to_string(), text: text.to_string() });
        }
    }

    /// Everything reported since the last call.
    pub fn poll(&self) -> Vec<Outcome> {
        self.outcomes.try_iter().collect()
    }
}

fn run(mut ask: Ask, target: &str, terms: &[String], jobs: Receiver<Job>, out: Sender<Outcome>) {
    let mut context: Vec<(String, String)> = Vec::new();
    let mut fails = 0;
    // `recv` ends when the translator is dropped.
    while let Ok(first) = jobs.recv() {
        let mut batch = vec![first];
        let until = Instant::now() + GATHER;
        while batch.len() < MAX_BATCH {
            match jobs.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(job) => batch.push(job),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let (system, user) = prompt(target, terms, &context, &batch);
        let reply = ask(&system, &user).and_then(|reply| parse(&reply, batch.len()));
        match reply {
            Ok(lines) => {
                if fails > 0 && out.send(Outcome::Recovered).is_err() {
                    return;
                }
                fails = 0;
                for (job, translated) in batch.iter().zip(lines) {
                    let text = (!same_text(&job.text, &translated)).then(|| translated.clone());
                    context.push((job.text.clone(), translated));
                    if out.send(Outcome::Done { id: job.id, text }).is_err() {
                        return;
                    }
                }
                let keep = context.len().saturating_sub(CONTEXT_LINES);
                context.drain(..keep);
            }
            Err(e) => {
                fails += 1;
                if fails == 1 && out.send(Outcome::Failed(format!("{e:#}"))).is_err() {
                    return;
                }
                if fails >= FAILS_BEFORE_PAUSE {
                    std::thread::sleep(PAUSE_AFTER_FAILS);
                }
            }
        }
    }
}

/// The request for one batch: lines numbered 1…n, earlier lines as
/// context, the glossary's terms to keep.
pub fn prompt(target: &str, terms: &[String], context: &[(String, String)], batch: &[Job]) -> (String, String) {
    let mut system = format!(
        "You translate a live meeting transcript into {target}. Each line was spoken by one person and may be in any language. \
         Translate every numbered line on its own, in a natural spoken style, keeping names, numbers and dates exactly. \
         If a line already is in {target}, repeat it unchanged. Reply with the same numbered lines and nothing else: \
         no notes, no quotes, no speaker names."
    );
    if !terms.is_empty() {
        system.push_str(&format!(" Keep these terms as written: {}.", terms.join(", ")));
    }
    let mut user = String::new();
    if !context.is_empty() {
        user.push_str("Earlier lines, for context only (do not translate again):\n");
        for (original, translated) in context {
            user.push_str(&format!("- {original} → {translated}\n"));
        }
        user.push('\n');
    }
    user.push_str(&format!("Translate into {target}:\n"));
    for (i, job) in batch.iter().enumerate() {
        user.push_str(&format!("{}. {}: {}\n", i + 1, job.speaker, job.text));
    }
    (system, user)
}

/// The numbered lines of a reply, in order. Accepts "1." / "1)" / "1:"
/// and a speaker label the model repeated; fails unless all `n` are there.
pub fn parse(reply: &str, n: usize) -> Result<Vec<String>> {
    let mut found: Vec<Option<String>> = vec![None; n];
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(['-', '*', ' ']);
        let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
        let Ok(k) = digits.parse::<usize>() else { continue };
        if k == 0 || k > n {
            continue;
        }
        let rest = line[digits.len()..].trim_start_matches(['.', ')', ':']).trim();
        let rest = rest.trim_matches(['"', '“', '”']).trim();
        if !rest.is_empty() && found[k - 1].is_none() {
            found[k - 1] = Some(rest.to_string());
        }
    }
    // A single line without a number is that line's translation.
    if n == 1 && found[0].is_none() {
        let only = reply.trim().trim_matches(['"', '“', '”']).trim();
        if !only.is_empty() && !only.contains('\n') {
            found[0] = Some(only.to_string());
        }
    }
    let missing = found.iter().filter(|f| f.is_none()).count();
    if missing > 0 {
        bail!("the translation came back incomplete ({missing} of {n} lines missing)");
    }
    Ok(found.into_iter().flatten().collect())
}

/// The same words, ignoring case, spaces and punctuation: the line was
/// already in the target language.
fn same_text(a: &str, b: &str) -> bool {
    let key = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    key(a) == key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: usize, text: &str) -> Job {
        Job { id, speaker: "Lena".into(), text: text.into() }
    }

    #[test]
    fn prompt_numbers_lines_and_keeps_terms() {
        let (system, user) = prompt("English", &["Brightwell".into()], &[("Hallo".into(), "Hello".into())], &[job(7, "Wie geht's?"), job(8, "Gut.")]);
        assert!(system.contains("into English") && system.contains("Keep these terms as written: Brightwell."));
        assert!(user.contains("- Hallo → Hello"));
        assert!(user.ends_with("1. Lena: Wie geht's?\n2. Lena: Gut.\n"), "{user}");
    }

    #[test]
    fn parses_numbered_replies_leniently() {
        assert_eq!(parse("1. Hello\n2) Fine.\n", 2).unwrap(), ["Hello", "Fine."]);
        assert_eq!(parse("Sure:\n2: \"Fine.\"\n1. Hello", 2).unwrap(), ["Hello", "Fine."]);
        assert_eq!(parse("- 1. Lena: Hello", 1).unwrap(), ["Lena: Hello"]);
        assert_eq!(parse("Hello there", 1).unwrap(), ["Hello there"]);
        assert!(parse("1. Hello", 2).is_err());
        assert!(parse("", 1).is_err());
    }

    #[test]
    fn worker_batches_translates_and_skips_same_language() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = calls.clone();
        let ask: Ask = Box::new(move |_, user| {
            seen.lock().unwrap().push(user.to_string());
            // Echo "Ok" for the English line, translate the German one.
            let n = user.lines().filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit())).count();
            Ok((1..=n).map(|i| if i == 1 { "1. Hello everyone".to_string() } else { format!("{i}. Ok") }).collect::<Vec<_>>().join("\n"))
        });
        let t = LiveTranslator::with(ask, "en", Vec::new());
        t.push(0, "Lena", "Hallo zusammen");
        t.push(1, "Sam", "Ok");
        t.push(2, "Sam", "  ");
        let mut got = Vec::new();
        let until = Instant::now() + Duration::from_secs(5);
        while got.len() < 2 && Instant::now() < until {
            got.extend(t.poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got, [Outcome::Done { id: 0, text: Some("Hello everyone".into()) }, Outcome::Done { id: 1, text: None }]);
        assert_eq!(calls.lock().unwrap().len(), 1, "both lines go in one request");
    }

    #[test]
    fn reports_a_failure_once_then_recovery() {
        let mut n = 0;
        let ask: Ask = Box::new(move |_, _| {
            n += 1;
            if n <= 2 { bail!("unreachable") } else { Ok("1. Hi".into()) }
        });
        let t = LiveTranslator::with(ask, "en", Vec::new());
        let mut got = Vec::new();
        for id in 0..3 {
            t.push(id, "Lena", "Hallo");
            std::thread::sleep(GATHER + Duration::from_millis(300));
            got.extend(t.poll());
        }
        let until = Instant::now() + Duration::from_secs(5);
        while got.len() < 3 && Instant::now() < until {
            got.extend(t.poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got, [Outcome::Failed("unreachable".into()), Outcome::Recovered, Outcome::Done { id: 2, text: Some("Hi".into()) }]);
    }
}
