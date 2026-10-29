//! Updating the AppImage in place from the latest GitHub release. The
//! release's `.zsync` file (made for AppImageUpdate) carries the SHA-1 of
//! the new AppImage, so comparing it with the running file says whether
//! there's anything new without relying on version numbers. The update
//! downloads the whole AppImage next to the old one, checks it against
//! that SHA-1 and renames it over; the running app keeps its mounted copy
//! until it restarts.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const LATEST: &str = "https://api.github.com/repos/Mostaxd/Mostalgia/releases/latest";
const ASSET: &str = "Mostalgia-x86_64.AppImage";

/// A release whose AppImage differs from the running one.
#[derive(Clone, Debug)]
pub struct Update {
    /// "v0.2.1"
    pub tag: String,
    url: String,
    sha1: String,
    pub size: u64,
}

/// The AppImage this process runs from (its runtime sets `$APPIMAGE`).
pub fn running_appimage() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPIMAGE")?);
    path.is_file().then_some(path)
}

fn client(timeout: Option<Duration>) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .user_agent(concat!("Mostalgia/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Asks GitHub for the latest release. `None` when this isn't an AppImage
/// or it already is that release.
pub fn check() -> Result<Option<Update>> {
    let Some(appimage) = running_appimage() else { return Ok(None) };
    let client = client(Some(Duration::from_secs(30)))?;
    let release: Release = client.get(LATEST).send()?.error_for_status()?.json()?;
    let url = |name: &str| release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    let (Some(url), Some(zsync)) = (url(ASSET), url(&format!("{ASSET}.zsync"))) else {
        return Ok(None);
    };
    // Never an older release over a newer build: it would open a library
    // that build may have upgraded past what the release can read.
    if is_older(&release.tag_name, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let header = client.get(zsync).send()?.error_for_status()?.bytes()?;
    let (sha1, size) = zsync_header(&header).context("unreadable .zsync")?;
    if size == fs::metadata(&appimage)?.len() && sha1 == sha1_of(&mut File::open(&appimage)?)? {
        return Ok(None);
    }
    Ok(Some(Update { tag: release.tag_name, url, sha1, size }))
}

/// Whether release `tag` ("v0.2.1") is an older version than `current`
/// ("0.3.0"); a tag that isn't a version isn't.
fn is_older(tag: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        let v = v.trim_start_matches('v');
        v.split('-').next()?.split('.').map(|n| n.parse().ok()).collect()
    };
    matches!((parse(tag), parse(current)), (Some(t), Some(c)) if t < c)
}

/// The SHA-1 and length from a .zsync file's text header (it ends at the
/// first empty line; the block checksums follow).
fn zsync_header(data: &[u8]) -> Option<(String, u64)> {
    let end = data.windows(2).position(|w| w == b"\n\n")?;
    let text = std::str::from_utf8(&data[..end]).ok()?;
    let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim);
    let sha1 = field("SHA-1:")?.to_ascii_lowercase();
    let size = field("Length:")?.parse().ok()?;
    (sha1.len() == 40).then_some((sha1, size))
}

fn sha1_of(reader: &mut impl Read) -> Result<String> {
    let mut hash = sha1_smol::Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(hash.digest().to_string());
        }
        hash.update(&buf[..n]);
    }
}

/// Downloads the update and puts it in place of the running AppImage,
/// calling `progress` with bytes done. Blocks; `cancel` stops it.
pub fn install(update: &Update, cancel: &AtomicBool, mut progress: impl FnMut(u64)) -> Result<()> {
    let target = running_appimage().context("not running from an AppImage")?;
    let dir = target.parent().context("AppImage has no folder")?;
    let part = dir.join(format!(".{}.update", target.file_name().unwrap_or_default().to_string_lossy()));
    let result = download(update, &part, cancel, &mut progress).and_then(|()| replace(&part, &target));
    if result.is_err() {
        fs::remove_file(&part).ok();
    }
    result
}

fn download(update: &Update, part: &Path, cancel: &AtomicBool, progress: &mut impl FnMut(u64)) -> Result<()> {
    let mut file = File::create(part).with_context(|| format!("{}", part.parent().unwrap_or(part).display()))?;
    let mut response = client(None)?.get(&update.url).send()?.error_for_status()?;
    let mut hash = sha1_smol::Sha1::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    let mut last = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("{}", crate::i18n::tr("abgebrochen"));
        }
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hash.update(&buf[..n]);
        got += n as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            last = std::time::Instant::now();
            progress(got);
        }
    }
    file.sync_all()?;
    progress(got);
    if got != update.size || hash.digest().to_string() != update.sha1 {
        bail!("download doesn't match the release ({got} of {} bytes)", update.size);
    }
    Ok(())
}

fn replace(part: &Path, target: &Path) -> Result<()> {
    // Keep the old file's permissions (it was made executable by the user).
    fs::set_permissions(part, fs::metadata(target)?.permissions())?;
    fs::rename(part, target)?;
    Ok(())
}

/// Starts the (new) AppImage; the caller quits right after.
pub fn restart() -> Result<()> {
    let appimage = running_appimage().context("not running from an AppImage")?;
    std::process::Command::new(appimage)
        // Not the old mount's libraries.
        .env_remove("LD_LIBRARY_PATH")
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_offers_an_older_release() {
        assert!(is_older("v0.2.1", "0.3.0"));
        assert!(is_older("v0.2.1", "0.2.10"));
        assert!(!is_older("v0.2.1", "0.2.1"));
        assert!(!is_older("v0.3.0", "0.2.1"));
        assert!(!is_older("nightly", "0.2.1"));
    }

    #[test]
    fn reads_zsync_header() {
        let file = b"zsync: 0.6.2\nFilename: Mostalgia-x86_64.AppImage\nBlocksize: 4096\nLength: 66140664\nHash-Lengths: 2,2,5\nURL: Mostalgia-x86_64.AppImage\nSHA-1: 0A4D55A8D778E5022FAB701977C5D840BBC486D0\n\n\x01\x02\x00binary";
        assert_eq!(zsync_header(file), Some(("0a4d55a8d778e5022fab701977c5d840bbc486d0".into(), 66140664)));
        assert_eq!(zsync_header(b"Length: 1\nSHA-1: 12\n\n"), None);
    }

    #[test]
    fn hashes_like_sha1sum() {
        assert_eq!(sha1_of(&mut &b"abc"[..]).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
