//! Library export and import: the whole library in one `.mostalgia` file
//! (a zip archive), to move it to another computer or keep a backup.
//!
//! Inside: `manifest.json` (format version, app and database version,
//! what's included, one line per meeting), `meetings/<key>/meeting.json`
//! per meeting (transcript, summary, bookmarks, notes and their earlier
//! versions, the chat, decisions) and, optionally, its WAV tracks next to
//! it; optionally `documents/` (the documents drive with its collections)
//! and `voices.json` (known voice prints, opt-in: they identify people).
//!
//! A meeting's `key` is its session folder's name (`session-<unix time>`),
//! the same on every computer it travels to. Importing skips meetings
//! already in the library (same start and key or title), so importing the
//! same archive twice adds nothing; a skipped meeting whose audio is gone
//! here gets the archive's audio back. Search indexes aren't exported:
//! they're rebuilt in the background after an import.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use chrono::Local;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::audio::Source;
use crate::i18n::{tr, trf};
use crate::store::Store;
use crate::voices::{VoiceSample, Voices};

/// File extension of a library archive.
pub const EXTENSION: &str = "mostalgia";
/// `format` in the manifest: what tells an archive from any other zip.
const FORMAT: &str = "mostalgia-library"; // i18n: not UI
/// Version of the archive layout. Older archives are read (missing fields
/// take their defaults, see [`upgrade`]); newer ones are refused.
pub const FORMAT_VERSION: u32 = 1;

const MANIFEST: &str = "manifest.json";
const DOCUMENTS: &str = "documents/documents.json";
const VOICES: &str = "voices.json";

/// Where an import puts files (the data directory's, see [`Places::data`]).
#[derive(Clone, Debug)]
pub struct Places {
    pub recordings: PathBuf,
    pub documents: PathBuf,
    pub voices: PathBuf,
}

impl Places {
    /// The real data directory.
    pub fn data() -> Self {
        Self { recordings: crate::paths::recordings_dir(), documents: crate::docs::dir(), voices: Voices::path() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    /// The Mostalgia version that wrote it.
    #[serde(default)]
    pub app_version: String,
    /// Its database schema version (for information; import reads the JSON).
    #[serde(default)]
    pub schema_version: i32,
    /// Unix time.
    #[serde(default)]
    pub exported_at: i64,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub documents: bool,
    #[serde(default)]
    pub voices: bool,
    #[serde(default)]
    pub meetings: Vec<MeetingEntry>,
    #[serde(default)]
    pub document_count: usize,
    #[serde(default)]
    pub voice_count: usize,
}

/// A meeting as the manifest lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeetingEntry {
    pub key: String,
    pub title: String,
    /// Unix time.
    pub started_at: i64,
    #[serde(default)]
    pub duration_secs: f64,
    /// WAV tracks in the archive.
    #[serde(default)]
    pub audio_files: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct MeetingRecord {
    key: String,
    title: String,
    started_at: i64,
    duration_secs: f64,
    language: String,
    transcript_engine: Option<String>,
    transcript_error: Option<String>,
    companion: Option<String>,
    notes_focus: Option<String>,
    segments: Vec<SegmentRecord>,
    summary: Option<SummaryRecord>,
    /// Labels as stored (source switches are `mostalgia:<kind>` tokens).
    bookmarks: Vec<BookmarkRecord>,
    notes: Vec<NoteRecord>,
    note_versions: Vec<NoteVersionRecord>,
    chat: Vec<ChatRecord>,
    decisions: Vec<DecisionRecord>,
    /// WAV tracks next to `meeting.json` ("mic.wav", "system.wav").
    audio: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SegmentRecord {
    source: String,
    speaker: Option<String>,
    start_secs: f64,
    end_secs: f64,
    text: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SummaryRecord {
    provider: String,
    model: String,
    effort: Option<String>,
    created_at: i64,
    markdown: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct BookmarkRecord {
    at_secs: f64,
    label: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct NoteRecord {
    kind: String,
    text: String,
    at_secs: f64,
    created_at: i64,
    pinned: bool,
    origin: String,
}

impl Default for NoteRecord {
    fn default() -> Self {
        Self { kind: "point".into(), text: String::new(), at_secs: 0.0, created_at: 0, pinned: true, origin: "live".into() }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct NoteVersionRecord {
    archived_at: i64,
    focus: Option<String>,
    notes: Vec<NoteRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct ChatRecord {
    question: String,
    answer: String,
    created_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct DecisionRecord {
    /// Its id in the exporting library (what `superseded_by` refers to).
    id: i64,
    text: String,
    speaker: Option<String>,
    at_secs: Option<f64>,
    origin: String,
    edited: bool,
    superseded: bool,
    superseded_by: Option<i64>,
    created_at: i64,
}

impl Default for DecisionRecord {
    fn default() -> Self {
        Self {
            id: 0,
            text: String::new(),
            speaker: None,
            at_secs: None,
            origin: "summary".into(),
            edited: false,
            superseded: false,
            superseded_by: None,
            created_at: 0,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct DocumentsRecord {
    collections: Vec<CollectionRecord>,
    documents: Vec<DocumentRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct CollectionRecord {
    name: String,
    active: bool,
    created_at: i64,
}

impl Default for CollectionRecord {
    fn default() -> Self {
        Self { name: String::new(), active: true, created_at: 0 }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct DocumentRecord {
    title: String,
    file_name: String,
    kind: String,
    language: Option<String>,
    size_bytes: u64,
    hash: String,
    pages: Option<i64>,
    added_at: i64,
    active: bool,
    collection: Option<String>,
    /// The file's path in the archive.
    file: String,
}

impl Default for DocumentRecord {
    fn default() -> Self {
        Self {
            title: String::new(),
            file_name: String::new(),
            kind: String::new(),
            language: None,
            size_bytes: 0,
            hash: String::new(),
            pages: None,
            added_at: 0,
            active: true,
            collection: None,
            file: String::new(),
        }
    }
}

/// Known voices with each print's meeting as a key (ids differ per library).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct VoicesRecord {
    voices: Vec<VoiceRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct VoiceRecord {
    name: String,
    samples: Vec<VoiceSampleRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct VoiceSampleRecord {
    meeting: Option<String>,
    secs: f64,
    learned_at: i64,
    embedding: Vec<f32>,
}

/// What goes into an export besides the meetings' text.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExportOptions {
    /// The recordings' WAV tracks (large).
    pub audio: bool,
    /// The documents drive and its collections.
    pub documents: bool,
    /// Known voice prints.
    pub voices: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportReport {
    pub meetings: usize,
    pub audio_files: usize,
    pub documents: usize,
    pub voices: usize,
    /// Size of the archive.
    pub bytes: u64,
}

/// Which parts of an archive to import (a part it lacks is skipped anyway).
#[derive(Clone, Copy, Debug)]
pub struct ImportOptions {
    pub audio: bool,
    pub documents: bool,
    pub voices: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { audio: true, documents: true, voices: true }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportReport {
    pub meetings_added: usize,
    /// Already in the library.
    pub meetings_skipped: usize,
    /// Audio given back to meetings that were already here without it.
    pub audio_restored: usize,
    pub documents_added: usize,
    pub documents_skipped: usize,
    /// Voices added or given more prints.
    pub voices_merged: usize,
    /// Stopped before the end (what was imported so far stays).
    pub cancelled: bool,
}

/// What an import would do, shown before anything is written.
#[derive(Clone, Debug)]
pub struct Preview {
    pub manifest: Manifest,
    /// Meetings that would be added.
    pub new: Vec<MeetingEntry>,
    /// Meetings already in the library (skipped).
    pub existing: Vec<MeetingEntry>,
    pub documents_new: usize,
    pub documents_existing: usize,
    pub voices: usize,
    /// WAV tracks in the archive.
    pub audio_files: usize,
}

/// A suggested file name: `mostalgia-library-<date>.mostalgia`.
pub fn suggested_name() -> String {
    format!("mostalgia-library-{}.{EXTENSION}", Local::now().format("%Y-%m-%d"))
}

/// Folder names that are safe as a path component.
fn safe_key(key: &str) -> String {
    let key: String = key.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
    let key = key.trim_matches('.').to_string();
    if key.is_empty() { "meeting".into() } else { key }
}

fn dir_name(dir: &str) -> String {
    Path::new(dir).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn bool_col(r: &rusqlite::Row, i: usize) -> rusqlite::Result<bool> {
    Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0) != 0)
}

fn read_meeting(conn: &Connection, id: i64, key: String) -> Result<(MeetingRecord, PathBuf)> {
    let (title, started_at, duration_secs, language, dir, transcript_engine, transcript_error, companion, notes_focus) = conn.query_row(
        "SELECT title, started_at, duration_secs, language, dir, transcript_engine, transcript_error, companion, notes_focus FROM meetings WHERE id = ?1",
        [id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        },
    )?;
    let segments = conn
        .prepare("SELECT source, speaker, start_secs, end_secs, text FROM segments WHERE meeting_id = ?1 ORDER BY start_secs, id")?
        .query_map([id], |r| Ok(SegmentRecord { source: r.get(0)?, speaker: r.get(1)?, start_secs: r.get(2)?, end_secs: r.get(3)?, text: r.get(4)? }))?
        .collect::<rusqlite::Result<_>>()?;
    let summary = conn
        .query_row("SELECT provider, model, effort, created_at, markdown FROM summaries WHERE meeting_id = ?1", [id], |r| {
            Ok(SummaryRecord { provider: r.get(0)?, model: r.get(1)?, effort: r.get(2)?, created_at: r.get(3)?, markdown: r.get(4)? })
        })
        .optional()?;
    let bookmarks = conn
        .prepare("SELECT at_secs, label FROM bookmarks WHERE meeting_id = ?1 ORDER BY at_secs, id")?
        .query_map([id], |r| Ok(BookmarkRecord { at_secs: r.get(0)?, label: r.get(1)? }))?
        .collect::<rusqlite::Result<_>>()?;
    let notes_where = |condition: &str, value: i64| -> Result<Vec<NoteRecord>> {
        Ok(conn
            .prepare(&format!("SELECT kind, text, at_secs, created_at, pinned, origin FROM notes WHERE {condition} ORDER BY at_secs, id"))?
            .query_map([value], |r| {
                Ok(NoteRecord { kind: r.get(0)?, text: r.get(1)?, at_secs: r.get(2)?, created_at: r.get(3)?, pinned: bool_col(r, 4)?, origin: r.get(5)? })
            })?
            .collect::<rusqlite::Result<_>>()?)
    };
    let notes = notes_where("meeting_id = ?1 AND version_id IS NULL", id)?;
    let versions: Vec<(i64, i64, Option<String>)> = conn
        .prepare("SELECT id, archived_at, focus FROM note_versions WHERE meeting_id = ?1 ORDER BY id")?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let note_versions = versions
        .into_iter()
        .map(|(vid, archived_at, focus)| Ok(NoteVersionRecord { archived_at, focus, notes: notes_where("version_id = ?1", vid)? }))
        .collect::<Result<_>>()?;
    let chat = conn
        .prepare("SELECT question, answer, created_at FROM companion_chat WHERE meeting_id = ?1 ORDER BY id")?
        .query_map([id], |r| Ok(ChatRecord { question: r.get(0)?, answer: r.get(1)?, created_at: r.get(2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    let decisions = conn
        .prepare("SELECT id, text, speaker, at_secs, origin, edited, superseded, superseded_by, created_at FROM decisions WHERE meeting_id = ?1 ORDER BY id")?
        .query_map([id], |r| {
            Ok(DecisionRecord {
                id: r.get(0)?,
                text: r.get(1)?,
                speaker: r.get(2)?,
                at_secs: r.get(3)?,
                origin: r.get(4)?,
                edited: bool_col(r, 5)?,
                superseded: bool_col(r, 6)?,
                superseded_by: r.get(7)?,
                created_at: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let record = MeetingRecord {
        key,
        title,
        started_at,
        duration_secs,
        language,
        transcript_engine,
        transcript_error,
        companion,
        notes_focus,
        segments,
        summary,
        bookmarks,
        notes,
        note_versions,
        chat,
        decisions,
        audio: Vec::new(),
    };
    Ok((record, PathBuf::from(dir)))
}

fn json_options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(true)
}

/// WAV and documents go in as they are: compressing audio buys little and
/// costs a lot of time.
fn stored_options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored).large_file(true)
}

fn write_json<W: Write + std::io::Seek>(zip: &mut ZipWriter<W>, name: &str, value: &impl Serialize) -> Result<()> {
    zip.start_file(name, json_options())?;
    serde_json::to_writer_pretty(&mut *zip, value)?;
    Ok(())
}

fn add_file<W: Write + std::io::Seek>(zip: &mut ZipWriter<W>, name: &str, path: &Path) -> Result<()> {
    let mut src = BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
    zip.start_file(name, stored_options())?;
    std::io::copy(&mut src, zip).with_context(|| format!("adding {}", path.display()))?;
    Ok(())
}

/// Writes the whole library to `dest` (replaced only once complete).
/// `progress(done, total)` is called as it goes; `cancel` stops it and
/// leaves no file behind.
pub fn export(
    store: &Store,
    places: &Places,
    dest: &Path,
    opts: &ExportOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, usize),
) -> Result<ExportReport> {
    let conn = store.conn();
    let meetings: Vec<(i64, String)> = conn
        .prepare("SELECT id, dir FROM meetings ORDER BY started_at, id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let documents = if opts.documents { read_documents(conn)? } else { Vec::new() };
    let voices = if opts.voices { Some(load_voices(&places.voices)) } else { None };
    let total = meetings.len() + documents.len() + 1;

    let tmp = dest.with_extension(format!("{EXTENSION}.part"));
    let result = (|| -> Result<ExportReport> {
        let file = File::create(&tmp).with_context(|| trf("{} konnte nicht geschrieben werden", &[&tmp.display()]))?;
        let mut zip = ZipWriter::new(BufWriter::new(file));
        let mut report = ExportReport::default();
        let mut entries = Vec::new();
        let mut keys_by_id: HashMap<i64, String> = HashMap::new();
        let mut used: HashSet<String> = HashSet::new();
        for (done, (id, dir)) in meetings.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                bail!("{}", tr("Abgebrochen"));
            }
            progress(done, total);
            let base = safe_key(&dir_name(dir));
            let mut key = base.clone();
            for n in 1.. {
                if used.insert(key.clone()) {
                    break;
                }
                key = format!("{base}-{n}");
            }
            keys_by_id.insert(*id, key.clone());
            let (mut record, dir) = read_meeting(conn, *id, key.clone())?;
            if opts.audio {
                for source in [Source::Mic, Source::System] {
                    let path = dir.join(source.file_name());
                    if path.is_file() {
                        add_file(&mut zip, &format!("meetings/{key}/{}", source.file_name()), &path)?;
                        record.audio.push(source.file_name());
                    }
                }
            }
            report.audio_files += record.audio.len();
            entries.push(MeetingEntry {
                key: key.clone(),
                title: record.title.clone(),
                started_at: record.started_at,
                duration_secs: record.duration_secs,
                audio_files: record.audio.len(),
            });
            write_json(&mut zip, &format!("meetings/{key}/meeting.json"), &record)?;
            report.meetings += 1;
        }
        if opts.documents {
            let mut collections: Vec<CollectionRecord> = conn
                .prepare("SELECT name, active, created_at FROM doc_collections ORDER BY name COLLATE NOCASE")?
                .query_map([], |r| Ok(CollectionRecord { name: r.get(0)?, active: bool_col(r, 1)?, created_at: r.get(2)? }))?
                .collect::<rusqlite::Result<_>>()?;
            collections.retain(|c| !c.name.trim().is_empty());
            let mut records = Vec::new();
            for (i, (mut doc, path)) in documents.into_iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    bail!("{}", tr("Abgebrochen"));
                }
                progress(meetings.len() + i, total);
                if !path.is_file() {
                    continue;
                }
                doc.size_bytes = std::fs::metadata(&path)?.len();
                doc.file = format!("documents/files/{}-{}", i + 1, safe_key(&doc.file_name));
                add_file(&mut zip, &doc.file, &path)?;
                records.push(doc);
            }
            report.documents = records.len();
            write_json(&mut zip, DOCUMENTS, &DocumentsRecord { collections, documents: records })?;
        }
        if let Some(voices) = voices {
            let record = VoicesRecord {
                voices: voices
                    .voices
                    .into_iter()
                    .map(|v| VoiceRecord {
                        name: v.name,
                        samples: v
                            .samples
                            .into_iter()
                            .map(|s| VoiceSampleRecord {
                                meeting: s.meeting_id.and_then(|id| keys_by_id.get(&id).cloned()),
                                secs: s.secs,
                                learned_at: s.learned_at,
                                embedding: s.embedding,
                            })
                            .collect(),
                    })
                    .collect(),
            };
            report.voices = record.voices.len();
            write_json(&mut zip, VOICES, &record)?;
        }
        let manifest = Manifest {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            schema_version: crate::store::SCHEMA_VERSION,
            exported_at: Local::now().timestamp(),
            audio: opts.audio,
            documents: opts.documents,
            voices: opts.voices,
            meetings: entries,
            document_count: report.documents,
            voice_count: report.voices,
        };
        write_json(&mut zip, MANIFEST, &manifest)?;
        zip.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        report.bytes = std::fs::metadata(&tmp)?.len();
        Ok(report)
    })();
    match result {
        Ok(report) => {
            std::fs::rename(&tmp, dest).with_context(|| trf("{} konnte nicht geschrieben werden", &[&dest.display()]))?;
            progress(total, total);
            Ok(report)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// The documents drive's entries with their files (archive paths unset).
fn read_documents(conn: &Connection) -> Result<Vec<(DocumentRecord, PathBuf)>> {
    Ok(conn
        .prepare(
            "SELECT d.title, d.file_name, d.kind, d.language, d.size_bytes, d.hash, d.pages, d.added_at, d.active, k.name, d.path
             FROM documents d LEFT JOIN doc_collections k ON k.id = d.collection_id ORDER BY d.added_at, d.id",
        )?
        .query_map([], |r| {
            Ok((
                DocumentRecord {
                    title: r.get(0)?,
                    file_name: r.get(1)?,
                    kind: r.get(2)?,
                    language: r.get(3)?,
                    size_bytes: r.get::<_, i64>(4)? as u64,
                    hash: r.get(5)?,
                    pages: r.get(6)?,
                    added_at: r.get(7)?,
                    active: bool_col(r, 8)?,
                    collection: r.get(9)?,
                    file: String::new(),
                },
                PathBuf::from(r.get::<_, String>(10)?),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn load_voices(path: &Path) -> Voices {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_voices(path: &Path, voices: &Voices) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(voices)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// An opened archive.
struct Archive {
    zip: ZipArchive<BufReader<File>>,
    manifest: Manifest,
}

impl Archive {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| trf("{} konnte nicht gelesen werden", &[&path.display()]))?;
        let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|_| anyhow::anyhow!("{}", tr("Die Datei ist kein Mostalgia-Archiv")))?;
        let manifest: Manifest = {
            let entry = zip.by_name(MANIFEST).map_err(|_| anyhow::anyhow!("{}", tr("Die Datei ist kein Mostalgia-Archiv")))?;
            serde_json::from_reader(BufReader::new(entry)).map_err(|_| anyhow::anyhow!("{}", tr("Die Datei ist kein Mostalgia-Archiv")))?
        };
        anyhow::ensure!(manifest.format == FORMAT, "{}", tr("Die Datei ist kein Mostalgia-Archiv"));
        anyhow::ensure!(
            manifest.format_version <= FORMAT_VERSION,
            "{}",
            tr("Das Archiv stammt von einer neueren Mostalgia-Version. Bitte Mostalgia aktualisieren.")
        );
        Ok(Self { zip, manifest })
    }

    fn has(&self, name: &str) -> bool {
        self.zip.index_for_name(name).is_some()
    }

    fn json<T: serde::de::DeserializeOwned>(&mut self, name: &str) -> Result<T> {
        let entry = self.zip.by_name(name).with_context(|| format!("{name} missing from the archive"))?;
        let value: serde_json::Value = serde_json::from_reader(BufReader::new(entry)).with_context(|| format!("reading {name}"))?;
        Ok(serde_json::from_value(upgrade(value, self.manifest.format_version))?)
    }

    fn extract(&mut self, name: &str, dest: &Path) -> Result<()> {
        let mut entry = self.zip.by_name(name).with_context(|| format!("{name} missing from the archive"))?;
        let tmp = dest.with_extension("part");
        let copied = (|| -> Result<()> {
            let mut out = BufWriter::new(File::create(&tmp)?);
            std::io::copy(&mut entry, &mut out)?;
            out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            Ok(())
        })();
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.context(trf("{} konnte nicht geschrieben werden", &[&dest.display()])));
        }
        std::fs::rename(&tmp, dest)?;
        Ok(())
    }

    fn bytes(&mut self, name: &str) -> Result<Vec<u8>> {
        let mut entry = self.zip.by_name(name).with_context(|| format!("{name} missing from the archive"))?;
        let mut out = Vec::new();
        entry.read_to_end(&mut out)?;
        Ok(out)
    }
}

/// Brings a record from an older archive layout up to the current one.
/// Version 1 is the first; later layout changes add their steps here
/// (fields only added need nothing: they take their defaults).
fn upgrade(value: serde_json::Value, _from: u32) -> serde_json::Value {
    value
}

/// The library meeting an archive's meeting already is: same start, and
/// the same session folder name or title.
fn existing(conn: &Connection, entry: &MeetingEntry) -> Result<Option<(i64, PathBuf)>> {
    let candidates: Vec<(i64, String, String)> = conn
        .prepare("SELECT id, dir, title FROM meetings WHERE started_at = ?1 ORDER BY id")?
        .query_map([entry.started_at], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(candidates
        .iter()
        .find(|(_, dir, _)| safe_key(&dir_name(dir)) == safe_key(&entry.key))
        .or_else(|| candidates.iter().find(|(_, _, title)| *title == entry.title))
        .map(|(id, dir, _)| (*id, PathBuf::from(dir))))
}

fn document_exists(conn: &Connection, hash: &str, size: u64) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS (SELECT 1 FROM documents WHERE hash = ?1 AND size_bytes = ?2)", params![hash, size as i64], |r| r.get(0))?)
}

/// What importing `src` would add, without writing anything.
pub fn preview(store: &Store, src: &Path) -> Result<Preview> {
    let mut archive = Archive::open(src)?;
    let conn = store.conn();
    let (mut new, mut existing_ones) = (Vec::new(), Vec::new());
    for entry in &archive.manifest.meetings {
        if existing(conn, entry)?.is_some() { existing_ones.push(entry.clone()) } else { new.push(entry.clone()) }
    }
    let (mut documents_new, mut documents_existing) = (0, 0);
    if archive.has(DOCUMENTS) {
        let docs: DocumentsRecord = archive.json(DOCUMENTS)?;
        for d in &docs.documents {
            if document_exists(conn, &d.hash, d.size_bytes)? { documents_existing += 1 } else { documents_new += 1 }
        }
    }
    let voices = if archive.has(VOICES) { archive.json::<VoicesRecord>(VOICES)?.voices.len() } else { 0 };
    let audio_files = archive.manifest.meetings.iter().map(|m| m.audio_files).sum();
    Ok(Preview { manifest: archive.manifest, new, existing: existing_ones, documents_new, documents_existing, voices, audio_files })
}

/// A session folder for an imported meeting that no meeting uses yet.
fn free_dir(conn: &Connection, recordings: &Path, key: &str) -> Result<PathBuf> {
    let base = safe_key(key);
    for n in 0.. {
        let dir = recordings.join(if n == 0 { base.clone() } else { format!("{base}-{n}") });
        let taken: bool = conn.query_row("SELECT EXISTS (SELECT 1 FROM meetings WHERE dir = ?1)", [dir.to_string_lossy()], |r| r.get(0))?;
        if !taken && !dir.exists() {
            return Ok(dir);
        }
    }
    unreachable!()
}

/// The archive's audio of a meeting that may go into `dir`.
fn audio_names(record: &MeetingRecord) -> Vec<String> {
    let allowed = [Source::Mic.file_name(), Source::System.file_name()];
    record.audio.iter().filter(|a| allowed.contains(a)).cloned().collect()
}

/// Saves an archived meeting as a new one in `dir`; returns its id and the
/// archive's decision ids with their new ids.
fn insert_meeting(conn: &mut Connection, record: &MeetingRecord, dir: &Path) -> Result<(i64, Vec<(i64, i64, Option<i64>)>)> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO meetings (title, started_at, duration_secs, language, dir, transcript_engine, transcript_error, companion, notes_focus)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            record.title,
            record.started_at,
            record.duration_secs,
            if record.language.is_empty() { "auto" } else { &record.language },
            dir.to_string_lossy(),
            record.transcript_engine,
            record.transcript_error,
            record.companion,
            record.notes_focus,
        ],
    )?;
    let id = tx.last_insert_rowid();
    {
        let mut stmt = tx.prepare("INSERT INTO segments (meeting_id, source, speaker, start_secs, end_secs, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
        for s in &record.segments {
            let source = Source::parse(&s.source).unwrap_or(Source::System);
            stmt.execute(params![id, source.as_str(), s.speaker, s.start_secs, s.end_secs, s.text])?;
        }
    }
    if let Some(s) = &record.summary {
        tx.execute(
            "INSERT INTO summaries (meeting_id, provider, model, effort, created_at, markdown) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, s.provider, s.model, s.effort, s.created_at, s.markdown],
        )?;
    }
    {
        let mut stmt = tx.prepare("INSERT INTO bookmarks (meeting_id, at_secs, label) VALUES (?1, ?2, ?3)")?;
        for b in &record.bookmarks {
            stmt.execute(params![id, b.at_secs, b.label])?;
        }
    }
    let insert_notes = |notes: &[NoteRecord], version: Option<i64>| -> Result<()> {
        let mut stmt = tx.prepare(
            "INSERT INTO notes (meeting_id, kind, text, at_secs, created_at, pinned, origin, version_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for n in notes {
            stmt.execute(params![id, n.kind, n.text, n.at_secs, n.created_at, n.pinned, n.origin, version])?;
        }
        Ok(())
    };
    insert_notes(&record.notes, None)?;
    for v in &record.note_versions {
        tx.execute("INSERT INTO note_versions (meeting_id, archived_at, focus) VALUES (?1, ?2, ?3)", params![id, v.archived_at, v.focus])?;
        insert_notes(&v.notes, Some(tx.last_insert_rowid()))?;
    }
    {
        let mut stmt = tx.prepare("INSERT INTO companion_chat (meeting_id, question, answer, created_at) VALUES (?1, ?2, ?3, ?4)")?;
        for c in &record.chat {
            stmt.execute(params![id, c.question, c.answer, c.created_at])?;
        }
    }
    let mut decisions = Vec::new();
    {
        let mut stmt = tx.prepare(
            "INSERT INTO decisions (meeting_id, text, speaker, at_secs, origin, edited, superseded, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for d in &record.decisions {
            stmt.execute(params![id, d.text, d.speaker, d.at_secs, d.origin, d.edited, d.superseded, d.created_at])?;
            decisions.push((d.id, tx.last_insert_rowid(), d.superseded_by));
        }
    }
    tx.commit()?;
    Ok((id, decisions))
}

/// Imports `src` into the library: new meetings (with their audio, if
/// wanted), documents and voices. Meetings already here are skipped (they
/// get the archive's audio back if theirs is gone). Each meeting is saved
/// on its own, so a cancelled or failed import keeps what it finished.
pub fn import(
    store: &mut Store,
    places: &Places,
    src: &Path,
    opts: &ImportOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, usize),
) -> Result<ImportReport> {
    let mut archive = Archive::open(src)?;
    let mut report = ImportReport::default();
    let entries = archive.manifest.meetings.clone();
    let docs: Option<DocumentsRecord> = if opts.documents && archive.has(DOCUMENTS) { Some(archive.json(DOCUMENTS)?) } else { None };
    let total = entries.len() + docs.as_ref().map_or(0, |d| d.documents.len()) + 1;
    let mut ids_by_key: HashMap<String, i64> = HashMap::new();
    let mut decision_ids: HashMap<i64, i64> = HashMap::new();
    let mut supersedes: Vec<(i64, i64)> = Vec::new();

    for (done, entry) in entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(done, total);
        let record: MeetingRecord = archive.json(&format!("meetings/{}/meeting.json", entry.key))?;
        let audio = if opts.audio { audio_names(&record) } else { Vec::new() };
        if let Some((id, dir)) = existing(store.conn(), entry)? {
            ids_by_key.insert(entry.key.clone(), id);
            report.meetings_skipped += 1;
            let missing = [Source::Mic, Source::System].iter().all(|s| !dir.join(s.file_name()).exists());
            if missing && !audio.is_empty() {
                std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
                for name in &audio {
                    archive.extract(&format!("meetings/{}/{name}", entry.key), &dir.join(name))?;
                }
                report.audio_restored += 1;
            }
            continue;
        }
        let dir = free_dir(store.conn(), &places.recordings, &entry.key)?;
        if !audio.is_empty() {
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let extracted = audio.iter().try_for_each(|name| archive.extract(&format!("meetings/{}/{name}", entry.key), &dir.join(name)));
            if let Err(e) = extracted {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        }
        let (id, decisions) = match insert_meeting(store.conn_mut(), &record, &dir) {
            Ok(saved) => saved,
            Err(e) => {
                if !audio.is_empty() {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                return Err(e);
            }
        };
        for (old, new, by) in decisions {
            decision_ids.insert(old, new);
            if let Some(by) = by {
                supersedes.push((new, by));
            }
        }
        ids_by_key.insert(entry.key.clone(), id);
        report.meetings_added += 1;
    }
    // A decision replaced by one in another meeting: the ids changed.
    for (decision, by) in supersedes {
        if let Some(by) = decision_ids.get(&by) {
            store.conn().execute("UPDATE decisions SET superseded_by = ?2 WHERE id = ?1", params![decision, by])?;
        }
    }
    if report.meetings_added > 0 {
        crate::ask::search::invalidate_cache();
    }

    if let Some(docs) = docs.filter(|_| !report.cancelled) {
        import_documents(store, places, &mut archive, docs, &mut report, cancel, &|i| progress(entries.len() + i, total))?;
    }
    if opts.voices && archive.has(VOICES) && !report.cancelled {
        let record: VoicesRecord = archive.json(VOICES)?;
        report.voices_merged = merge_voices(&places.voices, record, &ids_by_key)?;
    }
    progress(total, total);
    Ok(report)
}

fn import_documents(
    store: &Store,
    places: &Places,
    archive: &mut Archive,
    docs: DocumentsRecord,
    report: &mut ImportReport,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize),
) -> Result<()> {
    let conn = store.conn();
    let mut collection_ids: HashMap<String, i64> = HashMap::new();
    let mut collection = |name: &str| -> Result<i64> {
        if let Some(id) = collection_ids.get(&name.to_lowercase()) {
            return Ok(*id);
        }
        let id = match conn.query_row("SELECT id FROM doc_collections WHERE name = ?1", [name], |r| r.get(0)).optional()? {
            Some(id) => id,
            None => {
                let k = docs.collections.iter().find(|c| c.name.eq_ignore_ascii_case(name));
                conn.execute(
                    "INSERT INTO doc_collections (name, active, created_at) VALUES (?1, ?2, ?3)",
                    params![name, k.is_none_or(|k| k.active), k.map_or_else(|| Local::now().timestamp(), |k| k.created_at)],
                )?;
                conn.last_insert_rowid()
            }
        };
        collection_ids.insert(name.to_lowercase(), id);
        Ok(id)
    };
    for (i, d) in docs.documents.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(i);
        if d.file.is_empty() || !archive.has(&d.file) || document_exists(conn, &d.hash, d.size_bytes)? {
            report.documents_skipped += 1;
            continue;
        }
        let bytes = archive.bytes(&d.file)?;
        let collection_id = match d.collection.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => Some(collection(name)?),
            None => None,
        };
        conn.execute(
            "INSERT INTO documents (title, file_name, path, kind, language, size_bytes, hash, pages, added_at, active, collection_id)
             VALUES (?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![d.title, d.file_name, d.kind, d.language, bytes.len() as i64, d.hash, d.pages, d.added_at, d.active, collection_id],
        )?;
        let id = conn.last_insert_rowid();
        let dest = places.documents.join(format!("{id}-{}", safe_key(&d.file_name)));
        if let Err(e) = std::fs::create_dir_all(&places.documents).and_then(|_| std::fs::write(&dest, &bytes)) {
            conn.execute("DELETE FROM documents WHERE id = ?1", [id])?;
            return Err(e).with_context(|| trf("{} konnte nicht gespeichert werden", &[&dest.display()]));
        }
        conn.execute("UPDATE documents SET path = ?2 WHERE id = ?1", params![id, dest.to_string_lossy()])?;
        report.documents_added += 1;
    }
    if report.documents_added > 0 {
        crate::docs::invalidate_cache();
    }
    Ok(())
}

/// Adds the archive's voice prints to the known voices (prints already
/// known are skipped); returns how many voices changed.
fn merge_voices(path: &Path, record: VoicesRecord, ids_by_key: &HashMap<String, i64>) -> Result<usize> {
    let mut voices = load_voices(path);
    let mut changed = 0;
    for v in record.voices {
        let name = v.name.trim();
        if name.is_empty() {
            continue;
        }
        let mut added = false;
        for s in v.samples {
            if s.embedding.is_empty() {
                continue;
            }
            let sample = VoiceSample {
                meeting_id: s.meeting.as_ref().and_then(|k| ids_by_key.get(k).copied()),
                secs: s.secs,
                learned_at: s.learned_at,
                embedding: s.embedding,
            };
            added |= voices.add_sample(name, sample);
        }
        changed += usize::from(added);
    }
    if changed > 0 {
        save_voices(path, &voices)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finalize::FinalSegment;
    use crate::store::{Bookmark, MarkKind, NewMeeting};
    use chrono::TimeZone;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mostalgia-library-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn places(&self, side: &str) -> Places {
            let root = self.0.join(side);
            Places { recordings: root.join("recordings"), documents: root.join("documents"), voices: root.join("voices.json") }
        }

        fn store(&self, side: &str) -> Store {
            Store::open(&self.0.join(side).join("mostalgia.db")).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn seg(source: Source, start: f64, text: &str, speaker: Option<&str>) -> FinalSegment {
        FinalSegment { source, start_secs: start, end_secs: start + 2.0, text: text.into(), speaker: speaker.map(Into::into) }
    }

    /// A library with two meetings (one with audio), a document, a voice.
    fn fill(s: &Scratch) -> (i64, i64) {
        let places = s.places("a");
        let mut store = s.store("a");
        let dir_a = places.recordings.join("session-1700000000");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::write(dir_a.join("mic.wav"), b"RIFF mic").unwrap();
        std::fs::write(dir_a.join("system.wav"), b"RIFF system").unwrap();
        let started = Local.timestamp_opt(1_700_000_000, 0).unwrap();
        let a = store
            .insert_meeting(
                &NewMeeting { title: "Budget", started_at: started, duration_secs: 60.0, language: "de", dir: &dir_a },
                &[seg(Source::Mic, 1.0, "Wir frieren das Budget ein.", None), seg(Source::System, 4.0, "Anna macht die Prognose.", Some("Anna"))],
            )
            .unwrap();
        store.save_summary(a, "claude", "m", Some("high"), "# Budget\n\n## Entscheidungen\n- Budget eingefroren").unwrap();
        store.set_transcript_info(a, "Whisper", None).unwrap();
        store.add_bookmarks(a, &[Bookmark { at_secs: 3.0, label: "Wichtig".into(), kind: MarkKind::Moment }, Bookmark::source_switch(Source::Mic, false, 30.0)]).unwrap();
        store.add_note_pinned(a, "decision", "Freeze spend", 1.0, true).unwrap();
        store.add_note_pinned(a, "fact", "Old point", 2.0, false).unwrap();
        store.replace_unpinned_notes(a, &[("action", "Anna: forecast", 4.0)], Some("money")).unwrap();
        store.add_companion_chat(a, &[("What?".into(), "Frozen [[t:1]].".into())]).unwrap();
        store.set_companion(a, "hoshi").unwrap();
        let b = store
            .insert_meeting(
                &NewMeeting {
                    title: "Notes",
                    started_at: Local.timestamp_opt(1_700_100_000, 0).unwrap(),
                    duration_secs: 30.0,
                    language: "en",
                    dir: &places.recordings.join("session-1700100000"),
                },
                &[seg(Source::System, 0.0, "Text only", None)],
            )
            .unwrap();
        let conn = store.conn();
        conn.execute("INSERT INTO decisions (meeting_id, text, created_at) VALUES (?1, 'Budget frozen', 1)", [a]).unwrap();
        let first = conn.last_insert_rowid();
        conn.execute("INSERT INTO decisions (meeting_id, text, edited, created_at) VALUES (?1, 'Budget unfrozen', 1, 2)", [b]).unwrap();
        let second = conn.last_insert_rowid();
        conn.execute("UPDATE decisions SET superseded = 1, superseded_by = ?2 WHERE id = ?1", params![first, second]).unwrap();
        conn.execute("INSERT INTO doc_collections (name, active, created_at) VALUES ('Projekt', 0, 5)", []).unwrap();
        std::fs::create_dir_all(&places.documents).unwrap();
        let doc = places.documents.join("1-plan.md");
        std::fs::write(&doc, "# Plan\n\nShip it.").unwrap();
        conn.execute(
            "INSERT INTO documents (title, file_name, path, kind, size_bytes, hash, added_at, collection_id) VALUES ('Plan', 'plan.md', ?1, 'md', 17, 'h1', 7, 1)",
            [doc.to_string_lossy()],
        )
        .unwrap();
        let mut voices = Voices::default();
        voices.learn("Anna", Some(b), vec![1.0, 0.0], 12.0);
        save_voices(&places.voices, &voices).unwrap();
        (a, b)
    }

    fn all(store: &Store) -> Vec<crate::store::Meeting> {
        store.list(100).unwrap().iter().map(|m| store.get(m.id).unwrap().unwrap()).collect()
    }

    #[test]
    fn roundtrip_with_everything_and_reimport_adds_nothing() -> Result<()> {
        let s = Scratch::new("roundtrip");
        fill(&s);
        let archive = s.0.join("library.mostalgia");
        let opts = ExportOptions { audio: true, documents: true, voices: true };
        let report = export(&s.store("a"), &s.places("a"), &archive, &opts, &AtomicBool::new(false), &|_, _| {})?;
        assert_eq!((report.meetings, report.audio_files, report.documents, report.voices), (2, 2, 1, 1));
        assert!(!archive.with_extension("mostalgia.part").exists());

        let mut target = s.store("b");
        let places = s.places("b");
        let preview = preview(&target, &archive)?;
        assert_eq!((preview.new.len(), preview.existing.len(), preview.documents_new, preview.voices, preview.audio_files), (2, 0, 1, 1, 2));
        assert_eq!(preview.manifest.schema_version, crate::store::SCHEMA_VERSION);

        let report = import(&mut target, &places, &archive, &ImportOptions::default(), &AtomicBool::new(false), &|_, _| {})?;
        assert_eq!((report.meetings_added, report.documents_added, report.voices_merged), (2, 1, 1));

        let (src, dst) = (all(&s.store("a")), all(&target));
        assert_eq!(src.len(), dst.len());
        for (a, b) in src.iter().zip(&dst) {
            assert_eq!((&a.title, a.started_at, a.duration_secs, &a.language), (&b.title, b.started_at, b.duration_secs, &b.language));
            assert_eq!(a.segments.iter().map(|x| (&x.text, x.speaker_label(), x.source)).collect::<Vec<_>>(), b.segments.iter().map(|x| (&x.text, x.speaker_label(), x.source)).collect::<Vec<_>>());
            assert_eq!(a.bookmarks, b.bookmarks);
            assert_eq!(a.notes.iter().map(|n| (&n.text, n.pinned, &n.origin)).collect::<Vec<_>>(), b.notes.iter().map(|n| (&n.text, n.pinned, &n.origin)).collect::<Vec<_>>());
            assert_eq!(a.chat.iter().map(|c| &c.answer).collect::<Vec<_>>(), b.chat.iter().map(|c| &c.answer).collect::<Vec<_>>());
            assert_eq!(a.summary.as_ref().map(|x| &x.markdown), b.summary.as_ref().map(|x| &x.markdown));
            assert_eq!((&a.companion, &a.transcript_engine), (&b.companion, &b.transcript_engine));
        }
        let budget = dst.iter().find(|m| m.title == "Budget").unwrap();
        assert_eq!(std::fs::read(budget.dir.join("system.wav"))?, b"RIFF system");
        assert!(budget.dir.starts_with(&places.recordings));
        assert_eq!(target.note_versions(budget.id)?[0].notes[0].text, "Old point");
        assert_eq!(target.notes_focus(budget.id)?.as_deref(), Some("money"));
        // The supersede link points at the new id.
        let (by, text): (i64, String) = target.conn().query_row(
            "SELECT d.superseded_by, n.text FROM decisions d JOIN decisions n ON n.id = d.superseded_by WHERE d.superseded = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert!(by > 0);
        assert_eq!(text, "Budget unfrozen");
        // Searchable right away.
        assert_eq!(target.search("prognose", 10)?.len(), 1);
        // Documents with their collection; voices tied to the new meeting.
        let (path, collection, active): (String, String, bool) = target.conn().query_row(
            "SELECT d.path, k.name, k.active FROM documents d JOIN doc_collections k ON k.id = d.collection_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!(std::fs::read_to_string(path)?, "# Plan\n\nShip it.");
        assert_eq!((collection.as_str(), active), ("Projekt", false));
        let notes = dst.iter().find(|m| m.title == "Notes").unwrap();
        assert_eq!(load_voices(&places.voices).get("Anna").unwrap().samples[0].meeting_id, Some(notes.id));

        // Again: nothing new.
        let preview = super::preview(&target, &archive)?;
        assert_eq!((preview.new.len(), preview.existing.len(), preview.documents_new, preview.documents_existing), (0, 2, 0, 1));
        let report = import(&mut target, &places, &archive, &ImportOptions::default(), &AtomicBool::new(false), &|_, _| {})?;
        assert_eq!((report.meetings_added, report.meetings_skipped, report.documents_added, report.voices_merged), (0, 2, 0, 0));
        assert_eq!(target.list(100)?.len(), 2);
        assert_eq!(load_voices(&places.voices).get("Anna").unwrap().samples.len(), 1);
        Ok(())
    }

    #[test]
    fn without_audio_and_restoring_lost_audio() -> Result<()> {
        let s = Scratch::new("audio");
        fill(&s);
        let (lean, full) = (s.0.join("lean.mostalgia"), s.0.join("full.mostalgia"));
        let none = AtomicBool::new(false);
        export(&s.store("a"), &s.places("a"), &lean, &ExportOptions::default(), &none, &|_, _| {})?;
        export(&s.store("a"), &s.places("a"), &full, &ExportOptions { audio: true, ..Default::default() }, &none, &|_, _| {})?;
        assert!(std::fs::metadata(&lean)?.len() < std::fs::metadata(&full)?.len());
        let mut zip = ZipArchive::new(File::open(&lean)?)?;
        assert!(zip.file_names().all(|n| !n.ends_with(".wav") && !n.starts_with("documents/") && n != VOICES));
        assert!(zip.by_name("meetings/session-1700000000/meeting.json").is_ok());

        let mut target = s.store("b");
        let places = s.places("b");
        import(&mut target, &places, &lean, &ImportOptions::default(), &none, &|_, _| {})?;
        let budget = all(&target).into_iter().find(|m| m.title == "Budget").unwrap();
        assert!(budget.tracks().is_empty());
        // The full archive gives the meeting its audio back, adds nothing else.
        let report = import(&mut target, &places, &full, &ImportOptions::default(), &none, &|_, _| {})?;
        assert_eq!((report.meetings_added, report.meetings_skipped, report.audio_restored), (0, 2, 1));
        assert_eq!(target.get(budget.id)?.unwrap().tracks().len(), 2);
        Ok(())
    }

    #[test]
    fn refuses_other_files_and_newer_archives() -> Result<()> {
        let s = Scratch::new("refuse");
        let store = s.store("b");
        let junk = s.0.join("junk.mostalgia");
        std::fs::write(&junk, b"not a zip")?;
        assert!(preview(&store, &junk).is_err());
        let newer = s.0.join("newer.mostalgia");
        let mut zip = ZipWriter::new(File::create(&newer)?);
        write_json(&mut zip, MANIFEST, &serde_json::json!({ "format": FORMAT, "format_version": FORMAT_VERSION + 1 }))?;
        zip.finish()?;
        assert!(preview(&store, &newer).unwrap_err().to_string().contains(tr("Das Archiv stammt von einer neueren Mostalgia-Version. Bitte Mostalgia aktualisieren.")));
        // A minimal archive (only what's required) still imports.
        let minimal = s.0.join("minimal.mostalgia");
        let mut zip = ZipWriter::new(File::create(&minimal)?);
        write_json(
            &mut zip,
            MANIFEST,
            &serde_json::json!({ "format": FORMAT, "format_version": 1, "meetings": [{ "key": "../x", "title": "Old", "started_at": 5 }] }),
        )?;
        write_json(&mut zip, "meetings/../x/meeting.json", &serde_json::json!({ "title": "Old", "started_at": 5, "audio": ["../../evil.wav"] }))?;
        zip.finish()?;
        let mut store = store;
        let places = s.places("b");
        let report = import(&mut store, &places, &minimal, &ImportOptions::default(), &AtomicBool::new(false), &|_, _| {})?;
        assert_eq!(report.meetings_added, 1);
        let m = all(&store).pop().unwrap();
        assert_eq!((m.title.as_str(), m.language.as_str()), ("Old", "auto"));
        assert!(m.dir.starts_with(&places.recordings));
        Ok(())
    }

    #[test]
    fn cancelled_export_leaves_nothing() {
        let s = Scratch::new("cancel");
        fill(&s);
        let dest = s.0.join("x.mostalgia");
        assert!(export(&s.store("a"), &s.places("a"), &dest, &ExportOptions::default(), &AtomicBool::new(true), &|_, _| {}).is_err());
        assert!(!dest.exists() && !dest.with_extension("mostalgia.part").exists());
    }
}
