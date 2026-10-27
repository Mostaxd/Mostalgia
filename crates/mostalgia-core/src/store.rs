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
}

pub struct Bookmark {
    pub at_secs: f64,
    pub label: String,
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
    pub summary: Option<Summary>,
    /// The engine that wrote the transcript ("Whisper … (lokal)", "OpenAI · whisper-1").
    pub transcript_engine: Option<String>,
    /// Why the transcript is incomplete (the engine failed), if it is.
    pub transcript_error: Option<String>,
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

const SCHEMA_VERSION: i32 = 5;

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
];

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
                    EXISTS (SELECT 1 FROM summaries x WHERE x.meeting_id = m.id)
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
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Meeting>> {
        let head = self
            .conn
            .query_row(
                "SELECT title, started_at, duration_secs, language, dir, transcript_engine, transcript_error FROM meetings WHERE id = ?1",
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
                    ))
                },
            )
            .optional()?;
        let Some((title, started_at, duration_secs, language, dir, transcript_engine, transcript_error)) = head else {
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
            .prepare("SELECT at_secs, label FROM bookmarks WHERE meeting_id = ?1 ORDER BY at_secs")?;
        let bookmarks = stmt
            .query_map([id], |r| {
                Ok(Bookmark {
                    at_secs: r.get(0)?,
                    label: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(Meeting {
            id,
            title,
            started_at: local(started_at),
            duration_secs,
            language,
            dir: PathBuf::from(dir),
            segments,
            bookmarks,
            summary,
            transcript_engine,
            transcript_error,
        }))
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
            stmt.execute(params![meeting_id, b.at_secs, b.label])?;
        }
        Ok(())
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

    pub fn update_segment_text(&self, segment_id: i64, text: &str) -> Result<()> {
        self.conn
            .execute("UPDATE segments SET text = ?1 WHERE id = ?2", params![text.trim(), segment_id])?;
        Ok(())
    }

    pub fn delete(&self, meeting_id: i64) -> Result<bool> {
        self.delete_meeting(meeting_id, false)
    }

    /// Removes a meeting and everything derived from it in one transaction:
    /// transcript (and its search index rows), summary, bookmarks, speaker
    /// names, "ask" passages and their index. With `delete_audio`, the
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
        for table in ["passages", "index_state", "segments", "summaries", "bookmarks"] {
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
        store.add_bookmarks(gone, &[Bookmark { at_secs: 1.0, label: "x".into() }])?;
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
        for table in ["segments", "summaries", "bookmarks", "passages", "index_state"] {
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