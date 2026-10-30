//! Deleting old meetings' audio: after `Config::audio_keep_days` a
//! meeting's WAV tracks go, and everything else (transcript, summary,
//! notes, speakers) stays. Only the tracks in the meeting's own folder
//! under the recordings directory are ever removed, never a meeting that
//! is busy, unfinished, waiting for crash recovery or pinned with "Keep
//! audio". Each deletion leaves [`DELETED`] in the folder (so the meeting
//! can say why its audio is gone) and a line in `cleanup.log`.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};

use crate::Source;
use crate::store::Store;

/// Marker: this meeting's audio was deleted, when and after how many days.
pub const DELETED: &str = "audio-deleted.json";
/// Marker: keep this meeting's audio.
pub const KEEP: &str = "keep-audio";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Deleted {
    /// Unix seconds.
    pub at: i64,
    pub days: u32,
}

/// A meeting whose audio the next cleanup would delete.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub meeting_id: i64,
    pub title: String,
    pub dir: PathBuf,
    pub bytes: u64,
}

/// How much audio there is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub meetings: usize,
    pub bytes: u64,
}

fn tracks(dir: &Path) -> Vec<PathBuf> {
    [Source::Mic, Source::System].into_iter().map(|s| dir.join(s.file_name())).filter(|p| p.is_file()).collect()
}

fn size(dir: &Path) -> u64 {
    tracks(dir).iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum()
}

/// `dir` is a folder of its own inside `recordings` (after resolving links).
fn inside(dir: &Path, recordings: &Path) -> bool {
    match (dir.canonicalize(), recordings.canonicalize()) {
        (Ok(d), Ok(r)) => d != r && d.starts_with(&r),
        _ => false,
    }
}

pub fn is_kept(dir: &Path) -> bool {
    dir.join(KEEP).exists()
}

/// Pins a meeting's audio (or unpins it).
pub fn set_kept(dir: &Path, keep: bool) -> Result<()> {
    if keep {
        std::fs::write(dir.join(KEEP), b"").context("pinning the audio")
    } else {
        match std::fs::remove_file(dir.join(KEEP)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).context("unpinning the audio"),
            _ => Ok(()),
        }
    }
}

/// When and after how many days the audio in `dir` was deleted, if it was.
pub fn deleted(dir: &Path) -> Option<Deleted> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(DELETED)).ok()?).ok()
}

struct Row {
    id: i64,
    title: String,
    started_at: i64,
    duration_secs: f64,
    dir: PathBuf,
    error: Option<String>,
}

fn rows(store: &Store) -> Result<Vec<Row>> {
    let mut stmt = store.conn().prepare("SELECT id, title, started_at, duration_secs, dir, transcript_error FROM meetings")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                title: r.get(1)?,
                started_at: r.get(2)?,
                duration_secs: r.get(3)?,
                dir: PathBuf::from(r.get::<_, String>(4)?),
                error: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Audio on disk in the library's meetings.
pub fn usage(store: &Store) -> Result<Usage> {
    let mut usage = Usage::default();
    for row in rows(store)? {
        let bytes = size(&row.dir);
        if bytes > 0 {
            usage.meetings += 1;
            usage.bytes += bytes;
        }
    }
    Ok(usage)
}

/// What a cleanup keeping `days` of audio would delete at `now`; `busy`
/// are meetings being worked on (never touched).
pub fn plan(store: &Store, recordings: &Path, days: u32, now: DateTime<Local>, busy: &[i64]) -> Result<Vec<Candidate>> {
    if days == 0 {
        return Ok(Vec::new());
    }
    let cutoff = now.timestamp() - days as i64 * 86_400;
    let mut out = Vec::new();
    for row in rows(store)? {
        // An old recording imported today counts from today.
        let added = std::fs::metadata(&row.dir).and_then(|m| m.modified()).ok().map_or(0, |t| DateTime::<Local>::from(t).timestamp());
        let ended = (row.started_at + row.duration_secs as i64).max(added.min(now.timestamp()));
        let safe = ended <= cutoff
            && !busy.contains(&row.id)
            // An unfinished transcript may still need the audio.
            && row.error.is_none()
            && inside(&row.dir, recordings)
            && !is_kept(&row.dir)
            && !row.dir.join(crate::recovery::MANIFEST).exists();
        if !safe {
            continue;
        }
        let bytes = size(&row.dir);
        if bytes > 0 {
            out.push(Candidate { meeting_id: row.id, title: row.title, dir: row.dir, bytes });
        }
    }
    Ok(out)
}

/// Deletes a candidate's tracks (only those), marks the folder and logs it.
pub fn delete(c: &Candidate, days: u32, recordings: &Path) -> Result<()> {
    anyhow::ensure!(inside(&c.dir, recordings), "{} is not in {}", c.dir.display(), recordings.display());
    let marker = Deleted { at: Local::now().timestamp(), days };
    std::fs::write(c.dir.join(DELETED), serde_json::to_string(&marker)?).with_context(|| format!("marking {}", c.dir.display()))?;
    for track in tracks(&c.dir) {
        std::fs::remove_file(&track).with_context(|| format!("deleting {}", track.display()))?;
    }
    let line = format!(
        "{} deleted the audio of meeting {} \"{}\" ({:.1} MB, older than {days} days) in {}\n",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        c.meeting_id,
        c.title,
        c.bytes as f64 / 1e6,
        c.dir.display()
    );
    eprint!("[mostalgia] {line}");
    // Next to the recordings directory (the data directory).
    let log = recordings.parent().unwrap_or(recordings).join("cleanup.log");
    std::fs::OpenOptions::new().create(true).append(true).open(&log).and_then(|mut f| f.write_all(line.as_bytes())).with_context(|| format!("writing {}", log.display()))?;
    Ok(())
}

/// "1.2 GB" / "340 MB".
pub fn human(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", (bytes as f64 / 1e6).round() as u64)
    }
}

/// Unix seconds as a local time (for the marker).
pub fn local(at: i64) -> DateTime<Local> {
    Local.timestamp_opt(at, 0).single().unwrap_or_else(Local::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewMeeting;

    struct Scratch {
        root: PathBuf,
        recordings: PathBuf,
        store: Store,
    }

    fn scratch(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!("mostalgia-cleanup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let recordings = root.join("recordings");
        std::fs::create_dir_all(&recordings).unwrap();
        let store = Store::open(&root.join("db.sqlite")).unwrap();
        Scratch { root, recordings, store }
    }

    fn set_dir_modified(dir: &Path, t: std::time::SystemTime) {
        // Windows opens a folder only with backup semantics, and needs the
        // right to write its attributes to change the time.
        #[cfg(windows)]
        let f = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            std::fs::OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(dir).unwrap()
        };
        #[cfg(not(windows))]
        let f = std::fs::File::open(dir).unwrap();
        f.set_modified(t).unwrap();
    }

    fn meeting(s: &mut Scratch, name: &str, days_ago: i64, dir: Option<PathBuf>) -> (i64, PathBuf) {
        let dir = dir.unwrap_or_else(|| s.recordings.join(name));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mic.wav"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("system.wav"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
        let started_at = Local::now() - chrono::Duration::days(days_ago);
        let id = s.store.insert_meeting(&NewMeeting { title: name, started_at, duration_secs: 3600.0, language: "de", dir: &dir }, &[]).unwrap();
        // The folder as old as the recording.
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(days_ago as u64 * 86_400);
        set_dir_modified(&dir, then);
        (id, dir)
    }

    #[test]
    fn deletes_only_old_unpinned_idle_tracks() {
        let mut s = scratch("plan");
        let (old, old_dir) = meeting(&mut s, "old", 10, None);
        let (_new, _) = meeting(&mut s, "new", 2, None);
        let (_kept, kept_dir) = meeting(&mut s, "kept", 30, None);
        set_kept(&kept_dir, true).unwrap();
        let (busy, _) = meeting(&mut s, "busy", 30, None);
        let (_unfinished, unfinished_dir) = meeting(&mut s, "unfinished", 30, None);
        s.store.set_transcript_info(_unfinished, "live", Some("engine failed")).unwrap();
        let (_recovering, rec_dir) = meeting(&mut s, "recovering", 30, None);
        std::fs::write(rec_dir.join(crate::recovery::MANIFEST), b"{}").unwrap();
        // A meeting whose folder is outside the recordings directory.
        let elsewhere = s.root.join("elsewhere");
        let (_outside, outside_dir) = meeting(&mut s, "outside", 30, Some(elsewhere));

        assert_eq!(usage(&s.store).unwrap(), Usage { meetings: 7, bytes: 7 * 4000 });
        assert!(plan(&s.store, &s.recordings, 0, Local::now(), &[]).unwrap().is_empty(), "off");
        // Imported today, though recorded long ago: counts from today.
        let (_imported, imported_dir) = meeting(&mut s, "imported", 2, None);
        s.store.conn().execute("UPDATE meetings SET started_at = started_at - 400 * 86400 WHERE id = ?1", [_imported]).unwrap();
        let plan = plan(&s.store, &s.recordings, 7, Local::now(), &[busy]).unwrap();
        assert!(imported_dir.join("mic.wav").exists());
        assert_eq!(plan.iter().map(|c| c.meeting_id).collect::<Vec<_>>(), [old]);
        assert_eq!(plan[0].bytes, 4000);

        delete(&plan[0], 7, &s.recordings).unwrap();
        assert!(!old_dir.join("mic.wav").exists() && !old_dir.join("system.wav").exists());
        assert!(old_dir.join("notes.txt").exists(), "nothing else in the folder");
        assert_eq!(deleted(&old_dir).map(|d| d.days), Some(7));
        assert!(unfinished_dir.join("mic.wav").exists() && outside_dir.join("mic.wav").exists() && kept_dir.join("mic.wav").exists());
        // Refuses a folder outside the recordings directory outright.
        let outside = Candidate { meeting_id: 0, title: String::new(), dir: outside_dir.clone(), bytes: 0 };
        assert!(delete(&outside, 7, &s.recordings).is_err());
        assert!(outside_dir.join("mic.wav").exists());
        set_kept(&kept_dir, false).unwrap();
        assert!(!is_kept(&kept_dir));
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(human(340_400_000), "340 MB");
        assert_eq!(human(1_260_000_000), "1.3 GB");
    }
}
