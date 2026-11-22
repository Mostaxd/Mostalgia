//! Importing a video by its link (YouTube, and the other sites yt-dlp
//! knows): yt-dlp downloads its sound into a scratch folder, then it is
//! imported like a file (see `media_import`) and the scratch folder goes.
//!
//! yt-dlp is fetched on first use from its GitHub releases (the
//! self-contained build for this system, no Python needed) into the data
//! directory's `tools/`, and fetched again when it is a week old: sites
//! change often and an old yt-dlp stops working. Without a network that
//! reaches GitHub, a yt-dlp in PATH is used instead.
//!
//! The audio-only M4A (AAC) stream is preferred, which decodes without
//! ffmpeg; other formats go through ffmpeg like any import.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};

use crate::i18n::tr;

const RELEASES: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/";
/// A managed yt-dlp older than this is fetched again before use.
const REFRESH_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);

/// `s` as a link to import (`https://` added when it was left out), or
/// `None` when it isn't one.
pub fn normalize(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return None;
    }
    let url = if s.starts_with("http://") || s.starts_with("https://") { s.to_string() } else { format!("https://{s}") };
    let host = url.split("://").nth(1)?.split(['/', '?', '#']).next()?;
    (host.contains('.') && !host.starts_with('.') && !host.ends_with('.')).then_some(url)
}

/// Stages of a link import, for progress displays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    /// Fetching yt-dlp itself (first use, or a weekly refresh).
    Tool(Option<f32>),
    /// Downloading the video's sound.
    Downloading(Option<f32>),
    /// Then the file import.
    Import(crate::media_import::Stage),
}

/// The release asset for this system.
fn asset() -> Option<&'static str> {
    if cfg!(target_os = "windows") {
        Some(if cfg!(target_arch = "aarch64") { "yt-dlp_arm64.exe" } else { "yt-dlp.exe" })
    } else if cfg!(target_os = "macos") {
        Some("yt-dlp_macos")
    } else if cfg!(target_os = "linux") {
        match std::env::consts::ARCH {
            "x86_64" => Some("yt-dlp_linux"),
            "aarch64" => Some("yt-dlp_linux_aarch64"),
            _ => None,
        }
    } else {
        None
    }
}

/// Where the fetched yt-dlp lives.
fn managed_path() -> PathBuf {
    crate::paths::data_dir().join("tools").join(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" })
}

fn works(program: &Path) -> bool {
    crate::media_import::quiet_command(program).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// A yt-dlp to run: the managed one (fetched or refreshed as needed),
/// else one in PATH.
fn ensure_tool(progress: &dyn Fn(Stage)) -> Result<PathBuf> {
    let managed = managed_path();
    let stale = managed
        .metadata()
        .and_then(|m| m.modified())
        .map(|t| SystemTime::now().duration_since(t).unwrap_or_default() > REFRESH_AFTER)
        .unwrap_or(true);
    if !stale && works(&managed) {
        return Ok(managed);
    }
    let fetched = fetch_tool(&managed, progress);
    if fetched.is_ok() && works(&managed) {
        return Ok(managed);
    }
    // An older managed copy still beats nothing.
    if managed.exists() && works(&managed) {
        return Ok(managed);
    }
    let in_path = crate::media_import::tool_path("yt-dlp");
    if works(&in_path) {
        return Ok(in_path);
    }
    match fetched {
        Err(e) => Err(e.context(tr("yt-dlp ließ sich nicht herunterladen"))),
        Ok(()) => bail!("{}", tr("Das heruntergeladene yt-dlp startet nicht")),
    }
}

fn fetch_tool(dest: &Path, progress: &dyn Fn(Stage)) -> Result<()> {
    let Some(asset) = asset() else { bail!("{}", tr("Für dieses System gibt es kein fertiges yt-dlp – bitte yt-dlp installieren")) };
    progress(Stage::Tool(None));
    std::fs::create_dir_all(dest.parent().context("tools folder")?)?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(None)
        .user_agent(concat!("Mostalgia/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut response = client.get(format!("{RELEASES}{asset}")).send()?;
    if !response.status().is_success() {
        bail!("HTTP {}", response.status());
    }
    let total = response.content_length().filter(|&l| l > 0);
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    let mut last = std::time::Instant::now();
    loop {
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        got += n as u64;
        if last.elapsed() >= Duration::from_millis(200) {
            last = std::time::Instant::now();
            progress(Stage::Tool(total.map(|t| (got as f64 / t as f64).min(1.0) as f32)));
        }
    }
    file.sync_all()?;
    drop(file);
    if let Some(t) = total {
        anyhow::ensure!(got == t, "{got} of {t} bytes");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
    }
    // Windows can't replace a file that is running; nothing runs it now.
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// What a download brought back.
struct Downloaded {
    file: PathBuf,
    title: String,
}

/// Downloads `url`'s sound into `dir` with `tool`.
fn download(tool: &Path, url: &str, dir: &Path, progress: &dyn Fn(Stage)) -> Result<Downloaded> {
    progress(Stage::Downloading(None));
    let mut child = crate::media_import::quiet_command(tool)
        .env("PYTHONIOENCODING", "utf-8")
        .args(["--no-playlist", "--playlist-items", "1", "--no-mtime", "--no-warnings", "--newline", "--encoding", "utf-8"])
        .args(["-f", "bestaudio[ext=m4a]/bestaudio[acodec^=mp4a]/bestaudio/best"])
        .args(["--no-simulate", "--progress"])
        .args(["--progress-template", "download:MOSTALGIA_PROGRESS %(progress.downloaded_bytes)s %(progress.total_bytes,progress.total_bytes_estimate)s"])
        .args(["--print", "MOSTALGIA_TITLE %(title)s"])
        .arg("-o")
        .arg(dir.join("audio.%(ext)s"))
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(tr("yt-dlp lässt sich nicht starten"))?;
    let stderr = child.stderr.take().context("yt-dlp errors")?;
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut s);
        s
    });
    let mut title = String::new();
    let mut last = std::time::Instant::now();
    for line in BufReader::new(child.stdout.take().context("yt-dlp output")?).split(b'\n') {
        let line = String::from_utf8_lossy(&line?).trim().to_string();
        if let Some(t) = line.strip_prefix("MOSTALGIA_TITLE ") {
            title = t.trim().to_string();
        } else if let Some(p) = line.strip_prefix("MOSTALGIA_PROGRESS ") {
            if last.elapsed() >= Duration::from_millis(200) {
                last = std::time::Instant::now();
                progress(Stage::Downloading(parse_progress(p)));
            }
        }
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        let reason = errors.lines().rev().find(|l| l.contains("ERROR")).or_else(|| errors.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("").trim();
        let reason = reason.strip_prefix("ERROR:").unwrap_or(reason).trim();
        // Without yt-dlp's "please report this issue …" tail.
        let reason = reason.split("; please report").next().unwrap_or(reason).trim();
        bail!("yt-dlp: {}", if reason.is_empty() { status.to_string() } else { reason.to_string() });
    }
    let file = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.file_stem().is_some_and(|s| s == "audio") && !p.extension().is_some_and(|e| e == "part" || e == "ytdl"))
        .context(tr("yt-dlp hat keine Datei geliefert"))?;
    if title.is_empty() || title == "NA" {
        title = tr("Video").to_string();
    }
    Ok(Downloaded { file, title })
}

/// "1048576 4194304" (bytes so far, in all; "NA" when unknown) → 0.25.
fn parse_progress(s: &str) -> Option<f32> {
    let mut parts = s.split_whitespace().map(|p| p.parse::<f64>().ok());
    let (done, total) = (parts.next()??, parts.next()??);
    (total > 0.0).then(|| (done / total).clamp(0.0, 1.0) as f32)
}

/// Imports the video at `url` as a meeting started at `started_at`:
/// downloads its sound, then imports it like a file (see
/// `media_import::import`) under the video's title, with the engine
/// `engine` makes (only once the download worked). Returns what was saved
/// and that title.
pub fn import(
    url: &str,
    started_at: DateTime<Local>,
    engine: impl FnOnce() -> Result<crate::transcribe::Engine>,
    opts: &crate::finalize::FinalPassOptions,
    language: &str,
    progress: &(dyn Fn(Stage) + Sync),
) -> Result<(crate::finalize::Saved, String)> {
    let tool = ensure_tool(progress)?;
    let scratch = crate::paths::data_dir().join("downloads").join(format!("link-{}-{}", std::process::id(), Local::now().timestamp_millis()));
    std::fs::create_dir_all(&scratch).with_context(|| format!("creating {}", scratch.display()))?;
    let result = (|| {
        let got = download(&tool, url, &scratch, progress)?;
        let mut engine = engine()?;
        let saved = crate::media_import::import(&got.file, &got.title, started_at, &mut engine, opts, language, &|s| progress(Stage::Import(s)))?;
        Ok((saved, got.title))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// The languages picked for an import applied to `config`: none keeps its
/// settings, one is transcribed in that language, several are detected
/// among themselves (auto, limited to those).
pub fn apply_languages(config: &mut crate::config::Config, picked: &[String]) {
    match picked {
        [] => {}
        [one] => config.language = one.clone(),
        many => {
            config.language = "auto".into();
            config.meeting_languages = many.to_vec();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        assert_eq!(normalize(" https://www.youtube.com/watch?v=abc "), Some("https://www.youtube.com/watch?v=abc".into()));
        assert_eq!(normalize("youtu.be/abc"), Some("https://youtu.be/abc".into()));
        assert_eq!(normalize("hello world"), None);
        assert_eq!(normalize("notalink"), None);
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn progress_lines() {
        assert_eq!(parse_progress("1048576 4194304"), Some(0.25));
        assert_eq!(parse_progress("1048576 NA"), None);
        assert_eq!(parse_progress("5 0"), None);
    }

    #[test]
    fn picked_languages() {
        let mut c = crate::config::Config::default();
        c.language = "de".into();
        apply_languages(&mut c, &[]);
        assert_eq!(c.language, "de");
        apply_languages(&mut c, &["ar".into()]);
        assert_eq!(c.language, "ar");
        apply_languages(&mut c, &["ar".into(), "en".into()]);
        assert_eq!((c.language.as_str(), c.meeting_languages.clone()), ("auto", vec!["ar".to_string(), "en".to_string()]));
    }
}
