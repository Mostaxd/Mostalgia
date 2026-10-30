//! The documents drive: files the user adds (Markdown, text, PDF, Word,
//! OpenDocument, HTML; English, German or any other language) are copied
//! into the data directory, split into chunks, embedded with the same
//! multilingual e5 model as the meetings and FTS-indexed, so answers can
//! search and cite them ([[d:ID@LOC]]: a PDF page, else a section).
//! Everything stays on this computer; only the chunks a question finds go
//! to the language model.

pub mod extract;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, TimeZone};
use rusqlite::{OptionalExtension, params};

pub use extract::Kind;

use crate::ask::embed;
use crate::i18n::{tr, trf};
use crate::paths;
use crate::store::Store;

/// Chunk length to aim for (characters): well inside e5's 512 tokens.
const CHUNK_CHARS: usize = 1_100;
/// Text repeated from the end of one chunk at the start of the next.
const OVERLAP_CHARS: usize = 150;
/// Largest file accepted.
pub const MAX_BYTES: u64 = 100 * 1024 * 1024;
/// Candidates per ranking before fusion, and the RRF constant.
const CANDIDATES: usize = 50;
const RRF_K: f64 = 60.0;

pub fn dir() -> PathBuf {
    paths::data_dir().join("documents")
}

/// A document in the drive.
#[derive(Clone, Debug)]
pub struct Document {
    pub id: i64,
    pub title: String,
    pub file_name: String,
    pub path: PathBuf,
    pub kind: Kind,
    /// "de", "en" or "" (other / unknown).
    pub language: String,
    pub size_bytes: u64,
    pub pages: Option<u32>,
    pub chunks: usize,
    pub added_at: DateTime<Local>,
    /// `None` while it is being indexed (or was never indexed).
    pub indexed_at: Option<DateTime<Local>>,
    /// Why it couldn't be read.
    pub error: Option<String>,
    /// Its chunks have embeddings (meaning search); without the model
    /// only keyword search finds it.
    pub embedded: bool,
    /// Answers (Ask, the chat, live suggestions) search it; switched off,
    /// it stays in the drive but isn't used.
    pub active: bool,
    /// The collection it belongs to (`None`: none).
    pub collection_id: Option<i64>,
    /// Answers use it: it is switched on and so is its collection.
    pub used: bool,
}

/// A group of documents (say, one project's), switched on or off for
/// answers as a whole.
#[derive(Clone, Debug)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub active: bool,
    pub documents: usize,
}

/// SQL: whether answers use document `d` (it and its collection are on).
const USED: &str = "(d.active AND COALESCE((SELECT k.active FROM doc_collections k WHERE k.id = d.collection_id), 1))";

impl Document {
    /// "p. 4" for a PDF page, "§4" for a section.
    pub fn loc_label(&self, loc: u32) -> String {
        loc_label(self.kind, loc)
    }
}

pub fn loc_label(kind: Kind, loc: u32) -> String {
    if kind.paged() { trf("S. {}", &[&loc]) } else { format!("§{loc}") }
}

/// A chunk of a document.
#[derive(Clone, Debug)]
pub struct Chunk {
    pub id: i64,
    pub loc: u32,
    pub heading: String,
    pub text: String,
}

/// A search result.
#[derive(Clone, Debug)]
pub struct DocHit {
    pub chunk_id: i64,
    pub document_id: i64,
    pub title: String,
    pub kind: Kind,
    pub loc: u32,
    pub heading: String,
    pub text: String,
    pub score: f64,
}

fn local(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_else(Local::now)
}

/// A stable content hash (FNV-1a, 64 bit) to spot the same file added twice.
fn hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// "de" or "en" from common words; "" when neither clearly wins.
pub fn guess_language(text: &str) -> &'static str {
    const DE: &[&str] = &["der", "die", "und", "das", "ist", "nicht", "mit", "ein", "eine", "für", "auf", "sich", "werden", "auch", "wird", "oder", "dem", "den", "zu", "von"]; // i18n: not UI
    const EN: &[&str] = &["the", "and", "is", "of", "to", "in", "that", "for", "with", "this", "are", "be", "on", "it", "as", "not", "or", "by", "from", "will"];
    let (mut de, mut en) = (0, 0);
    for w in text.split(|c: char| !c.is_alphabetic()).take(5_000) {
        let w = w.to_lowercase();
        de += DE.contains(&w.as_str()) as usize;
        en += EN.contains(&w.as_str()) as usize;
    }
    if de + en < 3 {
        ""
    } else if de * 2 > en * 3 {
        "de"
    } else if en * 2 > de * 3 {
        "en"
    } else {
        ""
    }
}

/// Splits sections into chunks of about [`CHUNK_CHARS`]: at paragraph
/// breaks, else sentence ends, else spaces, with a little overlap.
pub fn chunk(sections: &[extract::Section]) -> Vec<extract::Section> {
    let mut out = Vec::new();
    for s in sections {
        let text = s.text.trim();
        if text.chars().count() <= CHUNK_CHARS {
            if !text.is_empty() {
                out.push(extract::Section { text: text.to_string(), ..s.clone() });
            }
            continue;
        }
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut start = 0; // char index
        while start < chars.len() {
            let hard_end = (start + CHUNK_CHARS).min(chars.len());
            let mut end = hard_end;
            if hard_end < chars.len() {
                let window = &chars[start..hard_end];
                let min = window.len() / 2;
                let find = |pred: &dyn Fn(usize) -> bool| (min..window.len()).rev().find(|&i| pred(i));
                let byte = |i: usize| window[i].0;
                end = find(&|i| text[byte(i)..].starts_with("\n\n"))
                    .or_else(|| find(&|i| matches!(window[i].1, '.' | '!' | '?' | '\n') && window.get(i + 1).is_some_and(|c| c.1.is_whitespace())).map(|i| i + 1))
                    .or_else(|| find(&|i| window[i].1.is_whitespace()))
                    .map_or(hard_end, |i| start + i);
            }
            let from = chars[start].0;
            let to = chars.get(end).map_or(text.len(), |c| c.0);
            let piece = text[from..to].trim();
            if !piece.is_empty() {
                out.push(extract::Section { loc: s.loc, heading: s.heading.clone(), text: piece.to_string() });
            }
            if end >= chars.len() {
                break;
            }
            // Overlap: back up to a word start within the last OVERLAP_CHARS.
            let mut next = end.saturating_sub(OVERLAP_CHARS).max(start + 1);
            while next < end && !chars[next - 1].1.is_whitespace() {
                next += 1;
            }
            start = next;
        }
    }
    out
}

/// What gets embedded for a chunk: e5's passage prefix, the document and
/// heading, then the text.
/// What a chunk is embedded as: the document's title (and the heading)
/// before its text.
pub(crate) fn embed_text(title: &str, heading: &str, text: &str) -> String {
    if heading.is_empty() { format!("{title}\n{text}") } else { format!("{title} — {heading}\n{text}") }
}

/// Adds a file to the drive: copies it into the data directory and
/// records it (not indexed yet, see [`index`]). The same file added again
/// returns the existing document and `false`.
pub fn add(path: &Path) -> Result<(i64, bool)> {
    let kind = Kind::from_path(path).with_context(|| trf("Dateityp wird nicht unterstützt: {}", &[&path.display()]))?;
    let meta = std::fs::metadata(path).with_context(|| trf("{} konnte nicht gelesen werden", &[&path.display()]))?;
    if meta.len() > MAX_BYTES {
        bail!("{}", trf("{} ist zu groß (höchstens {} MB)", &[&path.display(), &(MAX_BYTES / 1024 / 1024)]));
    }
    let bytes = std::fs::read(path).with_context(|| trf("{} konnte nicht gelesen werden", &[&path.display()]))?;
    let h = hash(&bytes);
    let store = Store::open(&paths::database())?;
    let conn = store.conn();
    if let Some(id) = conn.query_row("SELECT id FROM documents WHERE hash = ?1 AND size_bytes = ?2", params![h, bytes.len() as i64], |r| r.get(0)).optional()? {
        return Ok((id, false));
    }
    let file_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "document".into());
    let title = path.file_stem().map(|n| n.to_string_lossy().replace(['_'], " ")).unwrap_or_else(|| file_name.clone());
    conn.execute(
        "INSERT INTO documents (title, file_name, path, kind, size_bytes, hash, added_at) VALUES (?1, ?2, '', ?3, ?4, ?5, ?6)",
        params![title, file_name, kind.id(), bytes.len() as i64, h, Local::now().timestamp()],
    )?;
    let id = conn.last_insert_rowid();
    let safe: String = file_name.chars().map(|c| if c.is_alphanumeric() || ".-_ ".contains(c) { c } else { '_' }).collect();
    let dest = dir().join(format!("{id}-{safe}"));
    let copied = std::fs::create_dir_all(dir()).and_then(|_| std::fs::write(&dest, &bytes));
    if let Err(e) = copied {
        conn.execute("DELETE FROM documents WHERE id = ?1", [id])?;
        return Err(e).with_context(|| trf("{} konnte nicht gespeichert werden", &[&dest.display()]));
    }
    conn.execute("UPDATE documents SET path = ?2 WHERE id = ?1", params![id, dest.to_string_lossy()])?;
    Ok((id, true))
}

/// Puts a Markdown text the app wrote (a meeting's summary and notes) into
/// the drive as `file_name`, titled `title`. A document of the same file
/// name made this way before is replaced, so saving again updates it.
/// Returns its id and whether it is new; it still needs [`index`].
pub fn put_markdown(title: &str, file_name: &str, text: &str) -> Result<(i64, bool)> {
    let bytes = text.as_bytes();
    let h = hash(bytes);
    let store = Store::open(&paths::database())?;
    let conn = store.conn();
    let existing: Option<(i64, String)> = conn
        .query_row("SELECT id, path FROM documents WHERE file_name = ?1 AND kind = ?2 ORDER BY id DESC LIMIT 1", params![file_name, Kind::Markdown.id()], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    if let Some((id, path)) = existing.filter(|(_, p)| Path::new(p).starts_with(dir())) {
        std::fs::write(&path, bytes).with_context(|| trf("{} konnte nicht gespeichert werden", &[&path]))?;
        conn.execute(
            "UPDATE documents SET title = ?2, size_bytes = ?3, hash = ?4, indexed_at = NULL, error = NULL WHERE id = ?1",
            params![id, title, bytes.len() as i64, h],
        )?;
        invalidate_cache();
        return Ok((id, false));
    }
    conn.execute(
        "INSERT INTO documents (title, file_name, path, kind, size_bytes, hash, added_at) VALUES (?1, ?2, '', ?3, ?4, ?5, ?6)",
        params![title, file_name, Kind::Markdown.id(), bytes.len() as i64, h, Local::now().timestamp()],
    )?;
    let id = conn.last_insert_rowid();
    let safe: String = file_name.chars().map(|c| if c.is_alphanumeric() || ".-_ ".contains(c) { c } else { '_' }).collect();
    let dest = dir().join(format!("{id}-{safe}"));
    if let Err(e) = std::fs::create_dir_all(dir()).and_then(|_| std::fs::write(&dest, bytes)) {
        conn.execute("DELETE FROM documents WHERE id = ?1", [id])?;
        return Err(e).with_context(|| trf("{} konnte nicht gespeichert werden", &[&dest.display()]));
    }
    conn.execute("UPDATE documents SET path = ?2 WHERE id = ?1", params![id, dest.to_string_lossy()])?;
    invalidate_cache();
    Ok((id, true))
}

/// Reads, chunks and embeds a document (again). A file that can't be read
/// keeps its entry with the error. Returns the number of chunks.
pub fn index(id: i64) -> Result<usize> {
    let mut store = Store::open(&paths::database())?;
    let doc = get_in(&store, id)?.context(tr("Dokument nicht gefunden"))?;
    let extracted = extract::extract(&doc.path, doc.kind);
    let conn = store.conn_mut();
    let (sections, pages) = match extracted {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("{e:#}");
            conn.execute("DELETE FROM doc_chunks WHERE document_id = ?1", [id])?;
            conn.execute("UPDATE documents SET error = ?2, indexed_at = ?3 WHERE id = ?1", params![id, msg, Local::now().timestamp()])?;
            invalidate_cache();
            bail!("{msg}");
        }
    };
    let chunks = chunk(&sections);
    let all: String = sections.iter().map(|s| s.text.as_str()).take(40).collect::<Vec<_>>().join("\n");
    let language = guess_language(&all);
    let texts: Vec<String> = chunks.iter().map(|c| embed_text(&doc.title, &c.heading, &c.text)).collect();
    // Without vectors (offline, no key) the chunks are found by keywords
    // and get vectors later (see `index_pending`).
    let vectors = crate::ask::reembed::active_model(conn)?.passages(&texts).unwrap_or_else(|e| {
        eprintln!("[mostalgia] document {id}: {e:#}");
        None
    });
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM doc_chunks WHERE document_id = ?1", [id])?;
    {
        let mut stmt = tx.prepare("INSERT INTO doc_chunks (document_id, loc, heading, text, embedding) VALUES (?1, ?2, ?3, ?4, ?5)")?;
        for (k, c) in chunks.iter().enumerate() {
            let blob = vectors.as_ref().map(|v| embed::to_bytes(&v[k]));
            stmt.execute(params![id, c.loc, c.heading, c.text, blob])?;
        }
    }
    let error = chunks.is_empty().then(|| tr("Keine Textinhalte gefunden").to_string());
    tx.execute(
        "UPDATE documents SET language = ?2, pages = ?3, indexed_at = ?4, error = ?5 WHERE id = ?1",
        params![id, language, pages, Local::now().timestamp(), error],
    )?;
    tx.commit()?;
    invalidate_cache();
    Ok(chunks.len())
}

/// Documents indexed while the embedding model wasn't there get vectors
/// (and ones never indexed get indexed). Returns how many.
pub fn index_pending(cancel: &std::sync::atomic::AtomicBool) -> Result<usize> {
    let store = Store::open(&paths::database())?;
    let ids: Vec<i64> = {
        let mut stmt = store.conn().prepare(
            "SELECT d.id FROM documents d WHERE d.indexed_at IS NULL
                OR (d.error IS NULL AND ?1 AND EXISTS (SELECT 1 FROM doc_chunks c WHERE c.document_id = d.id AND c.embedding IS NULL))",
        )?;
        stmt.query_map([crate::ask::reembed::active_model(store.conn())?.ready()], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    drop(store);
    let mut done = 0;
    for id in ids {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        if let Err(e) = index(id) {
            eprintln!("[mostalgia] document {id}: {e:#}");
        }
        done += 1;
    }
    Ok(done)
}

const SELECT: &str = "SELECT d.id, d.title, d.file_name, d.path, d.kind, d.language, d.size_bytes, d.pages, d.added_at, d.indexed_at, d.error,
    (SELECT COUNT(*) FROM doc_chunks c WHERE c.document_id = d.id),
    EXISTS (SELECT 1 FROM doc_chunks c WHERE c.document_id = d.id AND c.embedding IS NOT NULL),
    d.active, d.collection_id,
    -- USED
    (d.active AND COALESCE((SELECT k.active FROM doc_collections k WHERE k.id = d.collection_id), 1))
    FROM documents d";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Document> {
    Ok(Document {
        id: r.get(0)?,
        title: r.get(1)?,
        file_name: r.get(2)?,
        path: PathBuf::from(r.get::<_, String>(3)?),
        kind: Kind::parse(&r.get::<_, String>(4)?).unwrap_or(Kind::Text),
        language: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        size_bytes: r.get::<_, i64>(6)? as u64,
        pages: r.get(7)?,
        added_at: local(r.get(8)?),
        indexed_at: r.get::<_, Option<i64>>(9)?.map(local),
        error: r.get(10)?,
        chunks: r.get::<_, i64>(11)? as usize,
        embedded: r.get(12)?,
        active: r.get(13)?,
        collection_id: r.get(14)?,
        used: r.get(15)?,
    })
}

fn get_in(store: &Store, id: i64) -> Result<Option<Document>> {
    Ok(store.conn().query_row(&format!("{SELECT} WHERE d.id = ?1"), [id], row).optional()?)
}

/// All documents, newest first.
pub fn list() -> Result<Vec<Document>> {
    let store = Store::open(&paths::database())?;
    let mut stmt = store.conn().prepare(&format!("{SELECT} ORDER BY d.added_at DESC, d.id DESC"))?;
    Ok(stmt.query_map([], row)?.collect::<rusqlite::Result<_>>()?)
}

pub fn get(id: i64) -> Result<Option<Document>> {
    get_in(&Store::open(&paths::database())?, id)
}

/// Whether the drive has any readable documents.
pub fn any() -> bool {
    Store::open(&paths::database())
        .and_then(|s| {
            Ok(s.conn().query_row(&format!("SELECT EXISTS (SELECT 1 FROM doc_chunks c JOIN documents d ON d.id = c.document_id WHERE {USED})"), [], |r| r.get::<_, bool>(0))?)
        })
        .unwrap_or(false)
}

/// A document's chunks in reading order.
pub fn chunks(id: i64) -> Result<Vec<Chunk>> {
    let store = Store::open(&paths::database())?;
    let mut stmt = store.conn().prepare("SELECT id, loc, heading, text FROM doc_chunks WHERE document_id = ?1 ORDER BY loc, id")?;
    Ok(stmt
        .query_map([id], |r| Ok(Chunk { id: r.get(0)?, loc: r.get(1)?, heading: r.get(2)?, text: r.get(3)? }))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn rename(id: i64, title: &str) -> Result<()> {
    let title = title.trim();
    anyhow::ensure!(!title.is_empty(), "{}", tr("Der Name darf nicht leer sein"));
    Store::open(&paths::database())?.conn().execute("UPDATE documents SET title = ?2 WHERE id = ?1", params![id, title])?;
    Ok(())
}

/// Removes a document, its chunks and its copy in the data directory.
pub fn delete(id: i64) -> Result<bool> {
    let store = Store::open(&paths::database())?;
    let Some(doc) = get_in(&store, id)? else { return Ok(false) };
    let tx = store.conn().unchecked_transaction()?;
    // Explicitly, so the FTS delete trigger runs for every chunk.
    tx.execute("DELETE FROM doc_chunks WHERE document_id = ?1", [id])?;
    tx.execute("DELETE FROM documents WHERE id = ?1", [id])?;
    tx.commit()?;
    if doc.path.starts_with(dir()) {
        let _ = std::fs::remove_file(&doc.path);
    }
    invalidate_cache();
    Ok(true)
}

/// Chunk embeddings kept between searches (reloaded after any change).
fn cache() -> &'static Mutex<Option<HashMap<i64, Vec<f32>>>> {
    static CELL: OnceLock<Mutex<Option<HashMap<i64, Vec<f32>>>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

pub fn invalidate_cache() {
    *cache().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// FTS5 query: OR of the significant words, each prefix-matched.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|w| !crate::ask::search::is_stopword(w))
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// Which documents a search looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Within {
    /// Those answers use (switched on, in a switched-on collection or none).
    #[default]
    Used,
    /// Every document in the drive.
    Drive,
    /// One document, or one collection's documents, switched off or not:
    /// asked about explicitly.
    Document(i64),
    Collection(i64),
}

impl Within {
    /// SQL condition on documents `d`.
    fn condition(self) -> String {
        match self {
            Within::Used => USED.to_string(),
            Within::Drive => "1".to_string(),
            Within::Document(id) => format!("d.id = {id}"),
            Within::Collection(id) => format!("d.collection_id = {id}"),
        }
    }
}

/// Hybrid search over the drive (keywords + meaning, fused like the
/// meeting search), for answers: switched-off documents are left out.
/// `only` limits it to one document.
pub fn search(query: &str, only: Option<i64>, limit: usize) -> Result<Vec<DocHit>> {
    search_within(query, only.map_or(Within::Used, Within::Document), limit)
}

/// Like [`search`], over every document in the drive (switched off too),
/// for the Documents screen.
pub fn search_drive(query: &str, limit: usize) -> Result<Vec<DocHit>> {
    search_within(query, Within::Drive, limit)
}

/// A document's text in reading order, section by section (page by page
/// for a PDF), read from its copy again: unlike the chunks, without
/// overlap and with its Markdown intact. Falls back to the chunks when the
/// file is gone or unreadable.
pub fn sections(id: i64) -> Result<Vec<extract::Section>> {
    let doc = get(id)?.with_context(|| tr("Dokument nicht gefunden"))?;
    match extract::extract(&doc.path, doc.kind) {
        Ok((sections, _)) if !sections.is_empty() => Ok(sections.into_iter().filter(|s| !s.text.trim().is_empty()).collect()),
        _ => Ok(chunks(id)?.into_iter().map(|c| extract::Section { loc: c.loc, heading: c.heading, text: c.text }).collect()),
    }
}

/// Switches a document on or off for answers.
pub fn set_active(id: i64, active: bool) -> Result<()> {
    Store::open(&paths::database())?.conn().execute("UPDATE documents SET active = ?2 WHERE id = ?1", params![id, active])?;
    Ok(())
}

/// All collections by name, with how many documents each holds.
pub fn collections() -> Result<Vec<Collection>> {
    let store = Store::open(&paths::database())?;
    let mut stmt = store.conn().prepare(
        "SELECT k.id, k.name, k.active, (SELECT COUNT(*) FROM documents d WHERE d.collection_id = k.id)
         FROM doc_collections k ORDER BY k.name COLLATE NOCASE",
    )?;
    Ok(stmt
        .query_map([], |r| Ok(Collection { id: r.get(0)?, name: r.get(1)?, active: r.get(2)?, documents: r.get::<_, i64>(3)? as usize }))?
        .collect::<rusqlite::Result<_>>()?)
}

fn collection_name(name: &str) -> Result<&str> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "{}", tr("Der Name darf nicht leer sein"));
    Ok(name)
}

/// A new collection (or the one with that name); returns its id.
pub fn create_collection(name: &str) -> Result<i64> {
    let name = collection_name(name)?;
    let store = Store::open(&paths::database())?;
    let conn = store.conn();
    if let Some(id) = conn.query_row("SELECT id FROM doc_collections WHERE name = ?1", [name], |r| r.get(0)).optional()? {
        return Ok(id);
    }
    conn.execute("INSERT INTO doc_collections (name, created_at) VALUES (?1, ?2)", params![name, Local::now().timestamp()])?;
    Ok(conn.last_insert_rowid())
}

pub fn rename_collection(id: i64, name: &str) -> Result<()> {
    let name = collection_name(name)?;
    let store = Store::open(&paths::database())?;
    let taken: bool = store.conn().query_row("SELECT EXISTS (SELECT 1 FROM doc_collections WHERE name = ?1 AND id != ?2)", params![name, id], |r| r.get(0))?;
    anyhow::ensure!(!taken, "{}", trf("Es gibt schon eine Sammlung „{}“", &[&name]));
    store.conn().execute("UPDATE doc_collections SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

/// Switches a whole collection on or off for answers (each document keeps
/// its own switch).
pub fn set_collection_active(id: i64, active: bool) -> Result<()> {
    Store::open(&paths::database())?.conn().execute("UPDATE doc_collections SET active = ?2 WHERE id = ?1", params![id, active])?;
    Ok(())
}

/// Removes a collection; its documents stay in the drive, in none.
pub fn delete_collection(id: i64) -> Result<bool> {
    Ok(Store::open(&paths::database())?.conn().execute("DELETE FROM doc_collections WHERE id = ?1", [id])? > 0)
}

/// Puts a document into a collection (`None`: into none).
pub fn set_collection(id: i64, collection: Option<i64>) -> Result<()> {
    Store::open(&paths::database())?.conn().execute("UPDATE documents SET collection_id = ?2 WHERE id = ?1", params![id, collection])?;
    Ok(())
}

/// Hybrid search over the documents `within`.
pub fn search_within(query: &str, within: Within, limit: usize) -> Result<Vec<DocHit>> {
    let store = Store::open(&paths::database())?;
    let conn = store.conn();
    let condition = within.condition();
    let allowed: HashSet<i64> = {
        let mut stmt = conn.prepare(&format!("SELECT c.id FROM doc_chunks c JOIN documents d ON d.id = c.document_id WHERE {condition}"))?;
        stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    if allowed.is_empty() {
        return Ok(Vec::new());
    }
    let mut bm25: Vec<i64> = Vec::new();
    if let Some(q) = fts_query(query) {
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id FROM doc_chunks_fts f JOIN doc_chunks c ON c.id = f.rowid JOIN documents d ON d.id = c.document_id
             WHERE doc_chunks_fts MATCH ?1 AND {condition} ORDER BY rank LIMIT ?2"
        ))?;
        bm25 = stmt.query_map(params![q, CANDIDATES as i64], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    }
    let mut semantic: Vec<i64> = Vec::new();
    let qv = crate::ask::reembed::active_model(conn)?.query(query).unwrap_or_else(|e| {
        eprintln!("[mostalgia] documents search: {e:#}");
        None
    });
    if let Some(qv) = qv {
        let mut slot = cache().lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            let mut stmt = conn.prepare("SELECT id, embedding FROM doc_chunks WHERE embedding IS NOT NULL")?;
            let vectors = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, embed::from_bytes(&r.get::<_, Vec<u8>>(1)?))))?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?;
            *slot = Some(vectors);
        }
        let mut scored: Vec<(i64, f32)> = slot
            .as_ref()
            .unwrap()
            .iter()
            .filter(|(id, _)| allowed.contains(id))
            .map(|(id, v)| (*id, embed::similarity(v, &qv)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        semantic = scored.into_iter().take(CANDIDATES).map(|(id, _)| id).collect();
    }
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
        "SELECT c.document_id, d.title, d.kind, c.loc, c.heading, c.text FROM doc_chunks c JOIN documents d ON d.id = c.document_id WHERE c.id = ?1",
    )?;
    let mut hits = Vec::with_capacity(ranked.len());
    for (id, score) in ranked {
        hits.push(stmt.query_row([id], |r| {
            Ok(DocHit {
                chunk_id: id,
                document_id: r.get(0)?,
                title: r.get(1)?,
                kind: Kind::parse(&r.get::<_, String>(2)?).unwrap_or(Kind::Text),
                loc: r.get(3)?,
                heading: r.get(4)?,
                text: r.get(5)?,
                score,
            })
        })?);
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use extract::Section;

    #[test]
    fn chunks_split_long_sections_with_overlap() {
        let para = "This is a sentence about the budget. ".repeat(80);
        let chunks = chunk(&[Section { loc: 3, heading: "Budget".into(), text: para.clone() }, Section { loc: 4, heading: String::new(), text: "Short.".into() }]);
        assert!(chunks.len() >= 3, "{}", chunks.len());
        assert!(chunks.iter().all(|c| c.text.chars().count() <= CHUNK_CHARS));
        assert!(chunks[..chunks.len() - 1].iter().all(|c| c.loc == 3 && c.heading == "Budget" && c.text.ends_with('.')));
        assert_eq!(chunks.last().unwrap().text, "Short.");
        // Consecutive chunks overlap a little.
        let tail: String = chunks[0].text.chars().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
        assert!(chunks[1].text.contains(tail.trim()));
    }

    #[test]
    fn guesses_language() {
        assert_eq!(guess_language("Die Prognose für das dritte Quartal ist nicht gut und wird mit dem Team besprochen."), "de");
        assert_eq!(guess_language("The forecast for the third quarter is not good and will be discussed with the team."), "en");
        assert_eq!(guess_language("Hola"), "");
    }

    #[test]
    fn add_index_search_delete() {
        let dir = std::env::temp_dir().join(format!("mostalgia-docs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: tests in this crate that touch the data dir run in this one test.
        unsafe { std::env::set_var("MOSTALGIA_DATA_DIR", &dir) };
        let file = dir.join("Onboarding Guide.md");
        std::fs::write(&file, "# Onboarding\nNew hires get a laptop on day one.\n## VPN\nThe VPN password rotates every 90 days.").unwrap();
        let (id, new) = add(&file).unwrap();
        assert!(new);
        assert_eq!(add(&file).unwrap(), (id, false));
        assert_eq!(index(id).unwrap(), 2);
        let doc = get(id).unwrap().unwrap();
        assert_eq!((doc.title.as_str(), doc.kind, doc.chunks), ("Onboarding Guide", Kind::Markdown, 2));
        let hits = search("VPN password rotation", None, 5).unwrap();
        assert_eq!(hits[0].heading, "Onboarding › VPN");
        set_active(id, false).unwrap();
        assert!(!get(id).unwrap().unwrap().active);
        assert!(search("VPN", None, 5).unwrap().is_empty());
        assert!(!any());
        assert_eq!(search_drive("VPN", 5).unwrap().len(), 1);
        set_active(id, true).unwrap();
        assert_eq!(search("VPN", None, 5).unwrap().len(), 1);
        // A collection switched off takes its documents with it.
        let k = create_collection(" IT ").unwrap();
        assert_eq!(create_collection("it").unwrap(), k);
        set_collection(id, Some(k)).unwrap();
        set_collection_active(k, false).unwrap();
        let doc = get(id).unwrap().unwrap();
        assert!(doc.active && !doc.used && doc.collection_id == Some(k));
        assert!(search("VPN", None, 5).unwrap().is_empty());
        // Asked about explicitly, a switched-off collection is searched.
        assert_eq!(search_within("VPN", Within::Collection(k), 5).unwrap().len(), 1);
        assert_eq!(sections(id).unwrap()[1].text, "The VPN password rotates every 90 days.");
        assert_eq!(search_drive("VPN", 5).unwrap().len(), 1);
        let c = &collections().unwrap()[0];
        assert_eq!((c.name.as_str(), c.active, c.documents), ("IT", false, 1));
        let k2 = create_collection("Onboarding").unwrap();
        assert!(rename_collection(k2, "it").is_err());
        assert!(delete_collection(k).unwrap());
        let doc = get(id).unwrap().unwrap();
        assert!(doc.used && doc.collection_id.is_none());
        assert_eq!(search("VPN", None, 5).unwrap().len(), 1);
        assert!(delete(id).unwrap());
        assert!(search("VPN", None, 5).unwrap().is_empty());
        // A meeting's summary saved into the drive, then saved again.
        let (m, new) = put_markdown("Weekly", "2026-10-29-Weekly-summary-notes.md", "# Weekly\nThe launch moves to March.").unwrap();
        assert!(new);
        index(m).unwrap();
        assert_eq!(search("launch", None, 5).unwrap().len(), 1);
        let again = put_markdown("Weekly sync", "2026-10-29-Weekly-summary-notes.md", "# Weekly\nThe launch moves to April.").unwrap();
        assert_eq!(again, (m, false));
        index(m).unwrap();
        let doc = get(m).unwrap().unwrap();
        assert_eq!(doc.title, "Weekly sync");
        assert_eq!(sections(m).unwrap()[0].text, "The launch moves to April.");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
