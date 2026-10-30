use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone};
use rusqlite::{Connection, OptionalExtension, params};

use crate::audio::Source;
use crate::finalize::FinalSegment;

/// The meeting library: one SQLite file with meetings, their transcript
/// segments (full-text indexed) and summaries.
pub struct Store {
    conn: Connection,
}

pub struct MeetingSummary {
    pub id: i64,
    pub title: String,
    pub started_at: DateTime<Local>,
    pub duration_secs: f64,
    pub segment_count: i64,
    pub has_summary: bool,
    /// Added from a transcript as text (no audio), see `text_import`.
    pub text_import: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub at_secs: f64,
    pub label: String,
    /// A moment the user marked, or where a source was switched off / on.
    pub kind: MarkKind,
}

impl Bookmark {
    /// "Mic off" / "Mic on" (and desktop audio) at `at_secs`.
    pub fn source_switch(source: Source, on: bool, at_secs: f64) -> Self {
        let kind = if on { MarkKind::SourceOn(source) } else { MarkKind::SourceOff(source) };
        Self { at_secs, label: kind.label().to_string(), kind }
    }

    /// What goes into the `label` column: a moment's label, or a reserved
    /// token for a switch (no schema change needed).
    pub fn stored_label(&self) -> String {
        match self.kind {
            MarkKind::Moment => self.label.clone(),
            kind => format!("{SWITCH_PREFIX}{}", kind.as_str()),
        }
    }

    /// What to show: the user's label, or the switch in the UI language.
    pub fn display_label(&self) -> String {
        match self.kind {
            MarkKind::Moment => self.label.clone(),
            _ => self.kind.label().to_string(),
        }
    }
}

/// Prefix of a source switch's token in the bookmarks' `label` column.
const SWITCH_PREFIX: &str = "mostalgia:"; // i18n: not UI

/// What a [`Bookmark`] marks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkKind {
    /// "Mark moment" during a recording.
    #[default]
    Moment,
    /// A source switched off mid-recording (its track is silent from here).
    SourceOff(Source),
    /// A source switched (back) on mid-recording.
    SourceOn(Source),
}

impl MarkKind {
    /// Stable name (stored as `mostalgia:<name>`; "" for a moment).
    pub fn as_str(self) -> &'static str {
        match self {
            MarkKind::Moment => "",
            MarkKind::SourceOff(Source::Mic) => "mic_off",
            MarkKind::SourceOn(Source::Mic) => "mic_on",
            MarkKind::SourceOff(Source::System) => "system_off",
            MarkKind::SourceOn(Source::System) => "system_on",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "mic_off" => MarkKind::SourceOff(Source::Mic),
            "mic_on" => MarkKind::SourceOn(Source::Mic),
            "system_off" => MarkKind::SourceOff(Source::System),
            "system_on" => MarkKind::SourceOn(Source::System),
            _ => MarkKind::Moment,
        }
    }

    /// The kind a stored label stands for (a moment unless it's a switch token).
    pub fn from_stored_label(label: &str) -> Self {
        label.strip_prefix(SWITCH_PREFIX).map_or(MarkKind::Moment, Self::parse)
    }

    /// The switch's label in the UI language (empty for a moment).
    pub fn label(self) -> &'static str {
        use crate::i18n::tr;
        match self {
            MarkKind::Moment => "",
            MarkKind::SourceOff(Source::Mic) => tr("Mikrofon aus"),
            MarkKind::SourceOn(Source::Mic) => tr("Mikrofon an"),
            MarkKind::SourceOff(Source::System) => tr("Desktop-Audio aus"),
            MarkKind::SourceOn(Source::System) => tr("Desktop-Audio an"),
        }
    }
}

/// A point Mori noticed during the recording and the user pinned.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub id: i64,
    /// "decision", "action", "question", "fact" or "point" (a key point).
    pub kind: String,
    pub text: String,
    pub at_secs: f64,
    pub created_at: DateTime<Local>,
    /// Pinned by the user during the recording (or later).
    pub pinned: bool,
    /// "live" (taken during the recording) or "regenerated" (from the
    /// saved transcript, see `companion::regenerate_notes`).
    pub origin: String,
}

/// A set of notes a "regenerate" replaced, kept so it can be restored.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteVersion {
    pub id: i64,
    pub archived_at: DateTime<Local>,
    /// The focus the replaced notes were taken with, if any.
    pub focus: Option<String>,
    pub notes: Vec<Note>,
}

/// A question asked to Mori about the meeting while it was recorded, with
/// her answer (citations as `[[t:…]]` markers, see `ask::cite`).
#[derive(Clone, Debug, PartialEq)]
pub struct ChatTurn {
    pub id: i64,
    pub question: String,
    pub answer: String,
    pub created_at: DateTime<Local>,
}

pub struct Meeting {
    pub id: i64,
    pub title: String,
    pub started_at: DateTime<Local>,
    pub duration_secs: f64,
    pub language: String,
    pub dir: PathBuf,
    pub segments: Vec<StoredSegment>,
    pub bookmarks: Vec<Bookmark>,
    /// Mori's pinned notes, by time.
    pub notes: Vec<Note>,
    /// The chat with Mori during the recording, oldest first.
    pub chat: Vec<ChatTurn>,
    pub summary: Option<Summary>,
    /// The engine that wrote the transcript ("Whisper … (lokal)", "OpenAI · whisper-1").
    pub transcript_engine: Option<String>,
    /// Why the transcript is incomplete (the engine failed), if it is.
    pub transcript_error: Option<String>,
    /// Id of the companion who took the notes and chat ("mori", "tsumugi", "hoshi").
    pub companion: String,
}

impl Meeting {
    /// The meeting's WAV tracks that exist on disk.
    pub fn tracks(&self) -> Vec<(Source, PathBuf)> {
        [Source::Mic, Source::System]
            .into_iter()
            .map(|s| (s, self.dir.join(s.file_name())))
            .filter(|(_, p)| p.exists())
            .collect()
    }

    /// Added from a transcript as text: no audio (see `text_import`).
    pub fn is_text_import(&self) -> bool {
        crate::text_import::is_text_import(self.transcript_engine.as_deref())
    }

    /// Imported from text that had no times: the lines' times are made up.
    pub fn is_untimed(&self) -> bool {
        crate::text_import::is_untimed(self.transcript_engine.as_deref())
    }
}

pub struct StoredSegment {
    pub id: i64,
    pub source: Source,
    /// Who spoke, when known beyond the source (diarization, renames).
    pub speaker: Option<String>,
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

impl StoredSegment {
    /// The label to show: an explicit speaker, else "Ich" / "Andere".
    pub fn speaker_label(&self) -> &str {
        self.speaker.as_deref().unwrap_or(self.source.speaker_label())
    }
}

pub struct Summary {
    /// "claude" or "deepseek".
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub created_at: DateTime<Local>,
    pub markdown: String,
}

pub struct SearchHit {
    pub meeting_id: i64,
    pub title: String,
    pub started_at: DateTime<Local>,
    pub source: Source,
    pub start_secs: f64,
    /// The matching text with hits wrapped in `[` `]`.
    pub snippet: String,
}

pub struct NewMeeting<'a> {
    pub title: &'a str,
    pub started_at: DateTime<Local>,
    pub duration_secs: f64,
    pub language: &'a str,
    pub dir: &'a Path,
}

pub const SCHEMA_VERSION: i32 = 14;

const SCHEMA: &str = "
CREATE TABLE meetings (
    id            INTEGER PRIMARY KEY,
    title         TEXT NOT NULL,
    started_at    INTEGER NOT NULL,
    duration_secs REAL NOT NULL,
    language      TEXT NOT NULL,
    dir           TEXT NOT NULL UNIQUE
);
CREATE TABLE segments (
    id         INTEGER PRIMARY KEY,
    meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    source     TEXT NOT NULL,
    speaker    TEXT,
    start_secs REAL NOT NULL,
    end_secs   REAL NOT NULL,
    text       TEXT NOT NULL
);
CREATE INDEX segments_meeting ON segments(meeting_id, start_secs);
CREATE TABLE summaries (
    meeting_id INTEGER PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
    model      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    markdown   TEXT NOT NULL
);
CREATE VIRTUAL TABLE segments_fts USING fts5(
    text, content='segments', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER segments_ai AFTER INSERT ON segments BEGIN
    INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER segments_ad AFTER DELETE ON segments BEGIN
    INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER segments_au AFTER UPDATE OF text ON segments BEGIN
    INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
END;
";

/// Schema changes after version 1, applied in order.
const MIGRATIONS: &[&str] = &[
    // 2: bookmarks ("mark moment" during a recording)
    "CREATE TABLE bookmarks (
        id         INTEGER PRIMARY KEY,
        meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        at_secs    REAL NOT NULL,
        label      TEXT NOT NULL DEFAULT ''
    );",
    // 3: which provider and thinking level produced a summary
    "ALTER TABLE summaries ADD COLUMN provider TEXT NOT NULL DEFAULT 'claude';
     ALTER TABLE summaries ADD COLUMN effort TEXT;",
    // 4: "ask your meetings" — searchable passages (transcript windows,
    // summary sections) with embeddings, and what was indexed when.
    "CREATE TABLE passages (
        id         INTEGER PRIMARY KEY,
        meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL,
        start_secs REAL NOT NULL,
        end_secs   REAL NOT NULL,
        text       TEXT NOT NULL,
        embedding  BLOB
    );
    CREATE INDEX passages_meeting ON passages(meeting_id);
    CREATE VIRTUAL TABLE passages_fts USING fts5(
        text, content='passages', content_rowid='id',
        tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER passages_ai AFTER INSERT ON passages BEGIN
        INSERT INTO passages_fts(rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER passages_ad AFTER DELETE ON passages BEGIN
        INSERT INTO passages_fts(passages_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TABLE index_state (
        meeting_id  INTEGER PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
        fingerprint TEXT NOT NULL
    );",
    // 5: which speech-to-text engine wrote the transcript, and why it is
    // incomplete if that engine failed
    "ALTER TABLE meetings ADD COLUMN transcript_engine TEXT;
     ALTER TABLE meetings ADD COLUMN transcript_error TEXT;",
    // 6: Mori's pinned notes
    "CREATE TABLE notes (
        id         INTEGER PRIMARY KEY,
        meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL,
        text       TEXT NOT NULL,
        at_secs    REAL NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX notes_meeting ON notes(meeting_id, at_secs);",
    // 7: the chat with Mori during a recording
    "CREATE TABLE companion_chat (
        id         INTEGER PRIMARY KEY,
        meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        question   TEXT NOT NULL,
        answer     TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX companion_chat_meeting ON companion_chat(meeting_id, id);",
    // 8: all of Mori's live points are kept; the ones the user pinned are marked
    "ALTER TABLE notes ADD COLUMN pinned INTEGER NOT NULL DEFAULT 1;",
    // 9: regenerated notes; the sets they replaced stay restorable
    // (`version_id` set = archived), and the focus notes were taken with
    "CREATE TABLE note_versions (
        id          INTEGER PRIMARY KEY,
        meeting_id  INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        archived_at INTEGER NOT NULL,
        focus       TEXT
    );
    CREATE INDEX note_versions_meeting ON note_versions(meeting_id, id);
    ALTER TABLE notes ADD COLUMN version_id INTEGER REFERENCES note_versions(id) ON DELETE CASCADE;
    ALTER TABLE notes ADD COLUMN origin TEXT NOT NULL DEFAULT 'live';
    ALTER TABLE meetings ADD COLUMN notes_focus TEXT;",
    // 10: which companion took the notes and the chat (older meetings: Mori)
    "ALTER TABLE meetings ADD COLUMN companion TEXT;",
    // 11: the documents drive — uploaded files (a copy lives in the data
    // directory), split into searchable chunks with embeddings
    "CREATE TABLE documents (
        id         INTEGER PRIMARY KEY,
        title      TEXT NOT NULL,
        file_name  TEXT NOT NULL,
        path       TEXT NOT NULL,
        kind       TEXT NOT NULL,
        language   TEXT,
        size_bytes INTEGER NOT NULL,
        hash       TEXT NOT NULL,
        pages      INTEGER,
        added_at   INTEGER NOT NULL,
        indexed_at INTEGER,
        error      TEXT
    );
    CREATE TABLE doc_chunks (
        id          INTEGER PRIMARY KEY,
        document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
        loc         INTEGER NOT NULL,
        heading     TEXT NOT NULL DEFAULT '',
        text        TEXT NOT NULL,
        embedding   BLOB
    );
    CREATE INDEX doc_chunks_document ON doc_chunks(document_id, loc);
    CREATE VIRTUAL TABLE doc_chunks_fts USING fts5(
        heading, text, content='doc_chunks', content_rowid='id',
        tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER doc_chunks_ai AFTER INSERT ON doc_chunks BEGIN
        INSERT INTO doc_chunks_fts(rowid, heading, text) VALUES (new.id, new.heading, new.text);
    END;
    CREATE TRIGGER doc_chunks_ad AFTER DELETE ON doc_chunks BEGIN
        INSERT INTO doc_chunks_fts(doc_chunks_fts, rowid, heading, text) VALUES ('delete', old.id, old.heading, old.text);
    END;",
    // 12: documents can be switched off for answers (kept, not searched)
    "ALTER TABLE documents ADD COLUMN active INTEGER NOT NULL DEFAULT 1;",
    // 13: document collections (by project), switched on or off as a whole
    "CREATE TABLE doc_collections (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
        active     INTEGER NOT NULL DEFAULT 1,
        created_at INTEGER NOT NULL
    );
    ALTER TABLE documents ADD COLUMN collection_id INTEGER REFERENCES doc_collections(id) ON DELETE SET NULL;",
    // 14: the decision log — decisions across meetings, from the summaries
    // (`origin` 'summary') or edited by the user, with the transcript time
    // they were made at, and the later decision that replaced one
    "CREATE TABLE decisions (
        id            INTEGER PRIMARY KEY,
        meeting_id    INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        text          TEXT NOT NULL,
        speaker       TEXT,
        at_secs       REAL,
        origin        TEXT NOT NULL DEFAULT 'summary',
        edited        INTEGER NOT NULL DEFAULT 0,
        superseded    INTEGER NOT NULL DEFAULT 0,
        superseded_by INTEGER REFERENCES decisions(id) ON DELETE SET NULL,
        created_at    INTEGER NOT NULL
    );
    CREATE INDEX decisions_meeting ON decisions(meeting_id);",
];

/// Moves a meeting's current unpinned notes into a new archived version;
/// returns its id (`None`, and no version, when there are none).
fn archive_unpinned(tx: &Connection, meeting_id: i64) -> Result<Option<i64>> {
    let n: i64 = tx.query_row("SELECT COUNT(*) FROM notes WHERE meeting_id = ?1 AND version_id IS NULL AND pinned = 0", [meeting_id], |r| r.get(0))?;
    if n == 0 {
        return Ok(None);
    }
    let focus: Option<String> = tx.query_row("SELECT notes_focus FROM meetings WHERE id = ?1", [meeting_id], |r| r.get(0)).optional()?.flatten();
    tx.execute("INSERT INTO note_versions (meeting_id, archived_at, focus) VALUES (?1, ?2, ?3)", params![meeting_id, Local::now().timestamp(), focus])?;
    let version = tx.last_insert_rowid();
    tx.execute("UPDATE notes SET version_id = ?2 WHERE meeting_id = ?1 AND version_id IS NULL AND pinned = 0", params![meeting_id, version])?;
    Ok(Some(version))
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )
}

fn local(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_else(Local::now)
}

fn source_of(s: String) -> rusqlite::Result<Source> {
    Source::parse(&s).ok_or_else(|| rusqlite::Error::InvalidColumnType(0, s, rusqlite::types::Type::Text))
}

impl Store {
    /// The language of every meeting (as stored: a code, or "auto").
    pub fn meeting_languages(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT language FROM meetings")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Direct access for the search index (`crate::ask`).
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("creating data directory")?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        anyhow::ensure!(
            version <= SCHEMA_VERSION,
            "database {} was created by a newer Mostalgia",
            path.display()
        );
        if version == 0 {
            conn.execute_batch(SCHEMA).context("creating database schema")?;
        }
        for (i, migration) in MIGRATIONS.iter().enumerate() {
            if version < i as i32 + 2 {
                conn.execute_batch(migration).context("upgrading database schema")?;
            }
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
    }

    /// Saves a meeting and its transcript; returns the meeting id. A
    /// meeting already stored from the same directory is replaced.
    pub fn insert_meeting(&mut self, m: &NewMeeting, segments: &[FinalSegment]) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let dir = m.dir.to_string_lossy();
        tx.execute("DELETE FROM meetings WHERE dir = ?1", [&dir])?;
        tx.execute(
            "INSERT INTO meetings (title, started_at, duration_secs, language, dir) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![m.title, m.started_at.timestamp(), m.duration_secs, m.language, dir],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut stmt = tx.prepare(
                "INSERT INTO segments (meeting_id, source, speaker, start_secs, end_secs, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for s in segments {
                stmt.execute(params![id, s.source.as_str(), s.speaker, s.start_secs, s.end_secs, s.text])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    /// Records which engine wrote a meeting's transcript and, if it failed,
    /// why the transcript is incomplete (`None` clears the error).
    pub fn set_transcript_info(&self, meeting_id: i64, engine: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE meetings SET transcript_engine = ?2, transcript_error = ?3 WHERE id = ?1",
            params![meeting_id, engine, error],
        )?;
        Ok(())
    }

    /// Replaces a meeting's transcript (after transcribing it again).
    pub fn replace_segments(&mut self, meeting_id: i64, segments: &[FinalSegment]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM segments WHERE meeting_id = ?1", [meeting_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO segments (meeting_id, source, speaker, start_secs, end_secs, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for s in segments {
                stmt.execute(params![meeting_id, s.source.as_str(), s.speaker, s.start_secs, s.end_secs, s.text])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list(&self, limit: usize) -> Result<Vec<MeetingSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.title, m.started_at, m.duration_secs,
                    (SELECT COUNT(*) FROM segments s WHERE s.meeting_id = m.id),
                    EXISTS (SELECT 1 FROM summaries x WHERE x.meeting_id = m.id),
                    m.transcript_engine
             FROM meetings m ORDER BY m.started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(MeetingSummary {
                id: r.get(0)?,
                title: r.get(1)?,
                started_at: local(r.get(2)?),
                duration_secs: r.get(3)?,
                segment_count: r.get(4)?,
                has_summary: r.get(5)?,
                text_import: crate::text_import::is_text_import(r.get::<_, Option<String>>(6)?.as_deref()),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Meeting>> {
        let head = self
            .conn
            .query_row(
                "SELECT title, started_at, duration_secs, language, dir, transcript_engine, transcript_error, companion FROM meetings WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((title, started_at, duration_secs, language, dir, transcript_engine, transcript_error, companion)) = head else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT id, source, speaker, start_secs, end_secs, text FROM segments
             WHERE meeting_id = ?1 ORDER BY start_secs, id",
        )?;
        let segments = stmt
            .query_map([id], |r| {
                Ok(StoredSegment {
                    id: r.get(0)?,
                    source: source_of(r.get(1)?)?,
                    speaker: r.get(2)?,
                    start_secs: r.get(3)?,
                    end_secs: r.get(4)?,
                    text: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let summary = self
            .conn
            .query_row(
                "SELECT model, created_at, markdown, provider, effort FROM summaries WHERE meeting_id = ?1",
                [id],
                |r| {
                    Ok(Summary {
                        model: r.get(0)?,
                        created_at: local(r.get(1)?),
                        markdown: r.get(2)?,
                        provider: r.get(3)?,
                        effort: r.get(4)?,
                    })
                },
            )
            .optional()?;
        let mut stmt = self
            .conn
            .prepare("SELECT at_secs, label FROM bookmarks WHERE meeting_id = ?1 ORDER BY at_secs, id")?;
        let bookmarks = stmt
            .query_map([id], |r| {
                let label: String = r.get(1)?;
                Ok(Bookmark {
                    at_secs: r.get(0)?,
                    kind: MarkKind::from_stored_label(&label),
                    label,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let notes = self.notes(id)?;
        let chat = self.companion_chat(id)?;
        Ok(Some(Meeting {
            id,
            title,
            started_at: local(started_at),
            duration_secs,
            language,
            dir: PathBuf::from(dir),
            segments,
            bookmarks,
            notes,
            chat,
            summary,
            transcript_engine,
            transcript_error,
            companion: companion.filter(|c| !c.is_empty()).unwrap_or_else(|| "mori".into()),
        }))
    }

    /// A meeting's title and start, without loading its transcript.
    pub fn title(&self, id: i64) -> Result<Option<(String, DateTime<Local>)>> {
        Ok(self
            .conn
            .query_row("SELECT title, started_at FROM meetings WHERE id = ?1", [id], |r| {
                Ok((r.get::<_, String>(0)?, local(r.get(1)?)))
            })
            .optional()?)
    }

    /// Full-text search over all transcripts. Every word must match (as a
    /// prefix, accents ignored); results are best matches first.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let fts_query = query
            .split_whitespace()
            .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.title, m.started_at, s.source, s.start_secs,
                    snippet(segments_fts, 0, '[', ']', '…', 16)
             FROM segments_fts
             JOIN segments s ON s.id = segments_fts.rowid
             JOIN meetings m ON m.id = s.meeting_id
             WHERE segments_fts MATCH ?1
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, limit as i64], |r| {
            Ok(SearchHit {
                meeting_id: r.get(0)?,
                title: r.get(1)?,
                started_at: local(r.get(2)?),
                source: source_of(r.get(3)?)?,
                start_secs: r.get(4)?,
                snippet: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_summary(
        &self,
        meeting_id: i64,
        provider: &str,
        model: &str,
        effort: Option<&str>,
        markdown: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO summaries (meeting_id, provider, model, effort, created_at, markdown)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(meeting_id) DO UPDATE SET provider = excluded.provider, model = excluded.model,
                 effort = excluded.effort, created_at = excluded.created_at, markdown = excluded.markdown",
            params![meeting_id, provider, model, effort, Local::now().timestamp(), markdown],
        )?;
        Ok(())
    }

    pub fn set_title(&self, meeting_id: i64, title: &str) -> Result<()> {
        self.conn
            .execute("UPDATE meetings SET title = ?1 WHERE id = ?2", params![title, meeting_id])?;
        Ok(())
    }

    pub fn add_bookmarks(&self, meeting_id: i64, bookmarks: &[Bookmark]) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("INSERT INTO bookmarks (meeting_id, at_secs, label) VALUES (?1, ?2, ?3)")?;
        for b in bookmarks {
            stmt.execute(params![meeting_id, b.at_secs, b.stored_label()])?;
        }
        Ok(())
    }

    /// Saves one of Mori's notes; returns its id.
    pub fn add_note(&self, meeting_id: i64, kind: &str, text: &str, at_secs: f64) -> Result<i64> {
        self.add_note_pinned(meeting_id, kind, text, at_secs, true)
    }

    /// Saves one of Mori's points; `pinned` marks the ones the user pinned.
    pub fn add_note_pinned(&self, meeting_id: i64, kind: &str, text: &str, at_secs: f64, pinned: bool) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO notes (meeting_id, kind, text, at_secs, created_at, pinned) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![meeting_id, kind, text, at_secs, Local::now().timestamp(), pinned],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Pins or unpins a saved note.
    pub fn set_note_pinned(&self, note_id: i64, pinned: bool) -> Result<()> {
        self.conn.execute("UPDATE notes SET pinned = ?2 WHERE id = ?1", params![note_id, pinned])?;
        Ok(())
    }

    /// A meeting's current notes (not the archived versions), by time.
    pub fn notes(&self, meeting_id: i64) -> Result<Vec<Note>> {
        self.notes_where("meeting_id = ?1 AND version_id IS NULL", meeting_id)
    }

    fn notes_where(&self, condition: &str, value: i64) -> Result<Vec<Note>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, kind, text, at_secs, created_at, pinned, origin FROM notes WHERE {condition} ORDER BY at_secs, id"
        ))?;
        let rows = stmt.query_map([value], |r| {
            Ok(Note {
                id: r.get(0)?,
                kind: r.get(1)?,
                text: r.get(2)?,
                at_secs: r.get(3)?,
                created_at: local(r.get(4)?),
                pinned: r.get(5)?,
                origin: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// What Mori was asked to focus on for this meeting's notes, if anything.
    pub fn notes_focus(&self, meeting_id: i64) -> Result<Option<String>> {
        let focus: Option<Option<String>> =
            self.conn.query_row("SELECT notes_focus FROM meetings WHERE id = ?1", [meeting_id], |r| r.get(0)).optional()?;
        Ok(focus.flatten().filter(|f| !f.trim().is_empty()))
    }

    /// Records which companion took a meeting's notes and chat.
    pub fn set_companion(&self, meeting_id: i64, companion: &str) -> Result<()> {
        self.conn.execute("UPDATE meetings SET companion = ?2 WHERE id = ?1", params![meeting_id, companion])?;
        Ok(())
    }

    pub fn set_notes_focus(&self, meeting_id: i64, focus: Option<&str>) -> Result<()> {
        let focus = focus.map(str::trim).filter(|f| !f.is_empty());
        self.conn.execute("UPDATE meetings SET notes_focus = ?2 WHERE id = ?1", params![meeting_id, focus])?;
        Ok(())
    }

    /// Replaces a meeting's unpinned notes with `notes` (kind, text,
    /// seconds; origin "regenerated"), taken with `focus`. Pinned notes
    /// stay. The replaced ones are archived as a version (restorable with
    /// [`Store::restore_note_version`]); returns its id (`None` when there
    /// was nothing to replace).
    pub fn replace_unpinned_notes(&mut self, meeting_id: i64, notes: &[(&str, &str, f64)], focus: Option<&str>) -> Result<Option<i64>> {
        let tx = self.conn.transaction()?;
        let version = archive_unpinned(&tx, meeting_id)?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO notes (meeting_id, kind, text, at_secs, created_at, pinned, origin) VALUES (?1, ?2, ?3, ?4, ?5, 0, 'regenerated')",
            )?;
            let now = Local::now().timestamp();
            for (kind, text, at) in notes {
                stmt.execute(params![meeting_id, kind, text, at, now])?;
            }
        }
        let focus = focus.map(str::trim).filter(|f| !f.is_empty());
        tx.execute("UPDATE meetings SET notes_focus = ?2 WHERE id = ?1", params![meeting_id, focus])?;
        tx.commit()?;
        Ok(version)
    }

    /// Brings an archived set of notes back: the current unpinned notes are
    /// archived in its place (so that can be undone too), and the meeting's
    /// focus becomes the restored set's. Returns the id of the version the
    /// current notes went to (`None` if there were none).
    pub fn restore_note_version(&mut self, meeting_id: i64, version_id: i64) -> Result<Option<i64>> {
        let tx = self.conn.transaction()?;
        let focus: Option<Option<String>> = tx
            .query_row("SELECT focus FROM note_versions WHERE id = ?1 AND meeting_id = ?2", params![version_id, meeting_id], |r| r.get(0))
            .optional()?;
        let Some(focus) = focus else { anyhow::bail!("note version {version_id} not found") };
        let archived = archive_unpinned(&tx, meeting_id)?;
        tx.execute("UPDATE notes SET version_id = NULL WHERE version_id = ?1", [version_id])?;
        tx.execute("DELETE FROM note_versions WHERE id = ?1", [version_id])?;
        tx.execute("UPDATE meetings SET notes_focus = ?2 WHERE id = ?1", params![meeting_id, focus])?;
        tx.commit()?;
        Ok(archived)
    }

    /// The archived sets of a meeting's notes, newest first.
    pub fn note_versions(&self, meeting_id: i64) -> Result<Vec<NoteVersion>> {
        let mut stmt = self.conn.prepare("SELECT id, archived_at, focus FROM note_versions WHERE meeting_id = ?1 ORDER BY id DESC")?;
        let heads: Vec<(i64, i64, Option<String>)> =
            stmt.query_map([meeting_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        heads
            .into_iter()
            .map(|(id, at, focus)| Ok(NoteVersion { id, archived_at: local(at), focus, notes: self.notes_where("version_id = ?1", id)? }))
            .collect()
    }

    /// Saves the chat with Mori from a recording (question, answer) with
    /// the meeting, after any already saved.
    pub fn add_companion_chat(&mut self, meeting_id: i64, turns: &[(String, String)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO companion_chat (meeting_id, question, answer, created_at) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let now = Local::now().timestamp();
            for (q, a) in turns {
                stmt.execute(params![meeting_id, q, a, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// A meeting's chat with Mori, oldest first.
    pub fn companion_chat(&self, meeting_id: i64) -> Result<Vec<ChatTurn>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, question, answer, created_at FROM companion_chat WHERE meeting_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([meeting_id], |r| {
            Ok(ChatTurn { id: r.get(0)?, question: r.get(1)?, answer: r.get(2)?, created_at: local(r.get(3)?) })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_note(&self, note_id: i64) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM notes WHERE id = ?1", [note_id])? > 0)
    }

    /// Names a speaker: every line currently shown as `current` (an
    /// explicit name, or "Ich"/"Andere" for a source) gets `name`.
    pub fn rename_speaker(&self, meeting_id: i64, current: &str, name: &str) -> Result<usize> {
        let name = name.trim();
        let new = (!name.is_empty()).then_some(name);
        let source = [Source::Mic, Source::System]
            .into_iter()
            .find(|s| s.speaker_label() == current)
            .map(Source::as_str);
        Ok(self.conn.execute(
            "UPDATE segments SET speaker = ?1 WHERE meeting_id = ?2
               AND (speaker = ?3 OR (speaker IS NULL AND source = ?4))",
            params![new, meeting_id, current, source],
        )?)
    }

    /// Gives some lines of a meeting to another speaker (an empty name
    /// clears it, back to "Ich" / "Andere"). Returns the labels they had.
    pub fn set_segments_speaker(&self, meeting_id: i64, segment_ids: &[i64], name: &str) -> Result<Vec<String>> {
        let name = name.trim();
        let new = (!name.is_empty()).then_some(name);
        let tx = self.conn.unchecked_transaction()?;
        let mut previous: Vec<String> = Vec::new();
        for id in segment_ids {
            let old: Option<(String, Option<String>)> = tx
                .query_row("SELECT source, speaker FROM segments WHERE id = ?1 AND meeting_id = ?2", params![id, meeting_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            let Some((source, speaker)) = old else { continue };
            let label = speaker.unwrap_or_else(|| Source::parse(&source).unwrap_or(Source::System).speaker_label().to_string());
            if !previous.contains(&label) {
                previous.push(label);
            }
            tx.execute("UPDATE segments SET speaker = ?1 WHERE id = ?2", params![new, id])?;
        }
        tx.commit()?;
        Ok(previous)
    }

    pub fn update_segment_text(&self, segment_id: i64, text: &str) -> Result<()> {
        self.conn
            .execute("UPDATE segments SET text = ?1 WHERE id = ?2", params![text.trim(), segment_id])?;
        Ok(())
    }

    pub fn delete(&self, meeting_id: i64) -> Result<bool> {
        self.delete_meeting(meeting_id, false)
    }

    /// Removes a meeting and everything derived from it in one transaction:
    /// transcript (and its search index rows), summary, bookmarks, Mori's notes and chat,
    /// speaker names, "ask" passages and their index. With `delete_audio`, the
    /// session's WAV tracks go too (and the folder, if then empty).
    pub fn delete_meeting(&self, meeting_id: i64, delete_audio: bool) -> Result<bool> {
        let dir: Option<String> = self
            .conn
            .query_row("SELECT dir FROM meetings WHERE id = ?1", [meeting_id], |r| r.get(0))
            .optional()?;
        let Some(dir) = dir else { return Ok(false) };
        let tx = self.conn.unchecked_transaction()?;
        // Explicit deletes (not only ON DELETE CASCADE), so the FTS delete
        // triggers run for every row whatever the connection's settings.
        for table in ["passages", "index_state", "segments", "summaries", "bookmarks", "notes", "note_versions", "companion_chat"] {
            if table_exists(&tx, table)? {
                tx.execute(&format!("DELETE FROM {table} WHERE meeting_id = ?1"), [meeting_id])?;
            }
        }
        tx.execute("DELETE FROM meetings WHERE id = ?1", [meeting_id])?;
        tx.commit()?;
        crate::ask::search::invalidate_cache();

        if delete_audio {
            let dir = PathBuf::from(dir);
            for source in [Source::Mic, Source::System] {
                let _ = std::fs::remove_file(dir.join(source.file_name()));
            }
            // Only removes the folder if nothing else is in it.
            let _ = std::fs::remove_dir(&dir);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(source: Source, start: f64, text: &str) -> FinalSegment {
        FinalSegment {
            source,
            start_secs: start,
            end_secs: start + 2.0,
            text: text.into(),
            speaker: None,
        }
    }

    #[test]
    fn insert_search_summarize_delete() -> Result<()> {
        let path = std::env::temp_dir().join(format!("mostalgia-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path)?;
        let meeting = NewMeeting {
            title: "Test",
            started_at: Local::now(),
            duration_secs: 60.0,
            language: "de",
            dir: Path::new("/tmp/session-1"),
        };
        let id = store.insert_meeting(
            &meeting,
            &[
                seg(Source::Mic, 1.0, "Wir besprechen das Budget."),
                seg(Source::System, 4.0, "Die Präsentation ist am Freitag."),
            ],
        )?;

        // Prefix match, accents ignored.
        let hits = store.search("prasent", 10)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, Source::System);
        assert!(hits[0].snippet.contains("[Präsentation]"));
        // Quotes in the query don't break FTS syntax.
        assert!(store.search("\"budget", 10)?.len() == 1);

        store.save_summary(id, "claude", "m", Some("high"), "# Titel\n\nText")?;
        store.set_title(id, "Titel")?;
        let m = store.get(id)?.unwrap();
        assert_eq!(m.title, "Titel");
        assert_eq!(m.segments.len(), 2);
        assert_eq!(m.segments[0].speaker_label(), "Ich");
        assert!(m.summary.is_some());

        // Re-importing the same directory replaces the meeting.
        let id2 = store.insert_meeting(&meeting, &[seg(Source::Mic, 0.0, "Neu")])?;
        assert_eq!(store.list(10)?.len(), 1);
        assert!(store.search("budget", 10)?.is_empty());

        assert!(store.delete(id2)?);
        assert!(store.list(10)?.is_empty());
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    fn notes_roundtrip_and_cascade() -> Result<()> {
        let path = std::env::temp_dir().join(format!("mostalgia-notes-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path)?;
        let new = |dir: &'static str| NewMeeting { title: "N", started_at: Local::now(), duration_secs: 60.0, language: "en", dir: Path::new(dir) };
        let a = store.insert_meeting(&new("/tmp/notes-a"), &[seg(Source::Mic, 1.0, "Hello")])?;
        let b = store.insert_meeting(&new("/tmp/notes-b"), &[])?;
        store.add_note(a, "action", "Anna: forecast by Friday", 36.8)?;
        let first = store.add_note(a, "decision", "Freeze marketing spend", 27.0)?;
        store.add_note(b, "fact", "Other meeting", 3.0)?;
        let m = store.get(a)?.unwrap();
        assert_eq!(m.notes.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["Freeze marketing spend", "Anna: forecast by Friday"]);
        assert_eq!(m.notes[0].kind, "decision");
        assert!(m.notes[0].pinned);
        let unpinned = store.add_note_pinned(a, "question", "Who reviews?", 40.0, false)?;
        assert!(!store.notes(a)?.iter().find(|n| n.id == unpinned).unwrap().pinned);
        store.set_note_pinned(unpinned, true)?;
        assert!(store.notes(a)?.iter().find(|n| n.id == unpinned).unwrap().pinned);
        store.delete_note(unpinned)?;
        assert!(store.delete_note(first)?);
        assert_eq!(store.notes(a)?.len(), 1);
        assert!(store.delete_meeting(a, false)?);
        let left: i64 = store.conn.query_row("SELECT COUNT(*) FROM notes WHERE meeting_id = ?1", [a], |r| r.get(0))?;
        assert_eq!(left, 0);
        assert_eq!(store.notes(b)?.len(), 1);
        // Plain cascade (a meeting replaced from the same directory).
        store.insert_meeting(&new("/tmp/notes-b"), &[])?;
        let all: i64 = store.conn.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))?;
        assert_eq!(all, 0);
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    fn regenerated_notes_replace_unpinned_and_can_be_restored() -> Result<()> {
        let path = std::env::temp_dir().join(format!("mostalgia-note-versions-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path)?;
        let a = store.insert_meeting(
            &NewMeeting { title: "V", started_at: Local::now(), duration_secs: 60.0, language: "en", dir: Path::new("/tmp/versions-a") },
            &[seg(Source::Mic, 1.0, "Hello")],
        )?;
        store.add_note_pinned(a, "decision", "Freeze marketing spend", 27.0, true)?;
        store.add_note_pinned(a, "fact", "Budget is 10% under plan", 5.0, false)?;
        store.add_note_pinned(a, "action", "Ben books the room", 40.0, false)?;
        store.set_notes_focus(a, Some("pricing"))?;
        let texts = |s: &Store| -> Result<Vec<String>> { Ok(s.notes(a)?.into_iter().map(|n| n.text).collect()) };

        let v1 = store.replace_unpinned_notes(a, &[("action", "Anna sends the Q3 forecast by Friday", 36.0)], Some("  deadlines "))?.unwrap();
        assert_eq!(texts(&store)?, ["Freeze marketing spend", "Anna sends the Q3 forecast by Friday"]);
        assert_eq!(store.notes(a)?[1].origin, "regenerated");
        assert_eq!(store.notes_focus(a)?.as_deref(), Some("deadlines"));
        let versions = store.note_versions(a)?;
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].focus.as_deref(), Some("pricing"));
        assert_eq!(versions[0].notes.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["Budget is 10% under plan", "Ben books the room"]);
        // The meeting (and search) only sees the current notes.
        assert_eq!(store.get(a)?.unwrap().notes.len(), 2);

        // Undo: the old set comes back, the regenerated one is archived.
        let v2 = store.restore_note_version(a, v1)?.unwrap();
        assert_eq!(texts(&store)?, ["Budget is 10% under plan", "Freeze marketing spend", "Ben books the room"]);
        assert_eq!(store.notes_focus(a)?.as_deref(), Some("pricing"));
        let versions = store.note_versions(a)?;
        assert_eq!((versions.len(), versions[0].id), (1, v2));
        assert_eq!(versions[0].focus.as_deref(), Some("deadlines"));
        // Redo.
        store.restore_note_version(a, v2)?;
        assert_eq!(texts(&store)?, ["Freeze marketing spend", "Anna sends the Q3 forecast by Friday"]);
        assert!(store.restore_note_version(a, 9999).is_err());
        // Nothing unpinned: no empty version.
        store.set_note_pinned(store.notes(a)?[1].id, true)?;
        assert_eq!(store.replace_unpinned_notes(a, &[], None)?, None);
        assert_eq!(store.notes_focus(a)?, None);

        assert!(store.delete_meeting(a, false)?);
        let left: i64 = store.conn.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))?;
        let versions: i64 = store.conn.query_row("SELECT COUNT(*) FROM note_versions", [], |r| r.get(0))?;
        assert_eq!((left, versions), (0, 0));
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    fn companion_chat_roundtrip_and_cascade() -> Result<()> {
        let path = std::env::temp_dir().join(format!("mostalgia-chat-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path)?;
        let new = |dir: &'static str| NewMeeting { title: "C", started_at: Local::now(), duration_secs: 60.0, language: "en", dir: Path::new(dir) };
        let a = store.insert_meeting(&new("/tmp/chat-a"), &[])?;
        let b = store.insert_meeting(&new("/tmp/chat-b"), &[])?;
        store.add_companion_chat(a, &[("What was decided?".into(), "Spend is frozen [[t:27]].".into()), ("Who?".into(), "Anna [[t:36]].".into())])?;
        store.add_companion_chat(a, &[("And then?".into(), "Nothing yet.".into())])?;
        store.add_companion_chat(b, &[("Other?".into(), "Other.".into())])?;
        store.add_companion_chat(b, &[])?;
        let m = store.get(a)?.unwrap();
        assert_eq!(m.chat.iter().map(|t| t.question.as_str()).collect::<Vec<_>>(), ["What was decided?", "Who?", "And then?"]);
        assert_eq!(m.chat[0].answer, "Spend is frozen [[t:27]].");
        assert!(store.delete_meeting(a, false)?);
        assert!(store.companion_chat(a)?.is_empty());
        assert_eq!(store.companion_chat(b)?.len(), 1);
        // Plain cascade (a meeting replaced from the same directory).
        store.insert_meeting(&new("/tmp/chat-b"), &[])?;
        let all: i64 = store.conn.query_row("SELECT COUNT(*) FROM companion_chat", [], |r| r.get(0))?;
        assert_eq!(all, 0);
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    fn source_switches_are_stored_as_bookmarks() -> Result<()> {
        let path = std::env::temp_dir().join(format!("mostalgia-marks-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path)?;
        let id = store.insert_meeting(
            &NewMeeting { title: "M", started_at: Local::now(), duration_secs: 900.0, language: "de", dir: Path::new("/tmp/marks") },
            &[seg(Source::System, 1.0, "Hallo")],
        )?;
        let marks = vec![
            Bookmark { at_secs: 41.0, label: "Markierung 1".into(), kind: MarkKind::Moment },
            Bookmark::source_switch(Source::Mic, false, 754.0),
            Bookmark::source_switch(Source::Mic, true, 902.5),
            Bookmark::source_switch(Source::System, false, 903.0),
            Bookmark::source_switch(Source::System, true, 904.0),
        ];
        store.add_bookmarks(id, &marks)?;
        let back = store.get(id)?.unwrap().bookmarks;
        assert_eq!(back.iter().map(|b| (b.at_secs, b.kind)).collect::<Vec<_>>(), marks.iter().map(|b| (b.at_secs, b.kind)).collect::<Vec<_>>());
        assert_eq!(back[0].display_label(), "Markierung 1");
        assert_eq!(back[1].display_label(), crate::i18n::tr("Mikrofon aus"));
        // Stored in the existing label column, no schema change.
        let raw: String = store.conn.query_row("SELECT label FROM bookmarks WHERE meeting_id = ?1 AND at_secs = 754.0", [id], |r| r.get(0))?;
        assert_eq!(raw, "mostalgia:mic_off");
        for kind in ["", "mic_off", "mic_on", "system_off", "system_on"] {
            assert_eq!(MarkKind::parse(kind).as_str(), kind);
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    fn delete_removes_everything_searchable() -> Result<()> {
        let path = std::env::temp_dir().join(format!("mostalgia-del-{}.db", std::process::id()));
        let dir = std::env::temp_dir().join(format!("mostalgia-del-session-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("mic.wav"), b"x")?;
        let mut store = Store::open(&path)?;
        let keep = store.insert_meeting(
            &NewMeeting { title: "Bleibt", started_at: Local::now(), duration_secs: 60.0, language: "de", dir: Path::new("/tmp/keep") },
            &[seg(Source::Mic, 1.0, "Die Präsentation ist am Freitag.")],
        )?;
        let gone = store.insert_meeting(
            &NewMeeting { title: "Weg", started_at: Local::now(), duration_secs: 60.0, language: "de", dir: &dir },
            &[seg(Source::Mic, 1.0, "Geheimprojekt Zebra wird eingestellt.")],
        )?;
        store.save_summary(gone, "claude", "m", None, "# Weg\n\nZebra")?;
        store.add_bookmarks(gone, &[Bookmark { at_secs: 1.0, label: "x".into(), kind: MarkKind::Moment }])?;
        store.add_note(gone, "fact", "Zebra", 1.0)?;
        store.add_companion_chat(gone, &[("Zebra?".into(), "Yes [[t:1]].".into())])?;
        crate::ask::index::index_meeting(&mut store, gone)?;
        crate::ask::index::index_meeting(&mut store, keep)?;
        let filter = crate::ask::search::Filter::default();
        assert!(!store.search("zebra", 10)?.is_empty());
        assert!(!crate::ask::search::search(&store, "Zebra", &filter, 10)?.is_empty());

        assert!(store.delete_meeting(gone, true)?);
        // Transcript BM25, passage BM25 + vectors (hybrid search), raw FTS rows.
        assert!(store.search("zebra", 10)?.is_empty());
        assert!(crate::ask::search::search(&store, "Zebra", &filter, 10)?.iter().all(|h| h.meeting_id != gone));
        let fts_rows: i64 = store.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM segments_fts WHERE segments_fts MATCH 'zebra')
                  + (SELECT COUNT(*) FROM passages_fts WHERE passages_fts MATCH 'zebra')",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(fts_rows, 0);
        store.conn.execute_batch(
            "INSERT INTO segments_fts(segments_fts) VALUES('integrity-check');
             INSERT INTO passages_fts(passages_fts) VALUES('integrity-check');",
        )?;
        for table in ["segments", "summaries", "bookmarks", "passages", "index_state", "notes", "companion_chat"] {
            let n: i64 = store.conn.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE meeting_id = ?1"), [gone], |r| r.get(0))?;
            assert_eq!(n, 0, "{table}");
        }
        assert!(!dir.exists(), "audio folder removed");
        // The other meeting is untouched.
        assert!(!store.search("präsentation", 10)?.is_empty());
        std::fs::remove_file(&path)?;
        Ok(())
    }
}