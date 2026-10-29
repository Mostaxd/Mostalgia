//! Retrieval eval: builds a small library (meetings + documents) in a temp
//! data dir, then checks whether each question's answer is in the top hits
//! of the same search the Ask tools use. Prints recall@k and MRR per type.
//!
//! cargo run -p mostalgia-core --example rag_eval [-- corpus.json]

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{Local, NaiveDateTime, TimeZone};
use mostalgia_core::audio::Source;
use mostalgia_core::finalize::FinalSegment;
use mostalgia_core::store::{NewMeeting, Store};
use mostalgia_core::{ask, docs, paths};
use serde_json::Value;

const KS: [usize; 3] = [1, 5, 15];

fn main() -> Result<()> {
    let corpus = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/rag_eval.json")));
    let c: Value = serde_json::from_str(&std::fs::read_to_string(&corpus)?)?;
    let dir = std::env::temp_dir().join(format!("rag-eval-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    if std::env::var_os("MOSTALGIA_DATA_DIR").is_none() {
        // SAFETY: single-threaded, before anything reads it.
        unsafe { std::env::set_var("MOSTALGIA_DATA_DIR", &dir) };
    }
    println!("embedding model: {}", if ask::embed::available() { "local e5 (keywords + meaning)" } else { "MISSING (keywords only)" });

    let mut ids: BTreeMap<String, (bool, i64)> = BTreeMap::new();
    let mut store = Store::open(&paths::database())?;
    for m in c["meetings"].as_array().unwrap() {
        let at = NaiveDateTime::parse_from_str(m["date"].as_str().unwrap(), "%Y-%m-%d %H:%M")?;
        let lines = m["lines"].as_array().unwrap();
        let segs: Vec<FinalSegment> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let start = l[0].as_f64().unwrap();
                let end = lines.get(i + 1).and_then(|n| n[0].as_f64()).unwrap_or(start + 20.);
                let speaker = l[1].as_str().unwrap();
                FinalSegment {
                    source: if speaker == "Me" { Source::Mic } else { Source::System },
                    start_secs: start,
                    end_secs: end,
                    text: l[2].as_str().unwrap().into(),
                    speaker: (speaker != "Me" && speaker != "Others").then(|| speaker.into()),
                }
            })
            .collect();
        let mdir = dir.join(m["key"].as_str().unwrap());
        std::fs::create_dir_all(&mdir)?;
        let id = store.insert_meeting(
            &NewMeeting {
                title: m["title"].as_str().unwrap(),
                started_at: Local.from_local_datetime(&at).unwrap(),
                duration_secs: segs.last().map_or(0., |s| s.end_secs),
                language: m["language"].as_str().unwrap(),
                dir: &mdir,
            },
            &segs,
        )?;
        if let Some(s) = m["summary"].as_str() {
            store.save_summary(id, "eval", "eval", None, s)?;
        }
        ids.insert(m["key"].as_str().unwrap().into(), (true, id));
    }
    drop(store);
    ask::index_pending()?;
    for d in c["documents"].as_array().unwrap() {
        let f = dir.join(d["file"].as_str().unwrap());
        std::fs::write(&f, d["text"].as_str().unwrap())?;
        let (id, _) = docs::add(&f)?;
        docs::index(id)?;
        ids.insert(d["key"].as_str().unwrap().into(), (false, id));
    }

    // type -> (n, hits@k..., reciprocal ranks)
    let mut stats: BTreeMap<String, (usize, [usize; 3], f64)> = BTreeMap::new();
    let mut misses = Vec::new();
    for q in c["questions"].as_array().unwrap() {
        let text = q["q"].as_str().unwrap();
        let gold = q["gold"].as_array().unwrap();
        let doc_q = gold[0].get("d").is_some();
        // The texts of the ranked hits, with the meeting/document they're from.
        let ranked: Vec<(i64, String)> = if doc_q {
            docs::search(text, None, 15)?.into_iter().map(|h| (h.document_id, format!("{} {}", h.heading, h.text))).collect()
        } else {
            ask::search(text, &ask::Filter::default(), 15)?.into_iter().map(|h| (h.meeting_id, h.text)).collect()
        };
        let rank = ranked.iter().position(|(id, t)| {
            let t = t.to_lowercase();
            gold.iter().any(|g| {
                let key = g.get("m").or_else(|| g.get("d")).and_then(Value::as_str).unwrap();
                ids.get(key).is_some_and(|(_, gid)| gid == id) && t.contains(&g["contains"].as_str().unwrap().to_lowercase())
            })
        });
        let e = stats.entry(q["type"].as_str().unwrap().into()).or_default();
        e.0 += 1;
        for (i, k) in KS.iter().enumerate() {
            if rank.is_some_and(|r| r < *k) {
                e.1[i] += 1;
            }
        }
        e.2 += rank.map_or(0., |r| 1. / (r as f64 + 1.));
        if rank.is_none_or(|r| r >= 5) {
            misses.push(format!("  {:>4}  [{}] {text}", rank.map_or("-".into(), |r| (r + 1).to_string()), q["type"].as_str().unwrap()));
        }
    }
    println!("\n{:<14} {:>3} {:>6} {:>6} {:>6} {:>6}", "type", "n", "R@1", "R@5", "R@15", "MRR");
    let mut all = (0, [0; 3], 0.);
    for (t, (n, h, rr)) in &stats {
        println!("{t:<14} {n:>3} {:>6.2} {:>6.2} {:>6.2} {:>6.2}", h[0] as f64 / *n as f64, h[1] as f64 / *n as f64, h[2] as f64 / *n as f64, rr / *n as f64);
        all.0 += n;
        for i in 0..3 {
            all.1[i] += h[i];
        }
        all.2 += rr;
    }
    let n = all.0 as f64;
    println!("{:<14} {:>3} {:>6.2} {:>6.2} {:>6.2} {:>6.2}", "ALL", all.0, all.1[0] as f64 / n, all.1[1] as f64 / n, all.1[2] as f64 / n, all.2 / n);
    println!("\nnot in the top 5 (rank, '-' = not in top 15):");
    for m in misses {
        println!("{m}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
