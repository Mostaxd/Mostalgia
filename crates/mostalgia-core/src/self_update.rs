//! Updating the AppImage in place from the latest GitHub release. The
//! release's `.zsync` file (made for AppImageUpdate) carries the SHA-1 of
//! the new AppImage, so comparing it with the running file says whether
//! there's anything new without relying on version numbers. The update
//! downloads the whole AppImage next to the old one, checks it against
//! that SHA-1 and renames it over; the running app keeps its mounted copy
//! until it restarts.
//!
//! On Windows the app runs from the unpacked release zip (its folder has
//! `build-variant.txt`). There the update downloads the zip, checks it
//! against the release's `.sha256`, unpacks it and swaps the files in:
//! running programs and loaded DLLs can't be overwritten but can be
//! renamed, so each old file becomes `<name>.old` (removed on the next
//! start, see [`cleanup`]) and the new one takes its place. The folder
//! stays the same, so shortcuts keep working.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const LATEST: &str = "https://api.github.com/repos/Mostaxd/Mostalgia/releases/latest";
const ASSET: &str = "Mostalgia-x86_64.AppImage";
/// The Windows release zip, and its checksum file (`<sha256>  <name>`).
const WIN_ASSET: &str = "Mostalgia-windows-x86_64.zip";
/// In every Windows zip: tells an unpacked release from a dev build.
const WIN_MARKER: &str = "build-variant.txt";
/// Written after an update: the SHA-256 of the zip it came from.
const WIN_INSTALLED: &str = "release.sha256";

/// A release whose AppImage differs from the running one.
#[derive(Clone, Debug)]
pub struct Update {
    /// "v0.2.1"
    pub tag: String,
    url: String,
    digest: Digest,
    pub size: u64,
}

#[derive(Clone, Debug)]
enum Digest {
    /// The AppImage's, from its .zsync.
    Sha1(String),
    /// The Windows zip's, from its .sha256 (older releases have none:
    /// then only the size is checked).
    Sha256(Option<String>),
}

/// The AppImage this process runs from (its runtime sets `$APPIMAGE`).
pub fn running_appimage() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPIMAGE")?);
    path.is_file().then_some(path)
}

/// The unpacked Windows release this process runs from (its folder).
pub fn running_install() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    dir.join(WIN_MARKER).is_file().then_some(dir)
}

/// Whether this copy can update itself (an AppImage, or the Windows zip).
pub fn updatable() -> bool {
    running_appimage().is_some() || running_install().is_some()
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
    #[serde(default)]
    size: u64,
}

/// Asks GitHub for the latest release. `None` when this copy can't update
/// itself or it already is that release.
pub fn check() -> Result<Option<Update>> {
    if let Some(appimage) = running_appimage() {
        return check_appimage(&appimage);
    }
    match running_install() {
        Some(dir) => check_zip(&dir),
        None => Ok(None),
    }
}

fn check_appimage(appimage: &Path) -> Result<Option<Update>> {
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
    if size == fs::metadata(appimage)?.len() && sha1 == sha1_of(&mut File::open(appimage)?)? {
        return Ok(None);
    }
    Ok(Some(Update { tag: release.tag_name, url, digest: Digest::Sha1(sha1), size }))
}

fn check_zip(dir: &Path) -> Result<Option<Update>> {
    let client = client(Some(Duration::from_secs(30)))?;
    let release: Release = client.get(LATEST).send()?.error_for_status()?.json()?;
    let Some(zip) = release.assets.iter().find(|a| a.name == WIN_ASSET) else { return Ok(None) };
    if is_older(&release.tag_name, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let sha256 = match release.assets.iter().find(|a| a.name == format!("{WIN_ASSET}.sha256")) {
        Some(a) => Some(parse_sha256(&client.get(&a.browser_download_url).send()?.error_for_status()?.text()?).context("unreadable .sha256")?),
        None => None,
    };
    let installed = fs::read_to_string(dir.join(WIN_INSTALLED)).ok().and_then(|s| parse_sha256(&s));
    if !zip_is_new(&release.tag_name, sha256.as_deref(), installed.as_deref()) {
        return Ok(None);
    }
    Ok(Some(Update { tag: release.tag_name, url: zip.browser_download_url.clone(), digest: Digest::Sha256(sha256), size: zip.size }))
}

/// Whether the release's zip is something new for this install: a
/// different zip than the one it was updated from, or (unknown) a newer
/// version than this one.
fn zip_is_new(tag: &str, release_sha256: Option<&str>, installed_sha256: Option<&str>) -> bool {
    match (release_sha256, installed_sha256) {
        (Some(r), Some(i)) => r != i,
        _ => is_older(env!("CARGO_PKG_VERSION"), tag),
    }
}

/// The hash from a `sha256sum` line (`<64 hex>  <name>`).
fn parse_sha256(text: &str) -> Option<String> {
    let h = text.split_whitespace().next()?.to_ascii_lowercase();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
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

/// Downloads the update and puts it in place of the running AppImage (or
/// the running Windows install), calling `progress` with bytes done.
/// Blocks; `cancel` stops it.
pub fn install(update: &Update, cancel: &AtomicBool, mut progress: impl FnMut(u64)) -> Result<()> {
    if running_appimage().is_none()
        && let Some(dir) = running_install()
    {
        return install_zip(update, &dir, cancel, &mut progress);
    }
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
    let mut sha1 = sha1_smol::Sha1::new();
    let mut sha256 = <sha2::Sha256 as sha2::Digest>::new();
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
        match update.digest {
            Digest::Sha1(_) => sha1.update(&buf[..n]),
            Digest::Sha256(_) => sha2::Digest::update(&mut sha256, &buf[..n]),
        }
        got += n as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            last = std::time::Instant::now();
            progress(got);
        }
    }
    file.sync_all()?;
    progress(got);
    let matches = match &update.digest {
        Digest::Sha1(want) => sha1.digest().to_string() == *want,
        Digest::Sha256(want) => want.as_ref().is_none_or(|w| hex(&sha2::Digest::finalize(sha256)) == *w),
    };
    if got != update.size || !matches {
        bail!("download doesn't match the release ({got} of {} bytes)", update.size);
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn install_zip(update: &Update, dir: &Path, cancel: &AtomicBool, progress: &mut impl FnMut(u64)) -> Result<()> {
    let part = dir.join(".update.zip");
    let staging = dir.join(".update");
    let result = (|| {
        download(update, &part, cancel, progress)?;
        fs::remove_dir_all(&staging).ok();
        unpack(&part, &staging)?;
        // The zip holds one folder, `Mostalgia/`.
        let root = staging.join("Mostalgia");
        swap_in(if root.is_dir() { &root } else { &staging }, dir)?;
        if let Digest::Sha256(Some(sha)) = &update.digest {
            fs::write(dir.join(WIN_INSTALLED), format!("{sha}  {WIN_ASSET}\n"))?;
        }
        Ok(())
    })();
    fs::remove_file(&part).ok();
    fs::remove_dir_all(&staging).ok();
    result
}

/// Unpacks a zip into `to` (only plain relative paths).
fn unpack(zip_path: &Path, to: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(zip_path)?).context("reading the update")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else { continue };
        let path = to.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut entry, &mut File::create(&path)?)?;
    }
    Ok(())
}

/// Moves every file of `new` into `dir`, renaming a file it replaces to
/// `<name>.old` first (which works for a running program). On an error
/// everything moved so far is put back.
fn swap_in(new: &Path, dir: &Path) -> Result<()> {
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let result = (|| -> Result<()> {
        for entry in fs::read_dir(new)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let target = dir.join(entry.file_name());
            let old = if target.exists() {
                let old = old_name(&target);
                fs::rename(&target, &old).with_context(|| format!("replacing {}", target.display()))?;
                Some(old)
            } else {
                None
            };
            done.push((target.clone(), old));
            fs::rename(entry.path(), &target).with_context(|| format!("placing {}", target.display()))?;
        }
        Ok(())
    })();
    if result.is_err() {
        for (target, old) in done.into_iter().rev() {
            if let Some(old) = old {
                fs::remove_file(&target).ok();
                fs::rename(&old, &target).ok();
            } else {
                fs::remove_file(&target).ok();
            }
        }
    }
    result
}

/// A free `<name>.old` (`.old2`, … if an earlier one is still in use).
fn old_name(target: &Path) -> PathBuf {
    let name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
    for n in 0..100 {
        let suffix = if n == 0 { ".old".to_string() } else { format!(".old{n}") };
        let old = target.with_file_name(format!("{name}{suffix}"));
        if !old.exists() || fs::remove_file(&old).is_ok() {
            return old;
        }
    }
    target.with_file_name(format!("{name}.old-{}", std::process::id()))
}

/// Removes what an update on Windows left behind (`*.old` files of the
/// previous version). Call at startup; files still in use stay for next time.
pub fn cleanup() {
    if let Some(dir) = running_install() {
        cleanup_dir(&dir);
    }
}

fn cleanup_dir(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains(".old") && (name.ends_with(".old") || name.rsplit(".old").next().is_some_and(|s| s.chars().all(|c| c.is_ascii_digit() || c == '-'))) {
            fs::remove_file(entry.path()).ok();
        }
    }
}

fn replace(part: &Path, target: &Path) -> Result<()> {
    // Keep the old file's permissions (it was made executable by the user).
    fs::set_permissions(part, fs::metadata(target)?.permissions())?;
    fs::rename(part, target)?;
    Ok(())
}

/// Starts the (new) AppImage or Windows app; the caller quits right after.
pub fn restart() -> Result<()> {
    if running_appimage().is_none()
        && let Some(dir) = running_install()
    {
        std::process::Command::new(dir.join("mostalgia-app.exe")).current_dir(&dir).spawn()?;
        return Ok(());
    }
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
    fn reads_sha256_files() {
        let h = "9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08";
        assert_eq!(parse_sha256(&format!("{h}  Mostalgia-windows-x86_64.zip\n")), Some(h.to_ascii_lowercase()));
        assert_eq!(parse_sha256("abc  x.zip"), None);
        let mut s = <sha2::Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut s, b"test");
        assert_eq!(hex(&sha2::Digest::finalize(s)), h.to_ascii_lowercase());
    }

    #[test]
    fn a_zip_is_new_when_it_differs_or_is_newer() {
        assert!(zip_is_new("v0.2.0", Some("aa"), Some("bb")), "a rebuilt release");
        assert!(!zip_is_new("v99.0.0", Some("aa"), Some("aa")), "the one installed");
        assert!(zip_is_new("v99.0.0", None, None));
        assert!(!zip_is_new(&format!("v{}", env!("CARGO_PKG_VERSION")), Some("aa"), None), "same version, unknown install");
    }

    /// The new files take the old ones' places, the old ones stay as
    /// `.old` until cleaned up.
    #[test]
    fn update_files_swap_in() -> Result<()> {
        let root = std::env::temp_dir().join(format!("mostalgia-swap-{}", std::process::id()));
        let (dir, new) = (root.join("app"), root.join("new"));
        fs::create_dir_all(&dir)?;
        fs::create_dir_all(&new)?;
        fs::write(dir.join("mostalgia-app.exe"), "v1")?;
        fs::write(dir.join("keep.txt"), "mine")?;
        fs::write(new.join("mostalgia-app.exe"), "v2")?;
        fs::write(new.join("added.dll"), "dll")?;
        swap_in(&new, &dir)?;
        assert_eq!(fs::read_to_string(dir.join("mostalgia-app.exe"))?, "v2");
        assert_eq!(fs::read_to_string(dir.join("mostalgia-app.exe.old"))?, "v1");
        assert_eq!(fs::read_to_string(dir.join("added.dll"))?, "dll");
        assert_eq!(fs::read_to_string(dir.join("keep.txt"))?, "mine");
        cleanup_dir(&dir);
        assert!(!dir.join("mostalgia-app.exe.old").exists());
        assert!(dir.join("keep.txt").exists());

        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn unpacks_the_release_zip() -> Result<()> {
        let root = std::env::temp_dir().join(format!("mostalgia-unzip-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let zip_path = root.join("u.zip");
        {
            let mut z = zip::ZipWriter::new(File::create(&zip_path)?);
            let opts = zip::write::SimpleFileOptions::default();
            z.add_directory("Mostalgia/", opts)?;
            z.start_file("Mostalgia/mostalgia.exe", opts)?;
            z.write_all(b"exe")?;
            z.start_file("../evil.txt", opts)?;
            z.write_all(b"no")?;
            z.finish()?;
        }
        unpack(&zip_path, &root.join("out"))?;
        assert_eq!(fs::read_to_string(root.join("out/Mostalgia/mostalgia.exe"))?, "exe");
        assert!(!root.join("evil.txt").exists());
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn hashes_like_sha1sum() {
        assert_eq!(sha1_of(&mut &b"abc"[..]).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
