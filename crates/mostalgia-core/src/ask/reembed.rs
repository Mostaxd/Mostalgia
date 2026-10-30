//! Switching the embedding model. Vectors of two models can't be compared,
//! so the library is embedded again with the new model into
//! `embedding_next` while searches keep using the old vectors; once every
//! passage and document chunk has a new vector, both columns are swapped
//! in one transaction and the new model becomes the active one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::embed::{self, LOCAL_KEY, Model};
use crate::config::Config;
use crate::paths;
use crate::store::Store;

/// Passages and chunks embedded per step (one cloud request).
const STEP: usize = 64;

fn state(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM embed_state WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

fn set_state(conn: &Connection, key: &str, value: Option<&str>) -> Result<()> {
    match value {
        Some(v) => conn.execute(
            "INSERT INTO embed_state (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, v],
        )?,
        None => conn.execute("DELETE FROM embed_state WHERE key = ?1", [key])?,
    };
    Ok(())
}

/// Key of the model the searchable vectors come from.
pub(crate) fn active_key(conn: &Connection) -> Result<String> {
    Ok(state(conn, "active")?.unwrap_or_else(|| LOCAL_KEY.into()))
}

/// The model the searchable vectors come from (queries must use it too).
pub(crate) fn active_model(conn: &Connection) -> Result<Model> {
    Ok(Model::from_key(&active_key(conn)?, &Config::load()))
}

/// The model searches use right now.
pub fn active() -> Result<Model> {
    active_model(Store::open(&paths::database())?.conn())
}

/// Whether the library still has to be embedded with the model chosen in
/// the settings.
pub fn pending() -> Result<bool> {
    let store = Store::open(&paths::database())?;
    Ok(active_key(store.conn())? != Model::configured().key())
}

fn missing(conn: &Connection) -> Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM passages WHERE embedding_next IS NULL)
              + (SELECT COUNT(*) FROM doc_chunks WHERE embedding_next IS NULL)",
        [],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

fn clear_next(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "UPDATE passages SET embedding_next = NULL WHERE embedding_next IS NOT NULL;
         UPDATE doc_chunks SET embedding_next = NULL WHERE embedding_next IS NOT NULL;
         DELETE FROM embed_state WHERE key = 'next';",
    )?;
    Ok(())
}

/// The next rows without a new vector: (is a document chunk, id, text).
fn next_rows(conn: &Connection) -> Result<Vec<(bool, i64, String)>> {
    let mut stmt = conn.prepare("SELECT id, text FROM passages WHERE embedding_next IS NULL LIMIT ?1")?;
    let rows: Vec<(bool, i64, String)> =
        stmt.query_map([STEP as i64], |r| Ok((false, r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    if !rows.is_empty() {
        return Ok(rows);
    }
    let mut stmt = conn.prepare(
        "SELECT c.id, d.title, c.heading, c.text FROM doc_chunks c JOIN documents d ON d.id = c.document_id
         WHERE c.embedding_next IS NULL LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([STEP as i64], |r| {
            let (title, heading, text): (String, String, String) = (r.get(1)?, r.get(2)?, r.get(3)?);
            Ok((true, r.get(0)?, crate::docs::embed_text(&title, &heading, &text)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Embeds the library with the model chosen in the settings and makes it
/// the active one. Calls `progress(done, total)` after each step; stops
/// between steps once `cancel` is set or the choice changes again (the
/// vectors made so far are kept for the next run). Returns whether the
/// model was switched.
pub fn run(cancel: &AtomicBool, mut progress: impl FnMut(usize, usize)) -> Result<bool> {
    let target = Model::configured();
    let key = target.key();
    let mut store = Store::open(&paths::database())?;
    let conn = store.conn_mut();
    if active_key(conn)? == key {
        if state(conn, "next")?.is_some() {
            clear_next(conn)?;
        }
        return Ok(false);
    }
    if state(conn, "next")?.as_deref() != Some(key.as_str()) {
        clear_next(conn)?;
        set_state(conn, "next", Some(&key))?;
    }
    if !target.ready() {
        bail!("{}", target.not_ready_reason());
    }
    let total = missing(conn)?;
    let mut done = 0;
    progress(done, total);
    loop {
        if cancel.load(Ordering::Relaxed) || Model::configured().key() != key {
            return Ok(false);
        }
        let rows = next_rows(conn)?;
        if rows.is_empty() {
            // Rows added meanwhile are caught here: the check and the swap
            // happen under one write lock.
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if missing(&tx)? > 0 {
                continue;
            }
            tx.execute_batch(
                "UPDATE passages SET embedding = embedding_next, embedding_next = NULL;
                 UPDATE doc_chunks SET embedding = embedding_next, embedding_next = NULL;",
            )?;
            set_state(&tx, "active", Some(&key))?;
            set_state(&tx, "next", None)?;
            tx.commit()?;
            super::search::invalidate_cache();
            crate::docs::invalidate_cache();
            progress(total.max(done), total.max(done));
            return Ok(true);
        }
        let texts: Vec<String> = rows.iter().map(|(_, _, t)| t.clone()).collect();
        let vectors = target.passages(&texts)?.with_context(|| target.not_ready_reason())?;
        let tx = conn.transaction()?;
        for ((is_doc, id, _), v) in rows.iter().zip(&vectors) {
            let table = if *is_doc { "doc_chunks" } else { "passages" };
            tx.execute(&format!("UPDATE {table} SET embedding_next = ?2 WHERE id = ?1"), params![id, embed::to_bytes(v)])?;
        }
        tx.commit()?;
        done += rows.len();
        progress(done, total.max(done));
    }
}

/// Where the background switch is, for the settings.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    /// Why the last run stopped (it is tried again on the next start).
    pub error: Option<String>,
}

#[derive(Default)]
struct Job {
    running: bool,
    /// Started again while running: run once more afterwards.
    again: bool,
    handle: Option<JoinHandle<()>>,
}

fn status_cell() -> &'static Mutex<Status> {
    static CELL: OnceLock<Mutex<Status>> = OnceLock::new();
    CELL.get_or_init(Default::default)
}

fn job() -> &'static Mutex<Job> {
    static CELL: OnceLock<Mutex<Job>> = OnceLock::new();
    CELL.get_or_init(Default::default)
}

static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn status() -> Status {
    status_cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Runs [`run`] on a background thread (once more if it's already running,
/// so a choice changed mid-run is picked up).
pub fn start() {
    let mut j = job().lock().unwrap_or_else(|e| e.into_inner());
    if j.running {
        j.again = true;
        return;
    }
    j.running = true;
    CANCEL.store(false, Ordering::Relaxed);
    if let Some(old) = j.handle.take() {
        let _ = old.join();
    }
    j.handle = Some(std::thread::spawn(|| {
        loop {
            *status_cell().lock().unwrap_or_else(|e| e.into_inner()) = Status { running: true, ..Default::default() };
            let result = run(&CANCEL, |done, total| {
                let mut s = status_cell().lock().unwrap_or_else(|e| e.into_inner());
                s.done = done;
                s.total = total;
            });
            if let Err(e) = &result {
                eprintln!("[mostalgia] embeddings: {e:#}");
            }
            let mut j = job().lock().unwrap_or_else(|e| e.into_inner());
            let mut s = status_cell().lock().unwrap_or_else(|e| e.into_inner());
            s.error = result.err().map(|e| format!("{e:#}"));
            if j.again && !CANCEL.load(Ordering::Relaxed) {
                j.again = false;
                continue;
            }
            j.running = false;
            s.running = false;
            break;
        }
    }));
}

/// Stops a background run between two steps and waits for it (before the
/// app quits: exiting mid-embedding crashes in native teardown).
pub fn stop() {
    CANCEL.store(true, Ordering::Relaxed);
    let handle = job().lock().unwrap_or_else(|e| e.into_inner()).handle.take();
    if let Some(h) = handle {
        let _ = h.join();
    }
}
