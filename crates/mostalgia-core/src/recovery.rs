//! Crash safety for recordings. While a recording runs, its session folder
//! holds a small manifest ([`MANIFEST`]) and its WAV tracks are flushed with
//! valid header sizes and synced to disk every [`SYNC_SECS`] (see
//! `audio::TrackWriter`). A freeze, crash, `kill -9` or power loss leaves the
//! manifest behind; on the next start [`orphans`] finds the folder and
//! [`recording`] repairs the tracks (headers fixed up to the last whole
//! sample on disk) so the normal final pass can turn it into a meeting.
//! [`finish`] removes the manifest once the meeting is in the library.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};

use crate::Source;
use crate::recorder::Recording;

/// The "recording in progress" marker in a session folder.
pub const MANIFEST: &str = "recording.json";
/// How often the tracks are flushed (header sizes included) and synced to
/// disk: the most audio a crash or power loss can cost.
pub const SYNC_SECS: f64 = 2.0;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    /// Unix seconds the recording started.
    pub started_at: i64,
    /// The process recording it (a live one is never recovered).
    pub pid: u32,
}

/// Marks `dir` as a recording in progress (written atomically and synced).
pub fn begin(dir: &Path, manifest: &Manifest) -> Result<()> {
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    let mut f = std::fs::File::create(&tmp).context("writing the recording manifest")?;
    f.write_all(serde_json::to_string_pretty(manifest)?.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(MANIFEST))?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// The recording in `dir` is safely in the library: no recovery needed.
pub fn finish(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(MANIFEST));
}

pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).context("reading the recording manifest")?;
    serde_json::from_str(&text).context("reading the recording manifest")
}

/// Whether `pid` is a running Mostalgia process (not just any process that
/// got the number since).
fn alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(not(target_os = "macos"))]
    let command = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    // No /proc on macOS: the executable's path (its name is mostalgia-app
    // or mostalgia, inside Mostalgia.app or not).
    #[cfg(target_os = "macos")]
    let command = {
        let mut buf = [0u8; 4096];
        let n = unsafe { libc::proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
        buf[..n.max(0) as usize].to_vec()
    };
    String::from_utf8_lossy(&command).contains("mostalgia")
}

/// Session folders under `recordings` left behind by a recording that never
/// finished (its process is gone), oldest first.
pub fn orphans(recordings: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(recordings) else { return Vec::new() };
    let mut found: Vec<(i64, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(MANIFEST).exists())
        .filter_map(|p| match read_manifest(&p) {
            Ok(m) if alive(m.pid) => None,
            Ok(m) => Some((m.started_at, p)),
            // Unreadable: still left behind; `recording` says why.
            Err(_) => Some((0, p)),
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

/// Makes a WAV cut off by a crash readable: the RIFF and data sizes are set
/// to what is on disk, down to the last whole 16-bit sample. Returns the
/// samples it holds.
pub fn repair_wav(path: &Path) -> Result<u64> {
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path).with_context(|| format!("opening {}", path.display()))?;
    let len = f.metadata()?.len();
    let mut head = vec![0u8; len.min(4096) as usize];
    f.read_exact(&mut head)?;
    anyhow::ensure!(head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE", "{}: not a WAV file", path.display());
    // Walk the chunks to "data".
    let mut at = 12usize;
    let data_start = loop {
        anyhow::ensure!(at + 8 <= head.len(), "{}: no audio data", path.display());
        let id = &head[at..at + 4];
        let size = u32::from_le_bytes(head[at + 4..at + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            break at + 8;
        }
        at += 8 + size + (size & 1);
    };
    let bytes = (len.saturating_sub(data_start as u64)) & !1;
    // A cut mid-sample: drop the half sample.
    f.set_len(data_start as u64 + bytes)?;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&((data_start as u64 - 8 + bytes) as u32).to_le_bytes())?;
    f.seek(SeekFrom::Start(data_start as u64 - 4))?;
    f.write_all(&(bytes as u32).to_le_bytes())?;
    f.sync_all()?;
    Ok(bytes / 2)
}

/// The unfinished recording in `dir`, its tracks repaired, ready for
/// `finalize::save_recording` (no live transcript: that was in memory).
pub fn recording(dir: &Path) -> Result<Recording> {
    let manifest = read_manifest(dir)?;
    let mut tracks = Vec::new();
    let mut samples = 0u64;
    for source in [Source::Mic, Source::System] {
        let path = dir.join(source.file_name());
        if !path.exists() {
            continue;
        }
        let n = repair_wav(&path)?;
        samples = samples.max(n);
        tracks.push((source, path));
    }
    anyhow::ensure!(!tracks.is_empty(), "{}", crate::i18n::trf("Kein mic.wav oder system.wav in {}", &[&dir.display()]));
    let started_at: DateTime<Local> = Local.timestamp_opt(manifest.started_at, 0).single().unwrap_or_else(Local::now);
    Ok(Recording {
        dir: dir.to_path_buf(),
        started_at,
        duration_secs: samples as f64 / crate::SAMPLE_RATE as f64,
        tracks,
        live_lines: Vec::new(),
        live_errors: 0,
        languages: [None, None],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::TrackWriter;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    const SR: usize = crate::SAMPLE_RATE as usize;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mostalgia-recovery-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Records `secs` of a ramp in 100 ms blocks, toggling `muted` on the
    /// way, then "dies": the writer is leaked, never finalized.
    fn record_and_die(path: &Path, secs: usize, muted_at: Option<(usize, usize)>) {
        let mut track = TrackWriter::create(path, Source::Mic, Arc::new(AtomicU64::new(0)), None).unwrap();
        for b in 0..secs * 10 {
            let muted = muted_at.is_some_and(|(from, to)| (from * 10..to * 10).contains(&b));
            track.push(vec![0.25; SR / 10], muted).unwrap();
        }
        std::mem::forget(track);
    }

    #[test]
    fn killed_recording_loses_at_most_the_sync_interval() {
        let dir = scratch("kill");
        let path = dir.join("mic.wav");
        record_and_die(&path, 30, None);
        // Before repair the header only knows the last sync.
        let n = repair_wav(&path).unwrap();
        let lost = 30.0 - n as f64 / SR as f64;
        assert!((0.0..=SYNC_SECS + 0.5).contains(&lost), "lost {lost:.2} s");
        let samples = crate::finalize::read_wav(&path).unwrap();
        assert_eq!(samples.len() as u64, n, "playable after repair");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_anywhere_is_repaired() {
        let dir = scratch("trunc");
        let path = dir.join("system.wav");
        record_and_die(&path, 12, None);
        let full = std::fs::read(&path).unwrap();
        // Cut points: inside the header's tail, mid-sample, and around.
        let mut seed = 0x9e37_79b9_u64;
        for _ in 0..25 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let cut = 44 + (seed as usize % (full.len() - 44));
            std::fs::write(&path, &full[..cut]).unwrap();
            let n = repair_wav(&path).unwrap();
            assert_eq!(n as usize, (cut - 44) / 2, "cut at {cut}");
            assert_eq!(crate::finalize::read_wav(&path).unwrap().len(), n as usize);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphan_is_recovered_with_aligned_tracks() {
        let root = scratch("orphan");
        let dir = root.join("session-1793188873");
        std::fs::create_dir_all(&dir).unwrap();
        // A process number nothing uses.
        begin(&dir, &Manifest { started_at: 1793188873, pid: 0x3fff_fff0 }).unwrap();
        record_and_die(&dir.join("mic.wav"), 20, Some((5, 9)));
        record_and_die(&dir.join("system.wav"), 20, None);
        assert_eq!(orphans(&root), [dir.clone()]);
        let rec = recording(&dir).unwrap();
        assert_eq!(rec.tracks.len(), 2);
        assert!(rec.duration_secs >= 20.0 - SYNC_SECS - 0.5, "{}", rec.duration_secs);
        // Muting wrote silence, so both tracks cover the same time.
        let lens: Vec<usize> = rec.tracks.iter().map(|(_, p)| crate::finalize::read_wav(p).unwrap().len()).collect();
        assert!(lens[0].abs_diff(lens[1]) <= (SYNC_SECS as usize + 1) * SR, "{lens:?}");
        assert_eq!(rec.started_at.timestamp(), 1793188873);
        finish(&dir);
        assert!(orphans(&root).is_empty());
        // A recording still running (this process) is never an orphan.
        begin(&dir, &Manifest { started_at: 1, pid: std::process::id() }).unwrap();
        assert!(orphans(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
