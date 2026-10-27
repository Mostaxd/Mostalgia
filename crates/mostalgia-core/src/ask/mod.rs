//! "Ask your meetings": a local hybrid search index over transcripts and
//! summaries, and question answering on top of it with cited sources.

pub mod answer;
pub mod cite;
pub mod embed;
pub mod index;
pub mod search;

pub use answer::{AskEvent, Citation, Piece, ask, parse_answer};
pub use search::{Filter, Hit};

use anyhow::Result;

use crate::paths;
use crate::store::Store;

/// (Re)indexes one meeting in the library (after a final pass or a new
/// summary). Returns the number of passages.
pub fn index_meeting(meeting_id: i64) -> Result<usize> {
    let mut store = Store::open(&paths::database())?;
    index::index_meeting(&mut store, meeting_id)
}

/// Indexes all meetings that are new or changed (one-time backfill, and a
/// cheap no-op afterwards). Returns how many were indexed.
pub fn index_pending() -> Result<usize> {
    index_pending_until(&std::sync::atomic::AtomicBool::new(false))
}

/// Like [`index_pending`], but stops between meetings once `cancel` is set
/// (so an app can quit without waiting for a long backfill).
pub fn index_pending_until(cancel: &std::sync::atomic::AtomicBool) -> Result<usize> {
    let mut store = Store::open(&paths::database())?;
    index::index_pending(&mut store, cancel)
}

/// Hybrid search over the library.
pub fn search(query: &str, filter: &Filter, limit: usize) -> Result<Vec<Hit>> {
    let store = Store::open(&paths::database())?;
    search::search(&store, query, filter, limit)
}
