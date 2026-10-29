//! The user's own vocabulary: terms Whisper should know (product names,
//! colleagues, project keys) and fix rules for words it keeps mishearing
//! ("Bright Well" → "Brightwell"). Terms go to the engine as a hint before
//! each chunk of audio; fix rules run over every transcript it returns, and
//! can be applied to the library once to correct older meetings too.
//! Stored as JSON in the data directory, next to the known voices.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::store::Store;

/// Longest hint passed to the engine. Whisper keeps at most 224 prompt
/// tokens; this stays well below (~150–200 tokens) so the hint never
/// crowds out the audio, and a long list doesn't make it repeat words.
pub const MAX_HINT_CHARS: usize = 500;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Glossary {
    /// Words spelled the way they should appear, most important first.
    pub terms: Vec<String>,
    pub fixes: Vec<Fix>,
}

/// Replace `from` (any case, whole words) with `to`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub from: String,
    pub to: String,
}

impl Glossary {
    pub fn path() -> PathBuf {
        crate::paths::data_dir().join("glossary.json")
    }

    /// The glossary (empty if the file is missing or unreadable).
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
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?).context("writing glossary")?;
        std::fs::rename(&tmp, &path).context("saving glossary")?;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.fixes.is_empty()
    }

    /// Adds a term (ignored if empty or already there, in any case).
    /// Returns whether it was added.
    pub fn add_term(&mut self, term: &str) -> bool {
        let term = clean(term);
        if term.is_empty() || self.has_term(&term) {
            return false;
        }
        self.terms.push(term);
        true
    }

    pub fn has_term(&self, term: &str) -> bool {
        let term = clean(term).to_lowercase();
        self.terms.iter().any(|t| t.to_lowercase() == term)
    }

    pub fn remove_term(&mut self, term: &str) -> bool {
        let before = self.terms.len();
        self.terms.retain(|t| t != term);
        self.terms.len() != before
    }

    /// Adds a fix rule, replacing an existing one for the same `from`, and
    /// keeps `to` as a term so the engine hears it right in the first place.
    /// Returns false for a rule that would change nothing.
    pub fn add_fix(&mut self, from: &str, to: &str) -> bool {
        let (from, to) = (clean(from), clean(to));
        if from.is_empty() || to.is_empty() || from == to {
            return false;
        }
        let key = from.to_lowercase();
        self.fixes.retain(|f| f.from.to_lowercase() != key);
        self.fixes.push(Fix { from, to: to.clone() });
        self.add_term(&to);
        true
    }

    pub fn remove_fix(&mut self, from: &str) -> bool {
        let before = self.fixes.len();
        self.fixes.retain(|f| f.from != from);
        self.fixes.len() != before
    }

    /// The hint for the engine: terms in order, then fix targets not
    /// already listed, as long as they fit in [`MAX_HINT_CHARS`].
    /// `None` without any terms.
    pub fn hint(&self) -> Option<String> {
        let mut seen: Vec<String> = Vec::new();
        let mut out = String::new();
        for term in self.terms.iter().chain(self.fixes.iter().map(|f| &f.to)) {
            let key = term.to_lowercase();
            if term.is_empty() || seen.contains(&key) {
                continue;
            }
            let extra = if out.is_empty() { term.len() } else { term.len() + 2 };
            if out.len() + extra + 1 > MAX_HINT_CHARS {
                // A shorter term later on may still fit.
                continue;
            }
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(term);
            seen.push(key);
        }
        (!out.is_empty()).then(|| format!("{out}."))
    }

    /// The fix rules, compiled.
    pub fn fixer(&self) -> Fixer {
        Fixer {
            rules: self.fixes.iter().filter_map(|f| Some((pattern(&f.from)?, f.to.clone()))).collect(),
        }
    }
}

/// Compiled fix rules (see [`Glossary::fixer`]).
#[derive(Default)]
pub struct Fixer {
    rules: Vec<(Regex, String)>,
}

impl Fixer {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// `text` with every rule applied.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (re, to) in &self.rules {
            if re.is_match(&out) {
                out = re.replace_all(&out, regex::NoExpand(to)).into_owned();
            }
        }
        out
    }
}

/// `from` as a case-insensitive pattern for whole words: any run of spaces
/// between its words, and word boundaries where it starts or ends with a
/// letter or digit ("C++" can't have one after it).
fn pattern(from: &str) -> Option<Regex> {
    let words: Vec<String> = from.split_whitespace().map(regex::escape).collect();
    if words.is_empty() {
        return None;
    }
    let edge = |c: Option<char>| if c.is_some_and(char::is_alphanumeric) { r"\b" } else { "" };
    let re = format!("(?i){}{}{}", edge(from.trim().chars().next()), words.join(r"\s+"), edge(from.trim().chars().last()));
    Regex::new(&re).ok()
}

/// Whitespace collapsed, ends trimmed.
fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The glossary as saved, cheap to call per utterance: read again only
/// when the file changed (the app's settings or another process saved it).
pub fn current() -> Arc<Glossary> {
    static CACHE: Mutex<Option<(Option<SystemTime>, Arc<Glossary>)>> = Mutex::new(None);
    let modified = std::fs::metadata(Glossary::path()).and_then(|m| m.modified()).ok();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match &*cache {
        Some((at, g)) if *at == modified => g.clone(),
        _ => {
            let g = Arc::new(if modified.is_some() { Glossary::load() } else { Glossary::default() });
            *cache = Some((modified, g.clone()));
            g
        }
    }
}

/// Most words a learned fix may span on either side; a longer edit is a
/// rewrite, not a misheard word.
const MAX_FIX_WORDS: usize = 4;

/// A fix rule from one edit of a transcript line: the words that changed,
/// when that's a short phrase replaced by another ("… die Bright Well
/// App …" → "… die Brightwell App …" gives "Bright Well" → "Brightwell").
/// `None` for additions, deletions, punctuation-only changes and rewrites.
pub fn fix_from_edit(before: &str, after: &str) -> Option<Fix> {
    let a: Vec<&str> = before.split_whitespace().collect();
    let b: Vec<&str> = after.split_whitespace().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (from, to) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    if from.is_empty() || to.is_empty() || from.len() > MAX_FIX_WORDS || to.len() > MAX_FIX_WORDS {
        return None;
    }
    let strip = |words: &[&str]| words.join(" ").trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    let (from, to) = (strip(from), strip(to));
    // Only commas or full stops changed.
    let letters = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    (!from.is_empty() && !to.is_empty() && letters(&from) != letters(&to)).then_some(Fix { from, to })
}

/// What [`apply_to_library`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub lines: usize,
    pub summaries: usize,
    pub meetings: usize,
}

/// Runs `fixer` over every stored transcript line and summary. The search
/// index notices the changed text and indexes those meetings again.
pub fn apply_to_library(store: &mut Store, fixer: &Fixer) -> Result<Applied> {
    let mut applied = Applied::default();
    if fixer.is_empty() {
        return Ok(applied);
    }
    let tx = store.conn_mut().transaction()?;
    let mut meetings: Vec<i64> = Vec::new();
    {
        let mut read = tx.prepare("SELECT id, meeting_id, text FROM segments")?;
        let mut write = tx.prepare("UPDATE segments SET text = ?1 WHERE id = ?2")?;
        let rows: Vec<(i64, i64, String)> =
            read.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, meeting, text) in rows {
            let fixed = fixer.apply(&text);
            if fixed != text {
                write.execute(params![fixed, id])?;
                applied.lines += 1;
                if !meetings.contains(&meeting) {
                    meetings.push(meeting);
                }
            }
        }
        let mut read = tx.prepare("SELECT meeting_id, markdown FROM summaries")?;
        let mut write = tx.prepare("UPDATE summaries SET markdown = ?1 WHERE meeting_id = ?2")?;
        let rows: Vec<(i64, String)> = read.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        for (meeting, markdown) in rows {
            let fixed = fixer.apply(&markdown);
            if fixed != markdown {
                write.execute(params![fixed, meeting])?;
                applied.summaries += 1;
                if !meetings.contains(&meeting) {
                    meetings.push(meeting);
                }
            }
        }
    }
    tx.commit()?;
    applied.meetings = meetings.len();
    Ok(applied)
}

/// A term the library suggests for the glossary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub term: String,
    pub kind: SuggestionKind,
    /// How often it appears in the documents (0 for names).
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    /// Someone named in a transcript or among the known voices.
    Name,
    /// A ticket project key ("BRW" from "BRW-142").
    ProjectKey,
    /// A word from the documents the transcripts never contain.
    Term,
}

/// Most suggestions offered at once.
const MAX_SUGGESTIONS: usize = 40;
/// A document word counts as a term from this many mentions on.
const MIN_DOC_MENTIONS: usize = 3;

/// Terms the user's own material suggests, not yet in `glossary`: people
/// named in transcripts or known by voice, ticket project keys, and words
/// the documents use often that Whisper has never written (product names,
/// internal jargon), most frequent first.
pub fn suggest(store: &Store, glossary: &Glossary) -> Result<Vec<Suggestion>> {
    let conn = store.conn();
    let mut out: Vec<Suggestion> = Vec::new();
    let push = |out: &mut Vec<Suggestion>, s: Suggestion| {
        let key = s.term.to_lowercase();
        if !glossary.has_term(&s.term) && !out.iter().any(|o| o.term.to_lowercase() == key) {
            out.push(s);
        }
    };

    let mut names: Vec<String> = conn
        .prepare("SELECT DISTINCT speaker FROM segments WHERE speaker IS NOT NULL")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    names.extend(crate::voices::Voices::load().voices.into_iter().map(|v| v.name));
    names.sort_by_key(|n| n.to_lowercase());
    for name in names {
        if !crate::voices::is_generic_label(&name) {
            push(&mut out, Suggestion { term: clean(&name), kind: SuggestionKind::Name, count: 0 });
        }
    }

    let docs: Vec<String> = conn
        .prepare("SELECT c.text FROM doc_chunks c JOIN documents d ON d.id = c.document_id WHERE d.active = 1")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let key_re = Regex::new(r"\b([A-Z][A-Z0-9]{1,9})-[0-9]{1,6}\b").expect("valid regex");
    let mut keys: Vec<(String, usize)> = Vec::new();
    let mut words: Vec<(String, usize)> = Vec::new();
    let count = |list: &mut Vec<(String, usize)>, w: &str| match list.iter_mut().find(|(t, _)| t == w) {
        Some((_, n)) => *n += 1,
        None => list.push((w.to_string(), 1)),
    };
    for text in &docs {
        for c in key_re.captures_iter(text) {
            count(&mut keys, &c[1]);
        }
        for word in text.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')) {
            let word = word.trim_matches(|c: char| c == '-' || c == '_');
            if is_term_shaped(word) && !key_re.is_match(word) {
                count(&mut words, word);
            }
        }
    }
    keys.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (key, n) in keys {
        push(&mut out, Suggestion { term: key, kind: SuggestionKind::ProjectKey, count: n });
    }

    // Words the transcripts already contain are heard fine.
    words.retain(|(_, n)| *n >= MIN_DOC_MENTIONS);
    words.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut in_transcripts = conn.prepare("SELECT 1 FROM segments_fts WHERE segments_fts MATCH ?1 LIMIT 1")?;
    for (word, n) in words {
        if out.len() >= MAX_SUGGESTIONS {
            break;
        }
        let quoted = format!("\"{}\"", word.replace('"', ""));
        if in_transcripts.exists([quoted])? {
            continue;
        }
        push(&mut out, Suggestion { term: word, kind: SuggestionKind::Term, count: n });
    }
    out.truncate(MAX_SUGGESTIONS);
    Ok(out)
}

/// Looks like a name, product or acronym rather than an ordinary word:
/// capitalized with a capital or digit inside ("PayFlow", "S4HANA"), an
/// acronym of 2–6 capitals, or a capitalized word of at least 4 letters
/// (German nouns pass too, but common ones show up in transcripts and
/// are dropped there).
fn is_term_shaped(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 2 || chars.len() > 30 || !chars[0].is_uppercase() || chars.iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let inner_caps = chars[1..].iter().any(|c| c.is_uppercase() || c.is_ascii_digit());
    let all_caps = chars.iter().all(|c| c.is_uppercase() || c.is_ascii_digit());
    if all_caps {
        return (2..=6).contains(&chars.len()) && !COMMON_ACRONYMS.contains(&word);
    }
    inner_caps || (chars.len() >= 4 && chars[1..].iter().all(|c| c.is_lowercase()))
}

/// Acronyms Whisper writes correctly anyway.
const COMMON_ACRONYMS: &[&str] = &[
    "PDF", "OK", "CEO", "CTO", "CFO", "HR", "IT", "API", "URL", "USB", "EU", "USA", "UK", "AI", "KI", "FAQ", "PS", "TODO", "ID",
    "GMBH", "AG", "CET", "CEST", "UTC", "PM", "AM", "DE", "EN", "ZB", "BZW", "USW", "ETC", "QA",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn glossary(fixes: &[(&str, &str)]) -> Glossary {
        let mut g = Glossary::default();
        for (from, to) in fixes {
            g.add_fix(from, to);
        }
        g
    }

    #[test]
    fn fixes_whole_words_in_any_case_and_spacing() {
        let f = glossary(&[("Bright Well", "Brightwell"), ("jira", "Jira")]).fixer();
        assert_eq!(f.apply("Die bright  well App läuft."), "Die Brightwell App läuft.");
        assert_eq!(f.apply("Bright Well, Bright Well!"), "Brightwell, Brightwell!");
        // Not inside other words.
        assert_eq!(f.apply("Bright Wellington und jiraffe"), "Bright Wellington und jiraffe");
        assert_eq!(f.apply("Ticket in jira"), "Ticket in Jira");
    }

    #[test]
    fn fixes_symbols_and_leave_replacement_literal() {
        let f = glossary(&[("c plus plus", "C++"), ("dollar", "$1")]).fixer();
        assert_eq!(f.apply("In c plus plus geschrieben"), "In C++ geschrieben");
        assert_eq!(f.apply("ein dollar"), "ein $1");
    }

    #[test]
    fn fix_targets_become_terms_and_rules_replace_each_other() {
        let mut g = glossary(&[("Bright Well", "Brightwell")]);
        assert_eq!(g.terms, ["Brightwell"]);
        assert!(g.add_fix("bright well", "Brightwell GmbH"));
        assert_eq!(g.fixes, [Fix { from: "bright well".into(), to: "Brightwell GmbH".into() }]);
        assert!(!g.add_fix("same", "same"));
        assert!(!g.add_term("brightwell"));
    }

    #[test]
    fn hint_lists_terms_and_stays_short() {
        let mut g = Glossary::default();
        assert_eq!(g.hint(), None);
        g.add_term("Brightwell");
        g.add_term("Anna Lehmann");
        g.add_fix("Pay flow", "PayFlow");
        g.add_fix("bright wel", "Brightwell");
        assert_eq!(g.hint().as_deref(), Some("Brightwell, Anna Lehmann, PayFlow."));
        for i in 0..200 {
            g.add_term(&format!("Begriff{i}"));
        }
        let hint = g.hint().unwrap();
        assert!(hint.len() <= MAX_HINT_CHARS, "{}", hint.len());
        assert!(hint.starts_with("Brightwell, Anna Lehmann, PayFlow, Begriff0"));
    }

    #[test]
    fn learns_fix_from_edit() {
        let fix = |a, b| fix_from_edit(a, b).map(|f| (f.from, f.to));
        assert_eq!(fix("Die Bright Well App ist live.", "Die Brightwell App ist live."), Some(("Bright Well".into(), "Brightwell".into())));
        assert_eq!(fix("Frag mal Bright Well.", "Frag mal Brightwell."), Some(("Bright Well".into(), "Brightwell".into())));
        assert_eq!(fix("Anna kommt", "Lena kommt"), Some(("Anna".into(), "Lena".into())));
        assert_eq!(fix("das brightwell Team", "das Brightwell Team"), Some(("brightwell".into(), "Brightwell".into())));
        // Additions, deletions and rewrites teach nothing.
        assert_eq!(fix("Gut so", "Gut so."), None);
        assert_eq!(fix("Gut", "Gut so"), None);
        assert_eq!(fix("Gut so", "Gut"), None);
        assert_eq!(fix("eins zwei drei vier fünf sechs", "a b c d e f"), None);
        assert_eq!(fix("gleich", "gleich"), None);
    }

    #[test]
    fn punctuation_only_edits_teach_nothing() {
        assert_eq!(fix_from_edit("Ja klar", "Ja, klar"), None);
        assert_eq!(fix_from_edit("Okay das passt", "Okay, das passt."), None);
    }

    #[test]
    fn term_shapes() {
        for w in ["PayFlow", "S4HANA", "BRW", "Brightwell", "Lehmann", "iOS-App"] {
            assert_eq!(is_term_shaped(w), w != "iOS-App", "{w}");
        }
        for w in ["und", "Ja", "PDF", "2024", "A"] {
            assert!(!is_term_shaped(w), "{w}");
        }
    }

    #[test]
    fn applies_to_library_and_suggests_from_documents() -> Result<()> {
        use crate::finalize::FinalSegment;
        use crate::store::NewMeeting;
        let dir = std::env::temp_dir().join(format!("mostalgia-glossary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let mut store = Store::open(&dir.join("test.db"))?;
        let seg = |start: f64, speaker: Option<&str>, text: &str| FinalSegment {
            source: crate::Source::System,
            start_secs: start,
            end_secs: start + 2.0,
            text: text.into(),
            speaker: speaker.map(Into::into),
        };
        let id = store.insert_meeting(
            &NewMeeting { title: "Sync", started_at: chrono::Local::now(), duration_secs: 60.0, language: "de", dir: &dir.join("m1") },
            &[seg(1.0, Some("Anna Lehmann"), "Die Bright Well App ist fertig."), seg(4.0, Some("Sprecher 2"), "Super, danke.")],
        )?;
        store.save_summary(id, "test", "m", None, "# Sync\n\n## Kernpunkte\n- Bright Well ist fertig\n")?;
        store.conn().execute_batch(
            "INSERT INTO documents (id, title, file_name, path, kind, size_bytes, hash, added_at)
               VALUES (1, 'Plan', 'plan.md', '/x', 'md', 1, 'h', 0);
             INSERT INTO doc_chunks (document_id, loc, text) VALUES
               (1, 0, 'Brightwell ersetzt PayFlow. Siehe BRW-12, BRW-13 und BRW-14. Die App von Brightwell.'),
               (1, 1, 'Brightwell und PayFlow im PDF. Die App, die App. Super PayFlow.');",
        )?;

        let mut g = glossary(&[("Bright Well", "Brightwell")]);
        let applied = apply_to_library(&mut store, &g.fixer())?;
        assert_eq!(applied, Applied { lines: 1, summaries: 1, meetings: 1 });
        let m = store.get(id)?.unwrap();
        assert_eq!(m.segments[0].text, "Die Brightwell App ist fertig.");
        assert!(m.summary.unwrap().markdown.contains("- Brightwell ist fertig"));
        assert_eq!(apply_to_library(&mut store, &g.fixer())?, Applied::default());

        g.remove_term("Brightwell");
        let terms: Vec<(String, SuggestionKind)> = suggest(&store, &g)?.into_iter().map(|s| (s.term, s.kind)).collect();
        assert_eq!(
            terms,
            [
                ("Anna Lehmann".to_string(), SuggestionKind::Name),
                ("BRW".to_string(), SuggestionKind::ProjectKey),
                // "Brightwell" is in the transcript now, "App" and "Super" are
                // short or ordinary, "PDF" is common.
                ("PayFlow".to_string(), SuggestionKind::Term),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
