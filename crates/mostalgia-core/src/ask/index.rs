//! Building the passage index: transcript windows plus summary sections,
//! each embedded for meaning search and FTS-indexed for keywords.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::embed;
use crate::chunker::format_ts;
use crate::store::{Meeting, Store};

/// Target transcript window length…
const WINDOW_SECS: f64 = 90.0;
/// …with this much overlap into the next window.
const OVERLAP_SECS: f64 = 15.0;

pub struct NewPassage {
    pub kind: &'static str,
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

/// What changes in a meeting that the index depends on.
fn fingerprint(m: &Meeting) -> String {
    let words: usize = m.segments.iter().map(|s| s.text.len()).sum();
    let speakers: String = m.segments.iter().filter_map(|s| s.speaker.as_deref()).collect::<Vec<_>>().join(",");
    format!(
        "v1|{}|{}|{}|{}|{}",
        m.title,
        m.segments.len(),
        words,
        speakers.len(),
        m.summary.as_ref().map_or(0, |s| s.created_at.timestamp())
    )
}

fn header(m: &Meeting) -> String {
    format!("Besprechung: {} · {}", m.title, m.started_at.format("%d.%m.%Y %H:%M"))
}

pub fn build_passages(m: &Meeting) -> Vec<NewPassage> {
    let mut out = Vec::new();
    let head = header(m);
    let segs = &m.segments;
    let mut i = 0;
    while i < segs.len() {
        let start = segs[i].start_secs;
        let mut j = i;
        let mut lines = Vec::new();
        while j < segs.len() && (j == i || segs[j].start_secs - start < WINDOW_SECS) {
            let s = &segs[j];
            lines.push(format!("[{}] {}: {}", format_ts(s.start_secs), s.speaker_label(), s.text));
            j += 1;
        }
        let end = segs[j - 1].end_secs;
        out.push(NewPassage {
            kind: "transcript",
            start_secs: start,
            end_secs: end,
            text: format!("{head}\n{}", lines.join("\n")),
        });
        if j >= segs.len() {
            break;
        }
        // Next window starts with the lines of the last ~15 s.
        let mut next = j;
        while next > i + 1 && segs[next - 1].start_secs > end - OVERLAP_SECS {
            next -= 1;
        }
        i = next.max(i + 1);
    }
    if let Some(summary) = &m.summary {
        let mut section = String::new();
        let mut heading = String::new();
        let flush = |heading: &str, body: &str, out: &mut Vec<NewPassage>| {
            if body.trim().is_empty() {
                return;
            }
            let h = heading.to_lowercase();
            let kind = if h.contains("aufgab") || h.contains("task") || h.contains("action") || h.contains("المهام") {
                "tasks"
            } else {
                "summary"
            };
            out.push(NewPassage {
                kind,
                start_secs: 0.0,
                end_secs: 0.0,
                text: format!("{head}\nZusammenfassung – {heading}\n{}", body.trim()),
            });
        };
        for line in summary.markdown.lines() {
            if let Some(h) = line.strip_prefix("## ") {
                flush(&heading, &section, &mut out);
                heading = h.trim().to_string();
                section.clear();
            } else if !line.starts_with("# ") {
                section.push_str(line);
                section.push('\n');
            }
        }
        flush(&heading, &section, &mut out);
    }
    out
}

/// (Re)indexes one meeting. Returns the number of passages written.
pub fn index_meeting(store: &mut Store, meeting_id: i64) -> Result<usize> {
    let Some(m) = store.get(meeting_id)? else {
        return Ok(0);
    };
    let passages = build_passages(&m);
    let texts: Vec<String> = passages.iter().map(|p| format!("passage: {}", p.text)).collect();
    let t = std::time::Instant::now();
    let vectors = embed::embed(&texts)?;
    if std::env::var_os("MOSTALGIA_TIMING").is_some() {
        let chars: usize = texts.iter().map(|t| t.len()).sum();
        eprintln!("[timing] meeting {meeting_id}: {} passages ({chars} chars) embedded in {} ms", texts.len(), t.elapsed().as_millis());
    }
    let fp = fingerprint(&m);
    let tx = store.conn_mut().transaction()?;
    tx.execute("DELETE FROM passages WHERE meeting_id = ?1", [meeting_id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO passages (meeting_id, kind, start_secs, end_secs, text, embedding) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (k, p) in passages.iter().enumerate() {
            let blob = vectors.as_ref().map(|v| embed::to_bytes(&v[k]));
            stmt.execute(params![meeting_id, p.kind, p.start_secs, p.end_secs, p.text, blob])?;
        }
    }
    // Without the model the fingerprint stays unset so a later run adds vectors.
    if vectors.is_some() {
        tx.execute(
            "INSERT INTO index_state (meeting_id, fingerprint) VALUES (?1, ?2)
             ON CONFLICT(meeting_id) DO UPDATE SET fingerprint = excluded.fingerprint",
            params![meeting_id, fp],
        )?;
    }
    tx.commit()?;
    super::search::invalidate_cache();
    Ok(passages.len())
}

/// Indexes every meeting that is new or changed since it was indexed.
/// Returns how many meetings were (re)indexed.
pub fn index_pending(store: &mut Store, cancel: &std::sync::atomic::AtomicBool) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt = store.conn().prepare("SELECT id FROM meetings ORDER BY started_at DESC")?;
        stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let mut done = 0;
    for id in ids {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let Some(m) = store.get(id)? else { continue };
        let stored: Option<String> = store
            .conn()
            .query_row("SELECT fingerprint FROM index_state WHERE meeting_id = ?1", [id], |r| r.get(0))
            .optional()?;
        if stored.as_deref() != Some(fingerprint(&m).as_str()) {
            index_meeting(store, id)?;
            done += 1;
        }
    }
    Ok(done)
}
