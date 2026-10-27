//! Hybrid search over passages: FTS5 BM25 for exact words, e5 embeddings
//! for meaning, fused with reciprocal rank fusion.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDate, TimeZone};
use rusqlite::params;

use super::embed;
use crate::store::Store;

/// Candidates taken from each ranking before fusion.
const CANDIDATES: usize = 50;
/// RRF constant (the usual 60).
const RRF_K: f64 = 60.0;

#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub meeting: Option<i64>,
}

impl Filter {
    /// Unix-second bounds for meetings.started_at (inclusive dates).
    fn bounds(&self) -> (i64, i64) {
        let at = |d: NaiveDate, end: bool| {
            let t = if end { d.and_hms_opt(23, 59, 59) } else { d.and_hms_opt(0, 0, 0) }.unwrap();
            Local.from_local_datetime(&t).earliest().map_or(0, |dt| dt.timestamp())
        };
        (
            self.from.map_or(i64::MIN, |d| at(d, false)),
            self.to.map_or(i64::MAX, |d| at(d, true)),
        )
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub passage_id: i64,
    pub meeting_id: i64,
    pub title: String,
    pub started_at: DateTime<Local>,
    pub kind: String,
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
    pub score: f64,
}

/// Embeddings kept in memory between searches; reloaded when the index
/// changes (any (re)index invalidates it).
struct VecCache {
    vectors: HashMap<i64, Vec<f32>>,
}

fn cache() -> &'static Mutex<Option<VecCache>> {
    static CELL: OnceLock<Mutex<Option<VecCache>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

pub(crate) fn invalidate_cache() {
    *cache().lock().unwrap() = None;
}

const STOPWORDS: &[&str] = &[
    "der", "die", "das", "und", "oder", "ein", "eine", "einen", "ist", "sind", "war", "wir", "ich", "du", "sie", "er", "es",
    "was", "wie", "wer", "wann", "wo", "warum", "welche", "welcher", "haben", "hat", "hatte", "zu", "im", "in", "am", "an",
    "auf", "mit", "von", "für", "über", "den", "dem", "des", "nicht", "noch", "auch", "so", "dass", "letzte", "letzten",
    "woche", "the", "a", "an", "and", "or", "is", "are", "was", "what", "who", "when", "where", "did", "do", "we", "of",
    "to", "in", "on", "for", "about", "with", "uns", "unser", "unsere", "gesagt", "besprochen",
];

/// FTS5 query: OR of the significant words, each prefix-matched.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

pub fn search(store: &Store, query: &str, filter: &Filter, limit: usize) -> Result<Vec<Hit>> {
    let conn = store.conn();
    let (lo, hi) = filter.bounds();
    // Passages allowed by the filters (cheap: no blobs).
    let allowed: HashSet<i64> = {
        let mut stmt = conn.prepare(
            "SELECT p.id FROM passages p JOIN meetings m ON m.id = p.meeting_id
             WHERE m.started_at BETWEEN ?1 AND ?2 AND (?3 IS NULL OR m.id = ?3)",
        )?;
        stmt.query_map(params![lo, hi, filter.meeting], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    if allowed.is_empty() {
        return Ok(Vec::new());
    }

    // Keyword ranking (BM25).
    let mut bm25: Vec<i64> = Vec::new();
    if let Some(q) = fts_query(query) {
        let mut stmt = conn.prepare(
            "SELECT p.id FROM passages_fts f JOIN passages p ON p.id = f.rowid JOIN meetings m ON m.id = p.meeting_id
             WHERE passages_fts MATCH ?1 AND m.started_at BETWEEN ?2 AND ?3 AND (?4 IS NULL OR m.id = ?4)
             ORDER BY rank LIMIT ?5",
        )?;
        bm25 = stmt
            .query_map(params![q, lo, hi, filter.meeting, CANDIDATES as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
    }

    // Meaning ranking (cosine over normalized e5 vectors).
    let mut semantic: Vec<i64> = Vec::new();
    if let Some(qv) = embed::embed(&[format!("query: {query}")])?.and_then(|mut v| v.pop()) {
        let mut slot = cache().lock().unwrap();
        if slot.is_none() {
            let mut stmt = conn.prepare("SELECT id, embedding FROM passages WHERE embedding IS NOT NULL")?;
            let vectors = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, embed::from_bytes(&r.get::<_, Vec<u8>>(1)?))))?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?;
            *slot = Some(VecCache { vectors });
        }
        let mut scored: Vec<(i64, f32)> = slot
            .as_ref()
            .unwrap()
            .vectors
            .iter()
            .filter(|(id, _)| allowed.contains(id))
            .map(|(id, v)| (*id, v.iter().zip(&qv).map(|(a, b)| a * b).sum()))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        semantic = scored.into_iter().take(CANDIDATES).map(|(id, _)| id).collect();
    }

    // Reciprocal rank fusion.
    let mut fused: HashMap<i64, f64> = HashMap::new();
    for list in [&bm25, &semantic] {
        for (rank, id) in list.iter().enumerate() {
            *fused.entry(*id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    let mut ranked: Vec<(i64, f64)> = fused.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(limit);

    let mut stmt = conn.prepare(
        "SELECT p.meeting_id, m.title, m.started_at, p.kind, p.start_secs, p.end_secs, p.text
         FROM passages p JOIN meetings m ON m.id = p.meeting_id WHERE p.id = ?1",
    )?;
    let mut hits = Vec::with_capacity(ranked.len());
    for (id, score) in ranked {
        let hit = stmt.query_row([id], |r| {
            Ok(Hit {
                passage_id: id,
                meeting_id: r.get(0)?,
                title: r.get(1)?,
                started_at: Local.timestamp_opt(r.get(2)?, 0).single().unwrap_or_else(Local::now),
                kind: r.get(3)?,
                start_secs: r.get(4)?,
                end_secs: r.get(5)?,
                text: r.get(6)?,
                score,
            })
        })?;
        hits.push(hit);
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_drops_stopwords_and_prefixes() {
        assert_eq!(fts_query("Was hat Anna über die Prognose gesagt?").unwrap(), "\"anna\"* OR \"prognose\"*");
        assert!(fts_query("was ist das").is_none());
    }
}
